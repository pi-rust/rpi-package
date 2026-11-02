use super::*;
use serde_json::json;
struct Fixture {
    root: PathBuf,
    cwd: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("session-catalog-{}", uuid::Uuid::new_v4()));
        let cwd = root.join("workspace");
        fs::create_dir_all(&cwd).unwrap();
        Self { root, cwd }
    }
    fn catalog(&self, limits: CatalogLimits) -> SessionCatalog {
        new(self.root.join("sessions"), limits).unwrap()
    }
    fn write(&self, id: &str, ms: i64) -> PathBuf {
        self.write_cwd(id, ms, &self.cwd)
    }
    fn write_cwd(&self, id: &str, ms: i64, cwd: &Path) -> PathBuf {
        let dir = self
            .root
            .join("sessions")
            .join(id)
            .join(encoded_cwd(cwd.to_str().unwrap()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(filename(ms, id).unwrap());
        fs::write(
            &file,
            format!(
                "{}\nNOT JSON BODY\n",
                json!({"kind":"header","version":4,"id":id,"createdAt":ms,"cwd":cwd})
            ),
        )
        .unwrap();
        file
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn query(cursor: Option<String>, limit: usize) -> CatalogQuery {
    CatalogQuery {
        cwd: None,
        cursor,
        limit,
    }
}
async fn all(c: &SessionCatalog, limit: usize) -> Vec<SessionSummary> {
    let mut result = vec![];
    let mut cursor = None;
    for _ in 0..10000 {
        let page = c
            .page(query(cursor, limit), CatalogControl::default())
            .await
            .unwrap();
        result.extend(page.sessions);
        cursor = page.next_cursor;
        if cursor.is_none() {
            return result;
        }
    }
    panic!("no progress")
}
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
#[test]
fn session_catalog_pure_validation_and_dates() {
    let f = Fixture::new();
    let absent = f.root.join("not-created");
    assert!(new(absent.clone(), CatalogLimits::default()).is_ok());
    assert!(!absent.exists());
    assert!(matches!(
        new(PathBuf::from("relative"), CatalogLimits::default()),
        Err(CatalogError::InvalidInput)
    ));
    let limits = CatalogLimits {
        max_workers: 3,
        ..CatalogLimits::default()
    };
    assert!(matches!(
        new(absent, limits),
        Err(CatalogError::InvalidInput)
    ));
    for (ms, date) in [
        (0, "1970-01-01T00-00-00-000Z"),
        (-1, "1969-12-31T23-59-59-999Z"),
        (-62167219200000, "0000-01-01T00-00-00-000Z"),
        (253402300799999, "9999-12-31T23-59-59-999Z"),
        (1700000000123, "2023-11-14T22-13-20-123Z"),
    ] {
        assert_eq!(filename(ms, "id").unwrap(), format!("{date}_id.jsonl"));
    }
    assert!(filename(-62167219200001, "id").is_none());
    assert!(filename(253402300800000, "id").is_none());
    assert!(canonical_uuid("00000000-0000-0000-0000-000000000000"));
    assert!(!canonical_uuid("AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA"));
}
#[tokio::test]
async fn session_catalog_missing_root_empty_no_writes_and_shutdown() {
    let f = Fixture::new();
    let c = f.catalog(CatalogLimits::default());
    assert!(all(&c, 50).await.is_empty());
    assert!(!f.root.join("sessions").exists());
    c.shutdown().await;
    c.shutdown().await;
    assert_eq!(
        c.page(query(None, 50), CatalogControl::default())
            .await
            .unwrap_err(),
        CatalogError::Cancelled
    );
}
#[tokio::test]
async fn session_catalog_resumable_sorted_frozen_idempotent_pages() {
    let f = Fixture::new();
    let mut expected = vec![];
    for ms in [0, 100, 100, -1] {
        let id = id();
        f.write(&id, ms);
        expected.push((ms, id));
    }
    expected.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let limits = CatalogLimits {
        step_entries: 1,
        step_read_bytes: 7,
        ..CatalogLimits::default()
    };
    let c = f.catalog(limits);
    let first = c
        .page(query(None, 1), CatalogControl::default())
        .await
        .unwrap();
    assert!(first.sessions.is_empty());
    let consumed = first.next_cursor.clone().unwrap();
    let second = c
        .page(query(first.next_cursor, 1), CatalogControl::default())
        .await
        .unwrap();
    assert_eq!(
        c.page(query(Some(consumed), 1), CatalogControl::default())
            .await
            .unwrap_err(),
        CatalogError::InvalidCursor
    );
    let mut cursor = second.next_cursor;
    let mut actual = second.sessions;
    while let Some(t) = cursor {
        let p = c
            .page(query(Some(t.clone()), 1), CatalogControl::default())
            .await
            .unwrap();
        if !p.sessions.is_empty()
            && !lock(&c.inner.state).generations[0]
                .data
                .as_ref()
                .unwrap()
                .scan
                .is_some()
        {
            // A scan continuation is single-use even if this step freezes.
            let is_page = lock(&c.inner.state).generations[0]
                .tokens
                .iter()
                .any(|v| v.value == t);
            if is_page {
                let retry = c
                    .page(query(Some(t), 1), CatalogControl::default())
                    .await
                    .unwrap();
                assert_eq!(p, retry);
                // Frozen pages survive transcript mutation and discovery additions.
                f.write(&id(), 500);
            }
        }
        actual.extend(p.sessions);
        cursor = p.next_cursor;
    }
    assert_eq!(
        actual
            .iter()
            .map(|s| (s.created_at_ms, s.id.clone()))
            .collect::<Vec<_>>(),
        expected
    );
    c.shutdown().await;
}
#[tokio::test]
async fn session_catalog_filters_deleted_cwd_and_cursor_binding() {
    let f = Fixture::new();
    let live = id();
    let deleted = id();
    f.write(&live, 0);
    let gone = f.root.join("deleted");
    f.write_cwd(&deleted, 1, &gone);
    let c = f.catalog(CatalogLimits::default());
    assert_eq!(all(&c, 50).await.len(), 2);
    let p = c
        .page(
            CatalogQuery {
                cwd: Some(f.cwd.clone()),
                cursor: None,
                limit: 50,
            },
            CatalogControl::default(),
        )
        .await
        .unwrap();
    assert_eq!(p.sessions.len(), 1);
    assert_eq!(p.sessions[0].id, live);
    assert_eq!(
        c.page(
            CatalogQuery {
                cwd: Some(gone),
                cursor: None,
                limit: 50
            },
            CatalogControl::default()
        )
        .await
        .unwrap_err(),
        CatalogError::InvalidInput
    );
    let p = c
        .page(query(None, 1), CatalogControl::default())
        .await
        .unwrap();
    let cursor = p.next_cursor.unwrap();
    assert_eq!(
        c.page(
            CatalogQuery {
                cwd: Some(f.cwd.clone()),
                cursor: Some(cursor.clone()),
                limit: 1
            },
            CatalogControl::default()
        )
        .await
        .unwrap_err(),
        CatalogError::InvalidCursor
    );
    assert_eq!(
        c.page(query(Some(cursor.clone()), 2), CatalogControl::default())
            .await
            .unwrap_err(),
        CatalogError::InvalidCursor
    );
    assert_eq!(
        c.page(query(Some(cursor), 1), CatalogControl::default())
            .await
            .unwrap()
            .sessions
            .len(),
        1
    );
    c.shutdown().await;
}
#[tokio::test]
async fn session_catalog_fail_closed_budgets() {
    for kind in 0..3 {
        let f = Fixture::new();
        f.write(&id(), 0);
        let limits = match kind {
            0 => CatalogLimits {
                total_entries: 2,
                ..CatalogLimits::default()
            },
            1 => CatalogLimits {
                total_read_bytes: 10,
                ..CatalogLimits::default()
            },
            _ => CatalogLimits {
                summary_bytes: 1,
                ..CatalogLimits::default()
            },
        };
        let c = f.catalog(limits);
        assert_eq!(
            c.page(query(None, 50), CatalogControl::default())
                .await
                .unwrap_err(),
            CatalogError::LimitExceeded
        );
        assert!(lock(&c.inner.state).generations.is_empty());
        c.shutdown().await;
    }
    let f = Fixture::new();
    let dir = f.root.join("sessions");
    fs::create_dir(&dir).unwrap();
    for n in 0..4 {
        fs::write(dir.join(format!("unrelated-{n}")), b"ignored").unwrap();
    }
    let c = f.catalog(CatalogLimits {
        total_entries: 3,
        ..CatalogLimits::default()
    });
    assert_eq!(
        c.page(query(None, 50), CatalogControl::default())
            .await
            .unwrap_err(),
        CatalogError::LimitExceeded
    );
    c.shutdown().await;
}
#[tokio::test]
async fn session_catalog_headers_identity_layout_duplicates_and_utf8() {
    for kind in 0..10 {
        let f = Fixture::new();
        let sid = id();
        let path = f.write(&sid, 0);
        let mut header = json!({"kind":"header","version":4,"id":sid,"createdAt":0,"cwd":f.cwd});
        match kind {
            0 => header["version"] = json!(3),
            1 => header["id"] = json!(id()),
            2 => header["cwd"] = json!("relative"),
            3 => header["createdAt"] = json!(1),
            4 => header["createdAt"] = json!(253402300800000i64),
            5 => header["kind"] = json!("entry"),
            6 => {
                let other = f.root.join("other");
                fs::create_dir(&other).unwrap();
                f.write_cwd(&sid, 0, &other);
            }
            7 => header["cwd"] = json!(f.root.join("wrong")),
            8 => {}
            9 => {}
            _ => unreachable!(),
        }
        fs::write(&path, format!("{header}\n")).unwrap();
        if kind == 8 {
            fs::write(&path, b"\xff\n").unwrap();
        }
        if kind == 9 {
            fs::write(&path, vec![b'x'; 65536]).unwrap();
        }
        let c = f.catalog(CatalogLimits::default());
        let e = c
            .page(query(None, 50), CatalogControl::default())
            .await
            .unwrap_err();
        assert_eq!(
            e,
            if kind == 6 {
                CatalogError::Conflict
            } else if kind == 9 {
                CatalogError::LimitExceeded
            } else {
                CatalogError::Unavailable
            },
            "case {kind}"
        );
        c.shutdown().await;
    }
}
#[tokio::test]
async fn session_catalog_duplicate_header_keys_fail_closed() {
    let f = Fixture::new();
    let sid = id();
    let path = f.write(&sid, 0);
    let header = json!({"kind":"header","version":4,"id":sid,"createdAt":0,"cwd":f.cwd});
    // Even identical duplicate values are ambiguous metadata, not a valid header.
    for key in ["kind", "version", "id", "createdAt", "cwd"] {
        let text = serde_json::to_string(&header).unwrap();
        let duplicate = format!("{{\"{key}\":{},{}\n", header[key], &text[1..]);
        fs::write(&path, duplicate).unwrap();
        let c = f.catalog(CatalogLimits::default());
        assert_eq!(
            c.page(query(None, 50), CatalogControl::default())
                .await
                .unwrap_err(),
            CatalogError::Unavailable,
            "duplicate {key}"
        );
        c.shutdown().await;
    }
}
#[tokio::test]
async fn session_catalog_header_exact_bound_and_no_body_parse() {
    let f = Fixture::new();
    let sid = id();
    let path = f.write(&sid, 0);
    let mut v =
        json!({"kind":"header","version":4,"id":sid,"createdAt":0,"cwd":f.cwd,"padding":""});
    let overhead = serde_json::to_vec(&v).unwrap().len() + 1;
    v["padding"] = json!("x".repeat(65536 - overhead));
    fs::write(&path, format!("{v}\n{}", "body not parsed".repeat(10000))).unwrap();
    let c = f.catalog(CatalogLimits::default());
    assert_eq!(all(&c, 50).await.len(), 1);
    c.shutdown().await;
    v["padding"] = json!("x".repeat(65537 - overhead));
    fs::write(path, format!("{v}\n")).unwrap();
    let c = f.catalog(CatalogLimits::default());
    assert_eq!(
        c.page(query(None, 50), CatalogControl::default())
            .await
            .unwrap_err(),
        CatalogError::LimitExceeded
    );
    c.shutdown().await;
}
#[tokio::test]
async fn session_catalog_eviction_expiry_and_forged_cursors() {
    let f = Fixture::new();
    f.write(&id(), 0);
    f.write(&id(), 1);
    let c = f.catalog(CatalogLimits {
        max_generations: 1,
        idle_ttl: Duration::from_millis(20),
        ..CatalogLimits::default()
    });
    let t = c
        .page(query(None, 1), CatalogControl::default())
        .await
        .unwrap()
        .next_cursor
        .unwrap();
    let t2 = c
        .page(query(None, 1), CatalogControl::default())
        .await
        .unwrap()
        .next_cursor
        .unwrap();
    assert_eq!(
        c.page(query(Some(t), 1), CatalogControl::default())
            .await
            .unwrap_err(),
        CatalogError::InvalidCursor
    );
    assert_eq!(
        c.page(
            query(Some("../../secrets".into()), 1),
            CatalogControl::default()
        )
        .await
        .unwrap_err(),
        CatalogError::InvalidCursor
    );
    let other = f.catalog(CatalogLimits::default());
    assert_eq!(
        other
            .page(query(Some(t2.clone()), 1), CatalogControl::default())
            .await
            .unwrap_err(),
        CatalogError::InvalidCursor
    );
    other.shutdown().await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        c.page(query(Some(t2), 1), CatalogControl::default())
            .await
            .unwrap_err(),
        CatalogError::InvalidCursor
    );
    c.shutdown().await;
}
#[tokio::test]
async fn session_catalog_controls_and_active_deadline() {
    let f = Fixture::new();
    f.write(&id(), 0);
    let c = f.catalog(CatalogLimits::default());
    let control = CatalogControl::default();
    control.cancel();
    assert_eq!(
        c.page(query(None, 1), control).await.unwrap_err(),
        CatalogError::Cancelled
    );
    assert_eq!(
        c.page(query(None, 1), CatalogControl::with_timeout(Duration::ZERO))
            .await
            .unwrap_err(),
        CatalogError::DeadlineExceeded
    );
    let tiny = f.catalog(CatalogLimits {
        active_time: Duration::from_nanos(1),
        ..CatalogLimits::default()
    });
    assert_eq!(
        tiny.page(query(None, 50), CatalogControl::default())
            .await
            .unwrap_err(),
        CatalogError::LimitExceeded
    );
    tiny.shutdown().await;
    c.shutdown().await;
}
#[test]
fn session_catalog_future_drop_retains_worker_owner_and_admission() {
    use std::future::Future;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    rt.block_on(async {
        let f = Fixture::new();
        f.write(&id(), 0);
        let c = Arc::new(f.catalog(CatalogLimits {
            step_entries: 1,
            max_workers: 1,
            ..CatalogLimits::default()
        }));
        let cursor = c
            .page(query(None, 50), CatalogControl::default())
            .await
            .unwrap()
            .next_cursor
            .unwrap();
        // Occupy the sole blocking thread so admission/drop/duplicate tests are deterministic.
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (started_tx, started_rx) = oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            let _ = started_tx.send(());
            release_rx.recv().unwrap();
        });
        started_rx.await.unwrap();
        let control = CatalogControl::default();
        let observed = control.clone();
        let mut future = Box::pin(c.page(query(Some(cursor.clone()), 50), control));
        let waker = futures_util::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        assert!(future.as_mut().poll(&mut cx).is_pending());
        assert_eq!(
            c.page(query(Some(cursor), 50), CatalogControl::default())
                .await
                .unwrap_err(),
            CatalogError::Conflict
        );
        drop(future);
        assert_eq!(observed.check(), Err(CatalogError::Cancelled));
        // Cancelled queued workers still own their slot, even for a fresh generation.
        assert_eq!(
            c.page(query(None, 50), CatalogControl::default())
                .await
                .unwrap_err(),
            CatalogError::Conflict
        );
        let mut shutdown = Box::pin(c.shutdown());
        assert!(shutdown.as_mut().poll(&mut cx).is_pending());
        drop(shutdown);
        release_tx.send(()).unwrap();
        blocker.await.unwrap();
        // Shutdown remains usable/complete after the original shutdown future was dropped.
        c.shutdown().await;
        let s = lock(&c.inner.state);
        assert_eq!(s.active, 0);
        assert!(s.workers.is_empty());
        assert!(s.generations.is_empty());
    });
}
#[tokio::test]
async fn session_catalog_public_futures_are_send_and_owner_drop_cancels() {
    fn send<T: Send>(_: T) {}
    let f = Fixture::new();
    let c = f.catalog(CatalogLimits::default());
    send(c.page(query(None, 50), CatalogControl::default()));
    send(c.shutdown());
    let inner = c.inner.clone();
    drop(c);
    assert_eq!(inner.stop.check(), Err(CatalogError::Cancelled));
}
#[cfg(unix)]
#[tokio::test]
async fn session_catalog_symlink_negatives() {
    use std::os::unix::fs::symlink;
    for level in 0..3 {
        let f = Fixture::new();
        let sid = id();
        let path = f.write(&sid, 0);
        let target = match level {
            0 => path.parent().unwrap().parent().unwrap().to_owned(),
            1 => path.parent().unwrap().to_owned(),
            _ => path.clone(),
        };
        let moved = f.root.join("moved");
        fs::rename(&target, &moved).unwrap();
        symlink(&moved, &target).unwrap();
        let c = f.catalog(CatalogLimits::default());
        assert_eq!(
            c.page(query(None, 50), CatalogControl::default())
                .await
                .unwrap_err(),
            CatalogError::AccessDenied
        );
        c.shutdown().await;
    }
}
#[cfg(unix)]
#[test]
fn session_catalog_non_utf8_candidate_recognition_without_filesystem() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let sid = id();
    let mut raw = vec![0xff];
    raw.extend_from_slice(format!("_{sid}.jsonl").as_bytes());
    let name = OsString::from_vec(raw);
    assert_eq!(
        candidate_name(&name, Some(&sid)),
        Err(CatalogError::Unavailable)
    );
    assert_eq!(candidate_name(&name, Some(&id())), Ok(None));
    assert_eq!(candidate_name(&name, None), Ok(None));
}

// Linux filesystems support arbitrary non-NUL name bytes. macOS APFS rejects
// this fixture at creation (EILSEQ); the pure production recognition test above
// still executes there, rather than skipping an unexpected fixture failure.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn session_catalog_non_utf8_same_id_candidate_fails_closed() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let f = Fixture::new();
    let sid = id();
    let path = f.write(&sid, 0);
    let mut name = vec![0xff];
    name.extend_from_slice(format!("_{sid}.jsonl").as_bytes());
    fs::copy(&path, path.parent().unwrap().join(OsString::from_vec(name))).unwrap();
    let c = f.catalog(CatalogLimits::default());
    assert_eq!(
        c.page(query(None, 50), CatalogControl::default())
            .await
            .unwrap_err(),
        CatalogError::Unavailable
    );
    c.shutdown().await;
}

#[cfg(windows)]
#[tokio::test]
async fn session_catalog_junction_negatives() {
    use std::os::windows::process::CommandExt;
    for level in 0..2 {
        let f = Fixture::new();
        let sid = id();
        let path = f.write(&sid, 0);
        let target = if level == 0 {
            path.parent().unwrap().parent().unwrap()
        } else {
            path.parent().unwrap()
        };
        let moved = f.root.join("moved");
        fs::rename(target, &moved).unwrap();
        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(target)
            .arg(&moved)
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(output.status.success());
        let c = f.catalog(CatalogLimits::default());
        assert_eq!(
            c.page(query(None, 50), CatalogControl::default())
                .await
                .unwrap_err(),
            CatalogError::AccessDenied
        );
        c.shutdown().await;
        fs::remove_dir(target).unwrap();
    }
}
