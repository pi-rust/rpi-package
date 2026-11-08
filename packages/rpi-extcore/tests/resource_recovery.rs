//! Public Service/Registry recovery regressions against an isolated deterministic native peer.
use rpi_extcore::{
    registry::{Registry, RegistryEntry},
    reply::ReplyKind,
    resources::*,
    service::{self, BackendConfig, ForkRequest, Service},
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};
fn mock() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("resource-mock-build-{}", std::process::id()));
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
fn selection() -> ResourceSelection {
    serde_json::from_value(json!({"version":1,"tools":{"mode":"none"}})).unwrap()
}
fn record() -> ResourceConfiguration {
    ResourceConfiguration {
        selection: selection(),
        revision: 1,
        settings: Default::default(),
        current_settings: None,
        observation: None,
        operations: vec![],
    }
}
fn entry(id: &str) -> RegistryEntry {
    RegistryEntry {
        session_id: id.into(),
        project_id: None,
        title: None,
        parent_session_id: None,
        fork_entry_id: None,
        relation: None,
        created_at: 0,
        updated_at: 0,
    }
}
struct Fixture {
    root: PathBuf,
    owner: Arc<Service>,
}
impl Fixture {
    fn new(capture: bool) -> Self {
        let root = std::env::temp_dir().join(format!("resource-recovery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("owner-alive"), "fixture owner").unwrap();
        let path = root.join("registry.json");
        let mut registry = Registry::load(path.clone()).unwrap();
        let mut record = record();
        if capture {
            record.current_settings = Some(ResourceSettingsCapture {
                settings: ResourceSessionSettings {
                    model: Some("saved-model".into()),
                    provider: Some("saved-provider".into()),
                    thinking: Some("high".into()),
                    ..Default::default()
                },
                captured_at_ms: 1,
                host_id: "stopped-host".into(),
            });
        }
        registry
            .set_resource_configuration(entry("source"), record)
            .unwrap();
        let owner = Service::new(BackendConfig {
            project: root.clone(),
            registry: path,
            rpi_bin: mock(),
            ..Default::default()
        })
        .unwrap();
        Self { root, owner }
    }
    fn flag(&self, name: &str) {
        std::fs::write(self.root.join(name), "1").unwrap();
    }
    async fn open(&self) {
        let (kind, body) = service::open_session(&self.owner, "source".into()).await;
        assert_eq!(kind, ReplyKind::Success, "{body}");
    }
    async fn end(&self) {
        self.owner.shutdown().await;
    }
    fn disk(&self) -> Registry {
        Registry::load(self.root.join("registry.json")).unwrap()
    }
    fn starts(&self) -> usize {
        std::fs::read_dir(&self.root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("started-"))
            .count()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // The std-only peer watchdog exits even if a regression panics before
        // normal async shutdown, including the caller-owned baseline bug.
        let _ = std::fs::remove_file(self.root.join("owner-alive"));
    }
}
async fn wait_file(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
async fn state(owner: &Service, id: &str) -> Value {
    owner.live_session(id).await.unwrap().state().await.unwrap()
}
#[tokio::test]
async fn fork_readback_failure_never_publishes_legacy_destination() {
    let f = Fixture::new(false);
    f.flag("fail-readback");
    let reply = service::clone_session(&f.owner, "source".into(), ForkRequest::default()).await;
    f.end().await;
    assert_eq!(reply.kind, ReplyKind::HostUnavailable, "{}", reply.value);
    assert_eq!(
        f.disk().entries().len(),
        1,
        "failed exact fork must not leave a legacy-openable entry"
    );
    for file in std::fs::read_dir(&f.root).unwrap().filter_map(Result::ok) {
        let name = file.file_name().to_string_lossy().to_string();
        if let Some(id) = name.strip_prefix("started-") {
            assert!(
                f.root.join(format!("stopped-{id}")).exists(),
                "candidate cleanup: {id}"
            );
        }
    }
}
#[tokio::test]
async fn live_fork_and_clone_preserve_settings_and_refuse_busy_source() {
    let f = Fixture::new(false);
    f.open().await;
    let host = f.owner.live_session("source").await.unwrap();
    host.set_model("live-model", Some("live-provider"))
        .await
        .unwrap();
    host.set_thinking_level("high").await.unwrap();
    let mut actual = Vec::new();
    for clone in [false, true] {
        let reply = if clone {
            service::clone_session(&f.owner, "source".into(), ForkRequest::default()).await
        } else {
            service::fork_session(&f.owner, "source".into(), ForkRequest::default()).await
        };
        assert_eq!(reply.kind, ReplyKind::Created, "{}", reply.value);
        actual.push(state(&f.owner, reply.value["sessionId"].as_str().unwrap()).await);
    }
    let starts = f.starts();
    let lease = host.try_run_lease().unwrap();
    let busy = service::clone_session(&f.owner, "source".into(), ForkRequest::default()).await;
    drop(lease);
    let after = f.starts();
    let source = state(&f.owner, "source").await;
    f.end().await;
    for value in actual {
        assert_eq!(value["model"], "live-model");
        assert_eq!(value["provider"], "live-provider");
        assert_eq!(value["thinkingLevel"], "high");
    }
    assert_eq!(busy.kind, ReplyKind::Conflict);
    assert_eq!(starts, after);
    assert_eq!(source["model"], "live-model");
}
#[tokio::test]
async fn cancelled_fork_completion_is_owned_and_shutdown_quiesces_it() {
    let f = Fixture::new(false);
    f.flag("pause-readback");
    let owner = f.owner.clone();
    let caller = tokio::spawn(async move {
        service::clone_session(&owner, "source".into(), ForkRequest::default()).await
    });
    wait_file(&f.root.join("readback-paused")).await;
    let id = std::fs::read_to_string(f.root.join("readback-paused")).unwrap();
    caller.abort();
    let _ = caller.await;
    let owner = f.owner.clone();
    let shutdown = tokio::spawn(async move { owner.shutdown().await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let waited = !shutdown.is_finished();
    std::fs::remove_file(f.root.join("pause-readback")).unwrap();
    shutdown.await.unwrap();
    let disk = f.disk();
    assert!(
        waited,
        "shutdown must own completion, not merely the requesting future"
    );
    assert!(
        disk.get(&id).is_none() || disk.resource_configuration(&id).is_some(),
        "shutdown permits absent or fully exact publication, never a legacy entry"
    );
    assert!(f.root.join(format!("stopped-{id}")).exists());
}
#[tokio::test]
async fn source_storage_latch_refuses_clone_before_native_launch() {
    let f = Fixture::new(false);
    f.open().await;
    std::fs::create_dir(f.root.join("registry.json.tmp")).unwrap();
    let request = ApplyResourcesRequest {
        expected_revision: 1,
        operation_id: "storage-failure".into(),
        target: selection(),
        supersedes_operation_id: None,
    };
    let failed = f
        .owner
        .apply_resources("source".into(), request)
        .await
        .unwrap_err();
    std::fs::remove_dir(f.root.join("registry.json.tmp")).unwrap();
    let starts = f.starts();
    let reply = service::clone_session(&f.owner, "source".into(), ForkRequest::default()).await;
    let after = f.starts();
    f.end().await;
    assert_eq!(failed.kind, ResourceErrorKind::Storage);
    assert_eq!(reply.kind, ReplyKind::Internal, "{}", reply.value);
    assert_eq!(starts, after);
}
#[path = "support/resource_headroom_recovery.rs"]
mod headroom;
#[path = "support/resource_recovery_lifecycle.rs"]
mod lifecycle;
#[test]
fn bare_filename_registry_persistence_matches_memory() {
    // Run with an isolated cwd, never mutate global cwd or the source tree.
    if std::env::var_os("RPI_REGISTRY_BARE_CHILD").is_none() {
        let root = std::env::temp_dir().join(format!("registry-bare-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "bare_filename_registry_persistence_matches_memory",
                "--nocapture",
            ])
            .env("RPI_REGISTRY_BARE_CHILD", "1")
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let path = PathBuf::from("sessions.json");
    let mut registry = Registry::load(path.clone()).unwrap();
    let result = registry.set_resource_configuration(entry("session"), record());
    let disk = Registry::load(path.clone()).unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(
        result.is_ok(),
        "{result:?}; rename must not be followed by empty-parent fsync"
    );
    assert_eq!(registry.entries().len(), disk.entries().len());
    assert!(registry.resource_configuration("session").is_some());
}
