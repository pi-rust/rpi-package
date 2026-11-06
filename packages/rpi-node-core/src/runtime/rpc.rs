#![cfg(not(target_arch = "wasm32"))]

//! Native JSONL transport for `rpi --mode rpc`.
//!
//! The child protocol is deliberately kept separate from HTTP/SSE. A single
//! reader owns stdout, matches responses by id, and broadcasts notifications;
//! stderr is drained independently so diagnostic output can never corrupt the
//! JSONL channel.

use rpi_node_api::{AgentDescriptor, CapabilitySet, TransportIdentity};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::time::{Duration, Instant, timeout, timeout_at};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{Mutex, broadcast, oneshot, watch},
};

use super::native_lifecycle::{CleanupGuard, State, terminate_child};
use super::transport::wire_event;
use super::{RuntimeError, RuntimeEventStream};
use rpi_node_api::api::WireEvent;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_FRAME_BYTES: usize = 1_048_576;

struct RpcInner {
    stdin: Mutex<ChildStdin>,
    child: Arc<Mutex<Child>>,
    next_id: AtomicU64,
    pending: State<HashMap<u64, oneshot::Sender<Result<Value, RuntimeError>>>>,
    events: broadcast::Sender<WireEvent>,
    terminal: watch::Sender<Option<String>>,
    descriptor: RwLock<AgentDescriptor>,
    commands: RwLock<Vec<String>>,
}

struct RequestGuard {
    inner: Arc<RpcInner>,
    id: u64,
    writing: bool,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.inner.pending.lock().remove(&self.id);
        if self.writing {
            fail_pending(&self.inner, "rpi RPC request write cancelled", true);
            terminate_child(&self.inner.child);
        }
    }
}

/// A live native `rpi --mode rpc` process.
#[derive(Clone)]
pub struct RpiRpcClient {
    inner: Arc<RpcInner>,
    // Shared only by public handles, never by the stdout task.
    _lifetime: Arc<CleanupGuard>,
}

impl RpiRpcClient {
    pub async fn spawn(
        executable: impl AsRef<Path>,
        cwd: impl Into<PathBuf>,
    ) -> Result<Self, RuntimeError> {
        let mut command = Command::new(executable.as_ref());
        command
            .args(["--mode", "rpc"])
            .current_dir(cwd.into())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|error| RuntimeError::Process(error.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RuntimeError::Process("rpi RPC stdin was not piped".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RuntimeError::Process("rpi RPC stdout was not piped".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| RuntimeError::Process("rpi RPC stderr was not piped".into()))?;

        let (events, _) = broadcast::channel(256);
        let (ready_tx, ready_rx) = oneshot::channel();
        let descriptor = AgentDescriptor {
            identity: TransportIdentity {
                protocol: "rpi".into(),
                version: None,
                transport: "jsonl-stdio".into(),
                agent_name: Some("rpi --mode rpc".into()),
            },
            capabilities: CapabilitySet::rpc_default(),
        };
        let inner = Arc::new(RpcInner {
            stdin: Mutex::new(stdin),
            child: Arc::new(Mutex::new(child)),
            next_id: AtomicU64::new(1),
            pending: State::new(HashMap::new()),
            events,
            terminal: watch::channel(None).0,
            descriptor: RwLock::new(descriptor),
            commands: RwLock::new(Vec::new()),
        });

        let owned = Arc::clone(&inner);
        let lifetime = Arc::new(CleanupGuard::new(move || {
            fail_pending(&owned, "last rpi RPC client dropped", true);
            terminate_child(&owned.child);
        }));
        tokio::spawn(read_stdout(stdout, Arc::clone(&inner), ready_tx));
        tokio::spawn(drain_stderr(stderr));
        let ready = timeout(DEFAULT_TIMEOUT, ready_rx)
            .await
            .map_err(|_| RuntimeError::RequestTimeout("waiting for rpi RPC ready".into()))?
            .map_err(|_| RuntimeError::ChildExited("rpi RPC closed before ready".into()))??;
        if ready.get("protocol").and_then(Value::as_str) != Some("rpi") {
            return Err(RuntimeError::ProtocolMismatch(
                "rpi RPC ready frame did not advertise protocol=rpi".into(),
            ));
        }
        {
            let mut descriptor = inner
                .descriptor
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            apply_ready_descriptor(&mut descriptor, &ready);
            *inner
                .commands
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = ready
                .get("supportedCommands")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
        }
        Ok(Self {
            inner,
            _lifetime: lifetime,
        })
    }

    #[must_use]
    pub fn descriptor(&self) -> AgentDescriptor {
        self.inner
            .descriptor
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub async fn request(&self, command: &str, payload: Value) -> Result<Value, RuntimeError> {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.inner.pending.lock();
            if let Some(reason) = self.inner.terminal.borrow().clone() {
                return Err(RuntimeError::ChildExited(reason));
            }
            pending.insert(id, tx);
        }

        let mut cancellation = RequestGuard {
            inner: Arc::clone(&self.inner),
            id,
            writing: false,
        };
        let mut message = match payload {
            Value::Object(object) => object,
            _ => Map::new(),
        };
        message.insert("id".into(), json!(id));
        message.insert("type".into(), json!(command));
        let mut line = serde_json::to_vec(&Value::Object(message))?;
        line.push(b'\n');
        if line.len() > MAX_FRAME_BYTES {
            self.inner.pending.lock().remove(&id);
            return Err(RuntimeError::Rpc(
                "request exceeds the RPC frame limit".into(),
            ));
        }
        cancellation.writing = true;
        let owned = Arc::clone(&self.inner);
        if let Err(error) = super::transport::write_native_frame(
            &self.inner.stdin,
            &line,
            self.inner.terminal.subscribe(),
            deadline,
            move || {
                fail_pending(&owned, "rpi RPC incomplete frame", true);
                terminate_child(&owned.child);
            },
        )
        .await
        {
            // A cancelled/partial write cannot safely be followed by another
            // JSONL frame. Mark the process unusable before returning.
            fail_pending(&self.inner, "rpi RPC stdin write failed", true);
            let _ = self.inner.child.lock().await.start_kill();
            return Err(error);
        }
        cancellation.writing = false;
        let result = timeout_at(deadline, rx)
            .await
            .map_err(|_| {
                RuntimeError::RequestTimeout(format!("waiting for rpi RPC response to {command}"))
            })
            .and_then(|result| {
                result.map_err(|_| {
                    RuntimeError::ChildExited("rpi RPC response channel closed".into())
                })
            })
            .and_then(std::convert::identity);
        self.inner.pending.lock().remove(&id);
        result
    }

    pub fn subscribe_events(&self) -> Result<RuntimeEventStream, RuntimeError> {
        Ok(super::transport::native_event_stream(
            self.inner.events.subscribe(),
            self.inner.terminal.subscribe(),
            "RPC",
        ))
    }

    pub async fn stop(&self) -> Result<(), RuntimeError> {
        if self.inner.terminal.borrow().is_none() {
            // Graceful stop is best-effort; a hung peer must not hold node
            // switching hostage for the normal request timeout.
            let _ = timeout(
                Duration::from_millis(250),
                self.request("stop", Value::Null),
            )
            .await;
        }
        fail_pending(&self.inner, "rpi RPC stopped", true);
        let mut child = self.inner.child.lock().await;
        if child
            .try_wait()
            .map_err(|error| RuntimeError::Process(error.to_string()))?
            .is_none()
        {
            child
                .kill()
                .await
                .map_err(|error| RuntimeError::Process(error.to_string()))?;
        }
        drop(child);
        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl super::RuntimeTransport for RpiRpcClient {
    fn descriptor(&self) -> AgentDescriptor {
        // Call the inherent accessor explicitly; using `self.descriptor()` here
        // recursively dispatches this trait method until the stack overflows.
        RpiRpcClient::descriptor(self)
    }

    fn supports_command(&self, command: &str) -> bool {
        self.inner
            .commands
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|supported| supported == command)
    }

    fn supports_config_option(&self, _session_id: &str, config_id: &str) -> bool {
        match config_id {
            "model" => self.supports_command("set_model"),
            "thinking" | "thought_level" => self.supports_command("set_thinking_level"),
            _ => false,
        }
    }

    async fn stop(&self) -> Result<(), RuntimeError> {
        RpiRpcClient::stop(self).await
    }

    async fn open_session(&self, _cwd: &str) -> Result<String, RuntimeError> {
        let state = self.request("get_state", Value::Null).await?;
        state
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| RuntimeError::Rpc("RPC get_state omitted sessionId".into()))
    }

    async fn set_config_option(
        &self,
        _session_id: &str,
        config_id: &str,
        value: Value,
    ) -> Result<Value, RuntimeError> {
        let (command, payload) = match config_id {
            "model" => ("set_model", json!({"modelId": value})),
            "thinking" | "thought_level" => ("set_thinking_level", json!({"level": value})),
            _ => return Err(super::unsupported("session/set_config_option", "rpi")),
        };
        self.request(command, payload).await
    }

    async fn request(
        &self,
        _session_id: &str,
        command: &str,
        payload: Value,
    ) -> Result<Value, RuntimeError> {
        let value = RpiRpcClient::request(self, command, payload).await?;
        if value.is_null() {
            return Ok(value);
        }
        Ok(value)
    }

    async fn subscribe(
        &self,
        _session_id: &str,
        _after: u64,
    ) -> Result<RuntimeEventStream, RuntimeError> {
        std::future::ready(()).await;
        self.subscribe_events()
    }
}

fn apply_ready_descriptor(descriptor: &mut AgentDescriptor, ready: &Value) {
    descriptor.identity.version = ready
        .get("piCompatibilityTarget")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let commands = ready
        .get("supportedCommands")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let has = |name: &str| commands.iter().any(|item| item.as_str() == Some(name));
    let capabilities = &mut descriptor.capabilities;
    capabilities.sessions = has("get_state");
    capabilities.prompt = has("prompt");
    capabilities.streaming = capabilities.prompt;
    capabilities.abort = has("abort");
    capabilities.dialogs = has("ui_dialog_response");
    capabilities.models = has("get_available_models");
    capabilities.thinking_levels = has("get_available_thinking_levels");
    capabilities.tools = has("get_available_tools");
    capabilities.queue = has("get_queue") || has("queue_message");
    capabilities.history = has("get_history") || has("get_messages");
    capabilities.provider_management =
        has("get_auth_status") && (has("set_api_key") || has("oauth"));
}

async fn read_stdout<R>(
    stdout: R,
    inner: Arc<RpcInner>,
    ready_tx: oneshot::Sender<Result<Value, RuntimeError>>,
) where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stdout).lines();
    let mut ready_tx = Some(ready_tx);
    while let Ok(Some(line)) = lines.next_line().await {
        if line.len() > MAX_FRAME_BYTES {
            fail_pending(&inner, "RPC frame exceeds the frame limit", true);
            if let Some(tx) = ready_tx.take() {
                let _ = tx.send(Err(RuntimeError::Rpc(
                    "RPC ready frame exceeds the frame limit".into(),
                )));
            }
            return;
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                fail_pending(&inner, &format!("invalid RPC JSONL frame: {error}"), false);
                if let Some(tx) = ready_tx.take() {
                    let _ = tx.send(Err(RuntimeError::Rpc(format!(
                        "invalid RPC JSONL frame: {error}"
                    ))));
                }
                return;
            }
        };
        if value.get("type").and_then(Value::as_str) == Some("ready") {
            if let Some(tx) = ready_tx.take() {
                let _ = tx.send(Ok(value));
            }
            continue;
        }
        if value.get("type").and_then(Value::as_str) == Some("response") {
            if let Some(id) = value.get("id").and_then(Value::as_u64) {
                let sender = inner.pending.lock().remove(&id);
                if let Some(sender) = sender {
                    let result = if value.get("success").and_then(Value::as_bool) == Some(false) {
                        Err(RuntimeError::Rpc(
                            value
                                .get("error")
                                .and_then(Value::as_str)
                                .unwrap_or("RPC command failed")
                                .into(),
                        ))
                    } else {
                        Ok(value.get("data").cloned().unwrap_or(Value::Null))
                    };
                    let _ = sender.send(result);
                }
                continue;
            }
        }
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message")
            .to_owned();
        let id = value
            .get("eventId")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let seq = value.get("seq").and_then(Value::as_u64);
        let _ = inner.events.send(wire_event(id, seq, event_type, value));
    }
    fail_pending(&inner, "rpi RPC stdout reached EOF", true);
    if let Some(tx) = ready_tx.take() {
        let _ = tx.send(Err(RuntimeError::ChildExited(
            "rpi RPC closed before ready".into(),
        )));
    }
}

fn fail_pending(inner: &RpcInner, message: &str, child_exit: bool) {
    inner.terminal.send_if_modified(|terminal| {
        if terminal.is_some() {
            return false;
        }
        *terminal = Some(message.to_owned());
        true
    });
    let mut pending = inner.pending.lock();
    for (_, sender) in pending.drain() {
        let error = if child_exit {
            RuntimeError::ChildExited(message.into())
        } else {
            RuntimeError::Rpc(message.into())
        };
        let _ = sender.send(Err(error));
    }
}

async fn drain_stderr<R>(stderr: R)
where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stderr).lines();
    while lines.next_line().await.ok().flatten().is_some() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn response_wait_cancellation_removes_pending_without_poisoning_rpc() {
        let mut child = crate::runtime::native_lifecycle::test_child();
        let inner = Arc::new(RpcInner {
            commands: RwLock::new(Vec::new()),
            stdin: Mutex::new(child.stdin.take().unwrap()),
            child: Arc::new(Mutex::new(child)),
            next_id: AtomicU64::new(1),
            pending: State::new(HashMap::new()),
            events: broadcast::channel(8).0,
            terminal: watch::channel(None).0,
            descriptor: RwLock::new(AgentDescriptor {
                identity: TransportIdentity {
                    protocol: "rpi".into(),
                    version: None,
                    transport: "jsonl-stdio".into(),
                    agent_name: None,
                },
                capabilities: CapabilitySet::rpc_default(),
            }),
        });
        let (tx, rx) = oneshot::channel();
        inner.pending.lock().insert(7, tx);
        let guard = RequestGuard {
            inner: Arc::clone(&inner),
            id: 7,
            writing: false,
        };
        drop(guard);
        assert!(
            rx.await.is_err(),
            "cancelled sender must be removed immediately"
        );
        assert_eq!(inner.pending.lock().len(), 0);
        assert!(inner.terminal.borrow().is_none());
        terminate_child(&inner.child);
        tokio::time::timeout(Duration::from_secs(2), inner.child.lock().await.wait())
            .await
            .unwrap()
            .unwrap();
    }
}
