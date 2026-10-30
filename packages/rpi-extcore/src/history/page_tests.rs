use super::*;
use tokio::io::{AsyncBufReadExt, BufReader};

fn data() -> Value {
    json!({"messages":[{"entryId":"b","role":"user"}],"total":2,"leafId":"b","firstId":"b","lastId":"b","beforeId":null,"hasMore":true})
}
#[test]
fn malformed_pages_are_never_empty_success() {
    assert!(validate_page(data(), None, 1, None).is_ok());
    for (key, value) in [
        ("messages", json!([])),
        ("firstId", json!("wrong")),
        ("lastId", json!("wrong")),
        ("beforeId", json!("wrong")),
        ("total", json!(0)),
        ("hasMore", json!(false)),
        ("leafId", json!(42)),
    ] {
        let mut bad = data();
        bad[key] = value;
        assert!(validate_page(bad, None, 1, None).is_err(), "{key}");
    }
    let anchor = validate_page(data(), None, 1, None).unwrap().anchor;
    for key in ["total", "leafId"] {
        let mut bad = data();
        bad[key] = json!("changed");
        assert!(validate_page(bad, None, 1, Some(&anchor)).is_err());
    }
    assert!(validate_page(data(), Some("b"), 1, None).is_err());
    assert!(validate_page(data(), None, 129, None).is_err());
    let mut bad = data();
    bad["messages"] = json!([{"entryId":"b","role":"user"},{"entryId":"b","role":"user"}]);
    assert!(validate_page(bad, None, 2, None).is_err());
    let mut bad = data();
    bad["messages"][0]["content"] = json!("x".repeat(MAX_PAGE_BYTES));
    assert!(validate_page(bad, None, 1, None).is_err());
    let empty = json!({"messages":[],"total":0,"leafId":null,"firstId":null,"lastId":null,"beforeId":null,"hasMore":false});
    assert!(validate_page(empty, None, 128, None).is_ok());
}

fn fixture(scenario: &'static str, pages: bool) -> (Arc<HostSession>, tokio::task::JoinHandle<()>) {
    let (writer, peer) = tokio::io::duplex(4096);
    let (host, replies) = HostSession::test_connection_with_ready(
        writer,
        32,
        json!({"supportedCommands":if pages {vec!["get_messages"]} else {vec![]} }),
    );
    let task = tokio::spawn(async move {
        let mut lines = BufReader::new(peer).lines();
        let mut calls = 0;
        while let Some(line) = lines.next_line().await.unwrap() {
            let request: Value = serde_json::from_str(&line).unwrap();
            calls += 1;
            if scenario == "stall" {
                continue;
            }
            let data = if request["type"] == "get_state" {
                assert!(!pages, "bad page replies must not trigger fallback");
                json!({"leafId":if scenario=="changed-fallback" && calls>1 {"other"} else {"b"}})
            } else {
                assert_eq!(request["type"], "get_messages");
                let end = request["beforeId"]
                    .as_str()
                    .map(|s| s.parse::<usize>().unwrap())
                    .unwrap_or(513);
                let limit = request["limit"].as_u64().unwrap() as usize;
                assert!((1..=128).contains(&limit));
                let start = end.saturating_sub(limit);
                let mut data = json!({"messages":(start..end).map(|i|json!({"entryId":i.to_string(),"role":"user","content":i.to_string()})).collect::<Vec<_>>(),"total":513,"leafId":"512","beforeId":request["beforeId"],"firstId":start.to_string(),"lastId":(end-1).to_string(),"hasMore":start>0});
                if calls > 1 {
                    match scenario {
                        "echo" => data["beforeId"] = Value::Null,
                        "branch" => data["leafId"] = json!("other"),
                        "total" => data["total"] = json!(514),
                        "repeat" => {
                            data["messages"][1]["entryId"] = json!("512");
                        }
                        "incomplete" => data["hasMore"] = json!(false),
                        "bad-final" if limit == 1 => data["leafId"] = json!("other"),
                        _ => {}
                    }
                }
                data
            };
            replies
                .send(json!({"type":"response","id":request["id"],"success":true,"data":data}))
                .unwrap();
        }
    });
    (host, task)
}
#[tokio::test]
async fn traverses_513_oldest_first_and_holds_existing_lease() {
    let (host, task) = fixture("healthy", true);
    let read = HistoryRead::begin(host.clone()).await.unwrap();
    assert!(host.try_run_lease().is_err());
    let mut spool = read.prepare(None).await.unwrap();
    let path = spool.path.clone();
    let mut ids = Vec::new();
    while let Some(page) = spool.pop().unwrap() {
        assert!(page.len() <= 128);
        ids.extend(
            page.into_iter()
                .map(|m| m["entryId"].as_str().unwrap().parse::<usize>().unwrap()),
        );
    }
    assert_eq!(ids, (0..513).collect::<Vec<_>>());
    assert!(host.try_run_lease().is_err());
    drop(spool);
    assert!(!path.exists());
    drop(read);
    assert!(host.try_run_lease().is_ok());
    task.abort();
}
#[tokio::test]
async fn rejects_changed_incomplete_and_repeated_traversals_without_fallback() {
    for scenario in [
        "echo",
        "branch",
        "total",
        "repeat",
        "incomplete",
        "bad-final",
    ] {
        let (host, task) = fixture(scenario, true);
        let read = HistoryRead::begin(host.clone()).await.unwrap();
        assert!(
            read.prepare(Some(Path::new("must-not-open")))
                .await
                .is_err(),
            "{scenario}"
        );
        drop(read);
        assert!(host.try_run_lease().is_ok());
        task.abort();
    }
}
#[tokio::test]
async fn preparation_deadline_and_cancellation_release_lease() {
    let (host, task) = fixture("stall", true);
    let mut read = HistoryRead::begin(host.clone()).await.unwrap();
    read.deadline = Instant::now() + Duration::from_millis(25);
    assert!(read.prepare(None).await.unwrap_err().contains("timed out"));
    drop(read);
    assert!(host.try_run_lease().is_ok());
    let read = HistoryRead::begin(host.clone()).await.unwrap();
    {
        let future = read.prepare(None);
        tokio::pin!(future);
        assert!(tokio::time::timeout(Duration::from_millis(10), &mut future)
            .await
            .is_err());
        host.abort_run().await.unwrap();
        assert!(future.await.unwrap_err().contains("cancelled"));
    }
    drop(read);
    assert!(host.try_run_lease().is_ok());
    task.abort();
}
#[tokio::test]
async fn spool_budgets_privacy_and_cancellation_cleanup() {
    let mut spool = HistorySpool::create().unwrap();
    let path = spool.path.clone();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    spool.push(&[json!({"entryId":"new"})]).unwrap();
    spool.push(&[json!({"entryId":"old"})]).unwrap();
    assert_eq!(spool.pop().unwrap().unwrap()[0]["entryId"], "old");
    assert!(spool.push(&vec![Value::Null; 129]).is_err());
    assert!(spool.push(&[json!("x".repeat(MAX_PAGE_BYTES))]).is_err());
    spool.end = MAX_SPOOL_BYTES;
    assert!(spool.push(&[Value::Null]).is_err());
    let mut future = Box::pin(async move {
        let _owner = spool;
        std::future::pending::<()>().await;
    });
    assert!(tokio::time::timeout(Duration::from_millis(1), &mut future)
        .await
        .is_err());
    drop(future);
    assert!(!path.exists());
}
fn transcript() -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("history-fallback-{}.jsonl", uuid::Uuid::new_v4()));
    std::fs::write(&path,concat!("{\"kind\":\"header\",\"version\":4}\n",
        "{\"kind\":\"entry\",\"id\":\"a\",\"parentId\":null,\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"first\"}}\n",
        "{\"kind\":\"entry\",\"id\":\"b\",\"parentId\":\"a\",\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"content\":\"last\"}}\n")).unwrap();
    path
}
#[tokio::test]
async fn old_host_bounded_fallback_reuses_branch_validation() {
    let path = transcript();
    let (host, task) = fixture("healthy", false);
    let read = HistoryRead::begin(host).await.unwrap();
    let mut spool = read.prepare(Some(&path)).await.unwrap();
    let messages = spool.pop().unwrap().unwrap();
    assert_eq!(messages[0]["content"], "first");
    assert_eq!(messages[1]["content"], "last");
    for content in ["{bad json}","{\"kind\":\"header\",\"version\":4}\n{\"kind\":\"entry\",\"id\":\"b\",\"parentId\":\"b\"}\n"] {
        std::fs::write(&path,content).unwrap(); assert!(read.prepare(Some(&path)).await.is_err());
    }
    std::fs::write(&path, vec![b'x'; MAX_PAGE_BYTES + 1]).unwrap();
    assert!(read.prepare(Some(&path)).await.is_err());
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(b"{\"kind\":\"header\",\"version\":4}\n")
        .unwrap();
    let mut blank_lines = vec![b' '; 1024 * 1024];
    *blank_lines.last_mut().unwrap() = b'\n';
    for _ in 0..64 {
        file.write_all(&blank_lines).unwrap();
    }
    assert!(read
        .prepare(Some(&path))
        .await
        .unwrap_err()
        .contains("budget"));
    drop(file);
    task.abort();
    std::fs::remove_file(path).unwrap();
    let path = transcript();
    let (host, task) = fixture("changed-fallback", false);
    let read = HistoryRead::begin(host).await.unwrap();
    assert!(read
        .prepare(Some(&path))
        .await
        .unwrap_err()
        .contains("branch changed"));
    task.abort();
    std::fs::remove_file(path).unwrap();
}
