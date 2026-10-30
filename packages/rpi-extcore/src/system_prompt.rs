//! Idle-only, protocol-neutral native system prompt configuration.
//!
//! Native owns branch persistence and append idempotence. This module never
//! retries a mutation whose acknowledgement may have been lost.
use crate::host::HostSession;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::sync::oneshot;

/// How native should combine the supplied text with its system prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Append,
    Replace,
}

/// Configure an idle host's system prompt (at most 256 KiB of UTF-8 text).
///
/// Requires the optional native `set_system_prompt` capability. The owned
/// worker retains the run lease until acknowledgement or host retirement.
/// Cancellation, a failed/invalid acknowledgement, or the five-second RPC
/// deadline retires the host: an unobserved mutation must not remain reusable.
pub async fn configure(host: &Arc<HostSession>, mode: Mode, text: &str) -> Result<(), String> {
    if text.len() > 256 * 1024 {
        return Err("system prompt exceeds 256 KiB".into());
    }
    if !host.supports_command("set_system_prompt") {
        return Err("native host does not support set_system_prompt".into());
    }
    let lease = host.try_run_lease()?;
    let command = json!({
        "type": "set_system_prompt",
        "mode": match mode { Mode::Append => "append", Mode::Replace => "replace" },
        "prompt": text,
    });
    let host = host.clone();
    let (mut result_tx, result_rx) = oneshot::channel();
    // The creator-owned worker survives cancellation of this API future. Its
    // receiver is the cancellation signal; no adapter must clean up the host.
    tokio::spawn(async move {
        let _lease = lease;
        let result = tokio::select! {
            biased;
            _ = result_tx.closed() => {
                host.terminate().await;
                return;
            }
            response = host.send_timeout(command, Duration::from_secs(5)) => {
                response.and_then(|response| {
                    if response["success"] == true {
                        Ok(())
                    } else {
                        Err("native rejected set_system_prompt or returned an invalid acknowledgement".into())
                    }
                })
            }
        };
        if result.is_err() {
            host.terminate().await;
        }
        // Cancellation can race with a successful acknowledgement.
        if result_tx.send(result).is_err() {
            host.terminate().await;
        }
    });
    result_rx
        .await
        .map_err(|_| "system prompt configuration worker stopped".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tokio::io::{AsyncBufReadExt, BufReader};

    fn mock(
        supported: bool,
    ) -> (
        Arc<HostSession>,
        crate::host::TestEvents,
        BufReader<tokio::io::DuplexStream>,
    ) {
        let (input, output) = tokio::io::duplex(1024 * 1024);
        let commands = if supported {
            json!(["set_system_prompt"])
        } else {
            json!([])
        };
        let (host, events) = HostSession::test_connection_with_ready(
            input,
            16,
            json!({"type":"ready","protocol":"rpi","supportedCommands":commands}),
        );
        (host, events, BufReader::new(output))
    }

    async fn next(reader: &mut BufReader<tokio::io::DuplexStream>) -> Value {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[tokio::test]
    async fn gates_bounds_and_typed_wire_modes() {
        let (unsupported, _, _) = mock(false);
        assert!(configure(&unsupported, Mode::Append, "x")
            .await
            .unwrap_err()
            .contains("does not support"));
        unsupported.terminate().await;
        let (host, events, mut reader) = mock(true);
        assert!(configure(&host, Mode::Replace, &"é".repeat(128 * 1024 + 1))
            .await
            .unwrap_err()
            .contains("256 KiB"));
        let lease = host.try_run_lease().unwrap();
        assert!(configure(&host, Mode::Append, "x")
            .await
            .unwrap_err()
            .contains("already running"));
        drop(lease);
        for (mode, wire, text) in [
            (Mode::Append, "append", "x".repeat(256 * 1024)),
            (Mode::Replace, "replace", String::new()),
        ] {
            let owner = host.clone();
            let expected = text.clone();
            let call = tokio::spawn(async move { configure(&owner, mode, &text).await });
            let request = next(&mut reader).await;
            assert_eq!(request["type"], "set_system_prompt");
            assert_eq!(request["mode"], wire);
            assert_eq!(request["prompt"], expected);
            assert!(host.try_run_lease().is_err());
            events
                .send(json!({"type":"response","id":request["id"],"success":true}))
                .unwrap();
            call.await.unwrap().unwrap();
        }
        assert!(host.try_run_lease().is_ok());
        host.terminate().await;
    }

    #[tokio::test]
    async fn cancelled_mutation_retires_host_before_releasing_lease() {
        let (host, _events, mut reader) = mock(true);
        let owner = host.clone();
        let call = tokio::spawn(async move { configure(&owner, Mode::Append, "pending").await });
        let request = next(&mut reader).await;
        assert_eq!(request["type"], "set_system_prompt");
        assert!(host.try_run_lease().is_err());
        call.abort();
        let _ = call.await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if host.try_run_lease().is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(host.snapshot().await.status, "closed");
        assert!(configure(&host, Mode::Replace, "later").await.is_err());
    }
}
