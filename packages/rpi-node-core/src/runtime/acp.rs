#![cfg(not(target_arch = "wasm32"))]

//! ACP v1 JSON-RPC client over stdio.

use rpi_node_api::{AgentDescriptor, CapabilitySet, TransportIdentity};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::time::{Duration, Instant, timeout_at};
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
const PROMPT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_FRAME_BYTES: usize = 1_048_576;

struct PendingPermission {
    dialog: rpi_node_api::api::PendingDialog,
    responding: bool,
}

struct PendingRequest {
    sender: oneshot::Sender<Result<Value, RuntimeError>>,
    config_session: Option<String>,
    restore_boundary: bool,
}

struct AcpInner {
    stdin: Mutex<ChildStdin>,
    child: Arc<Mutex<Child>>,
    next_id: AtomicU64,
    next_event: AtomicU64,
    pending: State<HashMap<String, PendingRequest>>,
    permissions: State<HashMap<String, PendingPermission>>,
    config_options: State<HashMap<String, Vec<Value>>>,
    events: broadcast::Sender<WireEvent>,
    terminal: watch::Sender<Option<String>>,
    descriptor: RwLock<AgentDescriptor>,
}

struct RequestGuard {
    inner: Arc<AcpInner>,
    key: String,
    writing: bool,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.inner.pending.lock().remove(&self.key);
        if self.writing {
            fail_pending(&self.inner, "ACP request write cancelled");
            terminate_child(&self.inner.child);
        }
    }
}

#[derive(Clone)]
pub struct AcpClient {
    inner: Arc<AcpInner>,
    // Reader ownership must not keep the public client lifetime alive.
    _lifetime: Arc<CleanupGuard>,
}

impl AcpClient {
    pub async fn spawn(
        executable: impl AsRef<Path>,
        args: &[String],
        cwd: impl Into<PathBuf>,
    ) -> Result<Self, RuntimeError> {
        let mut command = Command::new(executable.as_ref());
        command
            .args(args)
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
            .ok_or_else(|| RuntimeError::Process("ACP stdin was not piped".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RuntimeError::Process("ACP stdout was not piped".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| RuntimeError::Process("ACP stderr was not piped".into()))?;
        let (events, _) = broadcast::channel(256);
        let inner = Arc::new(AcpInner {
            stdin: Mutex::new(stdin),
            child: Arc::new(Mutex::new(child)),
            next_id: AtomicU64::new(1),
            next_event: AtomicU64::new(1),
            pending: State::new(HashMap::new()),
            permissions: State::new(HashMap::new()),
            config_options: State::new(HashMap::new()),
            events,
            terminal: watch::channel(None).0,
            descriptor: RwLock::new(AgentDescriptor {
                identity: TransportIdentity {
                    protocol: "acp".into(),
                    version: Some("1".into()),
                    transport: "json-rpc-stdio".into(),
                    agent_name: None,
                },
                capabilities: CapabilitySet::acp_default(),
            }),
        });
        let owned = Arc::clone(&inner);
        let lifetime = Arc::new(CleanupGuard::new(move || {
            fail_pending(&owned, "last ACP client dropped");
            terminate_child(&owned.child);
        }));
        tokio::spawn(read_stdout(stdout, Arc::clone(&inner)));
        tokio::spawn(drain_stderr(stderr));
        let client = Self {
            inner,
            _lifetime: lifetime,
        };
        let initialized = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientInfo": {"name": "rpi-work", "version": env!("CARGO_PKG_VERSION")},
                    "clientCapabilities": {}
                }),
            )
            .await?;
        if initialized.get("protocolVersion").and_then(Value::as_u64) != Some(1) {
            return Err(RuntimeError::ProtocolMismatch(
                "ACP agent did not negotiate protocolVersion=1".into(),
            ));
        }
        {
            let mut descriptor = client
                .inner
                .descriptor
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            descriptor.identity.agent_name = initialized
                .get("agentInfo")
                .and_then(|info| info.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            if initialized
                .get("agentCapabilities")
                .and_then(|caps| caps.get("loadSession"))
                .and_then(Value::as_bool)
                == Some(true)
            {
                // Loading replays events; it is not a random-access history API.
                descriptor.capabilities.session_load = true;
            }
            if initialized
                .pointer("/agentCapabilities/sessionCapabilities/resume")
                .is_some()
            {
                descriptor.capabilities.session_resume = true;
            }
        }
        Ok(client)
    }

    #[must_use]
    pub fn descriptor(&self) -> AgentDescriptor {
        self.descriptor_snapshot()
    }

    fn descriptor_snapshot(&self) -> AgentDescriptor {
        self.inner
            .descriptor
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RuntimeError> {
        if method == "session/cancel" {
            self.notify(method, params).await?;
            return Ok(Value::Null);
        }
        let started = Instant::now();
        let deadline = started
            + if method == "session/prompt" {
                PROMPT_TIMEOUT
            } else {
                DEFAULT_TIMEOUT
            };
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let key = id.to_string();
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.inner.pending.lock();
            if let Some(reason) = self.inner.terminal.borrow().clone() {
                return Err(RuntimeError::ChildExited(reason));
            }
            pending.insert(
                key.clone(),
                PendingRequest {
                    sender: tx,
                    restore_boundary: matches!(method, "session/load" | "session/resume"),
                    config_session: matches!(
                        method,
                        "session/new"
                            | "session/load"
                            | "session/resume"
                            | "session/set_config_option"
                    )
                    .then(|| {
                        params
                            .get("sessionId")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned()
                    }),
                },
            );
        }
        let mut cancellation = RequestGuard {
            inner: Arc::clone(&self.inner),
            key: key.clone(),
            writing: false,
        };
        let message = json!({"jsonrpc":"2.0", "id": id, "method": method, "params": params});
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        if line.len() > MAX_FRAME_BYTES {
            self.inner.pending.lock().remove(&key);
            return Err(RuntimeError::Acp(
                "ACP frame exceeds the frame limit".into(),
            ));
        }
        cancellation.writing = true;
        let owned = Arc::clone(&self.inner);
        if let Err(error) = super::transport::write_native_frame(
            &self.inner.stdin,
            &line,
            self.inner.terminal.subscribe(),
            started + DEFAULT_TIMEOUT,
            move || {
                fail_pending(&owned, "ACP incomplete request frame");
                terminate_child(&owned.child);
            },
        )
        .await
        {
            fail_pending(&self.inner, "ACP stdin write failed");
            let _ = self.inner.child.lock().await.start_kill();
            return Err(error);
        }
        cancellation.writing = false;
        let result = timeout_at(deadline, rx)
            .await
            .map_err(|_| {
                RuntimeError::RequestTimeout(format!("waiting for ACP response to {method}"))
            })
            .and_then(|result| {
                result.map_err(|_| RuntimeError::ChildExited("ACP response channel closed".into()))
            })
            .and_then(std::convert::identity);
        self.inner.pending.lock().remove(&key);
        result
    }

    pub async fn new_session(&self, cwd: impl Into<String>) -> Result<String, RuntimeError> {
        let result = self
            .request("session/new", json!({"cwd": cwd.into(), "mcpServers": []}))
            .await?;
        let session_id = result
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| RuntimeError::Acp("session/new omitted sessionId".into()))?;
        Ok(session_id)
    }

    pub async fn load_session(&self, session_id: &str, cwd: &str) -> Result<Value, RuntimeError> {
        let result = self
            .request(
                "session/load",
                json!({"sessionId": session_id, "cwd": cwd, "mcpServers": []}),
            )
            .await?;
        Ok(result)
    }

    pub async fn resume_session(&self, session_id: &str, cwd: &str) -> Result<Value, RuntimeError> {
        let result = self
            .request(
                "session/resume",
                json!({"sessionId": session_id, "cwd": cwd, "mcpServers": []}),
            )
            .await?;
        Ok(result)
    }

    /// A snapshot of the full options negotiated for this session, not a catalog.
    #[must_use]
    pub fn session_config_options(&self, session_id: &str) -> Vec<Value> {
        self.inner
            .config_options
            .lock()
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    fn negotiated_config(&self, session_id: &str, config_id: &str) -> Option<Value> {
        let configs = self.inner.config_options.lock();
        let options = configs.get(session_id)?;
        let selectable = |option: &&Value| option["type"] == "select";
        // Exact negotiated IDs take precedence; only known facade aliases may fall back.
        options
            .iter()
            .filter(selectable)
            .find(|option| option["id"] == config_id)
            .or_else(|| {
                let alias = match config_id {
                    "thinking" => "thought_level",
                    "thought_level" => "thinking",
                    _ => return None,
                };
                options
                    .iter()
                    .filter(selectable)
                    .find(|option| option["id"] == alias)
            })
            .cloned()
    }

    pub async fn set_config_option(
        &self,
        session_id: &str,
        config_id: &str,
        value: Value,
    ) -> Result<Value, RuntimeError> {
        let option = self
            .negotiated_config(session_id, config_id)
            .ok_or_else(|| super::unsupported("session/set_config_option", "acp"))?;
        let id = option
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| super::unsupported("session/set_config_option", "acp"))?;
        if !option
            .get("options")
            .and_then(Value::as_array)
            .is_some_and(|options| config_value_is_negotiated(options, &value))
        {
            return Err(RuntimeError::Acp(format!(
                "unnegotiated value for ACP config {id}"
            )));
        }
        let result = self
            .request(
                "session/set_config_option",
                json!({"sessionId": session_id, "configId": id, "value": value}),
            )
            .await?;
        Ok(result)
    }

    pub async fn prompt(&self, session_id: &str, text: &str) -> Result<Value, RuntimeError> {
        let result = self
            .request(
                "session/prompt",
                json!({"sessionId": session_id, "prompt": [{"type":"text", "text": text}]}),
            )
            .await?;
        let stop_reason = result
            .get("stopReason")
            .and_then(Value::as_str)
            .unwrap_or("end_turn");
        let _ = self.inner.events.send(wire_event(
            Some(format!(
                "acp-settled-{}",
                self.inner.next_event.fetch_add(1, Ordering::Relaxed)
            )),
            None,
            "agent_settled",
            json!({"type":"agent_settled", "outcome": acp_outcome(stop_reason), "sessionId": session_id}),
        ));
        Ok(result)
    }

    pub async fn cancel(&self, session_id: &str) -> Result<Value, RuntimeError> {
        self.request("session/cancel", json!({"sessionId": session_id}))
            .await
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), RuntimeError> {
        let mut line =
            serde_json::to_vec(&json!({"jsonrpc":"2.0", "method":method, "params":params}))?;
        line.push(b'\n');
        if line.len() > MAX_FRAME_BYTES {
            return Err(RuntimeError::Acp(
                "ACP frame exceeds the frame limit".into(),
            ));
        }
        // The shared writer serializes complete frames and poisons the transport
        // before releasing stdin if cancelled during a partial write.
        let owned = Arc::clone(&self.inner);
        let result = super::transport::write_native_frame(
            &self.inner.stdin,
            &line,
            self.inner.terminal.subscribe(),
            Instant::now() + DEFAULT_TIMEOUT,
            move || {
                fail_pending(&owned, "ACP incomplete notification frame");
                terminate_child(&owned.child);
            },
        )
        .await;
        if result.is_err() {
            fail_pending(&self.inner, "ACP notification write failed");
            terminate_child(&self.inner.child);
        }
        result
    }

    pub async fn respond_to_agent_request(
        &self,
        request_id: Value,
        result: Value,
    ) -> Result<(), RuntimeError> {
        let key = request_id.to_string();
        let message = json!({"jsonrpc":"2.0", "id": request_id, "result": result});
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        if line.len() > MAX_FRAME_BYTES {
            return Err(RuntimeError::Acp(
                "ACP frame exceeds the frame limit".into(),
            ));
        }
        {
            let mut permissions = self.inner.permissions.lock();
            let pending = permissions.get_mut(&key).ok_or_else(|| {
                RuntimeError::Acp("unknown or already answered ACP permission request".into())
            })?;
            if pending.responding {
                return Err(RuntimeError::Acp(
                    "ACP permission response already in progress".into(),
                ));
            }
            pending.dialog.validate_acp_permission_result(&result)?;
            pending.responding = true;
        }
        let owned = Arc::clone(&self.inner);
        let mut cancellation = CleanupGuard::new(move || {
            fail_pending(&owned, "ACP permission response cancelled");
            terminate_child(&owned.child);
        });
        let owned = Arc::clone(&self.inner);
        if let Err(error) = super::transport::write_native_frame(
            &self.inner.stdin,
            &line,
            self.inner.terminal.subscribe(),
            Instant::now() + DEFAULT_TIMEOUT,
            move || {
                fail_pending(&owned, "ACP incomplete permission frame");
                terminate_child(&owned.child);
            },
        )
        .await
        {
            fail_pending(&self.inner, "ACP permission write failed");
            let _ = self.inner.child.lock().await.start_kill();
            cancellation.disarm();
            return Err(error);
        }
        if let Some(pending) = self.inner.permissions.lock().remove(&key) {
            let _ = self.inner.events.send(wire_event(
                None,
                None,
                "ui_dialog_closed",
                json!({"type":"ui_dialog_closed", "requestId":pending.dialog.request_id, "jsonRpcRequestId":pending.dialog.response_id}),
            ));
        }
        cancellation.disarm();
        Ok(())
    }

    pub fn subscribe_events(&self) -> Result<RuntimeEventStream, RuntimeError> {
        Ok(super::transport::native_event_stream(
            self.inner.events.subscribe(),
            self.inner.terminal.subscribe(),
            "ACP",
        ))
    }

    pub async fn stop(&self) -> Result<(), RuntimeError> {
        fail_pending(&self.inner, "ACP stopped");
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
impl super::RuntimeTransport for AcpClient {
    fn descriptor(&self) -> AgentDescriptor {
        self.descriptor_snapshot()
    }

    fn supports_command(&self, command: &str) -> bool {
        matches!(command, "prompt" | "abort" | "cancel")
    }

    fn supports_config_option(&self, session_id: &str, config_id: &str) -> bool {
        self.negotiated_config(session_id, config_id).is_some()
    }

    fn session_config_options(&self, session_id: &str) -> Vec<Value> {
        AcpClient::session_config_options(self, session_id)
    }

    async fn respond_to_agent_request(
        &self,
        request_id: Value,
        result: Value,
    ) -> Result<(), RuntimeError> {
        AcpClient::respond_to_agent_request(self, request_id, result).await
    }

    async fn stop(&self) -> Result<(), RuntimeError> {
        AcpClient::stop(self).await
    }

    async fn open_session(&self, cwd: &str) -> Result<String, RuntimeError> {
        self.new_session(cwd).await
    }

    async fn set_config_option(
        &self,
        session_id: &str,
        config_id: &str,
        value: Value,
    ) -> Result<Value, RuntimeError> {
        AcpClient::set_config_option(self, session_id, config_id, value).await
    }

    async fn load_session(&self, session_id: &str, cwd: &str) -> Result<Value, RuntimeError> {
        AcpClient::load_session(self, session_id, cwd).await
    }

    async fn resume_session(&self, session_id: &str, cwd: &str) -> Result<Value, RuntimeError> {
        AcpClient::resume_session(self, session_id, cwd).await
    }

    async fn request(
        &self,
        session_id: &str,
        command: &str,
        payload: Value,
    ) -> Result<Value, RuntimeError> {
        match command {
            "prompt" => {
                self.prompt(
                    session_id,
                    payload
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )
                .await
            }
            "abort" | "cancel" => self.cancel(session_id).await,
            _ => Err(super::unsupported(command, "acp")),
        }
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

async fn read_stdout<R>(stdout: R, inner: Arc<AcpInner>)
where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.len() > MAX_FRAME_BYTES {
            fail_pending(&inner, "ACP frame exceeds the frame limit");
            return;
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                fail_pending(&inner, &format!("invalid ACP JSON-RPC frame: {error}"));
                return;
            }
        };
        let method = value.get("method").and_then(Value::as_str);
        if method == Some("session/update") {
            if let Some(event) = update_event(&value, &inner) {
                let _ = inner.events.send(event);
            }
            continue;
        }
        if method == Some("session/request_permission") {
            let request_id = value.get("id").cloned().unwrap_or(Value::Null);
            let params = value.get("params").cloned().unwrap_or(Value::Null);
            let Ok(event) = super::permissions::permission_event(request_id.clone(), params) else {
                fail_pending(&inner, "invalid ACP permission request");
                return;
            };
            let mut permissions = inner.permissions.lock();
            if inner.terminal.borrow().is_some() {
                continue;
            }
            if let Ok(Some(dialog)) = super::dialog_from_wire(&event) {
                permissions.insert(
                    request_id.to_string(),
                    PendingPermission {
                        dialog,
                        responding: false,
                    },
                );
            }
            let _ = inner.events.send(event);
            continue;
        }
        if let Some(id) = value.get("id") {
            let key = id.to_string();
            if let Some(pending) = inner.pending.lock().remove(&key) {
                let result = if let Some(error) = value.get("error") {
                    Err(RuntimeError::Acp(error.to_string()))
                } else {
                    let result = value.get("result").cloned().unwrap_or(Value::Null);
                    if let Some(session_id) = pending.config_session {
                        let session_id = result
                            .get("sessionId")
                            .and_then(Value::as_str)
                            .unwrap_or(&session_id);
                        if !session_id.is_empty() {
                            // Apply responses and notifications in wire order, before waking callers.
                            update_session_config(&inner, session_id, &result);
                            if pending.restore_boundary {
                                let _ = inner.events.send(wire_event(
                                    None,
                                    None,
                                    "acp_session_restored",
                                    json!({"type":"acp_session_restored", "sessionId":session_id}),
                                ));
                            }
                        }
                    }
                    Ok(result)
                };
                let _ = pending.sender.send(result);
            }
        }
    }
    fail_pending(&inner, "ACP stdout reached EOF");
}

fn update_event(value: &Value, inner: &AcpInner) -> Option<WireEvent> {
    if let Some(params) = value.get("params") {
        if params
            .pointer("/update/sessionUpdate")
            .and_then(Value::as_str)
            == Some("config_options_update")
        {
            if let Some(session_id) = params.get("sessionId").and_then(Value::as_str) {
                update_session_config(inner, session_id, &params["update"]);
            }
        }
    }
    let event_id = format!(
        "acp-event-{}",
        inner.next_event.fetch_add(1, Ordering::Relaxed)
    );
    translate_update(value, event_id)
}

fn translate_update(value: &Value, event_id: String) -> Option<WireEvent> {
    let params = value.get("params")?;
    let update = params.get("update")?;
    let kind = update.get("sessionUpdate").and_then(Value::as_str)?;
    let session_id = params.get("sessionId").cloned().unwrap_or(Value::Null);
    let content = update.get("content").cloned().unwrap_or(Value::Null);
    let text = content
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let event = match kind {
        "agent_message_chunk" => {
            let mut data = json!({
                "type": "message_update",
                "eventType": "text_delta",
                "delta": text,
                "sessionId": session_id,
                "message": {"role":"assistant"}
            });
            if let Some(message_id) = update.get("messageId").and_then(Value::as_str) {
                data["message"]["entryId"] = json!(message_id);
            }
            data
        }
        "agent_thought_chunk" => json!({
            "type": "message_update",
            "eventType": "thinking_delta",
            "delta": text,
            "sessionId": session_id
        }),
        "tool_call" => json!({
            "type": "tool_execution_start",
            "toolCallId": update.get("toolCallId").and_then(Value::as_str).unwrap_or(&event_id),
            "toolName": update.get("title").and_then(Value::as_str).unwrap_or("tool"),
            "arguments": update.get("rawInput").cloned().unwrap_or(Value::Null),
            "sessionId": session_id
        }),
        "tool_call_update" => {
            let status = update
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("in_progress");
            if matches!(status, "completed" | "failed") {
                json!({
                    "type": "tool_execution_end",
                    "toolCallId": update.get("toolCallId").and_then(Value::as_str).unwrap_or(&event_id),
                    "result": update.get("rawOutput").cloned().unwrap_or(Value::Null),
                    "error": (status == "failed").then_some("ACP tool call failed"),
                    "sessionId": session_id
                })
            } else {
                json!({
                    "type": "tool_execution_update",
                    "toolCallId": update.get("toolCallId").and_then(Value::as_str).unwrap_or(&event_id),
                    "partialResult": update.get("content").cloned().unwrap_or(Value::Null),
                    "sessionId": session_id
                })
            }
        }
        _ => json!({"type": kind, "sessionId": session_id, "update": update}),
    };
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or(kind)
        .to_owned();
    Some(wire_event(Some(event_id), None, event_type, event))
}

fn update_session_config(inner: &AcpInner, session_id: &str, result: &Value) {
    if let Some(options) = result.get("configOptions").and_then(Value::as_array) {
        inner
            .config_options
            .lock()
            .insert(session_id.to_owned(), options.clone());
    }
}

fn config_value_is_negotiated(options: &[Value], value: &Value) -> bool {
    value.is_string()
        && options.iter().any(|option| {
            if let Some(group) = option.get("options").and_then(Value::as_array) {
                config_value_is_negotiated(group, value)
            } else {
                option.get("value") == Some(value)
            }
        })
}

#[cfg(test)]
fn json_rpc_id(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn acp_outcome(stop_reason: &str) -> &'static str {
    match stop_reason {
        "cancelled" => "aborted",
        "max_tokens" => "failed",
        _ => "completed",
    }
}

fn fail_pending(inner: &AcpInner, message: &str) {
    {
        let mut permissions = inner.permissions.lock();
        for (_, pending) in permissions.drain() {
            let _ = inner.events.send(wire_event(
                None,
                None,
                "ui_dialog_closed",
                json!({"type":"ui_dialog_closed","requestId":pending.dialog.request_id,"jsonRpcRequestId":pending.dialog.response_id}),
            ));
        }
        // Serialize closure with permission registration: no request/card may
        // be registered after the terminal signal has been published.
        inner.terminal.send_if_modified(|terminal| {
            if terminal.is_some() {
                return false;
            }
            *terminal = Some(message.to_owned());
            true
        });
    }
    let mut pending = inner.pending.lock();
    for (_, pending) in pending.drain() {
        let _ = pending
            .sender
            .send(Err(RuntimeError::ChildExited(message.into())));
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
    async fn response_wait_cancellation_removes_pending_without_poisoning_acp() {
        let mut child = crate::runtime::native_lifecycle::test_child();
        let inner = Arc::new(AcpInner {
            stdin: Mutex::new(child.stdin.take().unwrap()),
            child: Arc::new(Mutex::new(child)),
            next_id: AtomicU64::new(1),
            next_event: AtomicU64::new(1),
            pending: State::new(HashMap::new()),
            permissions: State::new(HashMap::new()),
            config_options: State::new(HashMap::new()),
            events: broadcast::channel(8).0,
            terminal: watch::channel(None).0,
            descriptor: RwLock::new(AgentDescriptor {
                identity: TransportIdentity {
                    protocol: "acp".into(),
                    version: None,
                    transport: "json-rpc-stdio".into(),
                    agent_name: None,
                },
                capabilities: CapabilitySet::acp_default(),
            }),
        });
        let (tx, rx) = oneshot::channel();
        inner.pending.lock().insert(
            "7".into(),
            PendingRequest {
                sender: tx,
                config_session: None,
                restore_boundary: false,
            },
        );
        let guard = RequestGuard {
            inner: Arc::clone(&inner),
            key: "7".into(),
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

    #[test]
    fn config_values_must_be_negotiated_in_flat_or_grouped_options() {
        let options = json!([
            {"value":"flat","name":"Flat"},
            {"group":"Group","options":[{"value":"grouped","name":"Grouped"}]}
        ]);
        let options = options.as_array().unwrap();
        assert!(config_value_is_negotiated(options, &json!("flat")));
        assert!(config_value_is_negotiated(options, &json!("grouped")));
        for value in [json!("unknown"), json!("Group"), Value::Null, json!(7)] {
            assert!(!config_value_is_negotiated(options, &value));
        }
    }

    #[test]
    fn acp_text_update_becomes_native_text_delta() {
        let value = json!({"params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","messageId":"m","content":{"type":"text","text":"hello"}}}});
        let event = translate_update(&value, "event-1".into()).expect("event");
        assert_eq!(event.event_type, "message_update");
        assert_eq!(event.data["eventType"], "text_delta");
        assert_eq!(event.data["delta"], "hello");
    }

    #[test]
    fn acp_completed_tool_update_becomes_native_tool_end() {
        let value = json!({"params":{"sessionId":"s","update":{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"completed","rawOutput":{"text":"done"}}}});
        let event = translate_update(&value, "event-2".into()).expect("event");
        assert_eq!(event.event_type, "tool_execution_end");
        assert_eq!(event.data["toolCallId"], "tool-1");
    }

    #[test]
    fn json_rpc_ids_preserve_string_and_number_values() {
        assert_eq!(
            json_rpc_id(&json!("request-1")).as_deref(),
            Some("request-1")
        );
        assert_eq!(json_rpc_id(&json!(7)).as_deref(), Some("7"));
        assert_eq!(json_rpc_id(&Value::Null), None);
    }
    #[test]
    fn acp_stop_reasons_are_mapped_explicitly() {
        assert_eq!(acp_outcome("cancelled"), "aborted");
        assert_eq!(acp_outcome("max_tokens"), "failed");
        assert_eq!(acp_outcome("end_turn"), "completed");
    }
}
