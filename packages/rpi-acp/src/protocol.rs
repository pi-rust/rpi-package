//! The ACP (Agent Client Protocol) stdio server.
//!
//! Hand-rolled over the stable ACP **v1** method set. The protocol is JSON-RPC
//! 2.0 with one message per line on stdin/stdout:
//!
//! * client → agent requests we implement: `initialize`, `session/new`,
//!   `session/prompt`, `session/load`, `session/resume`; notification `session/cancel`.
//! * agent → client: `session/update` notifications (streamed text, thinking,
//!   tool calls) and one JSON-RPC response per request.
//!
//! We deliberately do **not** implement the client-side `fs/*` and `terminal/*`
//! methods: rpi already has native `read`/`write`/`edit`/`bash` tools, so the
//! agent runs its own tools in `session/new.cwd` instead of borrowing the
//! editor's. That keeps the surface small and the behavior identical to the
//! terminal agent.

use crate::diagln;
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
}

impl Writer {
    pub fn new() -> Self {
        Self {
            out: Mutex::new(Box::new(tokio::io::stdout())),
            pending: std::sync::Mutex::new(HashMap::new()),
            closed: std::sync::atomic::AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            questions: std::sync::atomic::AtomicBool::new(false),
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
    }

    pub(crate) fn supports_questions(&self) -> bool {
        self.questions.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn with_output(out: impl AsyncWrite + Unpin + Send + 'static) -> Self {
        Self {
            out: Mutex::new(Box::new(out)),
            pending: std::sync::Mutex::new(HashMap::new()),
            closed: std::sync::atomic::AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            questions: std::sync::atomic::AtomicBool::new(false),
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
            "_meta": {"pi-rust/questions": 1},
            "loadSession": true,
            "sessionCapabilities": {"resume": {}},
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
            // No auth methods are advertised, but tolerate a stray call.
            (Some(id), Some("authenticate")) => writer.respond(&id, json!({})).await,
            (Some(id), Some("session/new")) => {
                let cwd = message
                    .pointer("/params/cwd")
                    .and_then(Value::as_str)
                    .unwrap_or(".")
                    .to_string();
                let session_id = uuid::Uuid::new_v4().to_string();
                match open_session(&session_id, PathBuf::from(&cwd), None).await {
                    Ok((session, _)) => {
                        sessions.lock().await.insert(session_id.clone(), session);
                        writer.respond(&id, json!({"sessionId": session_id})).await;
                    }
                    Err(error) => writer.respond_error(&id, -32000, &error).await,
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
                                sessions.lock().await.insert(session_id.to_owned(), session);
                                for update in updates {
                                    writer
                                        .notify(
                                            "session/update",
                                            json!({"sessionId":session_id,"update":update}),
                                        )
                                        .await;
                                }
                                writer.respond(&id, json!({})).await;
                            }
                            Err(error) => {
                                // Keep an already attached backend alive on a replay error.
                                if !sessions.lock().await.contains_key(session_id) {
                                    session.kill().await;
                                }
                                writer.respond_error(&id, -32000, &error).await;
                            }
                        }
                    }
                    Err(error) => writer.respond_error(&id, -32000, &error).await,
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

async fn open_session(
    id: &str,
    cwd: PathBuf,
    saved: Option<&std::path::Path>,
) -> Result<(Arc<Session>, Value), String> {
    if !cwd.is_absolute() || !cwd.is_dir() {
        return Err("cwd must be an existing absolute directory".into());
    }
    let session = Session::spawn(id.to_owned(), cwd, saved)?;
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
        assert_eq!(
            result["agentCapabilities"]["sessionCapabilities"]["resume"],
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
}
