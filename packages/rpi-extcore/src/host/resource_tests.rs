use super::*;
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::test]
async fn raw_prompt_acceptance_without_projected_event_still_blocks_idle_configuration() {
    let (writer, peer) = tokio::io::duplex(4096);
    let (host, injector) = HostSession::test_connection(writer, 16);
    let fixture = tokio::spawn(async move {
        let mut lines = BufReader::new(peer).lines();
        let request: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(request["type"], "prompt");
        injector.send(json!({"type":"response","id":request["id"],"success":true,"data":{"accepted":true}})).unwrap();
        // No operation_accepted or agent_start publication. A native get_state
        // would report idle here, but preflight must fail BEFORE querying it.
        if let Some(line) = lines.next_line().await.unwrap() {
            let next: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(
                next["type"], "stop",
                "idle preflight queried native despite local unsettled admission"
            );
        }
    });
    host.command(
        "prompt",
        json!({"message":"queued but not projected"}),
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_eq!(host.snapshot().await.status, "ready");
    let fence = host.resource_fence().unwrap();
    assert!(host
        .verify_resource_idle(&fence)
        .await
        .unwrap_err()
        .message
        .contains("unsettled"));
    assert!(host.verify_project_idle(&fence).await.is_err());
    drop(fence);
    assert!(host
        .command("resume_pending", json!({}), Duration::from_secs(1))
        .await
        .unwrap_err()
        .contains("unsettled"));
    host.shutdown().await;
    fixture.await.unwrap();
}

#[tokio::test]
async fn matching_native_validation_rejections_allow_the_next_valid_execution() {
    for command in ["prompt", "compact", "resume_pending"] {
        let (writer, peer) = tokio::io::duplex(4096);
        let (host, injector) = HostSession::test_connection(writer, 16);
        let fixture = tokio::spawn(async move {
            let mut lines = BufReader::new(peer).lines();
            let rejected: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(rejected["type"], command);
            injector.send(json!({"type":"response","id":rejected["id"],"success":false,"error":"validation rejected before acceptance"})).unwrap();
            let accepted: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(accepted["type"], "prompt");
            injector.send(json!({"type":"response","id":accepted["id"],"success":true,"data":{"accepted":true}})).unwrap();
            injector
                .send(json!({"type":"agent_settled","outcome":"completed"}))
                .unwrap();
        });
        assert!(host
            .command(command, json!({}), Duration::from_secs(1))
            .await
            .unwrap_err()
            .contains("validation rejected"));
        let run = host
            .begin_prompt("valid after rejected command".into())
            .await
            .unwrap();
        fixture.await.unwrap();
        drop(run);
        host.terminate().await;
    }
}

#[tokio::test]
async fn run_lease_does_not_block_queue_or_dialog_controls_but_blocks_configuration() {
    let (writer, peer) = tokio::io::duplex(4096);
    let (host, injector) = HostSession::test_connection(writer, 16);
    let run = host.begin_prompt("running".into()).await.unwrap();
    assert!(host.resource_fence().is_err());
    let fixture = tokio::spawn(async move {
        let mut lines = BufReader::new(peer).lines();
        for expected in [
            "prompt",
            "queue_message",
            "ui_dialog_response",
            "cancel_queued",
            "abort",
        ] {
            let value: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(value["type"], expected);
            if expected != "prompt" && expected != "abort" {
                injector.send(json!({"type":"response","id":value["id"],"success":true,"data":{"accepted":true}})).unwrap();
            }
        }
    });
    for kind in ["queue_message", "ui_dialog_response", "cancel_queued"] {
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            host.command(kind, json!({}), Duration::from_secs(1)),
        )
        .await
        .unwrap();
        assert!(result.is_ok(), "{kind}: {result:?}");
        assert!(host.resource_fence().is_err());
    }
    host.abort_run().await.unwrap();
    fixture.await.unwrap();
    drop(run);
    host.shutdown().await;
}

#[tokio::test]
async fn retired_arc_never_reopens_admission_after_old_generation_settlement() {
    let (writer, _peer) = tokio::io::duplex(4096);
    let (host, injector) = HostSession::test_connection(writer, 16);
    let _events = host.subscribe_raw();
    let fence = host.resource_fence().unwrap();
    host.retire_resources(&fence);
    let queued = host.clone();
    let waiting = tokio::spawn(async move { queued.notify(json!({"type":"queue_message"})).await });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    injector
        .send(json!({"type":"agent_settled","outcome":"completed"}))
        .unwrap();
    drop(fence);
    assert!(waiting.await.unwrap().unwrap_err().contains("retired"));
    assert!(host.begin_prompt("late".into()).await.is_err());
    assert!(host
        .command("resume_pending", json!({}), Duration::from_secs(1))
        .await
        .is_err());
    assert!(host
        .command("ui_dialog_response", json!({}), Duration::from_secs(1))
        .await
        .is_err());
    host.shutdown().await;
}
