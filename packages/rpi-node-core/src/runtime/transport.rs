use super::{RuntimeError, RuntimeEventStream};
use rpi_node_api::api::WireEvent;
pub(super) use rpi_node_api::api::events::wire_event;

/// Native protocol operations, independent of client projections and networking.
///
/// Implementations preserve negotiated command/config support. Futures need not
/// be `Send`; adapting this port must not create another process owner. This is
/// not an authorization boundary and does not make raw commands safe for users.
#[async_trait::async_trait(?Send)]
pub trait RuntimeTransport {
    fn descriptor(&self) -> rpi_node_api::AgentDescriptor;
    fn supports_command(&self, command: &str) -> bool;
    fn supports_config_option(&self, session_id: &str, config_id: &str) -> bool;

    fn session_config_options(&self, _session_id: &str) -> Vec<serde_json::Value> {
        Vec::new()
    }

    async fn open_session(&self, cwd: &str) -> Result<String, RuntimeError>;
    async fn stop(&self) -> Result<(), RuntimeError>;

    async fn respond_to_agent_request(
        &self,
        _request_id: serde_json::Value,
        _result: serde_json::Value,
    ) -> Result<(), RuntimeError> {
        Err(super::unsupported(
            "session/request_permission",
            &self.descriptor().identity.protocol,
        ))
    }

    async fn load_session(
        &self,
        _session_id: &str,
        _cwd: &str,
    ) -> Result<serde_json::Value, RuntimeError> {
        Err(super::unsupported(
            "session/load",
            &self.descriptor().identity.protocol,
        ))
    }

    async fn resume_session(
        &self,
        _session_id: &str,
        _cwd: &str,
    ) -> Result<serde_json::Value, RuntimeError> {
        Err(super::unsupported(
            "session/resume",
            &self.descriptor().identity.protocol,
        ))
    }

    async fn set_config_option(
        &self,
        session_id: &str,
        config_id: &str,
        value: serde_json::Value,
    ) -> Result<serde_json::Value, RuntimeError>;

    async fn request(
        &self,
        session_id: &str,
        command: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, RuntimeError>;

    async fn subscribe(
        &self,
        session_id: &str,
        after: u64,
    ) -> Result<RuntimeEventStream, RuntimeError>;
}

/// Native streams terminate on process failure even while the client still owns
/// its broadcast sender. Buffered events are delivered before the terminal error.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn native_event_stream(
    mut receiver: tokio::sync::broadcast::Receiver<WireEvent>,
    mut terminal: tokio::sync::watch::Receiver<Option<String>>,
    protocol: &'static str,
) -> RuntimeEventStream {
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};
    let stream = async_stream::try_stream! {
        let lag_error = |count| {
            let message = format!("{protocol} event stream lagged by {count} events");
            if protocol == "ACP" { RuntimeError::Acp(message) } else { RuntimeError::Rpc(message) }
        };
        loop {
            let reason = terminal.borrow().clone();
            if let Some(reason) = reason {
                loop {
                    match receiver.try_recv() {
                        Ok(event) => yield event,
                        Err(TryRecvError::Lagged(count)) => Err(lag_error(count))?,
                        Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                    }
                }
                Err(RuntimeError::ChildExited(reason))?;
            }
            let event = tokio::select! {
                biased;
                event = receiver.recv() => Some(event),
                changed = terminal.changed() => {
                    if changed.is_err() { break; }
                    None
                }
            };
            if let Some(event) = event {
                match event {
                    Ok(event) => yield event,
                    Err(RecvError::Lagged(count)) => Err(lag_error(count))?,
                    Err(RecvError::Closed) => break,
                }
            }
        }
    };
    Box::pin(stream)
}

/// Write a frame with a single request deadline and interruption on terminal
/// state. A caller must invalidate the transport after any failed/partial write.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn write_native_frame<W: tokio::io::AsyncWrite + Unpin>(
    stdin: &tokio::sync::Mutex<W>,
    line: &[u8],
    mut terminal: tokio::sync::watch::Receiver<Option<String>>,
    deadline: tokio::time::Instant,
    on_incomplete: impl FnOnce() + Send + Sync + 'static,
) -> Result<(), RuntimeError> {
    use tokio::io::AsyncWriteExt;
    let write_terminal = terminal.clone();
    let write = async move {
        let mut stdin = stdin.lock().await;
        // The terminal branch may have been polled before a cancelled writer
        // poisoned the stream. Recheck after obtaining exclusive stdin access.
        let reason = write_terminal.borrow().clone();
        if let Some(reason) = reason {
            return Err(RuntimeError::ChildExited(reason));
        }
        // Declared after stdin, so it invalidates synchronously BEFORE stdin
        // unlocks on cancellation, write error, timeout, or EOF interruption.
        let mut incomplete = super::native_lifecycle::CleanupGuard::new(on_incomplete);
        stdin
            .write_all(line)
            .await
            .map_err(|error| RuntimeError::Process(error.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|error| RuntimeError::Process(error.to_string()))?;
        incomplete.disarm();
        Ok(())
    };
    let stopped = async {
        loop {
            let reason = terminal.borrow().clone();
            if let Some(reason) = reason {
                return reason;
            }
            if terminal.changed().await.is_err() {
                return "native transport closed".to_owned();
            }
        }
    };
    tokio::select! {
        biased;
        reason = stopped => Err(RuntimeError::ChildExited(reason)),
        result = tokio::time::timeout_at(deadline, write) => result
            .map_err(|_| RuntimeError::RequestTimeout("writing native frame".into()))?,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use std::sync::Arc;

    #[tokio::test]
    async fn native_stream_drains_buffer_before_terminal_even_with_a_retained_sender() {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            for protocol in ["ACP", "RPC"] {
                let (events, receiver) = tokio::sync::broadcast::channel(8);
                for seq in [1, 2] {
                    events.send(wire_event(Some(format!("event-{seq}")), Some(seq), "ready", serde_json::json!({}))).unwrap();
                }
                let (_terminal, stopped) = tokio::sync::watch::channel(Some("fixture EOF".into()));
                let mut stream = native_event_stream(receiver, stopped, protocol);
                for seq in [1, 2] {
                    assert_eq!(stream.next().await.unwrap().unwrap().seq, Some(seq));
                }
                assert!(matches!(stream.next().await.unwrap(), Err(RuntimeError::ChildExited(reason)) if reason == "fixture EOF"));
                assert!(stream.next().await.is_none());
                // Keep the broadcast sender alive: terminal state, not channel
                // closure, must end subscriptions retained by a caller.
                assert_eq!(events.receiver_count(), 0);
            }
        }).await.expect("a retained sender must not keep terminal streams alive");
    }

    #[tokio::test]
    async fn native_stream_lag_keeps_protocol_category_in_live_and_terminal_branches() {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            for protocol in ["ACP", "RPC"] {
                for terminal in [None, Some("fixture EOF".to_owned())] {
                    let (events, receiver) = tokio::sync::broadcast::channel(1);
                    for seq in 0..3 {
                        events
                            .send(wire_event(None, Some(seq), "ready", serde_json::json!({})))
                            .unwrap();
                    }
                    let (_terminal, stopped) = tokio::sync::watch::channel(terminal);
                    let mut stream = native_event_stream(receiver, stopped, protocol);
                    let error = stream.next().await.unwrap().unwrap_err();
                    assert_eq!(
                        error.to_string(),
                        format!(
                            "{protocol} transport error: {protocol} event stream lagged by 2 events"
                        )
                    );
                    match (protocol, error) {
                        ("ACP", RuntimeError::Acp(_)) | ("RPC", RuntimeError::Rpc(_)) => {}
                        (_, other) => panic!("wrong lag category: {other:?}"),
                    }
                    assert!(stream.next().await.is_none());
                }
            }
        })
        .await
        .expect("lag must fail explicitly rather than waiting for another event");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn native_write_interrupts_backpressure_and_lock_wait_on_eof_or_deadline() {
        use tokio::{
            io::duplex,
            sync::{Mutex, watch},
            time::{Duration, Instant, timeout},
        };
        for locked in [false, true] {
            for stopped in [false, true] {
                let (writer, _unread_pipe) = duplex(1);
                let stdin = Mutex::new(writer);
                let held_lock = if locked {
                    Some(stdin.lock().await)
                } else {
                    None
                };
                let (terminal, receiver) = watch::channel(None);
                let deadline =
                    Instant::now() + Duration::from_millis(if stopped { 1000 } else { 30 });
                let mut write = std::pin::pin!(write_native_frame(
                    &stdin,
                    b"blocked frame",
                    receiver,
                    deadline,
                    || {},
                ));
                tokio::select! {
                    result = &mut write => panic!("write should be blocked: {result:?}"),
                    () = tokio::time::sleep(Duration::from_millis(5)) => {}
                }
                if stopped {
                    terminal.send_replace(Some("fixture EOF".into()));
                }
                let result = timeout(Duration::from_secs(1), write).await.unwrap();
                if stopped {
                    assert!(matches!(result, Err(RuntimeError::ChildExited(_))));
                } else {
                    assert!(matches!(result, Err(RuntimeError::RequestTimeout(_))));
                }
                drop(held_lock);
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn incomplete_frame_poisons_queued_writer_before_unlocking_stdin() {
        use tokio::{
            io::{AsyncReadExt, duplex},
            sync::{Mutex, watch},
            time::{Duration, Instant},
        };
        let (writer, mut reader) = duplex(1);
        let stdin = Arc::new(Mutex::new(writer));
        let (terminal, receiver) = watch::channel(None);
        let deadline = Instant::now() + Duration::from_secs(1);
        let observed_lock = Arc::clone(&stdin);
        let poison = terminal.clone();
        let mut first = Box::pin(write_native_frame(
            &stdin,
            b"partial",
            receiver.clone(),
            deadline,
            move || {
                assert!(
                    observed_lock.try_lock().is_err(),
                    "poison must run while stdin is still locked"
                );
                poison.send_replace(Some("incomplete frame".into()));
            },
        ));
        assert!(futures_util::poll!(&mut first).is_pending());
        let mut queued = Box::pin(write_native_frame(
            &stdin,
            b"next frame",
            receiver,
            deadline,
            || {},
        ));
        assert!(futures_util::poll!(&mut queued).is_pending());
        drop(first);
        assert!(matches!(queued.await, Err(RuntimeError::ChildExited(_))));
        let mut byte = [0];
        reader.read_exact(&mut byte).await.unwrap();
        assert_eq!(byte, [b'p']);
        // The queued writer must not have appended any bytes.
        assert!(futures_util::poll!(Box::pin(reader.read(&mut byte))).is_pending());
        assert!(terminal.borrow().is_some());
    }
}
