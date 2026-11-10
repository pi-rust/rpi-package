use super::*;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};

async fn fixture(
    state: &Service,
    id: &str,
    project_id: &str,
) -> (Arc<HostSession>, tokio::task::JoinHandle<()>) {
    let (writer, peer) = tokio::io::duplex(8192);
    let ready = json!({"type":"ready","protocol":"rpi","supportedCommands":["get_state","get_messages","get_queue"]});
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
    let (host, worker) = fixture(&service, "session", &service.default_project_id).await;
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
