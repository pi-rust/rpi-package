use super::*;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncBufReadExt, BufReader};

async fn fixture(
    state: &Service,
    id: &str,
    project_id: &str,
    queued: Arc<AtomicBool>,
    reads: Arc<AtomicUsize>,
) -> (Arc<HostSession>, tokio::task::JoinHandle<()>) {
    let (writer, peer) = tokio::io::duplex(8192);
    let ready = json!({"type":"ready","protocol":"rpi","supportedCommands":["get_state","get_messages","get_queue","get_session_branch"]});
    let (host, injector) = HostSession::test_connection_with_ready(writer, 16, ready);
    state
        .registry
        .write()
        .await
        .upsert(RegistryEntry {
            session_id: id.into(),
            project_id: Some(project_id.into()),
            title: None,
            parent_session_id: None,
            fork_entry_id: None,
            relation: None,
            created_at: 0,
            updated_at: 0,
        })
        .unwrap();
    state.sessions.write().await.insert(id.into(), host.clone());
    let worker = tokio::spawn(async move {
        let mut lines = BufReader::new(peer).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let request: Value = serde_json::from_str(&line).unwrap();
            let data = match request["type"].as_str().unwrap() {
                "stop" => break,
                "get_state" => {
                    json!({"activeOperation":null,"queue":{"steeringCount":0,"followUpCount":0}})
                }
                "get_messages" => json!({"pendingResume":false}),
                "get_queue" => json!({"count":0,"items":[]}),
                "get_session_branch" => {
                    reads.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(request["customTypes"], json!([crate::durable::ENTRY_TYPE]));
                    if queued.load(Ordering::SeqCst) {
                        json!({"filtered":true,"entries":[{"id":"intent-1","type":"custom","customType":crate::durable::ENTRY_TYPE,"data":{"format":rpi_durable_core::JOURNAL_FORMAT,"version":1,"kind":"snapshot","baseRevision":0,"revision":1,"state":{"revision":1,"queue":[{"id":"queue-1","text":"pending","status":"queued"}]}}}]})
                    } else {
                        json!({"filtered":true,"entries":[]})
                    }
                }
                other => panic!("unexpected fixture request {other}"),
            };
            injector
                .send(json!({"type":"response","id":request["id"],"success":true,"data":data}))
                .unwrap();
        }
    });
    (host, worker)
}
#[tokio::test]
async fn project_guard_fences_captured_hosts_and_rechecks_canonical_durable_intent() {
    let root =
        std::env::temp_dir().join(format!("resource-project-guard-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    let service = Service::new(BackendConfig {
        project: root.join("a"),
        projects: vec![root.join("b")],
        registry: root.join("registry.json"),
        ..BackendConfig::default()
    })
    .unwrap();
    let a_id = service.default_project_id.clone();
    let b_id = service
        .projects
        .read()
        .await
        .entries
        .iter()
        .find(|p| p.id != a_id)
        .unwrap()
        .id
        .clone();
    let queued = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));
    let b_reads = Arc::new(AtomicUsize::new(0));
    let (a, a_worker) = fixture(&service, "a", &a_id, queued.clone(), reads.clone()).await;
    let (b, b_worker) = fixture(
        &service,
        "b",
        &b_id,
        Arc::new(AtomicBool::new(false)),
        b_reads.clone(),
    )
    .await;
    let run = a.try_run_lease().unwrap();
    assert_eq!(
        service
            .resource_project_fence(&a_id)
            .await
            .err()
            .unwrap()
            .kind,
        ResourceErrorKind::Conflict
    );
    drop(run);
    let guard = service.resource_project_fence(&a_id).await.unwrap();
    assert_eq!(guard.session_ids(), &[String::from("a")]);
    assert_eq!(b_reads.load(Ordering::SeqCst), 0);
    assert!(a.try_run_lease().is_err());
    assert!(b.try_run_lease().is_ok());
    let captured = a.clone();
    let waiting =
        tokio::spawn(async move { captured.notify(json!({"type":"queue_message"})).await });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    waiting.abort();
    let _ = waiting.await;
    assert_eq!(
        service
            .resource_project_fence(&a_id)
            .await
            .err()
            .unwrap()
            .kind,
        ResourceErrorKind::Conflict
    );
    drop(guard);
    assert!(a.try_run_lease().is_ok());
    // Cached projection is deliberately empty; native's just-committed canonical
    // journal contains a queue item. No event refresh is delivered to the cache.
    service
        .durable
        .write()
        .await
        .insert("a".into(), DurableRuntime::empty(a.clone()));
    queued.store(true, Ordering::SeqCst);
    assert!(service
        .durable_projection("a")
        .await
        .unwrap()
        .state()
        .await
        .queue
        .is_empty());
    assert!(service
        .resource_project_fence(&a_id)
        .await
        .err()
        .unwrap()
        .message
        .contains("durable work"));
    assert!(reads.load(Ordering::SeqCst) >= 2);
    service.shutdown().await;
    a_worker.await.unwrap();
    b_worker.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn resource_write_failure_latches_old_arc_and_reopen_without_in_memory_authorization() {
    let root =
        std::env::temp_dir().join(format!("resource-storage-latch-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let service = Service::new(BackendConfig {
        project: root.clone(),
        registry: root.join("registry.json"),
        ..BackendConfig::default()
    })
    .unwrap();
    let (host, worker) = fixture(
        &service,
        "session",
        &service.default_project_id,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let selection: ResourceSelection =
        serde_json::from_value(json!({"version":1,"tools":{"mode":"none"}})).unwrap();
    let record = ResourceConfiguration {
        selection,
        revision: 1,
        settings: Default::default(),
        current_settings: None,
        observation: None,
        operations: vec![],
    };
    std::fs::create_dir(service.config.registry.with_extension("json.tmp")).unwrap();
    assert_eq!(
        save_record(&service, "session", record)
            .await
            .unwrap_err()
            .kind,
        ResourceErrorKind::Storage
    );
    assert!(host
        .notify(json!({"type":"prompt","message":"must not execute"}))
        .await
        .unwrap_err()
        .contains("retired"));
    assert_eq!(
        service
            .resource_configuration("session")
            .await
            .unwrap_err()
            .kind,
        ResourceErrorKind::Storage
    );
    assert_eq!(
        super::super::open_session(&service, "session".into())
            .await
            .0,
        ReplyKind::Internal
    );
    assert!(service
        .registry
        .read()
        .await
        .resource_configuration("session")
        .is_none());
    std::fs::remove_dir(service.config.registry.with_extension("json.tmp")).unwrap();
    service.shutdown().await;
    worker.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
