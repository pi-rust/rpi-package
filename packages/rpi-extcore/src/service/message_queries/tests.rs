use super::*;
use crate::{host::HostSession, service::BackendConfig};
use tokio::io::{AsyncBufReadExt, BufReader};
async fn fixture(
    commands: &[&str],
    data: Value,
) -> (
    std::sync::Arc<Service>,
    std::path::PathBuf,
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::UnboundedReceiver<Value>,
) {
    let root = std::env::temp_dir().join(format!("rpi-message-query-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let service = Service::new(BackendConfig {
        project: root.clone(),
        registry: root.join("sessions.json"),
        ..Default::default()
    })
    .unwrap();
    let (writer, peer) = tokio::io::duplex(8192);
    let (session, replies) = HostSession::test_connection_with_ready(
        writer,
        16,
        json!({"type":"ready","supportedCommands":commands}),
    );
    service
        .sessions
        .write()
        .await
        .insert("history".into(), session);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let mut lines = BufReader::new(peer).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            let request: Value = serde_json::from_str(&line).unwrap();
            tx.send(request.clone()).unwrap();
            let mut response = data.clone();
            response["type"] = json!("response");
            response["id"] = request["id"].clone();
            replies.send(response).unwrap();
        }
    });
    (service, root, task, rx)
}
async fn cleanup(
    service: std::sync::Arc<Service>,
    root: std::path::PathBuf,
    task: tokio::task::JoinHandle<()>,
) {
    task.abort();
    service.shutdown().await;
    drop(service);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn summary_forwards_native_cursor_and_requires_capability_without_scanning() {
    let data = json!({"success":true,"data":{"filtered":true,"role":"user","messages":[{"entryId":"u1","role":"user","preview":"first","timestamp":1,"hasImages":false}],"firstId":"u1","lastId":"u1","nextBeforeId":"u1","hasMore":true,"leafId":"leaf"}});
    let (service, root, task, mut requests) = fixture(&["get_message_summaries"], data).await;
    let query = serde_json::from_value(json!({"limit":1,"beforeId":"u2","leafId":"leaf"})).unwrap();
    let response = message_summaries(&service, "history".into(), query).await;
    assert_eq!(response.kind, ReplyKind::Success);
    assert_eq!(response.value["nextBeforeId"], "u1");
    let command = requests.recv().await.unwrap();
    assert_eq!(command["type"], "get_message_summaries");
    assert_eq!(command["beforeId"], "u2");
    assert_eq!(command["limit"], 1);
    let result = message_window(
        &service,
        "history".into(),
        serde_json::from_value(json!({"anchorId":"u1"})).unwrap(),
    )
    .await;
    assert_eq!(result.kind, ReplyKind::Unsupported);
    assert!(requests.try_recv().is_err());
    cleanup(service, root, task).await;
}
#[tokio::test]
async fn bad_limits_and_conflicting_cursors_never_reach_host() {
    let (service, root, task, mut requests) = fixture(
        &["get_message_summaries", "get_message_window"],
        json!({"success":true}),
    )
    .await;
    for value in [
        json!({"limit":0}),
        json!({"limit":101}),
        json!({"role":"assistant"}),
        json!({"beforeId":""}),
    ] {
        assert_eq!(
            message_summaries(
                &service,
                "history".into(),
                serde_json::from_value(value).unwrap()
            )
            .await
            .kind,
            ReplyKind::InvalidInput
        );
    }
    for value in [json!({}), json!({"anchorId":"u1","beforeId":"u2"})] {
        assert_eq!(
            message_window(
                &service,
                "history".into(),
                serde_json::from_value(value).unwrap()
            )
            .await
            .kind,
            ReplyKind::InvalidInput
        );
    }
    assert!(requests.try_recv().is_err());
    cleanup(service, root, task).await;
}
#[tokio::test]
async fn branch_change_is_conflict_and_invalid_native_pages_are_not_accepted() {
    let (service, root, task, _requests) = fixture(&["get_message_summaries"], json!({"success":false,"error":"invalid_query: branch_changed: snapshot leaf no longer selected"})).await;
    // Keep the fixture observation receiver alive.
    let query = serde_json::from_value(json!({})).unwrap();
    assert_eq!(
        message_summaries(&service, "history".into(), query)
            .await
            .kind,
        ReplyKind::Conflict
    );
    cleanup(service, root, task).await;
    let (service, root, task, _requests) = fixture(
        &["get_message_summaries"],
        json!({"success":true,"data":{"messages":[]}}),
    )
    .await;
    assert_eq!(
        message_summaries(
            &service,
            "history".into(),
            serde_json::from_value(json!({})).unwrap()
        )
        .await
        .kind,
        ReplyKind::HostUnavailable
    );
    cleanup(service, root, task).await;
}
#[tokio::test]
async fn context_page_preserves_bidirectional_metadata_and_anchor() {
    let (service, root, task, _requests) = fixture(&["get_message_window"], json!({"success":true,"data":{"messages":[{"entryId":"u1","role":"user","content":"bounded"}],"firstId":"u1","lastId":"u1","nextBeforeId":"u1","nextAfterId":"u1","hasMoreBefore":true,"hasMoreAfter":true,"leafId":"leaf"}})).await;
    let query = serde_json::from_value(json!({"anchorId":"u1","leafId":"leaf","limit":1})).unwrap();
    assert_eq!(
        message_window(&service, "history".into(), query).await.kind,
        ReplyKind::Success
    );
    cleanup(service, root, task).await;
}
