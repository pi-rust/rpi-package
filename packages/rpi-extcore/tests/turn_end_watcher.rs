//! Regression: a created session's first completed run lands in the registry
//! through the real publish path, so "recent chats" can sort by the last
//! settled turn (#372).
use rpi_extcore::{
    reply::ReplyKind,
    service::{self, BackendConfig, Service},
};
use std::{path::PathBuf, sync::OnceLock, time::Duration};

fn mock() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("turn-end-mock-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let bin = root.join(format!("mock-native{}", std::env::consts::EXE_SUFFIX));
        let result = std::process::Command::new("rustc")
            .args(["--edition=2021", "--crate-name=resource_mock_native"])
            .arg(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/support/resource_mock_native.rs"),
            )
            .arg("-o")
            .arg(&bin)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        bin
    })
    .clone()
}

#[tokio::test]
async fn first_settled_run_of_a_created_session_is_recorded() {
    let root = std::env::temp_dir().join(format!("turn-end-watch-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    // The mock native exits once `owner-alive` disappears from its cwd; keep
    // the fixture alive for the whole test like resource_recovery.rs does.
    std::fs::write(root.join("owner-alive"), "1").unwrap();
    let service = Service::new(BackendConfig {
        project: root.clone(),
        registry: root.join("sessions.json"),
        rpi_bin: mock(),
        ..Default::default()
    })
    .unwrap();
    let (kind, body) = service::create_session(
        &service,
        service::CreateSessionRequest {
            project_id: None,
            resources: None,
        },
    )
    .await;
    assert_eq!(kind, ReplyKind::Created, "{body}");
    let id = body["sessionId"].as_str().unwrap().to_owned();

    // First prompt: its settlement must be recorded by the turn-end watcher.
    let session = service
        .live_session(&id)
        .await
        .expect("created session should be live");
    session
        .begin_prompt("first".into())
        .await
        .expect("prompt accepted");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let list = service::list_sessions(&service, service::SessionListQuery::default()).await;
            let recorded = list["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["sessionId"] == id)
                .and_then(|entry| entry["lastTurnEndedAt"].as_u64())
                .is_some();
            if recorded {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("first settled run should record lastTurnEndedAt");
}
