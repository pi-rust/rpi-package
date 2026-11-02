use super::*;
use crate::protocol::Writer;
use rpi_extcore::context_usage::parse_context_usage;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

fn known(id: &str, used: u64, size: u64) -> ContextUsage {
    parse_context_usage(&json!({"sessionId":id,"context":{
        "tokens":used,"window":size,"estimated":true,"trailingTokens":9
    }}))
    .unwrap()
    .unwrap()
}

#[test]
fn context_parser_projection_preserves_only_stable_fields_and_exact_identity() {
    let usage = known("acp", u64::MAX, 8);
    assert_eq!(
        project("acp", &usage).unwrap(),
        json!({
            "jsonrpc":"2.0","method":"session/update","params":{
                "sessionId":"acp","update":{"sessionUpdate":"usage_update","used":u64::MAX,"size":8}
            }
        })
    );
    assert!(project("other", &usage).is_none());
    for value in [
        json!({}),
        json!({"context":null}),
        json!({"context":{"tokens":0,"window":0}}),
    ] {
        assert!(parse_context_usage(&value).unwrap().is_none());
    }
    for value in [
        json!({"context":false}),
        json!({"sessionId":"acp","context":{"tokens":-1,"window":8}}),
        json!({"sessionId":"acp","context":{"tokens":1.5,"window":8}}),
    ] {
        assert!(parse_context_usage(&value).is_err());
    }
}

#[tokio::test]
async fn presentation_dedups_successful_pairs_but_always_forces_initial() {
    let (out, peer) = tokio::io::duplex(8192);
    let writer = Writer::with_output(out);
    let mut lines = BufReader::new(peer).lines();
    let mut presentation = Presentation::default();
    let usage = known("acp", 12, 100);
    presentation.deliver(&writer, "acp", &usage, true).await;
    assert!(lines.next_line().await.unwrap().is_some());
    presentation.deliver(&writer, "acp", &usage, false).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), lines.next_line())
            .await
            .is_err()
    );
    presentation.deliver(&writer, "acp", &usage, true).await;
    assert!(lines.next_line().await.unwrap().is_some());
    presentation
        .deliver(&writer, "acp", &known("acp", 12, 200), false)
        .await;
    assert!(lines.next_line().await.unwrap().is_some());
    presentation
        .deliver(&writer, "acp", &known("wrong", 13, 200), false)
        .await;
    assert_eq!(presentation.delivered, Some((12, 200)));
}

#[tokio::test]
async fn usage_partial_frame_timeout_closes_connection_and_pending_requests() {
    // A positive-capacity pipe allows a genuine partial usage frame, not just
    // a never-polled writer. Keep the peer alive without draining it.
    let (out, mut peer) = tokio::io::duplex(8);
    let writer = Arc::new(Writer::with_output(out));
    let pending = tokio::spawn({
        let writer = writer.clone();
        async move { writer.request("fixture", json!({})).await }
    });
    // Drain the complete request so it is positively pending before usage.
    use tokio::io::AsyncReadExt;
    let mut request = Vec::new();
    loop {
        let byte = peer.read_u8().await.unwrap();
        request.push(byte);
        if byte == b'\n' {
            break;
        }
    }
    assert_eq!(writer.pending_count(), 1);
    let mut presentation = Presentation::default();
    tokio::time::timeout(
        Duration::from_secs(3),
        presentation.deliver(&writer, "acp", &known("acp", 1, 100), true),
    )
    .await
    .unwrap();
    assert!(presentation.delivered.is_none());
    assert_eq!(pending.await.unwrap(), Value::Null);
    assert_eq!(writer.pending_count(), 0);
    assert!(writer
        .write_context_usage(&json!({"must":"not append"}))
        .await
        .is_err());
    assert_eq!(peer.read_u8().await.unwrap(), b'{');
}

#[tokio::test]
async fn dropped_partial_usage_future_is_connection_terminal() {
    let (out, _peer) = tokio::io::duplex(1);
    let writer = Arc::new(Writer::with_output(out));
    let task = tokio::spawn({
        let writer = writer.clone();
        async move {
            writer
                .write_context_usage(&json!({"usage":"partial"}))
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    task.abort();
    let _ = task.await;
    assert!(writer.write_context_usage(&json!({})).await.is_err());
}
