// Synthetic native-header fixture; all writes are test setup under the isolated
// runner's TEMP. Production list code does not create or modify any path.
struct CatalogFixture {
    root: PathBuf,
    left: PathBuf,
    right: PathBuf,
    expected: Vec<String>,
    files: Vec<(PathBuf, Vec<u8>)>,
}
impl CatalogFixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!("acp-list-pages-{}", uuid::Uuid::new_v4()));
        let root = base.join("sessions");
        let left = base.join("left");
        let right = base.join("right");
        std::fs::create_dir_all(&left).unwrap();
        std::fs::create_dir_all(&right).unwrap();
        let mut fixture = Self {
            root,
            left,
            right,
            expected: Vec::new(),
            files: Vec::new(),
        };
        for i in 0..135 {
            let id = uuid::Uuid::new_v4().to_string();
            let cwd = if i % 2 == 0 {
                &fixture.left
            } else {
                &fixture.right
            };
            let cwd_text = cwd.to_str().unwrap();
            let stripped = cwd_text
                .strip_prefix('/')
                .or_else(|| cwd_text.strip_prefix('\\'))
                .unwrap_or(cwd_text);
            let encoded = format!("--{}--", stripped.replace(['/', '\\', ':'], "-"));
            let directory = fixture.root.join(&id).join(encoded);
            std::fs::create_dir_all(&directory).unwrap();
            let timestamp = utc_date(i).unwrap().replace([':', '.'], "-");
            let file = directory.join(format!("{timestamp}_{id}.jsonl"));
            let bytes = format!(
                "{}\nnot-json-body: MUST NOT be parsed\n",
                json!({"kind":"header","version":4,"id":id,"cwd":cwd,"createdAt":i})
            )
            .into_bytes();
            std::fs::write(&file, &bytes).unwrap();
            fixture.files.push((file, bytes));
            fixture.expected.push(id);
        }
        fixture.expected.reverse();
        fixture
    }
}
impl Drop for CatalogFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.root.parent().unwrap());
    }
}

#[tokio::test]
async fn synthetic_135_headers_paginate_fixed_50_filter_and_preserve_files() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let fixture = CatalogFixture::new();
    let (output, peer) = tokio::io::duplex(65_536);
    let writer = Arc::new(Writer::with_output(output));
    let mut lines = BufReader::new(peer).lines();
    let mut tasks = ListTasks::new(Ok(fixture.root.clone()));
    for filter in [
        None,
        Some(fixture.left.clone()),
        Some(fixture.right.clone()),
    ] {
        let mut cursor: Option<String> = None;
        let mut retryable_cursor = false;
        let mut ids = Vec::new();
        let mut pages = 0;
        loop {
            pages += 1;
            assert!(pages < 100, "must finish bounded discovery");
            let mut params = json!({"limit":1}); // ignored: wire remains fixed50
            if let Some(filter) = &filter {
                params["cwd"] = json!(filter);
            }
            if let Some(cursor) = &cursor {
                params["cursor"] = json!(cursor);
            }
            tasks
                .start(json!(pages), pages.to_string(), &params, writer.clone())
                .await;
            let response: Value = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap()
            })
            .await
            .unwrap();
            tasks.reap().await;
            assert!(response.get("error").is_none(), "{response}");
            let result = &response["result"];
            let sessions = result["sessions"].as_array().unwrap();
            assert!(sessions.len() <= 50);
            for session in sessions {
                assert!(session.get("title").is_none());
                assert!(session.get("createdAt").is_none());
                assert!(session["updatedAt"].as_str().unwrap().ends_with('Z'));
                if let Some(filter) = &filter {
                    assert_eq!(session["cwd"], json!(filter));
                }
                ids.push(session["sessionId"].as_str().unwrap().to_string());
            }
            // Only tokens issued by a completed page are retryable. A scan
            // token remains single-use even when consuming it finishes discovery.
            if retryable_cursor && !sessions.is_empty() {
                tasks
                    .start(json!("retry"), "retry".into(), &params, writer.clone())
                    .await;
                let retry: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                tasks.reap().await;
                assert_eq!(retry["result"], *result);
            }
            retryable_cursor = !sessions.is_empty();
            cursor = result["nextCursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        let expected: Vec<_> = fixture
            .expected
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                // Descending 134..0, even positions correspond to the left cwd.
                filter.is_none() || (filter.as_ref() == Some(&fixture.left)) == (i % 2 == 0)
            })
            .map(|(_, id)| id.clone())
            .collect();
        assert_eq!(ids, expected);
        if filter.is_none() {
            assert!(pages >= 3);
        }
    }
    tasks.shutdown().await;
    for (path, bytes) in &fixture.files {
        assert_eq!(&std::fs::read(path).unwrap(), bytes);
    }
}
