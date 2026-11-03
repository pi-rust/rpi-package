use super::*;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

struct Fixture {
    root: PathBuf,
    service: Arc<Service>,
    id: String,
}
impl Fixture {
    async fn new() -> Self {
        // macOS temp_dir can be under /var -> /private/var. Start fixtures at
        // the real directory; production deletion must still reject symlinks.
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = base.join(format!("rpi-retirement-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("sessions")).unwrap();
        let service = Service::new(BackendConfig {
            project: root.clone(),
            registry: root.join("registry.json"),
            session_dir: Some(root.join("sessions")),
            rpi_bin: root.join("never-spawn.exe"),
            ..BackendConfig::default()
        })
        .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        service
            .registry
            .write()
            .await
            .upsert(RegistryEntry {
                session_id: id.clone(),
                project_id: Some(service.default_project_id.clone()),
                title: None,
                parent_session_id: None,
                fork_entry_id: None,
                relation: None,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
        Self { root, service, id }
    }
    fn history(&self) -> PathBuf {
        self.root
            .join("sessions")
            .join(format!("2026_{}.jsonl", self.id))
    }
    fn native_files(&self) -> Vec<PathBuf> {
        let primary = self.history();
        std::fs::write(
            &primary,
            format!("{}\n", json!({"kind":"header","version":4,"id":self.id})),
        )
        .unwrap();
        let mut files = vec![primary.clone()];
        for suffix in [".tmp", ".progress.jsonl", ".progress.jsonl.tmp"] {
            let path = PathBuf::from(format!("{}{suffix}", primary.display()));
            std::fs::write(&path, "fixture").unwrap();
            files.push(path);
        }
        let timings = self.service.config.registry.with_extension("tool-timings");
        std::fs::create_dir_all(&timings).unwrap();
        for suffix in [".json", ".json.tmp"] {
            let path = timings.join(format!("{}{suffix}", self.id));
            std::fs::write(&path, "{}").unwrap();
            files.push(path);
        }
        files
    }
    async fn connect(
        &self,
    ) -> (
        Arc<HostSession>,
        crate::host::TestEvents,
        tokio::io::DuplexStream,
    ) {
        let (writer, peer) = tokio::io::duplex(8192);
        let (host, events) = HostSession::test_connection_with_ready(
            writer,
            32,
            json!({"supportedCommands":["get_state","get_history"]}),
        );
        self.service
            .sessions
            .write()
            .await
            .insert(self.id.clone(), host.clone());
        (host, events, peer)
    }
    async fn finish(self) {
        self.service.shutdown().await;
        std::fs::remove_dir_all(self.root).unwrap();
    }
}
fn metadata(archived: Option<bool>) -> MetadataRequest {
    MetadataRequest {
        archived,
        pinned: None,
        tags: None,
    }
}
fn respond(events: &crate::host::TestEvents, request: &Value, data: Value) {
    let _ = events.send(json!({"type":"response","id":request["id"],"success":true,"data":data}));
}
fn idle_peer(
    peer: tokio::io::DuplexStream,
    events: crate::host::TestEvents,
) -> tokio::task::JoinHandle<Vec<String>> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(peer).lines();
        let mut seen = Vec::new();
        while let Ok(Some(line)) = lines.next_line().await {
            let request: Value = serde_json::from_str(&line).unwrap();
            seen.push(request["type"].as_str().unwrap().to_owned());
            let data = match request["type"].as_str().unwrap() {
                "get_state" => {
                    json!({"activeOperation":null,"queue":{"steering":[],"followUp":[]}})
                }
                "get_history" => json!({"openOperations":[]}),
                "stop" => break,
                other => panic!("unexpected command {other}"),
            };
            respond(&events, &request, data);
        }
        seen
    })
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(4), future)
        .await
        .expect("retirement deadline")
}

#[tokio::test]
async fn archive_idle_closes_preserves_history_and_restore_never_spawns() {
    let f = Fixture::new().await;
    let files = f.native_files();
    let (host, events, peer) = f.connect().await;
    let peer = idle_peer(peer, events);
    let reply = bounded(session_metadata(
        &f.service,
        f.id.clone(),
        metadata(Some(true)),
    ))
    .await;
    assert_eq!(reply.0, ReplyKind::Success);
    assert_eq!(reply.1["archived"], true);
    assert!(*host.closed_receiver().borrow());
    assert!(f.service.live_session(&f.id).await.is_none());
    assert!(f.service.durable_projection(&f.id).await.is_none());
    assert!(files.iter().all(|p| p.exists()));
    let seen = bounded(peer).await.unwrap();
    assert_eq!(&seen[..2], &["get_state", "get_history"]);
    // Fixture hosts have no child reader; writer can close before best-effort
    // stop is emitted. The closed signal is the service-level exit assertion.
    assert_eq!(
        get_session(&f.service, f.id.clone()).await.1["archived"],
        true
    );
    assert_eq!(
        open_session(&f.service, f.id.clone()).await.1["code"],
        "session_archived"
    );
    let restored = session_metadata(&f.service, f.id.clone(), metadata(Some(false))).await;
    assert_eq!(restored.0, ReplyKind::Success);
    assert_eq!(restored.1["archived"], false);
    assert!(f.service.sessions.read().await.is_empty());
    f.finish().await;
}

#[tokio::test]
async fn busy_lease_preserves_host_preferences_and_history_for_archive_and_delete() {
    let f = Fixture::new().await;
    let files = f.native_files();
    let (host, _events, peer) = f.connect().await;
    let lease = host.try_run_lease().unwrap();
    let result = session_metadata(&f.service, f.id.clone(), metadata(Some(true))).await;
    assert_eq!(result.0, ReplyKind::Conflict);
    assert_eq!(result.1["code"], "session_busy");
    assert_eq!(
        delete_session(&f.service, f.id.clone()).await.value["code"],
        "session_busy"
    );
    assert!(!f.service.registry.read().await.preferences(&f.id).archived);
    assert!(!*host.closed_receiver().borrow());
    assert!(files.iter().all(|p| p.exists()));
    let mut lines = BufReader::new(peer).lines();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), lines.next_line())
            .await
            .is_err()
    );
    drop(lease);
    f.finish().await;
}

#[tokio::test]
async fn delete_idle_closes_then_removes_native_sidecars_timings_and_registry() {
    let f = Fixture::new().await;
    let files = f.native_files();
    let (host, events, peer) = f.connect().await;
    let peer = idle_peer(peer, events);
    let result = bounded(delete_session(&f.service, f.id.clone())).await;
    assert_eq!(result.kind, ReplyKind::Empty, "{:?}", result.value);
    assert!(*host.closed_receiver().borrow());
    assert!(files.iter().all(|p| !p.exists()));
    assert!(f.service.registry.read().await.get(&f.id).is_none());
    assert!(f.service.registry.read().await.deletion(&f.id).is_none());
    assert!(f.service.sessions.read().await.is_empty());
    bounded(peer).await.unwrap();
    f.finish().await;
}

#[tokio::test]
async fn delete_unknown_is_not_found_without_touching_files() {
    let f = Fixture::new().await;
    let files = f.native_files();
    assert_eq!(
        delete_session(&f.service, uuid::Uuid::new_v4().to_string())
            .await
            .kind,
        ReplyKind::NotFound
    );
    assert!(files.iter().all(|p| p.exists()));
    f.finish().await;
}

async fn persist_plan(f: &Fixture) {
    let plan = crate::deletion::prepare(
        &f.root.join("sessions"),
        &f.id,
        &f.service.config.registry.with_extension("tool-timings"),
    )
    .unwrap();
    f.service
        .registry
        .write()
        .await
        .begin_deletion(&f.id, plan)
        .unwrap();
}
#[tokio::test]
async fn persisted_deleting_reload_blocks_open_fork_commands_and_timing_reads() {
    let f = Fixture::new().await;
    f.native_files();
    persist_plan(&f).await;
    let reloaded = Service::new(f.service.config.clone()).unwrap();
    assert_eq!(
        open_session(&reloaded, f.id.clone()).await.1["code"],
        "session_deleting"
    );
    assert_eq!(
        fork_session(&reloaded, f.id.clone(), ForkRequest::default())
            .await
            .value["code"],
        "session_deleting"
    );
    let cmd: ApiCommand = serde_json::from_value(json!({"type":"get_state"})).unwrap();
    assert_eq!(
        command(&reloaded, f.id.clone(), cmd).await.1["code"],
        "session_deleting"
    );
    assert_eq!(
        run_timings(&reloaded, f.id.clone(), RunTimingsQuery::default())
            .await
            .kind,
        ReplyKind::Conflict
    );
    assert_eq!(
        get_session(&reloaded, f.id.clone()).await.1["deleting"],
        true
    );
    assert_eq!(
        list_sessions(&reloaded, SessionListQuery::default()).await["sessions"][0]["deleting"],
        true
    );
    assert!(reloaded.sessions.read().await.is_empty());
    reloaded.shutdown().await;
    f.finish().await;
}

#[tokio::test]
async fn partial_execute_retry_uses_original_manifest_after_reload() {
    let f = Fixture::new().await;
    let files = f.native_files();
    persist_plan(&f).await;
    // Model a process interruption after primary removal. Re-preparing would
    // reject the now orphaned progress files, whereas exact-manifest retry works.
    std::fs::remove_file(f.history()).unwrap();
    let reloaded = Service::new(f.service.config.clone()).unwrap();
    let result = bounded(delete_session(&reloaded, f.id.clone())).await;
    assert_eq!(result.kind, ReplyKind::Empty, "{:?}", result.value);
    assert!(files.iter().all(|p| !p.exists()));
    assert!(reloaded.registry.read().await.get(&f.id).is_none());
    reloaded.shutdown().await;
    f.finish().await;
}

#[tokio::test]
async fn cancelled_delete_request_retains_close_and_cleanup_owner() {
    let f = Fixture::new().await;
    let files = f.native_files();
    let (host, events, peer) = f.connect().await;
    let mut lines = BufReader::new(peer).lines();
    let request = tokio::spawn({
        let state = f.service.clone();
        let id = f.id.clone();
        async move { delete_session(&state, id).await }
    });
    let frame: Value =
        serde_json::from_str(&bounded(lines.next_line()).await.unwrap().unwrap()).unwrap();
    assert_eq!(frame["type"], "get_state");
    request.abort();
    let _ = request.await;
    respond(
        &events,
        &frame,
        json!({"activeOperation":null,"queue":{"steering":[],"followUp":[]}}),
    );
    let frame: Value =
        serde_json::from_str(&bounded(lines.next_line()).await.unwrap().unwrap()).unwrap();
    assert_eq!(frame["type"], "get_history");
    respond(&events, &frame, json!({"openOperations":[]}));
    bounded(async {
        while f.service.registry.read().await.get(&f.id).is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(*host.closed_receiver().borrow());
    assert!(files.iter().all(|p| !p.exists()));
    f.finish().await;
}

#[tokio::test]
async fn pinned_and_tags_metadata_does_not_probe_or_close_active_host() {
    let f = Fixture::new().await;
    let (host, _events, peer) = f.connect().await;
    let _lease = host.try_run_lease().unwrap();
    let result = session_metadata(
        &f.service,
        f.id.clone(),
        MetadataRequest {
            pinned: Some(true),
            archived: None,
            tags: Some(vec!["review".into()]),
        },
    )
    .await;
    assert_eq!(result.0, ReplyKind::Success);
    assert_eq!(result.1["pinned"], true);
    assert_eq!(result.1["tags"], json!(["review"]));
    assert!(!*host.closed_receiver().borrow());
    let mut lines = BufReader::new(peer).lines();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), lines.next_line())
            .await
            .is_err()
    );
    f.finish().await;
}

#[tokio::test]
async fn unknown_idle_state_returns_fixed_unavailable_without_native_details() {
    let f = Fixture::new().await;
    let (host, events, peer) = f.connect().await;
    let request = tokio::spawn({
        let state = f.service.clone();
        let id = f.id.clone();
        async move { session_metadata(&state, id, metadata(Some(true))).await }
    });
    let mut lines = BufReader::new(peer).lines();
    let frame: Value =
        serde_json::from_str(&bounded(lines.next_line()).await.unwrap().unwrap()).unwrap();
    events.send(json!({"type":"response","id":frame["id"],"success":false,"error":"secret native path/token"})).unwrap();
    let result = bounded(request).await.unwrap();
    assert_eq!(result.0, ReplyKind::Unavailable);
    assert_eq!(
        result.1,
        json!({"error":"Session idle state could not be confirmed","code":"session_idle_unknown"})
    );
    assert!(!*host.closed_receiver().borrow());
    assert!(!f.service.registry.read().await.preferences(&f.id).archived);
    f.finish().await;
}

#[tokio::test]
async fn execute_failure_retains_exact_intent_and_blocks_new_native_writers() {
    let f = Fixture::new().await;
    f.native_files();
    persist_plan(&f).await;
    let before =
        serde_json::to_value(f.service.registry.read().await.deletion(&f.id).unwrap()).unwrap();
    // Replacement/new content must not refresh the plan or be removed.
    std::fs::write(f.history(), "replaced native file").unwrap();
    let result = bounded(delete_session(&f.service, f.id.clone())).await;
    assert_eq!(result.kind, ReplyKind::Conflict);
    assert!(f.history().exists());
    let after =
        serde_json::to_value(f.service.registry.read().await.deletion(&f.id).unwrap()).unwrap();
    assert_eq!(before, after);
    assert!(f.service.registry.read().await.get(&f.id).is_some());
    assert_eq!(
        open_session(&f.service, f.id.clone()).await.1["code"],
        "session_deleting"
    );
    f.finish().await;
}

#[tokio::test]
async fn external_lock_conflict_persists_no_intent_then_append_and_fresh_retry_succeeds() {
    use fs2::FileExt;
    use std::io::Write;
    let f = Fixture::new().await;
    let files = f.native_files();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(PathBuf::from(format!("{}.lock", f.history().display())))
        .unwrap();
    FileExt::try_lock_exclusive(&lock).unwrap();
    let reply = bounded(delete_session(&f.service, f.id.clone())).await;
    assert_eq!(reply.kind, ReplyKind::Conflict);
    assert_eq!(reply.value["code"], "session_in_use");
    assert!(f.service.registry.read().await.deletion(&f.id).is_none());
    assert!(f.service.registry.read().await.get(&f.id).is_some());
    assert!(files.iter().all(|p| p.exists()));
    std::fs::OpenOptions::new()
        .append(true)
        .open(f.history())
        .unwrap()
        .write_all(b"external writer appended before releasing its lock\n")
        .unwrap();
    FileExt::unlock(&lock).unwrap();
    drop(lock);
    let reply = bounded(delete_session(&f.service, f.id.clone())).await;
    assert_eq!(reply.kind, ReplyKind::Empty, "{:?}", reply.value);
    assert!(files.iter().all(|p| !p.exists()));
    assert!(f.service.registry.read().await.get(&f.id).is_none());
    f.finish().await;
}

#[tokio::test]
async fn empty_missing_and_drifted_configured_roots_preserve_registry_and_timings() {
    // Never resolve HOME/config or invoke native: each root is an explicit fixture.
    for layout in ["empty", "missing", "drift"] {
        let f = Fixture::new().await;
        let files = f.native_files();
        let history = f.history();
        match layout {
            "empty" => {
                for path in &files[..4] {
                    std::fs::remove_file(path).unwrap();
                }
            }
            "missing" => {
                std::fs::remove_dir_all(f.root.join("sessions")).unwrap();
            }
            "drift" => {
                // Native had written to another root; the configured root is empty.
                std::fs::rename(f.root.join("sessions"), f.root.join("old-native-root")).unwrap();
                std::fs::create_dir(f.root.join("sessions")).unwrap();
            }
            _ => unreachable!(),
        }
        let timing_before: Vec<_> = files[4..]
            .iter()
            .map(|p| std::fs::read(p).unwrap())
            .collect();
        let reply = bounded(delete_session(&f.service, f.id.clone())).await;
        assert_eq!(
            reply.kind,
            ReplyKind::Conflict,
            "{layout}: {:?}",
            reply.value
        );
        assert_eq!(reply.value["code"], "deletion_history_missing");
        assert!(f.service.registry.read().await.get(&f.id).is_some());
        assert!(f.service.registry.read().await.deletion(&f.id).is_none());
        for (path, before) in files[4..].iter().zip(timing_before) {
            assert_eq!(std::fs::read(path).unwrap(), before);
        }
        if layout == "missing" {
            assert!(!f.root.join("sessions").exists());
        }
        if layout == "drift" {
            assert!(f
                .root
                .join("old-native-root")
                .join(history.file_name().unwrap())
                .exists());
        }
        f.finish().await;
    }
}

#[tokio::test]
async fn persisted_partial_retry_rejects_changed_survivor_without_replacing_manifest() {
    let f = Fixture::new().await;
    let files = f.native_files();
    persist_plan(&f).await;
    let before =
        serde_json::to_value(f.service.registry.read().await.deletion(&f.id).unwrap()).unwrap();
    std::fs::remove_file(f.history()).unwrap();
    std::fs::write(&files[2], "changed progress after partial delete").unwrap();
    let reloaded = Service::new(f.service.config.clone()).unwrap();
    let result = bounded(delete_session(&reloaded, f.id.clone())).await;
    assert_eq!(result.kind, ReplyKind::Conflict);
    assert_eq!(result.value["code"], "deletion_identity_changed");
    let after =
        serde_json::to_value(reloaded.registry.read().await.deletion(&f.id).unwrap()).unwrap();
    assert_eq!(before, after);
    assert!(files[1..].iter().all(|p| p.exists()));
    assert!(reloaded.registry.read().await.get(&f.id).is_some());
    reloaded.shutdown().await;
    f.finish().await;
}

#[tokio::test]
async fn invalid_archive_metadata_never_probes_or_closes_host() {
    for tags in [
        vec!["x".repeat(33)],
        (0..13).map(|n| format!("tag-{n}")).collect(),
    ] {
        let f = Fixture::new().await;
        let (host, _events, _peer) = f.connect().await;
        let mut request = metadata(Some(true));
        request.tags = Some(tags);
        let result = bounded(session_metadata(&f.service, f.id.clone(), request)).await;
        assert_eq!(result.0, ReplyKind::InvalidInput);
        assert_eq!(result.1["code"], "invalid_session_tags");
        assert!(!*host.closed_receiver().borrow());
        assert!(!f.service.registry.read().await.preferences(&f.id).archived);
        assert!(Arc::ptr_eq(
            &host,
            &f.service.live_session(&f.id).await.unwrap()
        ));
        f.finish().await;
    }
}

#[tokio::test]
async fn retained_retirement_owners_are_bounded_without_mutation() {
    let f = Fixture::new().await;
    let permit = f
        .service
        .retirement_workers
        .clone()
        .acquire_many_owned(16)
        .await
        .unwrap();
    let archive = bounded(session_metadata(
        &f.service,
        f.id.clone(),
        metadata(Some(true)),
    ))
    .await;
    assert_eq!(archive.1["code"], "session_retirement_capacity");
    let delete = bounded(delete_session(&f.service, f.id.clone())).await;
    assert_eq!(delete.value["code"], "session_retirement_capacity");
    assert!(f.service.registry.read().await.get(&f.id).is_some());
    assert!(f.service.registry.read().await.deletion(&f.id).is_none());
    assert!(!f.service.registry.read().await.preferences(&f.id).archived);
    drop(permit);
    let restored = bounded(session_metadata(
        &f.service,
        f.id.clone(),
        metadata(Some(false)),
    ))
    .await;
    assert_eq!(restored.0, ReplyKind::Success);
    f.finish().await;
}

#[tokio::test]
async fn stalled_retirement_does_not_block_other_session_control_commands() {
    let retiring = Fixture::new().await;
    let other = Fixture::new().await;
    let (_, retirement_events, retirement_peer) = retiring.connect().await;
    let (other_host, other_events, other_peer) = other.connect().await;
    let other_entry = other
        .service
        .registry
        .read()
        .await
        .get(&other.id)
        .unwrap()
        .clone();
    retiring
        .service
        .registry
        .write()
        .await
        .upsert(other_entry)
        .unwrap();
    retiring
        .service
        .sessions
        .write()
        .await
        .insert(other.id.clone(), other_host);
    let archive = tokio::spawn({
        let service = retiring.service.clone();
        let id = retiring.id.clone();
        async move { session_metadata(&service, id, metadata(Some(true))).await }
    });
    let mut retirement_lines = BufReader::new(retirement_peer).lines();
    let probe: Value = serde_json::from_str(
        &bounded(retirement_lines.next_line())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(probe["type"], "get_state");
    // Leave A's idle probe unanswered while B's stop/dialog responses pass.
    let mut other_lines = BufReader::new(other_peer).lines();
    for kind in ["abort", "ui_dialog_response"] {
        let request = tokio::spawn({
            let service = retiring.service.clone();
            let id = other.id.clone();
            async move {
                let command_value = serde_json::from_value(
                    json!({"type":kind,"dialogId":"fixture-dialog","value":"fixture"}),
                )
                .unwrap();
                command(&service, id, command_value).await
            }
        });
        let frame: Value =
            serde_json::from_str(&bounded(other_lines.next_line()).await.unwrap().unwrap())
                .unwrap();
        assert_eq!(frame["type"], kind);
        respond(&other_events, &frame, json!({}));
        assert_eq!(bounded(request).await.unwrap().0, ReplyKind::Success);
        assert!(!archive.is_finished());
    }
    respond(
        &retirement_events,
        &probe,
        json!({"activeOperation":null,"queue":{"steering":[],"followUp":[]}}),
    );
    let probe: Value = serde_json::from_str(
        &bounded(retirement_lines.next_line())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(probe["type"], "get_history");
    respond(&retirement_events, &probe, json!({"openOperations":[]}));
    assert_eq!(bounded(archive).await.unwrap().0, ReplyKind::Success);
    retiring.finish().await;
    other.finish().await;
}
