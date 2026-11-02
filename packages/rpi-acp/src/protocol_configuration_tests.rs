use super::*;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Reports an actual AsyncWrite::Pending, not a timer-based assumption that
/// output must be blocked. The tiny duplex first accepts a partial frame.
pub(super) struct ObservedPending {
    pub output: tokio::io::DuplexStream,
    pub pending: Arc<tokio::sync::Notify>,
}
impl AsyncWrite for ObservedPending {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let result = Pin::new(&mut self.output).poll_write(cx, bytes);
        if result.is_pending() {
            self.pending.notify_one();
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.output).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.output).poll_shutdown(cx)
    }
}

#[tokio::test]
async fn terminal_partial_write_never_allows_a_later_frame() {
    use tokio::io::AsyncReadExt;
    let (output, mut peer) = tokio::io::duplex(8);
    let pending = Arc::new(tokio::sync::Notify::new());
    let writer = Arc::new(Writer::with_output(ObservedPending {
        output,
        pending: pending.clone(),
    }));
    let task_writer = writer.clone();
    let task = tokio::spawn(async move {
        task_writer
            .write(&json!({"long":"partial frame must not be followed"}))
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), pending.notified())
        .await
        .unwrap();
    writer.close();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(writer.write(&json!({"second":"forbidden"})).await.is_err());
    drop(writer);
    let mut bytes = Vec::new();
    peer.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(bytes.len(), 8);
    assert!(!bytes.contains(&b'\n'));
}

#[tokio::test]
async fn host_stop_interrupts_observed_pending_dispatch_response() {
    use rpi_plugin_sdk::stdio::InputLine;
    let (output, _peer) = tokio::io::duplex(8);
    let pending = Arc::new(tokio::sync::Notify::new());
    let writer = Arc::new(Writer::with_output(ObservedPending {
        output,
        pending: pending.clone(),
    }));
    let (stop_tx, stop) = watch::channel(false);
    let server = run_connection(
        stop,
        || Ok(InputLine::Pending),
        Some(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}})
                .to_string(),
        ),
        writer,
        Err("unused".into()),
    );
    let client = async {
        tokio::time::timeout(std::time::Duration::from_secs(1), pending.notified())
            .await
            .unwrap();
        stop_tx.send_replace(true);
    };
    let (exit, ()) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        tokio::join!(server, client)
    })
    .await
    .unwrap();
    assert_eq!(exit, 0);
}

#[tokio::test]
async fn configuration_deadline_does_not_apply_to_existing_writes() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let (output, peer) = tokio::io::duplex(8);
    let pending = Arc::new(tokio::sync::Notify::new());
    let writer = Arc::new(Writer::with_output(ObservedPending {
        output,
        pending: pending.clone(),
    }));
    let task_writer = writer.clone();
    let task =
        tokio::spawn(async move { task_writer.write(&json!({"id":99,"result":"legacy"})).await });
    pending.notified().await;
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    assert!(!task.is_finished());
    assert!(!writer.closed.load(Ordering::Acquire));
    let mut lines = BufReader::new(peer).lines();
    let frame: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(frame["id"], 99);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn configuration_deadline_closes_before_any_later_frame() {
    let (output, _peer) = tokio::io::duplex(8);
    let pending = Arc::new(tokio::sync::Notify::new());
    let writer = Arc::new(Writer::with_output(ObservedPending {
        output,
        pending: pending.clone(),
    }));
    let task_writer = writer.clone();
    let task = tokio::spawn(async move {
        task_writer
            .write_configuration(&json!({"configuration":"partial"}))
            .await
    });
    pending.notified().await;
    let error = tokio::time::timeout(std::time::Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(writer.closed.load(Ordering::Acquire));
    assert!(writer.write(&json!({"later":"forbidden"})).await.is_err());
}
