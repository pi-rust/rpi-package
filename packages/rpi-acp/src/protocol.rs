//! The ACP (Agent Client Protocol) stdio server.
//!
//! Hand-rolled over the stable ACP **v1** method set. The protocol is JSON-RPC
//! 2.0 with one message per line on stdin/stdout:
//!
//! * client → agent requests we implement: `initialize`, `session/new`,
//!   `session/prompt`, `session/load`, `session/resume`, and the rpi extension
//!   `session/durable`; notification `session/cancel`.
//! * agent → client: `session/update` notifications (streamed text, thinking,
//!   tool calls), optional `session/durable_update` notifications, and one
//!   JSON-RPC response per request.
//!
//! We deliberately do **not** implement the client-side `fs/*` and `terminal/*`
//! methods: rpi already has native `read`/`write`/`edit`/`bash` tools, so the
//! agent runs its own tools in `session/new.cwd` instead of borrowing the
//! editor's. That keeps the surface small and the behavior identical to the
//! terminal agent.

use crate::diagln;
use rpi_extcore::durable::{DurableMutation, QueueStatus};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(test)]
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{oneshot, watch, Mutex};

use crate::session::Session;

/// The pollable stdin reader, re-exported so the frontends can build one.
pub use rpi_plugin_sdk::stdio::StdinLines;

/// Take stdin for ACP, failing when it cannot carry a protocol.
///
/// A terminal cannot be an ACP channel — Zed (or any other client) spawns us with
/// a pipe. Checking once, before the server starts, turns a first-read failure
/// into an actionable error.
///
/// A successful probe may already have read a line (a client is free to send
/// `initialize` immediately), so it is returned alongside the reader and must be
/// passed on to [`run`] — dropping it would lose the first message.
pub fn acp_stdin() -> Result<(StdinLines, Option<String>), String> {
    let mut input = StdinLines::default();
    let pending = match input.poll_line() {
        Ok(rpi_plugin_sdk::stdio::InputLine::Line(line)) => Some(line),
        Ok(_) => None,
        Err(error) => return Err(error.to_string()),
    };
    Ok((input, pending))
}

#[derive(Debug, Deserialize)]
struct DurableRequest {
    operation: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(rename = "itemId", default)]
    item_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

/// ACP extension for durable state and branch-scoped queue coordination.
/// Standard ACP clients may ignore this method; rpi-aware clients can use it
/// without bypassing the native session branch.
fn durable_mutation(params: &Value) -> Result<DurableMutation, String> {
    let request: DurableRequest = serde_json::from_value(params.clone())
        .map_err(|error| format!("invalid durable request: {error}"))?;
    let item_id = || {
        request
            .item_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "itemId is required".to_string())
    };
    match request.operation.trim() {
        "start_run" => Ok(DurableMutation::StartRun),
        "settle_run" => Ok(DurableMutation::SettleRun),
        "recover_run" => Ok(DurableMutation::RecoverRun),
        "enqueue" => Ok(DurableMutation::Enqueue {
            text: request
                .text
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "text is required".to_string())?,
        }),
        "claim" => Ok(DurableMutation::Claim { id: item_id()? }),
        "finish" => {
            let status = match request.status.as_deref() {
                Some("completed") => QueueStatus::Completed,
                Some("failed") => QueueStatus::Failed,
                Some(other) => return Err(format!("invalid finish status: {other}")),
                None => return Err("status is required for finish".to_string()),
            };
            Ok(DurableMutation::Finish {
                id: item_id()?,
                status,
                note: request.note.unwrap_or_default(),
            })
        }
        "cancel" => Ok(DurableMutation::Cancel {
            id: item_id()?,
            note: request.note.unwrap_or_default(),
        }),
        other => Err(format!("unsupported durable operation: {other}")),
    }
}

/// ACP protocol major version we speak. Stable v1.
pub const PROTOCOL_VERSION: u64 = 1;

/// One block of a `session/prompt` message. Modeled types are the ones we act
/// on; every other ACP block (`image`, `audio`, …) lands in [`ContentBlock::Other`]
/// and is ignored, matching our advertised `promptCapabilities`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ResourceLink {
        #[serde(default)]
        name: Option<String>,
        uri: String,
    },
    Resource {
        resource: EmbeddedResource,
    },
    #[serde(other)]
    Other,
}

/// An embedded file in a `resource` block (`promptCapabilities.embeddedContext`).
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedResource {
    pub uri: String,
    #[serde(default)]
    pub text: Option<String>,
}

/// Serialized access to the process stdout — the single ACP response stream.
/// Shared by the request dispatcher and every in-flight prompt turn, so writes
/// are serialized and each message is exactly one flushed line.
pub struct Writer {
    out: Mutex<Box<dyn AsyncWrite + Unpin + Send>>,
    /// In-flight agent → client requests (e.g. `session/request_permission`),
    /// keyed by the JSON-RPC id we sent. The client's response (an object with
    /// `id` and no `method`) resolves the matching oneshot.
    pending: std::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    closed: std::sync::atomic::AtomicBool,
    next_id: AtomicU64,
    questions: std::sync::atomic::AtomicBool,
    /// Whether the client supports the standard ACP `elicitation/create` form.
    elicitation: std::sync::atomic::AtomicBool,
}

impl Writer {
    pub fn new() -> Self {
        Self {
            out: Mutex::new(Box::new(tokio::io::stdout())),
            pending: std::sync::Mutex::new(HashMap::new()),
            closed: std::sync::atomic::AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            questions: std::sync::atomic::AtomicBool::new(false),
            elicitation: std::sync::atomic::AtomicBool::new(false),
        }
    }

    async fn write(&self, value: &Value) -> std::io::Result<()> {
        let mut line = value.to_string();
        line.push('\n');
        let mut out = self.out.lock().await;
        out.write_all(line.as_bytes()).await?;
        out.flush().await
    }

    /// Answer a JSON-RPC request.
    pub async fn respond(&self, id: &Value, result: Value) {
        let _ = self
            .write(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
            .await;
    }

    /// Answer a JSON-RPC request with an error.
    pub async fn respond_error(&self, id: &Value, code: i64, message: &str) {
        let _ = self
            .write(&json!({
                "jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message},
            }))
            .await;
    }

    /// Session startup/restore failures are ordinary server errors. ACP reserves
    /// -32000 for authentication, which makes clients offer a login flow.
    async fn session_error(&self, id: &Value, operation: &str, error: &str) {
        diagln!("[rpi-acp] {operation} failed: {error}");
        self.respond_error(id, -32603, error).await;
    }

    /// Send a JSON-RPC notification (no `id`).
    pub async fn notify(&self, method: &str, params: Value) {
        let _ = self
            .write(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await;
    }

    /// Send an agent → client request and await its response. Resolves to the
    /// `result` object, or `Null` if the connection closes first.
    pub async fn request(&self, method: &str, params: Value) -> Value {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            if self.closed.load(Ordering::Acquire) {
                return Value::Null;
            }
            pending.insert(id, tx);
        }
        // A cancelled dialog drops its pending request even when the client never replies.
        let _request = PendingRequest { writer: self, id };
        if self
            .write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await
            .is_err()
        {
            self.close();
            return Value::Null;
        }
        rx.await.unwrap_or(Value::Null)
    }

    pub fn close(&self) {
        let mut pending = self.pending.lock().unwrap();
        self.closed.store(true, Ordering::Release);
        pending.clear();
    }

    pub(crate) fn negotiate_questions(&self, params: &Value) {
        self.questions.store(
            params
                .pointer("/clientCapabilities/_meta/pi-rust~1questions")
                .and_then(Value::as_u64)
                == Some(1),
            Ordering::Release,
        );
        self.elicitation.store(
            matches!(
                params.pointer("/clientCapabilities/elicitation/form"),
                Some(Value::Object(_)) | Some(Value::Bool(true))
            ),
            Ordering::Release,
        );
    }

    pub(crate) fn supports_questions(&self) -> bool {
        self.questions.load(Ordering::Acquire)
    }

    pub(crate) fn supports_elicitation(&self) -> bool {
        self.elicitation.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn with_output(out: impl AsyncWrite + Unpin + Send + 'static) -> Self {
        Self {
            out: Mutex::new(Box::new(out)),
            pending: std::sync::Mutex::new(HashMap::new()),
            closed: std::sync::atomic::AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            questions: std::sync::atomic::AtomicBool::new(false),
            elicitation: std::sync::atomic::AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    pub(crate) fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    /// Route a client response (an object with `id` and no `method`) to the
    /// request that produced it.
    pub(crate) async fn resolve(&self, id: u64, result: Value) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(result);
        }
    }
}

struct PendingRequest<'a> {
    writer: &'a Writer,
    id: u64,
}
impl Drop for PendingRequest<'_> {
    fn drop(&mut self) {
        self.writer.pending.lock().unwrap().remove(&self.id);
    }
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

/// The `terminal` auth method we advertise: the client re-runs this same agent
/// program with `--setup` in an interactive terminal. Described in [`crate::auth`].
///
/// ACP says an agent may advertise a `terminal` method only when the client
/// announced the matching capability, so [`Writer::negotiate_auth`] gates it.
fn terminal_auth_method() -> Value {
    json!({
        "id": crate::auth::AUTH_METHOD_ID,
        "name": "Log in from the terminal",
        "description": "Run rpi's provider credential setup in a terminal",
        "type": "terminal",
        "args": [crate::auth::SETUP_ARG],
        "env": {},
    })
}

/// The `initialize` result. Advertises stable ACP v1 and the content we accept.
///
/// `authMethods` carries the `terminal` login so a registry or editor client can
/// offer sign-in; see [`Writer::negotiate_auth`] for why it is capability-gated.
pub fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "agentCapabilities": {
            "_meta": {
                "pi-rust/questions": 1,
                "rpi/durable": {
                    "version": 1,
                    "optional": true,
                    "notifications": ["session/durable_update"]
                }
            },
            "loadSession": true,
            "auth": {"logout": {}},
            "sessionCapabilities": {"resume": {}, "delete": {}},
            "promptCapabilities": {
                // `--mode rpc` takes a single text prompt; no multimodal input.
                "image": false,
                "audio": false,
                // Editor context (open files / selections) arrives as `resource`
                // blocks, which we inline into the prompt text.
                "embeddedContext": true,
            },
        },
        "agentInfo": {
            "name": "rpi",
            "title": "rpi",
            "version": env!("CARGO_PKG_VERSION"),
        },
        // Empty unless the client negotiated terminal auth; see `authMethods`.fn
        "authMethods": [],
    })
}

/// The `initialize` result for a client that did (or did not) offer to run our
/// terminal login.
pub fn initialize_result_for(terminal_auth: bool) -> Value {
    let mut result = initialize_result();
    if terminal_auth {
        result["authMethods"] = json!([terminal_auth_method()]);
    }
    result
}

/// A client that can open our `--setup` in a terminal.
///
/// Two shapes are accepted because they coexist in the wild: the auth-methods
/// RFD's `clientCapabilities.auth.terminal`, and the flat
/// `clientCapabilities.terminal` + `clientCapabilities._meta["terminal-auth"]`
/// that the ACP Registry's verifier sends today. Both intend the same thing —
/// "this client can reproduce our launch command in an interactive terminal" —
/// and a client that offered neither gets no `terminal` method, as the RFD
/// requires.
fn client_supports_terminal_auth(params: &Value) -> bool {
    let capabilities = params.pointer("/clientCapabilities");
    let Some(capabilities) = capabilities else {
        return false;
    };
    let announced = [
        capabilities.pointer("/auth/terminal"),
        capabilities.get("terminal"),
        capabilities.pointer("/_meta/terminal-auth"),
    ];
    announced
        .iter()
        .any(|value| matches!(value, Some(Value::Bool(true)) | Some(Value::Object(_))))
}

/// Run the ACP server until the client closes stdin, then return an exit code.
///
/// The owner reports the returned code to the host's stdio lifecycle. The
/// stop receiver also lets SessionShutdown end an idle server without input.
pub async fn run(
    mut stop: watch::Receiver<bool>,
    mut input: StdinLines,
    mut pending: Option<String>,
) -> i32 {
    let writer = Arc::new(Writer::new());
    let sessions: Arc<Mutex<HashMap<String, Arc<Session>>>> = Arc::new(Mutex::new(HashMap::new()));

    let mut tick = tokio::time::interval(std::time::Duration::from_millis(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut exit_code = 0;
    // Why the loop ended, reported once at the bottom. Zed forwards agent stderr
    // into its own log, so this is what turns "transport closed" into a cause.
    let mut reason = "stdin closed (client hung up)";
    'input: loop {
        let line = loop {
            if let Some(line) = pending.take() {
                break line;
            }
            if *stop.borrow() {
                reason = "stopped by the host (session shutdown)";
                break 'input;
            }
            match input.poll_line() {
                Ok(rpi_plugin_sdk::stdio::InputLine::Line(line)) => break line,
                Ok(rpi_plugin_sdk::stdio::InputLine::Eof) => break 'input,
                Ok(rpi_plugin_sdk::stdio::InputLine::Pending) => {
                    tokio::select! {
                        biased;
                        _ = stop.changed() => {
                            reason = "stopped by the host (session shutdown)";
                            break 'input;
                        }
                        _ = tick.tick() => {},
                    }
                }
                Err(error) => {
                    diagln!("[rpi-acp] stdin read error: {error}");
                    reason = "stdin read error";
                    exit_code = 2;
                    break 'input;
                }
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => {
                diagln!("[rpi-acp] invalid json from client: {error}");
                continue;
            }
        };

        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let id = message.get("id").cloned();

        // `RPI_ACP_DEBUG=1` traces the method stream. It costs a line per
        // request, which is how we find out where a client gave up.
        if crate::diag::debug_enabled() {
            match &id {
                Some(id) => diagln!(
                    "[rpi-acp] <- {} {id}",
                    method.as_deref().unwrap_or("(response)")
                ),
                None => diagln!(
                    "[rpi-acp] <- {} (notification)",
                    method.as_deref().unwrap_or("?")
                ),
            }
        }

        match (id, method.as_deref()) {
            (Some(id), Some("initialize")) => {
                writer.negotiate_questions(&message["params"]);
                let terminal_auth = client_supports_terminal_auth(&message["params"]);
                writer
                    .respond(&id, initialize_result_for(terminal_auth))
                    .await;
            }
            // `authenticate` is not advertised (we use terminal auth), but tolerate
            // a stray call.
            (Some(id), Some("authenticate")) => writer.respond(&id, json!({})).await,
            (Some(id), Some("logout")) => match crate::auth::logout() {
                Ok(()) => writer.respond(&id, json!({})).await,
                Err(error) => writer.respond_error(&id, -32603, &error).await,
            },
            (Some(id), Some("session/new")) => {
                let cwd = message
                    .pointer("/params/cwd")
                    .and_then(Value::as_str)
                    .unwrap_or(".")
                    .to_string();
                let session_id = uuid::Uuid::new_v4().to_string();
                match open_session(&session_id, PathBuf::from(&cwd), None).await {
                    Ok((session, state)) => {
                        let mut result = json!({"sessionId": session_id});
                        match config_options_for(&session, &state).await {
                            Ok(config) => result["configOptions"] = config["configOptions"].clone(),
                            Err(error) => diagln!("[rpi-acp] session {session_id}: config options unavailable: {error}"),
                        }
                        sessions.lock().await.insert(session_id.clone(), session);
                        writer.respond(&id, result).await;
                    }
                    Err(error) => writer.session_error(&id, "session/new", &error).await,
                }
            }
            (Some(id), Some("session/durable")) => {
                let session_id = message
                    .pointer("/params/sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let session = sessions.lock().await.get(&session_id).cloned();
                let Some(session) = session else {
                    writer.respond_error(&id, -32602, "unknown sessionId").await;
                    continue;
                };
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                if params.get("operation").and_then(Value::as_str) == Some("status") {
                    match session.durable_state().await {
                        Ok(state) => {
                            writer
                                .respond(&id, json!({"sessionId": session_id, "state": state}))
                                .await
                        }
                        Err(error) => writer.respond_error(&id, -32001, &error).await,
                    }
                    continue;
                }
                let mutation = match durable_mutation(&params) {
                    Ok(mutation) => mutation,
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                match session.mutate_durable(mutation).await {
                    Ok(mut result) => {
                        result["sessionId"] = json!(session_id);
                        let notification = json!({
                            "sessionId": session_id,
                            "revision": result["state"]["revision"],
                            "state": result["state"],
                        });
                        writer.respond(&id, result).await;
                        writer.notify("session/durable_update", notification).await;
                    }
                    Err(error) if error.starts_with("durable_unavailable:") => {
                        writer.respond_error(&id, -32001, &error).await
                    }
                    Err(error) => writer.respond_error(&id, -32603, &error).await,
                }
            }
            (Some(id), Some("session/prompt")) => {
                let session_id = message
                    .pointer("/params/sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let blocks: Vec<ContentBlock> = message
                    .pointer("/params/prompt")
                    .and_then(Value::as_array)
                    .map(|array| {
                        array
                            .iter()
                            .filter_map(|value| serde_json::from_value(value.clone()).ok())
                            .collect()
                    })
                    .unwrap_or_default();
                let session = sessions.lock().await.get(&session_id).cloned();
                match session {
                    Some(session) => {
                        let writer = Arc::clone(&writer);
                        tokio::spawn(async move {
                            match session.prompt(&writer, &session_id, &blocks).await {
                                Ok(stop_reason) => {
                                    writer.respond(&id, json!({"stopReason":stop_reason})).await
                                }
                                Err(error) => {
                                    diagln!(
                                        "[rpi-acp] session {session_id}: prompt failed: {error}"
                                    );
                                    writer.respond_error(&id, -32603, &error).await;
                                }
                            }
                        });
                    }
                    None => writer.respond_error(&id, -32602, "unknown sessionId").await,
                }
            }
            (Some(id), Some(method @ ("session/load" | "session/resume"))) => {
                let session_id = message
                    .pointer("/params/sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let cwd = message
                    .pointer("/params/cwd")
                    .and_then(Value::as_str)
                    .unwrap_or(".");
                let existing = sessions.lock().await.get(session_id).cloned();
                let opened = if let Some(session) = existing {
                    session.state().await.map(|state| (session, state))
                } else {
                    match crate::history::saved_session(session_id) {
                        Ok(path) => open_session(session_id, PathBuf::from(cwd), Some(&path)).await,
                        Err(error) => Err(error),
                    }
                };
                match opened {
                    Ok((session, state)) => {
                        let updates = if method == "session/load" {
                            crate::history::saved_session(session_id).and_then(|path| {
                                crate::history::replay(&path, state["leafId"].as_str())
                            })
                        } else {
                            Ok(Vec::new())
                        };
                        match updates {
                            Ok(updates) => {
                                let mut result = json!({});
                                match config_options_for(&session, &state).await {
                                    Ok(config) => result["configOptions"] = config["configOptions"].clone(),
                                    Err(error) => diagln!("[rpi-acp] session {session_id}: config options unavailable: {error}"),
                                }
                                sessions.lock().await.insert(session_id.to_owned(), session);
                                for update in updates {
                                    writer
                                        .notify(
                                            "session/update",
                                            json!({"sessionId":session_id,"update":update}),
                                        )
                                        .await;
                                }
                                writer.respond(&id, result).await;
                            }
                            Err(error) => {
                                // Keep an already attached backend alive on a replay error.
                                if !sessions.lock().await.contains_key(session_id) {
                                    session.kill().await;
                                }
                                writer.session_error(&id, method, &error).await;
                            }
                        }
                    }
                    Err(error) => writer.session_error(&id, method, &error).await,
                }
            }
            (Some(id), Some("session/set_config_option")) => {
                let session_id = message
                    .pointer("/params/sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let config_id = message
                    .pointer("/params/configId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let value = message
                    .pointer("/params/value")
                    .cloned()
                    .unwrap_or(Value::Null);
                let session = sessions.lock().await.get(&session_id).cloned();
                match session {
                    Some(session) => {
                        let applied: Result<(), String> = match config_id.as_str() {
                            "model" => match value.as_str() {
                                Some(selected) => {
                                    let (provider, model_id) = split_model_value(selected);
                                    session.set_model(model_id, provider).await
                                }
                                None => Err("model value must be a string".to_string()),
                            },
                            "thinking" => match value.as_str() {
                                Some(level) => session.set_thinking_level(level).await,
                                None => Err("thinking value must be a string".to_string()),
                            },
                            other => Err(format!("unknown config option: {other}")),
                        };
                        match applied {
                            Ok(()) => {
                                // ACP requires the complete configuration state in
                                // the response, reflecting the just-applied value.
                                // Read it without the prompt lock so a switch can
                                // also complete while a run is in progress.
                                match session.state_unlocked().await {
                                    Ok(state) => match config_options_for(&session, &state).await {
                                        Ok(config) => writer.respond(&id, config).await,
                                        Err(error) => {
                                            writer
                                                .session_error(
                                                    &id,
                                                    "session/set_config_option",
                                                    &error,
                                                )
                                                .await
                                        }
                                    },
                                    Err(error) => {
                                        writer
                                            .session_error(&id, "session/set_config_option", &error)
                                            .await
                                    }
                                }
                            }
                            Err(error) => writer.respond_error(&id, -32602, &error).await,
                        }
                    }
                    None => writer.respond_error(&id, -32602, "unknown sessionId").await,
                }
            }
            (Some(id), Some("session/delete")) => {
                let session_id = message
                    .pointer("/params/sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                // Detach a live backend first, then drop its persisted files.
                // Deleting is idempotent: a session that is no longer running
                // (or was never saved) still answers successfully.
                if let Some(session) = sessions.lock().await.remove(&session_id) {
                    session.kill().await;
                }
                match crate::history::session_directory(&session_id) {
                    Ok(directory) => match std::fs::remove_dir_all(&directory) {
                        Ok(()) => writer.respond(&id, json!({})).await,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            writer.respond(&id, json!({})).await
                        }
                        Err(error) => {
                            writer
                                .respond_error(&id, -32603, &format!("delete session: {error}"))
                                .await
                        }
                    },
                    Err(error) => writer.respond_error(&id, -32602, &error).await,
                }
            }
            (Some(id), Some(other)) => {
                writer
                    .respond_error(&id, -32601, &format!("method not found: {other}"))
                    .await
            }
            // A client response (an object with `id` and no `method`) answers an
            // agent → client request, e.g. the `session/request_permission` we
            // sent for a plugin's UI dialog. Route it to the waiting caller.
            (Some(id), None) => {
                if let Some(numeric_id) = id.as_u64() {
                    let result = message.get("result").cloned().unwrap_or(Value::Null);
                    writer.resolve(numeric_id, result).await;
                }
            }
            (None, Some("session/cancel")) => {
                let session_id = message
                    .pointer("/params/sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let session = sessions.lock().await.get(&session_id).cloned();
                if let Some(session) = session {
                    let _ = session.abort().await;
                }
            }
            (None, _) => {}
        }
    }

    // Resolve pending client requests before tearing down every child.
    writer.close();
    // Client closed stdin or host requested shutdown.
    for (_, session) in sessions.lock().await.drain() {
        session.kill().await;
    }
    // One line, always: with Zed this lands in its log as `agent stderr`, and it
    // is what a client's "transport closed" actually corresponds to.
    diagln!("[rpi-acp] server stopping ({reason}, exit {exit_code})");
    exit_code
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Split a `provider/model` config value back into its two parts. A value
/// without a slash is treated as a bare model id.
fn split_model_value(value: &str) -> (Option<&str>, &str) {
    match value.split_once('/') {
        Some((provider, model_id)) if !provider.is_empty() && !model_id.is_empty() => {
            (Some(provider), model_id)
        }
        _ => (None, value),
    }
}

/// Map rpi's `get_available_models` + `get_available_thinking_levels` output and
/// the current `get_state` onto ACP v1 Session Config Options: a `model`
/// selector and a `thought_level` selector.
fn config_options(models: &Value, thinking: &Value, state: &Value) -> Value {
    // RPC hosts have returned both the legacy string fields and the newer
    // `{model: {id, provider, ...}}` snapshot. Accept both shapes so the
    // selected option remains stable after a live switch.
    let state_model = state
        .get("modelId")
        .and_then(Value::as_str)
        .or_else(|| state.get("model").and_then(Value::as_str))
        .or_else(|| state.pointer("/model/id").and_then(Value::as_str))
        .unwrap_or("");
    let (embedded_provider, current_model_id) = split_model_value(state_model);
    let current_provider = state
        .get("provider")
        .and_then(Value::as_str)
        .or_else(|| state.pointer("/model/provider").and_then(Value::as_str))
        .filter(|provider| !provider.is_empty())
        .or(embedded_provider)
        .unwrap_or("");
    let current_thinking = state
        .get("thinkingLevel")
        .and_then(Value::as_str)
        .unwrap_or("medium");

    // Keep the provider in the value so two providers may expose the same model
    // id without colliding. With multiple providers, use ACP's grouped select
    // form so Zed exposes the provider boundary.
    let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
    let mut selections: Vec<(String, String, String)> = Vec::new();
    if let Some(array) = models.as_array() {
        for model in array {
            let id = model.get("id").and_then(Value::as_str).unwrap_or("");
            if id.is_empty() {
                continue;
            }
            let name = model.get("name").and_then(Value::as_str).unwrap_or(id);
            let provider = model.get("provider").and_then(Value::as_str).unwrap_or("");
            let context = model
                .get("contextWindow")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let value = if provider.is_empty() {
                id.to_owned()
            } else {
                format!("{provider}/{id}")
            };
            let option = if context > 0 {
                json!({
                    "value": value,
                    "name": name,
                    "description": format!("{context} tokens")
                })
            } else {
                json!({ "value": value, "name": name })
            };
            if let Some((_, options)) = groups.iter_mut().find(|(group, _)| group == provider) {
                options.push(option);
            } else {
                groups.push((provider.to_owned(), vec![option]));
            }
            selections.push((provider.to_owned(), id.to_owned(), value));
        }
    }

    let model_options = if groups.len() > 1 {
        groups
            .into_iter()
            .map(|(provider, options)| {
                let name = if provider.is_empty() {
                    "Other".to_owned()
                } else {
                    provider.clone()
                };
                json!({ "group": provider, "name": name, "options": options })
            })
            .collect::<Vec<_>>()
    } else {
        groups.pop().map(|(_, options)| options).unwrap_or_default()
    };

    let current_value = selections
        .iter()
        .find(|(provider, id, _)| provider == current_provider && id == current_model_id)
        .map(|(_, _, value)| value.clone())
        .or_else(|| {
            selections
                .iter()
                .find(|(_, id, _)| id == current_model_id)
                .map(|(_, _, value)| value.clone())
        })
        // ACP requires currentValue to identify an available option. Older
        // hosts may not report modelId/provider yet, so use the first model
        // instead of emitting the invalid "/" value.
        .or_else(|| selections.first().map(|(_, _, value)| value.clone()))
        .unwrap_or_else(|| {
            if current_provider.is_empty() {
                current_model_id.to_owned()
            } else {
                format!("{current_provider}/{current_model_id}")
            }
        });

    let thinking_options: Vec<Value> = thinking
        .as_array()
        .map(|array| {
            array
                .iter()
                .filter_map(|level| {
                    level
                        .as_str()
                        .map(|level| json!({ "value": level, "name": capitalize(level) }))
                })
                .collect()
        })
        .unwrap_or_default();
    let current_thinking_value = thinking_options
        .iter()
        .find(|option| option["value"].as_str() == Some(current_thinking))
        .and_then(|option| option["value"].as_str())
        .or_else(|| {
            thinking_options
                .first()
                .and_then(|option| option["value"].as_str())
        })
        .unwrap_or(current_thinking);

    let mut options = Vec::new();
    if !selections.is_empty() {
        options.push(json!({
            "id": "model",
            "name": "Model / Provider",
            "description": "Select the model and provider used for the next prompt",
            "category": "model",
            "type": "select",
            "currentValue": current_value,
            "options": model_options,
        }));
    }
    if !thinking_options.is_empty() {
        options.push(json!({
            "id": "thinking",
            "name": "Thinking",
            "category": "thought_level",
            "type": "select",
            "currentValue": current_thinking_value,
            "options": thinking_options,
        }));
    }
    json!({ "configOptions": options })
}

/// Fetch the child's model catalog and thinking levels, then build the ACP
/// `configOptions` object. `state` is the `get_state` result already held by
/// the caller, so it is not re-read here.
async fn config_options_for(session: &Arc<Session>, state: &Value) -> Result<Value, String> {
    let models = session.available_models().await?;
    // Model switching must remain available if an older host lacks this optional query.
    let thinking = match session.available_thinking_levels().await {
        Ok(levels) => levels,
        Err(error) => {
            diagln!("[rpi-acp] thinking-level discovery unavailable: {error}");
            json!([])
        }
    };
    Ok(config_options(&models, &thinking, state))
}

async fn open_session(
    id: &str,
    cwd: PathBuf,
    saved: Option<&std::path::Path>,
) -> Result<(Arc<Session>, Value), String> {
    if !cwd.is_absolute() || !cwd.is_dir() {
        return Err("cwd must be an existing absolute directory".into());
    }
    let session = Session::spawn(id.to_owned(), cwd, saved).await?;
    match session.state().await {
        Ok(state) if state["sessionId"] == id => Ok((session, state)),
        result => {
            session.kill().await;
            Err(result
                .err()
                .unwrap_or_else(|| "backend opened a different sessionId".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_extension_parses_queue_mutations() {
        let mutation = durable_mutation(&json!({
            "operation": "finish",
            "itemId": "item-1",
            "status": "completed",
            "note": "passed"
        }))
        .unwrap();
        assert!(matches!(
            mutation,
            DurableMutation::Finish {
                id,
                status: QueueStatus::Completed,
                note
            } if id == "item-1" && note == "passed"
        ));
        assert!(durable_mutation(&json!({
            "operation": "finish",
            "itemId": "item-1",
            "status": "queued"
        }))
        .is_err());
    }

    #[test]
    fn initialize_advertises_durable_extension() {
        assert_eq!(
            initialize_result()["agentCapabilities"]["_meta"]["rpi/durable"]["optional"],
            true
        );
        assert_eq!(
            initialize_result()["agentCapabilities"]["_meta"]["rpi/durable"]["notifications"][0],
            "session/durable_update"
        );
    }

    #[tokio::test]
    async fn disconnect_resolves_pending_requests_and_rejects_new_ones() {
        let (output, peer) = tokio::io::duplex(1024);
        let writer = Arc::new(Writer::with_output(output));
        let request = tokio::spawn({
            let writer = writer.clone();
            async move { writer.request("test", json!({})).await }
        });
        let mut lines = BufReader::new(peer).lines();
        lines.next_line().await.unwrap().unwrap();
        assert_eq!(writer.pending_count(), 1);
        writer.close();
        assert_eq!(request.await.unwrap(), Value::Null);
        assert_eq!(writer.pending_count(), 0);
        assert_eq!(writer.request("test", json!({})).await, Value::Null);
    }

    #[test]
    fn initialize_advertises_v1_and_embedded_context() {
        let result = initialize_result();
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["agentCapabilities"]["loadSession"], true);
        assert_eq!(result["agentCapabilities"]["auth"]["logout"], json!({}));
        assert_eq!(
            result["agentCapabilities"]["sessionCapabilities"]["resume"],
            json!({})
        );
        assert_eq!(
            result["agentCapabilities"]["sessionCapabilities"]["delete"],
            json!({})
        );
        assert_eq!(
            result["agentCapabilities"]["promptCapabilities"]["embeddedContext"],
            true
        );
        assert_eq!(result["agentInfo"]["name"], "rpi");
        assert_eq!(result["agentCapabilities"]["_meta"]["pi-rust/questions"], 1);
        // Baseline v1: no auth method until the client offers a terminal.
        assert_eq!(result["authMethods"], json!([]));
    }

    #[test]
    fn terminal_auth_is_advertised_only_for_a_capable_client() {
        // The registry verifier's shape: flat `terminal` plus the `_meta` hint.
        assert!(client_supports_terminal_auth(&json!({
            "clientCapabilities": {"terminal": true,
                                   "_meta": {"terminal-auth": true}}
        })));
        // The RFD's shape, in both v1 spellings of the value.
        assert!(client_supports_terminal_auth(
            &json!({"clientCapabilities": {"auth": {"terminal": true}}})
        ));
        assert!(client_supports_terminal_auth(
            &json!({"clientCapabilities": {"auth": {"terminal": {}}}})
        ));
        for params in [
            json!({}),
            json!({"clientCapabilities": {}}),
            json!({"clientCapabilities": {"auth": {}}}),
            json!({"clientCapabilities": {"auth": {"terminal": false}}}),
            json!({"clientCapabilities": {"terminal": false, "_meta": {"terminal-auth": false}}}),
            json!({"clientCapabilities": {"_meta": {}}}),
        ] {
            assert!(!client_supports_terminal_auth(&params), "{params}");
        }

        let advertised = initialize_result_for(true);
        let methods = advertised["authMethods"].as_array().unwrap();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0]["type"], "terminal");
        assert_eq!(methods[0]["id"], crate::auth::AUTH_METHOD_ID);
        assert_eq!(methods[0]["args"], json!(["--setup"]));
        assert!(methods[0]["name"].as_str().is_some_and(|n| !n.is_empty()));
        // A terminal method must never carry a `command`; the client owns that.
        assert!(methods[0].get("command").is_none());
    }

    #[test]
    fn questions_require_an_explicit_supported_version() {
        let writer = Writer::new();
        for params in [
            json!({}),
            json!({"clientCapabilities":{"_meta":{"pi-rust/questions":true}}}),
            json!({"clientCapabilities":{"_meta":{"pi-rust/questions":2}}}),
        ] {
            writer.negotiate_questions(&params);
            assert!(!writer.supports_questions());
        }
        writer
            .negotiate_questions(&json!({"clientCapabilities":{"_meta":{"pi-rust/questions":1}}}));
        assert!(writer.supports_questions());
    }

    #[test]
    fn standard_elicitation_requires_form_capability() {
        let writer = Writer::new();
        for params in [
            json!({}),
            json!({"clientCapabilities":{"elicitation":{}}}),
            json!({"clientCapabilities":{"elicitation":{"url":{}}}}),
        ] {
            writer.negotiate_questions(&params);
            assert!(!writer.supports_elicitation(), "{params}");
        }
        writer.negotiate_questions(&json!({
            "clientCapabilities":{"elicitation":{"form":{}}}
        }));
        assert!(writer.supports_elicitation());
    }
    #[test]
    fn content_blocks_parse_text_link_and_resource() {
        let text: ContentBlock =
            serde_json::from_value(json!({"type":"text","text":"hi"})).unwrap();
        assert!(matches!(text, ContentBlock::Text { .. }));

        let link: ContentBlock = serde_json::from_value(
            json!({"type":"resource_link","uri":"file:///a.rs","name":"a.rs"}),
        )
        .unwrap();
        assert!(matches!(link, ContentBlock::ResourceLink { .. }));

        let resource: ContentBlock = serde_json::from_value(json!({
            "type": "resource",
            "resource": {"uri": "file:///a.rs", "mimeType": "text/rust", "text": "fn main(){}"}
        }))
        .unwrap();
        match resource {
            ContentBlock::Resource { resource } => {
                assert_eq!(resource.uri, "file:///a.rs");
                assert_eq!(resource.text.as_deref(), Some("fn main(){}"));
            }
            _ => panic!("expected resource block"),
        }
    }

    #[test]
    fn unknown_content_block_is_ignored() {
        let image: ContentBlock =
            serde_json::from_value(json!({"type":"image","data":"...","mimeType":"image/png"}))
                .unwrap();
        assert!(matches!(image, ContentBlock::Other));
    }

    #[test]
    fn split_model_value_splits_provider_and_id() {
        assert_eq!(
            split_model_value("anthropic/claude-sonnet-5"),
            (Some("anthropic"), "claude-sonnet-5")
        );
        assert_eq!(split_model_value("claude"), (None, "claude"));
        assert_eq!(split_model_value("a/b/c"), (Some("a"), "b/c"));
        // Malformed values fall back to a bare model id.
        assert_eq!(split_model_value("a/"), (None, "a/"));
        assert_eq!(split_model_value("/"), (None, "/"));
    }

    #[test]
    fn config_options_maps_models_and_thinking() {
        let models = json!({
            "models": [
                {"id": "sonnet", "name": "Claude Sonnet", "provider": "anthropic",
                 "contextWindow": 200000},
                {"id": "opus", "name": "Claude Opus", "provider": "anthropic",
                 "contextWindow": 200000},
            ]
        });
        let thinking = json!(["off", "medium", "high"]);
        let state = json!({"modelId": "sonnet", "provider": "anthropic", "thinkingLevel": "high"});

        let config = config_options(&models["models"], &thinking, &state);
        let options = config["configOptions"].as_array().unwrap();
        assert_eq!(options.len(), 2);

        let model = &options[0];
        assert_eq!(model["id"], "model");
        assert_eq!(model["category"], "model");
        assert_eq!(model["type"], "select");
        assert_eq!(model["currentValue"], "anthropic/sonnet");
        let model_options = model["options"].as_array().unwrap();
        assert_eq!(model_options.len(), 2);
        assert_eq!(model_options[0]["value"], "anthropic/sonnet");
        assert_eq!(model_options[0]["name"], "Claude Sonnet");
        assert_eq!(model_options[0]["description"], "200000 tokens");

        let thinking_option = &options[1];
        assert_eq!(thinking_option["id"], "thinking");
        assert_eq!(thinking_option["category"], "thought_level");
        assert_eq!(thinking_option["currentValue"], "high");
        let thinking_options = thinking_option["options"].as_array().unwrap();
        assert_eq!(thinking_options.len(), 3);
        assert_eq!(thinking_options[0]["value"], "off");
        assert_eq!(thinking_options[0]["name"], "Off");
    }

    #[test]
    fn config_options_groups_providers_and_accepts_legacy_state() {
        let models = json!([
            {"id": "same", "name": "Same A", "provider": "a", "contextWindow": 1000},
            {"id": "same", "name": "Same B", "provider": "b", "contextWindow": 2000},
        ]);
        let config = config_options(&models, &json!([]), &json!({"model": "same"}));
        let model = &config["configOptions"][0];
        assert_eq!(model["currentValue"], "a/same");
        let groups = model["options"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["group"], "a");
        assert_eq!(groups[1]["group"], "b");
        assert_eq!(groups[1]["options"][0]["value"], "b/same");
    }

    #[test]
    fn config_options_reads_object_model_state() {
        let config = config_options(
            &json!([{"id": "m", "provider": "provider"}]),
            &json!([]),
            &json!({"model": {"id": "m", "provider": "provider"}}),
        );
        assert_eq!(config["configOptions"][0]["currentValue"], "provider/m");
    }

    #[test]
    fn config_options_uses_first_model_when_state_has_no_model() {
        let config = config_options(
            &json!([{"id": "first", "provider": "provider"}]),
            &json!([]),
            &json!({}),
        );
        assert_eq!(config["configOptions"][0]["currentValue"], "provider/first");
    }

    #[test]
    fn config_options_falls_back_to_an_available_thinking_level() {
        let config = config_options(
            &json!([{"id": "m", "provider": "provider"}]),
            &json!(["off"]),
            &json!({"thinkingLevel": "medium"}),
        );
        assert_eq!(config["configOptions"][1]["currentValue"], "off");
    }

    #[test]
    fn config_options_omits_empty_groups() {
        let config = config_options(
            &json!([]),
            &json!([]),
            &json!({"modelId": "x", "provider": "y"}),
        );
        assert_eq!(config["configOptions"].as_array().unwrap().len(), 0);
    }
}
