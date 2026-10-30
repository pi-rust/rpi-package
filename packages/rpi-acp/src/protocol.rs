//! The ACP (Agent Client Protocol) stdio server.
//!
//! Hand-rolled over the stable ACP **v1** method set. The protocol is JSON-RPC
//! 2.0 with one message per line on stdin/stdout:
//!
//! * client → agent requests we implement: `initialize`, `session/new`,
//!   `session/prompt`, `session/load`, `session/resume`, `session/close`,
//!   `session/delete`, and the rpi extension `session/durable`; notifications
//!   `session/cancel` and `$/cancel_request`.
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
use rpi_extcore::durable::{DurableMutation, QueueStatus, ENTRY_TYPE};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(test)]
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{oneshot, watch, Mutex};

use crate::session::Session;

#[path = "session_extensions.rs"]
mod session_extensions;

/// The pollable stdin reader, re-exported so the frontends can build one.
pub use rpi_plugin_sdk::stdio::StdinLines;

/// Pollable input shared by stdio and bounded in-process transports.
pub trait Input {
    fn poll_line(&mut self) -> std::io::Result<rpi_plugin_sdk::stdio::InputLine>;
}

impl Input for StdinLines {
    fn poll_line(&mut self) -> std::io::Result<rpi_plugin_sdk::stdio::InputLine> {
        StdinLines::poll_line(self)
    }
}

impl Input for tokio::sync::mpsc::Receiver<String> {
    fn poll_line(&mut self) -> std::io::Result<rpi_plugin_sdk::stdio::InputLine> {
        use rpi_plugin_sdk::stdio::InputLine;
        Ok(match self.try_recv() {
            Ok(line) => InputLine::Line(line),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => InputLine::Pending,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => InputLine::Eof,
        })
    }
}

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
/// on; `image` and `audio` are forwarded as provider-neutral base64 content, while
/// every other unsupported ACP block is rejected by prompt parsing.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        #[serde(deserialize_with = "deserialize_media_data")]
        data: String,
        #[serde(rename = "mimeType", deserialize_with = "deserialize_image_mime")]
        mime_type: String,
    },
    Audio {
        #[serde(deserialize_with = "deserialize_media_data")]
        data: String,
        #[serde(rename = "mimeType", deserialize_with = "deserialize_audio_mime")]
        mime_type: String,
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
#[derive(Debug, Clone)]
pub struct EmbeddedResource {
    pub uri: String,
    pub text: Option<String>,
}

impl<'de> Deserialize<'de> for EmbeddedResource {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct TextResource {
            uri: String,
            #[serde(default)]
            text: Option<String>,
            // Reject presence, including null or text+blob, without retaining or
            // decoding unsupported binary bytes. Other ACP metadata stays opaque.
            #[serde(default, rename = "blob", deserialize_with = "reject_resource_blob")]
            _blob: (),
        }
        let resource = TextResource::deserialize(deserializer)?;
        Ok(Self {
            uri: resource.uri,
            text: resource.text,
        })
    }
}

fn reject_resource_blob<'de, D: serde::Deserializer<'de>>(_: D) -> Result<(), D::Error> {
    Err(serde::de::Error::custom("embedded resource blob is unsupported"))
}

// Per-block encoded data ceiling, not a new aggregate transport budget. Input
// readers retain their existing frame limits. Validate without allocating a
// decoded copy (16 MiB encoded is at most 12 MiB decoded).
const MAX_MEDIA_BASE64_BYTES: usize = 16 * 1024 * 1024;

fn deserialize_media_data<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    let data = String::deserialize(deserializer)?;
    if !valid_media_base64(&data) {
        return Err(serde::de::Error::custom(
            "media data must be non-empty canonical padded base64, at most 16 MiB encoded",
        ));
    }
    Ok(data)
}

fn valid_media_base64(data: &str) -> bool {
    fn sextet(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes = data.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_MEDIA_BASE64_BYTES || bytes.len() % 4 != 0 {
        return false;
    }
    let padding = if bytes.ends_with(b"==") {
        2
    } else if bytes.ends_with(b"=") {
        1
    } else {
        0
    };
    let body = &bytes[..bytes.len() - padding];
    if !body.iter().all(|&byte| sextet(byte).is_some()) {
        return false;
    }
    // Canonical RFC 4648 encoding has zero unused bits in its last sextet.
    let last = sextet(body[body.len() - 1]).unwrap_or(0);
    match padding {
        2 => last & 0x0f == 0,
        1 => last & 0x03 == 0,
        _ => true,
    }
}

fn deserialize_media_mime<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
    kind: &str,
) -> Result<String, D::Error> {
    let mime = String::deserialize(deserializer)?;
    let subtype = mime.strip_prefix(kind).and_then(|rest| rest.strip_prefix('/'));
    // A simple lowercase concrete media type, not a wildcard, parameterized
    // header, data URI, or codec allowlist. Payload decoding belongs downstream.
    if mime.len() > 256
        || !subtype.is_some_and(|subtype| {
            !subtype.is_empty()
                && subtype.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || b"!#$&^_.+-".contains(&byte)
                })
        })
    {
        return Err(serde::de::Error::custom(format!(
            "mimeType must be a simple lowercase {kind} media type (at most 256 bytes)"
        )));
    }
    Ok(mime)
}

fn deserialize_image_mime<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    deserialize_media_mime(deserializer, "image")
}

fn deserialize_audio_mime<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    deserialize_media_mime(deserializer, "audio")
}

#[cfg(test)]
#[path = "protocol_binary_tests.rs"]
mod binary_tests;

#[cfg(test)]
#[path = "protocol_command_order_tests.rs"]
mod command_order_tests;

/// Serialized access to the process stdout — the single ACP response stream.
/// Shared by the request dispatcher and every in-flight prompt turn, so writes
/// are serialized and each message is exactly one flushed line.
pub struct Writer {
    pub(crate) mcp_oauth_policy: crate::mcp_oauth::Provision,
    pub(crate) mcp_sampling_policy: crate::mcp_sampling::Provision,
    pub(crate) mcp_catalog: bool,
    oauth_cleanup: std::sync::Mutex<Vec<Arc<CleanupTask>>>,
    oauth_runtime: Option<tokio::runtime::Handle>,
    cleanup_admission: Arc<tokio::sync::Semaphore>,
    cleanup_join: Mutex<()>,
    subagents: AtomicBool,
    child_sessions: std::sync::Mutex<std::collections::HashSet<String>>,
    pub(crate) mcp_registrations: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    checked_pending: std::sync::Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    out: Mutex<Box<dyn AsyncWrite + Unpin + Send>>,
    /// In-flight agent → client requests (e.g. `session/request_permission`),
    /// keyed by the JSON-RPC id we sent. The client's response (an object with
    /// `id` and no `method`) resolves the matching oneshot.
    pending: std::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    closed: std::sync::atomic::AtomicBool,
    input_closed: AtomicBool,
    next_id: AtomicU64,
    protocol_version: AtomicU64,
    questions: std::sync::atomic::AtomicBool,
    /// Whether the client supports standard ACP `elicitation/create` form mode.
    elicitation: std::sync::atomic::AtomicBool,
    /// Whether the client supports standard ACP `elicitation/create` URL mode.
    elicitation_url: std::sync::atomic::AtomicBool,
    /// Whether the client accepts boolean session config options.
    boolean_config: std::sync::atomic::AtomicBool,
    /// Whether live notices may be sent as ACP notice updates.
    notices: std::sync::atomic::AtomicBool,
    /// Whether ID-addressed compaction lifecycle updates may be sent.
    compaction: std::sync::atomic::AtomicBool,
}

// The registry retains live handles if an error-response drain is cancelled.
struct CleanupTask {
    handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    finished: tokio::task::AbortHandle,
}
impl CleanupTask {
    fn is_finished(&self) -> bool { self.finished.is_finished() }
    async fn join(&self) {
        let mut handle = self.handle.lock().await;
        if let Some(task) = handle.as_mut() { let _ = task.await; }
        *handle = None;
    }
}

fn writer_timeout() -> std::time::Duration {
    std::env::var("RPI_ACP_TEST_WRITE_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or_else(|| std::time::Duration::from_secs(10))
}

impl Writer {
    pub fn new() -> Self {
        Self::with_transport_output(tokio::io::stdout())
    }

    fn with_transport_output(output: impl AsyncWrite + Unpin + Send + 'static) -> Self {
        Self {
            mcp_oauth_policy: crate::mcp_oauth::load(),
            mcp_sampling_policy: crate::mcp_sampling::load(),
            mcp_catalog: std::env::var("RPI_ACP_MCP_CATALOG").as_deref() == Ok("1"),
            oauth_cleanup: Default::default(),
            cleanup_admission: Arc::new(tokio::sync::Semaphore::new(128)),
            cleanup_join: Mutex::new(()),
            oauth_runtime: tokio::runtime::Handle::try_current().ok(),
            subagents: AtomicBool::new(false),
            child_sessions: Default::default(),
            mcp_registrations: Default::default(),
            checked_pending: Default::default(),
            out: Mutex::new(Box::new(output)),
            pending: std::sync::Mutex::new(HashMap::new()),
            closed: std::sync::atomic::AtomicBool::new(false),
            input_closed: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            protocol_version: AtomicU64::new(0),
            questions: std::sync::atomic::AtomicBool::new(false),
            elicitation: std::sync::atomic::AtomicBool::new(false),
            elicitation_url: std::sync::atomic::AtomicBool::new(false),
            boolean_config: std::sync::atomic::AtomicBool::new(false),
            notices: std::sync::atomic::AtomicBool::new(false),
            compaction: std::sync::atomic::AtomicBool::new(false),
        }
    }

    async fn write(&self, value: &Value) -> std::io::Result<()> {
        let mut line = value.to_string();
        line.push('\n');
        if self.closed.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "ACP transport is closed",
            ));
        }
        let result = tokio::time::timeout(writer_timeout(), async {
            // The timeout includes waiting for the writer mutex. A stalled
            // transport must not let a notification hold the server alive
            // forever while another task waits behind it.
            let mut out = self.out.lock().await;
            out.write_all(line.as_bytes()).await?;
            out.flush().await
        })
        .await;
        let result = match result {
            Ok(result) => result,
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "ACP output backpressure timeout",
            )),
        };
        if result.is_err() {
            // A write failure makes the JSON stream unusable. Closing here also
            // wakes pending agent-to-client requests and makes the main loop
            // perform its normal child/task cleanup instead of continuing.
            self.close();
        }
        result
    }

    /// Answer a JSON-RPC request.
    pub async fn respond(&self, id: &Value, mut result: Value) {
        if self.supports_v2() {
            crate::v2::map_config_options(&mut result);
        }
        let _ = self
            .write(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
            .await;
    }

    /// Answer a JSON-RPC request with an error.
    pub async fn respond_error(&self, id: &Value, code: i64, message: &str) {
        self.join_oauth_cleanup().await;
        let _ = self
            .write(&json!({
                "jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message},
            }))
            .await;
    }

    /// Send a JSON-RPC notification (no `id`).
    pub async fn notify(&self, method: &str, mut params: Value) {
        if self.supports_v2()
            && method == "session/update"
            && params.pointer("/update/sessionUpdate").and_then(Value::as_str)
                == Some("config_option_update")
        {
            params["update"] = crate::v2::map_session_update(params["update"].take());
        }
        let _ = self
            .write(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await;
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Send an agent → client request and await its response. Resolves to the
    /// `result` object, or `Null` if the connection closes first.
    pub async fn request(&self, method: &str, params: Value) -> Value {
        let params = if self.supports_v2() {
            crate::v2::map_request(method, params)
        } else {
            params
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            if self.is_closed() || self.is_input_closed() {
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

    /// Send an agent → client request and cancel it at the protocol boundary
    /// when the owning native request is cancelled or the session closes.
    pub(crate) async fn request_cancellable(
        &self,
        method: &str,
        params: Value,
        cancelled: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Value {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            if self.is_closed() || self.is_input_closed() || *cancelled.borrow() {
                return Value::Null;
            }
            pending.insert(id, tx);
        }
        let _request = PendingRequest { writer: self, id };
        if self
            .write(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await
            .is_err()
        {
            return Value::Null;
        }
        tokio::select! {
            biased;
            result = rx => result.unwrap_or(Value::Null),
            changed = cancelled.changed() => {
                if changed.is_ok() && *cancelled.borrow() {
                    self.pending.lock().unwrap().remove(&id);
                    self.notify("$/cancel_request", json!({"requestId": id})).await;
                    Value::Null
                } else {
                    Value::Null
                }
            }
        }
    }

    /// Unlike dialog requests, retain the outer ACP error domain, without diagnostics.
    pub async fn request_checked(&self, method: &str, params: Value, cancellation: rpi_extcore::mcp_external::CancellationToken) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.checked_pending.lock().unwrap();
            if self.is_closed() || self.is_input_closed() || cancellation.is_cancelled() { return Err("ACP MCP request unavailable".into()); }
            pending.insert(id, tx);
        }
        struct Pending<'a>(&'a Writer, u64);
        impl Drop for Pending<'_> {
            fn drop(&mut self) { self.0.checked_pending.lock().unwrap().remove(&self.1); }
        }
        let _pending = Pending(self, id);
        self.write(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})).await.map_err(|_| "ACP MCP transport failed")?;
        tokio::select! {
            biased;
            result = rx => result.map_err(|_| "ACP MCP transport closed".to_owned())?,
            _ = cancellation.cancelled() => {
                self.notify("$/cancel_request", json!({"requestId":id})).await;
                Err("ACP MCP operation cancelled".into())
            }
        }
    }

    pub(crate) async fn join_oauth_cleanup(&self) {
        // Serialize response boundaries and drain cleanup produced by root kills.
        let _join = self.cleanup_join.lock().await;
        loop {
            let tasks = self.oauth_cleanup.lock().unwrap().clone();
            if tasks.is_empty() { break; }
            for task in tasks { task.join().await; }
            self.oauth_cleanup.lock().unwrap().retain(|task| !task.is_finished());
        }
    }

    fn reserve_cleanup(&self) -> Result<tokio::sync::OwnedSemaphorePermit, String> {
        self.cleanup_admission.clone().try_acquire_owned()
            .map_err(|_| "ACP cleanup busy".to_owned())
    }

    fn track_cleanup(&self, permit: tokio::sync::OwnedSemaphorePermit,
        cleanup: impl std::future::Future<Output = ()> + Send + 'static) {
        // Core may drop a UI future on its private runtime. Use ACP's runtime.
        let runtime = self.oauth_runtime.clone()
            .unwrap_or_else(tokio::runtime::Handle::current);
        let mut tasks = self.oauth_cleanup.lock().unwrap();
        tasks.retain(|task| !task.is_finished());
        let handle = runtime.spawn(async move {
            cleanup.await;
            drop(permit);
        });
        tasks.push(Arc::new(CleanupTask {
            finished: handle.abort_handle(), handle: Mutex::new(Some(handle)),
        }));
    }

    /// Provider-owned control/deadline are awaited locally. The drop path also
    /// owns a bounded notification task, joined by error responses and teardown.
    pub(crate) async fn request_mcp_oauth(
        self: &Arc<Self>,
        params: Value,
        control: &rpi_extcore::mcp::McpControl,
        deadline: std::time::Instant,
    ) -> Result<Value, rpi_extcore::mcp::AuthError> {
        use rpi_extcore::mcp::AuthError;
        control.check().map_err(|_| AuthError::Cancelled)?;
        if std::time::Instant::now() >= deadline { return Err(AuthError::Timeout); }
        let permit = self.reserve_cleanup().map_err(|_| AuthError::Rejected)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.checked_pending.lock().unwrap();
            if self.is_closed() || self.is_input_closed() { return Err(AuthError::Cancelled); }
            pending.insert(id, tx);
        }
        struct Pending {
            writer: Arc<Writer>,
            id: u64,
            completed: bool,
            permit: Option<tokio::sync::OwnedSemaphorePermit>,
        }
        impl Drop for Pending {
            fn drop(&mut self) {
                self.writer.checked_pending.lock().unwrap().remove(&self.id);
                if !self.completed {
                    let writer = self.writer.clone();
                    let id = self.id;
                    self.writer.track_cleanup(self.permit.take().unwrap(), async move {
                        writer.notify("$/cancel_request", json!({"requestId":id})).await;
                    });
                }
            }
        }
        let mut pending = Pending { writer: self.clone(), id, completed: false, permit: Some(permit) };
        let work = async {
            self.write(&json!({"jsonrpc":"2.0","id":id,"method":"elicitation/create","params":params}))
                .await.map_err(|_| AuthError::Cancelled)?;
            rx.await.map_err(|_| AuthError::Cancelled)?.map_err(|_| AuthError::Rejected)
        };
        let result = tokio::select! {
            biased;
            _ = control.cancelled() => Err(AuthError::Cancelled),
            _ = tokio::time::sleep_until(deadline.into()) => Err(AuthError::Timeout),
            result = work => result,
        };
        if result.is_err() {
            // The canonical JSON-RPC ID, not an elicitation/native/child ID.
            // Best effort if output is already closed; bounded by Writer::write.
            self.notify("$/cancel_request", json!({"requestId":id})).await;
        }
        pending.completed = true;
        result
    }

    /// Stop accepting client work without discarding the surviving write half.
    /// Native terminal owners may still emit bounded best-effort teardown, but
    /// there can be no authoritative replies after the input dispatcher stops.
    pub(crate) fn close_input(&self) {
        let mut pending = self.pending.lock().unwrap();
        self.input_closed.store(true, Ordering::Release);
        pending.clear();
        self.checked_pending.lock().unwrap().clear();
    }

    pub(crate) fn is_input_closed(&self) -> bool {
        self.input_closed.load(Ordering::Acquire)
    }

    /// Called only for validated, owner-scoped native terminal teardown.
    pub(crate) async fn request_terminal_cleanup(
        &self,
        method: &str,
        params: Value,
        cancelled: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Value {
        if !matches!(method, "terminal/kill" | "terminal/release") {
            return Value::Null;
        }
        if !self.is_input_closed() {
            return self.request_cancellable(method, params, cancelled).await;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let _ = self.write(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})).await;
        // Sending is not acknowledgement. Never fabricate successful cleanup.
        Value::Null
    }

    pub fn close(&self) {
        self.checked_pending.lock().unwrap().clear();
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
        self.elicitation_url.store(
            matches!(
                params.pointer("/clientCapabilities/elicitation/url"),
                Some(Value::Object(_)) | Some(Value::Bool(true))
            ),
            Ordering::Release,
        );
        self.boolean_config.store(
            matches!(
                params.pointer("/clientCapabilities/session/configOptions/boolean"),
                Some(Value::Object(_))
            ),
            Ordering::Release,
        );
        self.notices.store(
            matches!(
                params.pointer("/clientCapabilities/session/notices"),
                Some(Value::Object(_))
            ),
            Ordering::Release,
        );
        self.compaction.store(
            matches!(
                params.pointer("/clientCapabilities/session/compaction"),
                Some(Value::Object(_))
            ),
            Ordering::Release,
        );
    }

    pub(crate) fn set_protocol_version(&self, version: u64) {
        self.protocol_version.store(version, Ordering::Release);
    }

    pub(crate) fn protocol_version(&self) -> u64 {
        self.protocol_version.load(Ordering::Acquire)
    }

    pub(crate) fn supports_v2(&self) -> bool {
        self.protocol_version() == crate::v2::PROTOCOL_VERSION
    }

    pub(crate) fn negotiate_subagents(&self, params: &Value) {
        self.subagents.store(self.supports_v2() || crate::subagents::negotiated(params), Ordering::Release);
    }

    pub(crate) fn supports_subagents(&self) -> bool {
        self.subagents.load(Ordering::Acquire)
    }

    pub(crate) fn register_child(&self, id: String) {
        self.child_sessions.lock().unwrap().insert(id);
    }

    /// Resolve only trusted top-level native provenance. Never infer a child
    /// association from nested tool params or from another root's registry entry.
    pub(crate) async fn interaction_target(&self, root: &str, event: &Value) -> Result<String, String> {
        let native = match event.get("sessionId") {
            None => root,
            Some(Value::String(id)) if !id.trim().is_empty() && id.len() <= 512 => id,
            _ => return Err("invalid interaction sessionId".into()),
        };
        if let Some(nested) = event.pointer("/params/sessionId") {
            if nested.as_str() != Some(native) {
                return Err("interaction params.sessionId conflicts with trusted source".into());
            }
        }
        if native == root {
            return Ok(root.to_owned());
        }
        if !self.supports_subagents() {
            return Err("child interaction requires negotiated subagents".into());
        }
        let scoped = format!("child:{}:{}:{}", root.len(), root, native);
        // The watcher and prompt have independent subscriptions. A child_start
        // may still be waiting for output when its first interaction arrives.
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if self.is_closed() {
                    return Err("ACP output closed during child publication".into());
                }
                if self.is_child(&scoped) {
                    return Ok(scoped.clone());
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.map_err(|_| "unannounced child interaction after two seconds".to_owned())?
    }

    pub(crate) fn is_child(&self, id: &str) -> bool {
        self.child_sessions.lock().unwrap().contains(id)
    }

    pub(crate) fn supports_questions(&self) -> bool {
        self.questions.load(Ordering::Acquire)
    }
    pub(crate) fn supports_boolean_config(&self) -> bool {
        self.boolean_config.load(Ordering::Acquire)
    }

    pub(crate) fn supports_notices(&self) -> bool {
        self.notices.load(Ordering::Acquire)
    }

    pub(crate) fn supports_compaction(&self) -> bool {
        self.compaction.load(Ordering::Acquire)
    }

    pub(crate) fn supports_elicitation(&self) -> bool {
        self.supports_elicitation_form() || self.supports_elicitation_url()
    }

    pub(crate) fn supports_elicitation_form(&self) -> bool {
        self.elicitation.load(Ordering::Acquire)
    }

    pub(crate) fn supports_elicitation_url(&self) -> bool {
        self.elicitation_url.load(Ordering::Acquire)
    }
    #[cfg(test)]
    pub(crate) fn with_output(out: impl AsyncWrite + Unpin + Send + 'static) -> Self {
        Self {
            mcp_oauth_policy: Ok(None),
            mcp_sampling_policy: Ok(None),
            mcp_catalog: false,
            oauth_cleanup: Default::default(),
            cleanup_admission: Arc::new(tokio::sync::Semaphore::new(128)),
            cleanup_join: Mutex::new(()),
            oauth_runtime: tokio::runtime::Handle::try_current().ok(),
            subagents: AtomicBool::new(false),
            child_sessions: Default::default(),
            mcp_registrations: Default::default(),
            checked_pending: Default::default(),
            out: Mutex::new(Box::new(out)),
            pending: std::sync::Mutex::new(HashMap::new()),
            closed: std::sync::atomic::AtomicBool::new(false),
            input_closed: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            protocol_version: AtomicU64::new(0),
            questions: std::sync::atomic::AtomicBool::new(false),
            elicitation: std::sync::atomic::AtomicBool::new(false),
            elicitation_url: std::sync::atomic::AtomicBool::new(false),
            boolean_config: std::sync::atomic::AtomicBool::new(false),
            notices: std::sync::atomic::AtomicBool::new(false),
            compaction: std::sync::atomic::AtomicBool::new(false),
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
                "rpi/history": {
                    "version": 1,
                    "method": "_rpi/history",
                    "maxPageMessages": 128,
                    "pagination": "older",
                    "order": "oldest-first"
                },
                "pi-rust/client-tools": {
                    "version": 1,
                    "textOnly": true,
                    "nativeProducerRequired": true
                },
                "rpi/durable": {
                    "version": 1,
                    "optional": true,
                    "notifications": ["session/durable_update"]
                }
            },
            "loadSession": true,
            "mcpCapabilities": {"http": true, "sse": false},
            "auth": {"logout": {}},
            "sessionCapabilities": {
                "list": {}, "resume": {}, "delete": {}, "close": {},
                "additionalDirectories": {}
            },            "promptCapabilities": {
                // The native agent lane accepts base64 image content and
                // forwards it to providers that advertise vision support.
                "image": true,
                // Audio is forwarded as provider-neutral base64 data. Providers
                // without an audio input modality explicitly downgrade/omit it;
                // ACP clients may therefore use this only with an audio model.
                "audio": true,
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

fn parse_additional_directories(params: &Value) -> Result<Vec<PathBuf>, String> {
    let Some(value) = params.get("additionalDirectories") else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| "additionalDirectories must be an array".to_string())?;
    let mut directories = Vec::with_capacity(values.len());
    for value in values {
        let directory = value
            .as_str()
            .ok_or_else(|| "additionalDirectories entries must be strings".to_string())?;
        let path = PathBuf::from(directory);
        if !path.is_absolute() || !path.is_dir() {
            return Err(format!(
                "additionalDirectories entries must be existing absolute directories: {directory}"
            ));
        }
        let canonical = std::fs::canonicalize(&path).map_err(|error| {
            format!("could not resolve additional directory {directory}: {error}")
        })?;
        if !directories.contains(&canonical) {
            directories.push(canonical);
        }
    }
    Ok(directories)
}

fn spawn_durable_update_watcher(
    session: Arc<Session>,
    writer: Arc<Writer>,
    session_id: String,
) -> Option<tokio::task::JoinHandle<()>> {
    let mut events = session.subscribe_durable_events()?;
    Some(tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event)
                    if event["type"] == "session_entry_committed"
                        && event["customType"].as_str() == Some(ENTRY_TYPE) =>
                {
                    if session.refresh_durable().await.unwrap_or(false) {
                        if let Some(notification) = session.durable_update(&session_id).await {
                            writer.notify("session/durable_update", notification).await;
                        }
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // A lost notification is reconciled immediately. There is
                    // intentionally no timer fallback: current native hosts
                    // provide the commit event capability.
                    if session.refresh_durable().await.unwrap_or(false) {
                        if let Some(notification) = session.durable_update(&session_id).await {
                            writer.notify("session/durable_update", notification).await;
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }))
}

#[derive(Debug)]
enum RpcMessage {
    Request {
        id: Option<Value>,
        method: String,
        params: Value,
    },
    Response {
        id: Value,
        result: Value,
        error: Option<Value>,
    },
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message.into()},
    })
}

fn valid_rpc_id(value: &Value) -> bool {
    value.is_null() || value.is_string() || value.as_i64().is_some() || value.as_u64().is_some()
}

/// Validate the JSON-RPC envelope before dispatch. In particular, a malformed
/// prompt block must not disappear through `filter_map`, and a response from
/// the client must not be mistaken for an unknown request.
fn parse_rpc_message(message: Value) -> Result<RpcMessage, Value> {
    let Some(object) = message.as_object() else {
        return Err(rpc_error(Value::Null, -32600, "invalid request"));
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return if object.get("id").is_none() {
            Err(Value::Null)
        } else {
            Err(rpc_error(
                object
                    .get("id")
                    .filter(|id| valid_rpc_id(id))
                    .cloned()
                    .unwrap_or(Value::Null),
                -32600,
                "invalid request",
            ))
        };
    }
    if let Some(method) = object.get("method") {
        let Some(method) = method.as_str() else {
            return if object.get("id").is_none() {
                Err(Value::Null)
            } else {
                Err(rpc_error(
                    object
                        .get("id")
                        .filter(|id| valid_rpc_id(id))
                        .cloned()
                        .unwrap_or(Value::Null),
                    -32600,
                    "invalid request",
                ))
            };
        };
        if object.contains_key("result") || object.contains_key("error") {
            return if object.get("id").is_none() {
                Err(Value::Null)
            } else {
                Err(rpc_error(
                    object
                        .get("id")
                        .filter(|id| valid_rpc_id(id))
                        .cloned()
                        .unwrap_or(Value::Null),
                    -32600,
                    "invalid request",
                ))
            };
        }
        let id = match object.get("id") {
            Some(id) if valid_rpc_id(id) => Some(id.clone()),
            Some(_) => {
                return Err(rpc_error(Value::Null, -32600, "invalid request"));
            }
            None => None,
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        // Method-specific validation returns -32602. Keep the envelope parser
        // permissive here so a known method with a scalar params value is
        // reported as invalid parameters rather than being misclassified as an
        // invalid JSON-RPC request.
        return Ok(RpcMessage::Request {
            id,
            method: method.to_owned(),
            params,
        });
    }

    // A client response may resolve an agent-to-client request. Malformed
    // responses have no request to answer, so they are ignored per JSON-RPC.
    let Some(id) = object.get("id").filter(|id| valid_rpc_id(id)).cloned() else {
        return Err(Value::Null);
    };
    let has_result = object.contains_key("result");
    let has_error = object.contains_key("error");
    if !has_result && !has_error {
        return Err(rpc_error(id, -32600, "invalid request"));
    }
    if has_result == has_error || (has_error && !object["error"].is_object()) {
        return Err(Value::Null);
    }
    Ok(RpcMessage::Response {
        id,
        result: object.get("result").cloned().unwrap_or(Value::Null),
        error: object.get("error").cloned(),
    })
}

fn request_params<'a>(
    params: &'a Value,
    method: &str,
) -> Result<&'a serde_json::Map<String, Value>, String> {
    params
        .as_object()
        .ok_or_else(|| format!("{method} params must be an object"))
}

fn required_string<'a>(
    params: &'a serde_json::Map<String, Value>,
    name: &str,
) -> Result<&'a str, String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{name} must be a non-empty string"))
}

fn prompt_params(params: &Value) -> Result<Vec<ContentBlock>, String> {
    let params = request_params(params, "session/prompt")?;
    let prompt = params
        .get("prompt")
        .and_then(Value::as_array)
        .ok_or_else(|| "prompt must be an array".to_string())?;
    prompt
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let block: ContentBlock = serde_json::from_value(value.clone())
                .map_err(|error| format!("prompt[{index}] is invalid: {error}"))?;
            if matches!(block, ContentBlock::Other) {
                return Err(format!("prompt[{index}] uses unsupported content type"));
            }
            if matches!(&block, ContentBlock::Image { data, mime_type } | ContentBlock::Audio { data, mime_type } if data.trim().is_empty() || mime_type.trim().is_empty()) {
                return Err(format!("prompt[{index}] media data and mimeType are required"));
            }
            Ok(block)
        })
        .collect()
}

fn request_key(id: &Value) -> String {
    serde_json::to_string(id).unwrap_or_else(|_| id.to_string())
}

fn session_list_params(params: &Value) -> Result<(Option<PathBuf>, Option<String>), String> {
    let params = if params.is_null() {
        None
    } else {
        Some(request_params(params, "session/list")?)
    };
    let cwd = params
        .and_then(|params| params.get("cwd"))
        .filter(|value| !value.is_null())
        .map(|value| {
            let cwd = value
                .as_str()
                .filter(|cwd| !cwd.trim().is_empty())
                .ok_or_else(|| "cwd must be an absolute path or null".to_string())?;
            let cwd = PathBuf::from(cwd);
            if !cwd.is_absolute() {
                return Err("cwd must be an absolute path or null".to_string());
            }
            Ok(cwd)
        })
        .transpose()?;
    let cursor = params
        .and_then(|params| params.get("cursor"))
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_str()
                .filter(|cursor| !cursor.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| "cursor must be a non-empty string or null".to_string())
        })
        .transpose()?;
    Ok((cwd, cursor))
}

fn iso_timestamp(milliseconds: i64) -> String {
    let seconds = milliseconds.div_euclid(1000);
    let millis = milliseconds.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        day_seconds / 3_600,
        (day_seconds % 3_600) / 60,
        day_seconds % 60,
    )
}

// Gregorian calendar conversion, valid for the Unix timestamp range used by
// native session headers. Kept here so extcore remains free of ACP/time types.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

struct PromptTask {
    session_id: String,
    session: Arc<Session>,
    handle: tokio::task::JoinHandle<()>,
}

/// The prompt task retains response ownership until settlement, even though
/// v2 acknowledges canonical insertion before the run completes.
pub(crate) struct PromptResponder {
    id: Value,
    inserted: bool,
}

impl PromptResponder {
    pub(crate) async fn inserted(&mut self, writer: &Writer, message_id: &str) {
        if writer.supports_v2() && !self.inserted {
            self.inserted = true;
            writer.respond(&self.id, crate::v2::prompt_result(message_id.to_owned())).await;
        }
    }
}

type PromptTasks = Arc<Mutex<HashMap<String, PromptTask>>>;

struct RequestTask {
    cancel: watch::Sender<bool>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

type RequestTasks = Arc<Mutex<HashMap<String, RequestTask>>>;

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const REQUEST_CLEANUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const MAX_IN_FLIGHT_REQUESTS: usize = 128;

enum RequestError {
    InvalidInput(String),
    Unavailable(String),
    Internal(String),
    Cancelled,
    TimedOut,
}

impl From<String> for RequestError {
    fn from(error: String) -> Self {
        Self::Internal(error)
    }
}

impl From<&str> for RequestError {
    fn from(error: &str) -> Self {
        Self::Internal(error.to_string())
    }
}

/// Opening inventory must follow the response that registers a new session in
/// the client. Replay/state synchronization still belongs before that response.
/// Keep this payload in the existing request task, not a delayed detached task.
struct RequestReply {
    result: Value,
    commands: Option<Value>,
}

async fn spawn_request<F, Fut>(
    tasks: &RequestTasks,
    prompt_tasks: &PromptTasks,
    id: Value,
    writer: Arc<Writer>,
    operation: F,
) -> bool
where
    F: FnOnce(watch::Receiver<bool>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<Value, RequestError>> + Send + 'static,
{
    spawn_request_reply(tasks, prompt_tasks, id, writer, move |cancelled| async move {
        operation(cancelled).await.map(|result| RequestReply { result, commands: None })
    }).await
}

async fn spawn_request_reply<F, Fut>(
    tasks: &RequestTasks,
    _prompt_tasks: &PromptTasks,
    id: Value,
    writer: Arc<Writer>,
    operation: F,
) -> bool
where
    F: FnOnce(watch::Receiver<bool>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<RequestReply, RequestError>> + Send + 'static,
{
    let key = request_key(&id);
    let (cancel, mut cancelled) = watch::channel(false);
    let mut tasks_guard = tasks.lock().await;
    if tasks_guard.len() >= MAX_IN_FLIGHT_REQUESTS || tasks_guard.contains_key(&key) {
        return false;
    }
    tasks_guard.insert(
        key.clone(),
        RequestTask {
            cancel: cancel.clone(),
            handle: None,
        },
    );
    drop(tasks_guard);
    let task_id = id.clone();
    let handle = tokio::spawn(async move {
        let mut operation = Box::pin(operation(cancelled.clone()));
        let outcome = tokio::select! {
            biased;
            _ = wait_cancelled(&mut cancelled) => {
                let result = tokio::time::timeout(REQUEST_CLEANUP_TIMEOUT, &mut operation).await;
                match result {
                    Ok(Ok(result)) => Ok(result), // a committed operation wins the race
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(RequestError::Cancelled),
                }
            }
            result = tokio::time::timeout(REQUEST_TIMEOUT, &mut operation) => {
                match result {
                    Ok(result) => result,
                    Err(_) => {
                        let _ = cancel.send(true);
                        match tokio::time::timeout(REQUEST_CLEANUP_TIMEOUT, &mut operation).await {
                            Ok(Ok(result)) => Ok(result),
                            Ok(Err(RequestError::Cancelled)) => Err(RequestError::TimedOut),
                            Ok(Err(error)) => Err(error),
                            Err(_) => Err(RequestError::TimedOut),
                        }
                    }
                }
            }
        };
        drop(operation); // Register drop guards before response cleanup boundaries.
        match outcome {
            Ok(reply) => {
                writer.respond(&task_id, reply.result).await;
                if let Some(commands) = reply.commands {
                    // respond() closes the writer on output failure; never
                    // publish inventory for a response that could not be sent.
                    if !writer.closed.load(Ordering::Acquire) {
                        writer.notify("session/update", commands).await;
                    }
                }
            }
            Err(RequestError::Unavailable(error)) => {
                writer.respond_error(&task_id, -32001, &error).await
            }
            Err(RequestError::Cancelled) => {
                writer
                    .respond_error(&task_id, -32800, "request cancelled")
                    .await
            }
            Err(RequestError::TimedOut) => {
                writer
                    .respond_error(&task_id, -32603, "request timed out")
                    .await
            }
            Err(RequestError::InvalidInput(error)) => {
                writer.respond_error(&task_id, -32602, &error).await
            }
            Err(RequestError::Internal(error)) => {
                writer.respond_error(&task_id, -32603, &error).await
            }
        }
    });
    attach_request_handle(tasks, &key, handle).await
}

async fn reserve_request(tasks: &RequestTasks, key: String) -> Option<watch::Sender<bool>> {
    let (cancel, _) = watch::channel(false);
    let mut guard = tasks.lock().await;
    if guard.len() >= MAX_IN_FLIGHT_REQUESTS || guard.contains_key(&key) {
        return None;
    }
    guard.insert(
        key,
        RequestTask {
            cancel: cancel.clone(),
            handle: None,
        },
    );
    Some(cancel)
}

async fn attach_request_handle(
    tasks: &RequestTasks,
    key: &str,
    handle: tokio::task::JoinHandle<()>,
) -> bool {
    let mut guard = tasks.lock().await;
    if let Some(task) = guard.get_mut(key) {
        task.handle = Some(handle);
        true
    } else {
        handle.abort();
        false
    }
}

async fn wait_cancelled(cancelled: &mut watch::Receiver<bool>) {
    while !*cancelled.borrow() {
        if cancelled.changed().await.is_err() {
            return;
        }
    }
}

async fn cancel_request_task(tasks: &RequestTasks, key: &str) -> bool {
    if let Some(task) = tasks.lock().await.get(key) {
        let _ = task.cancel.send(true);
        true
    } else {
        false
    }
}

async fn wait_request_tasks(tasks: &RequestTasks) {
    let drained: Vec<RequestTask> = {
        let mut guard = tasks.lock().await;
        guard.drain().map(|(_, task)| task).collect()
    };
    for task in &drained {
        let _ = task.cancel.send(true);
    }
    let deadline = tokio::time::Instant::now() + REQUEST_CLEANUP_TIMEOUT;
    for task in drained {
        if let Some(mut handle) = task.handle {
            if tokio::time::timeout_at(deadline, &mut handle)
                .await
                .is_err()
            {
                handle.abort();
                let _ = handle.await;
            }
        }
    }
}

async fn kill_session(session: Arc<Session>) {
    session.kill().await;
}

/// Own a new child until registry insertion, including errors and dropped futures.
struct OpeningSession(Option<Arc<Session>>, Arc<Writer>, Option<tokio::sync::OwnedSemaphorePermit>);
impl OpeningSession {
    fn reserve(writer: &Arc<Writer>) -> Result<Self, String> {
        Ok(Self(None, writer.clone(), Some(writer.reserve_cleanup()?)))
    }
}
impl Drop for OpeningSession {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            self.1.track_cleanup(self.2.take().unwrap(), kill_session(session));
        }
    }
}

async fn wait_prompt_task(task: PromptTask) {
    let PromptTask {
        session,
        mut handle,
        ..
    } = task;
    // Cancellation is a protocol/native operation, not a Rust task abort. Give
    // the native host time to emit agent_settled and let Session::prompt retire
    // dialogs and its run lease. A stalled native peer is force-terminated only
    // after that graceful window.
    if tokio::time::timeout(std::time::Duration::from_secs(30), &mut handle)
        .await
        .is_err()
    {
        kill_session(session).await;
        if tokio::time::timeout(std::time::Duration::from_secs(5), &mut handle)
            .await
            .is_err()
        {
            handle.abort();
        }
        let _ = handle.await;
    }
}

async fn stop_live_session(
    session_id: &str,
    sessions: &Arc<Mutex<HashMap<String, Arc<Session>>>>,
    durable_watchers: &Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    prompt_tasks: &PromptTasks,
    request_tasks: &RequestTasks,
) -> bool {
    let tasks: Vec<(String, PromptTask)> = {
        let mut guard = prompt_tasks.lock().await;
        let keys: Vec<String> = guard
            .iter()
            .filter(|(_, task)| task.session_id == session_id)
            .map(|(key, _)| key.clone())
            .collect();
        keys.into_iter()
            .filter_map(|key| guard.remove(&key).map(|task| (key, task)))
            .collect()
    };
    {
        let mut guard = request_tasks.lock().await;
        for (key, _) in &tasks {
            guard.remove(key);
        }
    }
    for (_, task) in &tasks {
        let _ = task.session.abort().await;
    }
    for (_, task) in tasks {
        wait_prompt_task(task).await;
    }
    let watcher = { durable_watchers.lock().await.remove(session_id) };
    if let Some(watcher) = watcher {
        watcher.abort();
    }
    let session = { sessions.lock().await.remove(session_id) };
    if let Some(session) = session {
        kill_session(session).await;
        true
    } else {
        false
    }
}

/// Remove completed prompt owners without holding the registry lock over a
/// join. The request registry is the shared ID namespace and capacity gate;
/// retaining a finished owner until this point also makes duplicate IDs
/// deterministic instead of allowing a second operation to start mid-cleanup.
async fn reap_finished_tasks(prompt_tasks: &PromptTasks, request_tasks: &RequestTasks) {
    let finished: Vec<String> = {
        let mut guard = prompt_tasks.lock().await;
        let keys = guard
            .iter()
            .filter(|(_, task)| task.handle.is_finished())
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in &keys {
            guard.remove(key);
        }
        keys
    };
    let mut requests = request_tasks.lock().await;
    for key in finished {
        if requests
            .get(&key)
            .and_then(|task| task.handle.as_ref())
            .is_some_and(tokio::task::JoinHandle::is_finished)
            || requests.get(&key).is_some_and(|task| task.handle.is_none())
        {
            requests.remove(&key);
        }
    }
    requests.retain(|_, task| match task.handle.as_ref() {
        Some(handle) => !handle.is_finished(),
        None => true,
    });
}

/// Run cleanup for in-flight prompts without aborting their Rust tasks first.
async fn stop_all_prompt_tasks(prompt_tasks: &PromptTasks, request_tasks: &RequestTasks) {
    let tasks: Vec<(String, PromptTask)> = {
        let mut guard = prompt_tasks.lock().await;
        guard.drain().collect()
    };
    {
        let mut guard = request_tasks.lock().await;
        for (key, _) in &tasks {
            guard.remove(key);
        }
    }
    for (_, task) in &tasks {
        let _ = task.session.abort().await;
    }
    for (_, task) in tasks {
        wait_prompt_task(task).await;
    }
}

/// Translate only explicitly negotiated client-owned tool capabilities. v2
/// removed the v1 filesystem/terminal fields; its optional compatibility path
/// must therefore be namespaced rather than inferred from legacy flags.
fn client_tool_options(params: &Value, v2: bool) -> rpi_extcore::host::ClientToolOptions {
    let capabilities = params.get("clientCapabilities").unwrap_or(&Value::Null);
    let enabled = |pointer: &str| capabilities.pointer(pointer).and_then(Value::as_bool) == Some(true);
    if v2 {
        rpi_extcore::host::ClientToolOptions {
            read_text: enabled("/_meta/pi-rust~1client-tools/readText"),
            write_text: enabled("/_meta/pi-rust~1client-tools/writeText"),
            terminal: enabled("/_meta/pi-rust~1client-tools/terminal"),
        }
    } else {
        rpi_extcore::host::ClientToolOptions {
            read_text: enabled("/fs/readTextFile"),
            write_text: enabled("/fs/writeTextFile"),
            terminal: enabled("/terminal"),
        }
    }
}

/// v2 draft replay cursors are inclusive. Only the pinned `start` variant
/// is implemented; unsupported/custom cursors must never degrade to no replay.
fn resume_replay_requested(value: Option<&Value>) -> Result<bool, String> {
    match value {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Object(cursor)) if cursor.get("type").and_then(Value::as_str) == Some("start") => Ok(true),
        _ => Err("unsupported or malformed replayFrom cursor".into()),
    }
}

/// Run until stdin closes. The owner reports this exit code to the host's
/// stdio lifecycle; the stop receiver also ends an idle server on shutdown.
pub async fn run(
    stop: watch::Receiver<bool>,
    input: StdinLines,
    pending: Option<String>,
) -> i32 {
    run_with_io(stop, input, pending, tokio::io::stdout()).await
}

/// Run the same dispatcher with transport-owned input and bounded output.
pub async fn run_with_io(
    mut stop: watch::Receiver<bool>,
    mut input: impl Input,
    mut pending: Option<String>,
    output: impl AsyncWrite + Unpin + Send + 'static,
) -> i32 {
    let writer = Arc::new(Writer::with_transport_output(output));
    let mut client_tools = rpi_extcore::host::ClientToolOptions::default();
    let mut experimental_sessions = false;
    let auth_negotiated: crate::auth_flow::Negotiated = Arc::new(std::sync::Mutex::new(None));
    let sessions: Arc<Mutex<HashMap<String, Arc<Session>>>> = Arc::new(Mutex::new(HashMap::new()));
    // Weak entries serialize same-session load/resume without retaining an
    // unbounded registry of IDs supplied by clients.
    let opening_locks: Arc<Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>>
        = Arc::new(Mutex::new(HashMap::new()));
    let durable_watchers: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let prompt_tasks: PromptTasks = Arc::new(Mutex::new(HashMap::new()));
    let request_tasks: RequestTasks = Arc::new(Mutex::new(HashMap::new()));

    let mut tick = tokio::time::interval(std::time::Duration::from_millis(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut exit_code = 0;
    // Why the loop ended, reported once at the bottom. Zed forwards agent stderr
    // into its own log, so this is what turns "transport closed" into a cause.
    let mut reason = "stdin closed (client hung up)";
    'input: loop {
        if writer.is_closed() {
            reason = "ACP transport write failed";
            break 'input;
        }
        let line = loop {
            if let Some(line) = pending.take() {
                break line;
            }
            if *stop.borrow() {
                reason = "stopped by the host (session shutdown)";
                break 'input;
            }
            if writer.is_closed() {
                reason = "ACP transport write failed";
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
                // Parse errors are the one JSON-RPC error that has no usable
                // request id. They still get a response, unlike notifications.
                if writer
                    .write(&rpc_error(
                        Value::Null,
                        -32700,
                        format!("parse error: {error}"),
                    ))
                    .await
                    .is_err()
                {
                    reason = "ACP transport write failed";
                    break 'input;
                }
                continue;
            }
        };
        let rpc = match parse_rpc_message(message) {
            Ok(message) => message,
            Err(error) if error.is_null() => continue,
            Err(error) => {
                if writer.write(&error).await.is_err() {
                    reason = "ACP transport write failed";
                    break 'input;
                }
                continue;
            }
        };
        let (id, method, params) = match rpc {
            RpcMessage::Request { id, method, params } => (id, method, params),
            RpcMessage::Response { id, result, error } => {
                if let Some(numeric_id) = id.as_u64() {
                    let checked = writer.checked_pending.lock().unwrap().remove(&numeric_id);
                    if let Some(tx) = checked {
                        let _ = tx.send(if error.is_some() { Err("ACP MCP outer request failed".into()) } else { Ok(result) });
                        continue;
                    }
                    if let Some(error) = error {
                        writer
                            .resolve(numeric_id, json!({"__acp_error": error}))
                            .await;
                    } else {
                        writer.resolve(numeric_id, result).await;
                    }
                }
                continue;
            }
        };

        // `RPI_ACP_DEBUG=1` traces the method stream. It costs a line per
        // request, which is how we find out where a client gave up.
        if crate::diag::debug_enabled() {
            match &id {
                Some(id) => diagln!("[rpi-acp] <- {} {id}", method),
                None => diagln!("[rpi-acp] <- {} (notification)", method),
            }
        }

        // Reap both protocol-specific prompt owners and generic requests from
        // the same shared ID namespace. Finished prompts are removed together
        // with their reservation, never before their response task completed.
        reap_finished_tasks(&prompt_tasks, &request_tasks).await;

        // Live children are connection-local and read-only. Never let child IDs
        // reach saved-session metadata, fork, load, or mutation paths.
        if method.starts_with("session/")
            && params.get("sessionId").and_then(Value::as_str).is_some_and(|id| writer.is_child(id))
        {
            if let Some(id) = id.as_ref() {
                writer.respond_error(id, -32602, "child session is live-only and read-only").await;
            }
            continue;
        }

        match (id, method.as_str()) {
            (Some(id), "initialize") => {
                let params = match request_params(&params, "initialize") {
                    Ok(params) => params,
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let requested_version = params.get("protocolVersion").and_then(Value::as_u64);
                let negotiated_version = match requested_version {
                    Some(version) if version >= crate::v2::PROTOCOL_VERSION => {
                        if let Err(error) =
                            crate::v2::valid_initialize_params(&Value::Object(params.clone()))
                        {
                            writer.respond_error(&id, -32602, &error).await;
                            continue;
                        }
                        crate::v2::PROTOCOL_VERSION
                    }
                    Some(version) if version >= PROTOCOL_VERSION => PROTOCOL_VERSION,
                    _ => {
                        writer
                            .respond_error(&id, -32602, "unsupported ACP protocol version")
                            .await;
                        continue;
                    }
                };
                if let Some(capabilities) =
                    params.get(if negotiated_version == crate::v2::PROTOCOL_VERSION {
                        "capabilities"
                    } else {
                        "clientCapabilities"
                    })
                {
                    if !capabilities.is_object() {
                        writer
                            .respond_error(&id, -32602, "client capabilities must be an object")
                            .await;
                        continue;
                    }
                }
                let initialize_params = if negotiated_version == crate::v2::PROTOCOL_VERSION {
                    crate::v2::legacy_capability_view(&Value::Object(params.clone()))
                } else {
                    Value::Object(params.clone())
                };
                writer.set_protocol_version(negotiated_version);
                writer.negotiate_questions(&initialize_params);
                writer.negotiate_subagents(&initialize_params);
                client_tools = client_tool_options(&initialize_params, writer.supports_v2());
                let terminal_auth = client_supports_terminal_auth(&initialize_params);
                experimental_sessions = session_extensions::negotiated(&Value::Object(params.clone()));
                let mut result = if negotiated_version == crate::v2::PROTOCOL_VERSION {
                    crate::v2::initialize_result(&initialize_result_for(terminal_auth))
                } else {
                    initialize_result_for(terminal_auth)
                };
                session_extensions::advertise(&mut result, experimental_sessions, writer.supports_v2());
                crate::mcp_acp::advertise(&mut result, experimental_sessions, writer.supports_v2());
                let auth_state = auth_negotiated.clone();
                let auth_writer = writer.clone();
                let accepted = spawn_request(&request_tasks, &prompt_tasks, id.clone(), writer.clone(), move |mut cancelled| async move {
                    let descriptor = tokio::select! {
                        biased;
                        _ = wait_cancelled(&mut cancelled) => return Err(RequestError::Cancelled),
                        descriptor = crate::auth_flow::probe(&auth_writer) => descriptor,
                    };
                    crate::auth_flow::advertise(&mut result, descriptor.as_ref(), auth_writer.supports_v2(), experimental_sessions);
                    *auth_state.lock().unwrap() = descriptor;
                    Ok(result)
                }).await;
                if !accepted { writer.respond_error(&id, -32600, "duplicate request id").await; }
            }
            (Some(id), "auth/status") => {
                if !experimental_sessions {
                    writer.respond_error(&id, -32601, "auth/status requires revision-pinned RFD opt-in").await;
                    continue;
                }
                if !params.is_null() && !params.is_object() {
                    writer.respond_error(&id, -32602, "params must be an object").await;
                    continue;
                }
                let accepted = spawn_request(&request_tasks, &prompt_tasks, id.clone(), writer.clone(), move |mut cancelled| async move {
                    let provider = crate::auth::configured_provider().map_err(RequestError::InvalidInput)?;
                    tokio::select! {
                        biased;
                        _ = wait_cancelled(&mut cancelled) => Err(RequestError::Cancelled),
                        result = crate::auth::check_async(&provider) => {
                            let verdict = result.map_err(|_| RequestError::Unavailable("credential status check failed".into()))?;
                            Ok(json!({"authenticated":matches!(verdict, crate::auth::Verdict::Ready { .. })}))
                        }
                    }
                }).await;
                if !accepted { writer.respond_error(&id, -32600, "duplicate request id").await; }
            }
            (Some(id), "auth/get_state") => {
                // The pinned ACP v2 schema has no auth/get_state method.
                writer
                    .respond_error(&id, -32601, "auth/get_state is not defined by ACP v2")
                    .await;
            }
            // `authenticate` is the v1 auth state settlement. The terminal
            // descriptor is executed by the client; this request verifies that
            // the configured provider is actually usable before reporting
            // success. Never answer it with a tolerant no-op.
            (Some(id), method @ ("authenticate" | "auth/login")) => {
                if (method == "auth/login") != writer.supports_v2() {
                    writer
                        .respond_error(
                            &id,
                            -32601,
                            "authentication method is not available for the negotiated protocol",
                        )
                        .await;
                    continue;
                }
                let params = match request_params(&params, method) {
                    Ok(params) => params,
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let method_id = match required_string(params, "methodId") {
                    Ok(method_id) => method_id,
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let descriptor = auth_negotiated.lock().unwrap().clone();
                if method_id != crate::auth_flow::AGENT_METHOD_ID || descriptor.is_none() {
                    writer.respond_error(&id, -32602, "unsupported or unadvertised agent authentication method").await;
                    continue;
                }
                let descriptor = descriptor.unwrap();
                let auth_writer = writer.clone();
                let parent = id.clone();
                let accepted = spawn_request(
                    &request_tasks,
                    &prompt_tasks,
                    id.clone(),
                    writer.clone(),
                    move |cancelled| async move {
                        crate::auth_flow::authenticate(auth_writer, parent, descriptor, cancelled).await.map_err(|error| match error {
                            crate::auth_flow::FlowError::Cancelled => RequestError::Cancelled,
                            crate::auth_flow::FlowError::Invalid => RequestError::InvalidInput("invalid authentication elicitation response".into()),
                            crate::auth_flow::FlowError::Failed => RequestError::Unavailable("authentication was not completed".into()),
                        })
                    },
                )
                .await;
                if !accepted {
                    writer
                        .respond_error(&id, -32600, "duplicate request id")
                        .await;
                }
            }
            (Some(id), method @ ("logout" | "auth/logout")) => {
                if (method == "auth/logout") != writer.supports_v2() {
                    writer
                        .respond_error(
                            &id,
                            -32601,
                            "authentication method is not available for the negotiated protocol",
                        )
                        .await;
                    continue;
                }
                if !params.is_null() && !params.is_object() {
                    writer
                        .respond_error(&id, -32602, "params must be an object")
                        .await;
                    continue;
                }
                let accepted = spawn_request(
                    &request_tasks,
                    &prompt_tasks,
                    id.clone(),
                    writer.clone(),
                    move |mut cancelled| async move {
                        tokio::select! {
                            biased;
                            _ = wait_cancelled(&mut cancelled) => Err(RequestError::Cancelled),
                            result = crate::auth::logout_async() => {
                                result.map(|()| json!({})).map_err(RequestError::from)
                            }
                        }
                    },
                )
                .await;
                if !accepted {
                    writer
                        .respond_error(&id, -32600, "duplicate request id")
                        .await;
                }
            }
            (Some(id), "session/new") => {
                let params = match request_params(&params, "session/new") {
                    Ok(params) => params,
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let cwd = match required_string(params, "cwd") {
                    Ok(cwd) => cwd.to_owned(),
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let system_prompt = match crate::system_prompt::parse(&Value::Object(params.clone()), experimental_sessions) {
                    Ok(prompt) => prompt,
                    Err(error) => { writer.respond_error(&id, -32602, &error).await; continue; }
                };
                if writer.supports_v2() {
                    if let Err(error) =
                        crate::v2::valid_session_setup_with_extensions(&Value::Object(params.clone()), experimental_sessions)
                    {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                }
                let mcp_servers = match crate::mcp::parse(&Value::Object(params.clone()), writer.supports_v2(), experimental_sessions) {
                    Ok(servers) => servers,
                    Err(error) => { writer.respond_error(&id, -32602, &error).await; continue; }
                };
                let additional_directories =
                    match parse_additional_directories(&Value::Object(params.clone())) {
                        Ok(directories) => directories,
                        Err(error) => {
                            writer.respond_error(&id, -32602, &error).await;
                            continue;
                        }
                    };
                let session_id = uuid::Uuid::new_v4().to_string();
                let sessions_for_task = sessions.clone();
                let watchers_for_task = durable_watchers.clone();
                let prompt_tasks_for_task = prompt_tasks.clone();
                let request_tasks_for_task = request_tasks.clone();
                let writer_for_task = writer.clone();
                let id_for_task = session_id.clone();
                let accepted = spawn_request_reply(
                    &request_tasks,
                    &prompt_tasks,
                    id.clone(),
                    writer.clone(),
                    move |mut cancelled| async move {
                        let mut child_owner = OpeningSession::reserve(&writer_for_task)?;
                        let mut opening = Box::pin(open_session(
                            &id_for_task,
                            PathBuf::from(cwd),
                            additional_directories.clone(),
                            None,
                            client_tools,
                            mcp_servers,
                            &writer_for_task,
                        ));
                        let (session, state) = tokio::select! {
                            biased;
                            _ = wait_cancelled(&mut cancelled) => {
                                return Err(RequestError::Cancelled);
                            }
                            result = &mut opening => result?,
                        };
                        child_owner.0 = Some(session.clone());
                        if let Some(prompt) = system_prompt.as_ref() {
                            tokio::select! {
                                biased;
                                _ = wait_cancelled(&mut cancelled) => return Err(RequestError::Cancelled),
                                result = session.configure_system_prompt(prompt) => result?,
                            }
                        }
                        if *cancelled.borrow() {
                            kill_session(session).await;
                            return Err(RequestError::Cancelled);
                        }
                        if let Err(error) = crate::history::save_additional_directories(
                            &id_for_task,
                            &additional_directories,
                        ) {
                            kill_session(session).await;
                            return Err(RequestError::Internal(error));
                        }
                        let mut result = json!({"sessionId": id_for_task});
                        match config_options_for_capability(
                            &session,
                            &state,
                            writer_for_task.supports_boolean_config()
                                && session.supports_command("set_compaction_enabled"),
                        )
                        .await {
                            Ok(config) => result["configOptions"] = config["configOptions"].clone(),
                            Err(error) => diagln!(
                                "[rpi-acp] session {id_for_task}: config options unavailable: {error}"
                            ),
                        }
                        if !writer_for_task.supports_v2() {
                            if let Some(modes) = session_modes(&session).await {
                                result["modes"] = modes;
                            }
                        }
                        let watcher = spawn_durable_update_watcher(
                            session.clone(),
                            writer_for_task.clone(),
                            id_for_task.clone(),
                        );

                        sessions_for_task
                            .lock()
                            .await
                            .insert(id_for_task.clone(), session.clone());
                        child_owner.0.take();
                        if let Some(watcher) = watcher {
                            watchers_for_task.lock().await.insert(id_for_task.clone(), watcher);
                        }
                        if *cancelled.borrow() {
                            stop_live_session(
                                &id_for_task,
                                &sessions_for_task,
                                &watchers_for_task,
                                &prompt_tasks_for_task,
                                &request_tasks_for_task,
                            )
                            .await;
                            return Err(RequestError::Cancelled);
                        }
                        let commands = prepare_session_commands(&session, &writer_for_task, &id_for_task).await;
                        if *cancelled.borrow() {
                            stop_live_session(
                                &id_for_task, &sessions_for_task, &watchers_for_task,
                                &prompt_tasks_for_task, &request_tasks_for_task,
                            ).await;
                            return Err(RequestError::Cancelled);
                        }
                        Ok(RequestReply { result, commands })
                    },
                )
                .await;
                if !accepted {
                    writer
                        .respond_error(&id, -32600, "duplicate request id")
                        .await;
                }
            }
            (Some(id), method @ ("session/set_title" | "session/archive" | "session/unarchive" | "session/fork")) => {
                if !experimental_sessions {
                    writer.respond_error(&id, -32601, "session RFD methods require explicit unstable negotiation").await;
                    continue;
                }
                let session_id = match request_params(&params, method).and_then(|p| required_string(p, "sessionId")) {
                    Ok(value) => value.to_owned(),
                    Err(error) => { writer.respond_error(&id, -32602, &error).await; continue; }
                };
                let validation = if method == "session/fork" {
                    session_extensions::validate_fork(&params).map(|_| ())
                } else if method == "session/set_title" {
                    session_extensions::normalized_title(&params).map(|_| ())
                } else { Ok(()) };
                if let Err(error) = validation {
                    writer.respond_error(&id, -32602, &error).await;
                    continue;
                }
                let opening_lock = {
                    let mut locks = opening_locks.lock().await;
                    locks.retain(|_, lock| lock.strong_count() > 0);
                    let lock = locks.get(&session_id).and_then(std::sync::Weak::upgrade)
                        .unwrap_or_else(|| Arc::new(Mutex::new(())));
                    locks.insert(session_id.clone(), Arc::downgrade(&lock));
                    lock
                };
                // Hold the same per-ID lock as load/close/delete through the
                // existence check and mutation, so deletion cannot race a rename.
                let opening_guard = opening_lock.lock_owned().await;
                if crate::history::saved_session(&session_id).is_err() {
                    writer.respond_error(&id, -32002, "saved session not found").await;
                    continue;
                }
                let context = session_extensions::Context {
                    sessions: sessions.clone(), watchers: durable_watchers.clone(),
                    prompts: prompt_tasks.clone(), requests: request_tasks.clone(),
                    writer: writer.clone(), client_tools,
                };
                let method = method.to_owned();
                let accepted = spawn_request(&request_tasks, &prompt_tasks, id.clone(), writer.clone(), move |cancelled| async move {
                    let _guard = opening_guard;
                    if *cancelled.borrow() { return Err(RequestError::Cancelled); }
                    if method == "session/fork" {
                        session_extensions::fork(params, session_id, context, cancelled).await
                    } else {
                        session_extensions::mutate(&method, params, session_id, context).await
                    }
                }).await;
                if !accepted { writer.respond_error(&id, -32600, "duplicate request id").await; }
            }
            (Some(id), "session/list") => {
                let archive_filter = match session_extensions::archive_filter(&params, experimental_sessions) {
                    Ok(value) => value,
                    Err(error) => { writer.respond_error(&id, -32602, &error).await; continue; }
                };
                let (cwd, cursor) = match session_list_params(&params) {
                    Ok(value) => value,
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let root = match crate::history::session_root() {
                    Ok(root) => root,
                    Err(error) => {
                        writer.respond_error(&id, -32603, &error).await;
                        continue;
                    }
                };
                let accepted = spawn_request(
                    &request_tasks,
                    &prompt_tasks,
                    id.clone(),
                    writer.clone(),
                    move |mut cancelled| async move {
                        let cancelled_flag = Arc::new(AtomicBool::new(false));
                        let worker_flag = cancelled_flag.clone();
                        let mut worker = tokio::task::spawn_blocking(move || {
                            rpi_extcore::session_metadata::list_sessions(
                                &root,
                                cwd.as_deref(),
                                cursor.as_deref(),
                                50,
                                archive_filter,
                                &worker_flag,
                            )
                        });
                        let page = tokio::select! {
                            biased;
                            _ = wait_cancelled(&mut cancelled) => {
                                cancelled_flag.store(true, Ordering::Release);
                                match worker.await {
                                    Ok(Ok(_)) => return Err(RequestError::Cancelled),
                                    Ok(Err(error)) => return Err(RequestError::Internal(error.to_string())),
                                    Err(error) => return Err(RequestError::Internal(error.to_string())),
                                }
                            }
                            result = &mut worker => result,
                        }
                        .map_err(|error| RequestError::Internal(error.to_string()))?
                        .map_err(|error| match error {
                            rpi_extcore::history::SavedSessionListError::InvalidCursor => {
                                RequestError::InvalidInput(error.to_string())
                            }
                            error => RequestError::Internal(error.to_string()),
                        })?;
                        let sessions = page
                            .sessions
                            .into_iter()
                            .map(|(session, metadata)| {
                                let mut result = json!({
                                    "sessionId": session.id,
                                    "cwd": session.cwd,
                                    "title": session.title,
                                    "updatedAt": iso_timestamp(session.updated_at),
                                });
                                if experimental_sessions { result["archived"] = json!(metadata.archived); }
                                if let Ok(directories) = crate::history::load_additional_directories(
                                    result["sessionId"].as_str().unwrap_or_default(),
                                ) {
                                    result["additionalDirectories"] = json!(directories
                                        .iter()
                                        .map(|directory| directory.to_string_lossy().into_owned())
                                        .collect::<Vec<_>>());
                                }
                                result
                            })
                            .collect::<Vec<_>>();
                        let mut result = json!({"sessions": sessions});
                        if let Some(cursor) = page.next_cursor {
                            result["nextCursor"] = json!(cursor);
                        }
                        Ok(result)
                    },
                )
                .await;
                if !accepted {
                    writer
                        .respond_error(&id, -32600, "duplicate request id")
                        .await;
                }
            }
            (Some(id), "session/durable") => {
                let params = match request_params(&params, "session/durable") {
                    Ok(params) => Value::Object(params.clone()),
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let session_id = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .filter(|id| !id.trim().is_empty())
                    .map(str::to_owned);
                let Some(session_id) = session_id else {
                    writer
                        .respond_error(&id, -32602, "sessionId must be a non-empty string")
                        .await;
                    continue;
                };
                let session = sessions.lock().await.get(&session_id).cloned();
                let Some(session) = session else {
                    writer.respond_error(&id, -32602, "unknown sessionId").await;
                    continue;
                };
                let is_status = params.get("operation").and_then(Value::as_str) == Some("status");
                let mutation = if is_status {
                    None
                } else {
                    match durable_mutation(&params) {
                        Ok(mutation) => Some(mutation),
                        Err(error) => {
                            writer.respond_error(&id, -32602, &error).await;
                            continue;
                        }
                    }
                };
                let writer_for_task = writer.clone();
                let session_id_for_task = session_id.clone();
                let accepted = spawn_request(
                    &request_tasks,
                    &prompt_tasks,
                    id.clone(),
                    writer.clone(),
                    move |_cancelled| async move {
                        if is_status {
                            return session
                                .durable_state()
                                .await
                                .map(|state| json!({"sessionId":session_id_for_task,"state":state}))
                                .map_err(|error| {
                                    if error.starts_with("durable_unavailable:") {
                                        RequestError::Unavailable(error)
                                    } else {
                                        RequestError::Internal(error)
                                    }
                                });
                        }
                        match session
                            .mutate_durable(mutation.expect("mutation for non-status"))
                            .await
                        {
                            Ok(mut result) => {
                                result["sessionId"] = json!(session_id_for_task);
                                let notification = json!({
                                    "sessionId": session_id_for_task,
                                    "revision": result["state"]["revision"],
                                    "state": result["state"],
                                });
                                writer_for_task
                                    .notify("session/durable_update", notification)
                                    .await;
                                Ok(result)
                            }
                            Err(error) if error.starts_with("durable_unavailable:") => {
                                Err(RequestError::Unavailable(error))
                            }
                            Err(error) => Err(RequestError::Internal(error)),
                        }
                    },
                )
                .await;
                if !accepted {
                    writer
                        .respond_error(&id, -32600, "duplicate request id")
                        .await;
                }
            }
            (Some(id), "rpi/client_request") => {
                let params = match request_params(&params, "rpi/client_request") {
                    Ok(params) => params,
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let session_id = match required_string(params, "sessionId") {
                    Ok(session_id) => session_id.to_owned(),
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let method = match required_string(params, "method") {
                    Ok(method) => method.to_owned(),
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let request_params = params.get("params").cloned().unwrap_or_else(|| json!({}));
                let session = sessions.lock().await.get(&session_id).cloned();
                let Some(session) = session else {
                    writer.respond_error(&id, -32602, "unknown sessionId").await;
                    continue;
                };
                let writer_for_task = writer.clone();
                let accepted = spawn_request(
                    &request_tasks,
                    &prompt_tasks,
                    id.clone(),
                    writer.clone(),
                    move |mut cancelled| async move {
                        tokio::select! {
                            biased;
                            _ = wait_cancelled(&mut cancelled) => Err(RequestError::Cancelled),
                            result = session.client_request(&writer_for_task, &method, request_params) => result.map_err(RequestError::Internal),
                        }
                    },
                ).await;
                if !accepted {
                    writer
                        .respond_error(&id, -32600, "duplicate request id")
                        .await;
                }
            }
            (Some(id), "session/prompt") => {
                let prompt = match prompt_params(&params) {
                    Ok(prompt) => prompt,
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let session_id = match params
                    .as_object()
                    .and_then(|params| params.get("sessionId"))
                    .and_then(Value::as_str)
                    .filter(|id| !id.trim().is_empty())
                {
                    Some(session_id) => session_id.to_owned(),
                    None => {
                        writer
                            .respond_error(&id, -32602, "sessionId must be a non-empty string")
                            .await;
                        continue;
                    }
                };
                let key = request_key(&id);
                if reserve_request(&request_tasks, key.clone()).await.is_none() {
                    writer
                        .respond_error(
                            &id,
                            -32600,
                            "duplicate request id or in-flight limit reached",
                        )
                        .await;
                    continue;
                }
                let session = sessions.lock().await.get(&session_id).cloned();
                match session {
                    Some(session) => {
                        let writer = Arc::clone(&writer);
                        let task_session = session.clone();
                        let task_session_id = session_id.clone();
                        let task_id = id.clone();
                        let handle = tokio::spawn(async move {
                            let mut response = PromptResponder { id: task_id.clone(), inserted: false };
                            match task_session
                                .prompt_with_responder(&writer, &task_session_id, &prompt, Some(&mut response))
                                .await
                            {
                                Ok(result) if !writer.supports_v2() => {
                                    writer.respond(&task_id, json!({"stopReason": result.stop_reason})).await;
                                }
                                Ok(_) if response.inserted => {}
                                result => {
                                    let error = result.err().unwrap_or_else(|| "native prompt settled without canonical user insertion".to_owned());
                                    diagln!("[rpi-acp] session {task_session_id}: prompt failed: {error}");
                                    if writer.supports_v2() {
                                        writer.notify("session/update", json!({
                                            "sessionId": task_session_id,
                                            "update": {"sessionUpdate":"state_update","state":"idle",
                                                "stopReason":"error","error":{"code":-32603,"message":error}}
                                        })).await;
                                    }
                                    if !response.inserted {
                                        writer.respond_error(&task_id, -32603, &error).await;
                                    }
                                }
                            }
                        });
                        prompt_tasks.lock().await.insert(
                            key,
                            PromptTask {
                                session_id,
                                session,
                                handle,
                            },
                        );
                    }
                    None => {
                        request_tasks.lock().await.remove(&key);
                        writer.respond_error(&id, -32602, "unknown sessionId").await
                    }
                }
            }
            (Some(id), "_rpi/history") => {
                // Explicit private extension. Standard session/load retains full
                // replay; this endpoint returns bounded older pages on demand.
                let params = match request_params(&params, "_rpi/history") {
                    Ok(params) => params,
                    Err(error) => { writer.respond_error(&id, -32602, &error).await; continue; }
                };
                let session_id = match required_string(params, "sessionId") {
                    Ok(value) => value.to_owned(),
                    Err(error) => { writer.respond_error(&id, -32602, &error).await; continue; }
                };
                let limit = match params.get("limit") {
                    None => 80,
                    Some(value) => match value.as_u64() {
                        Some(value) if (1..=rpi_extcore::history::page::MAX_PAGE_MESSAGES as u64).contains(&value) => value as usize,
                        _ => { writer.respond_error(&id, -32602, "history limit must be 1..128").await; continue; }
                    }
                };
                let cursor = match params.get("cursor") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(value)) => match crate::history::validate_cursor(value, &session_id) {
                        Ok(cursor) => Some(cursor),
                        Err(error) => { writer.respond_error(&id, -32602, &error).await; continue; }
                    },
                    _ => { writer.respond_error(&id, -32602, "invalid history cursor").await; continue; }
                };
                let session = sessions.lock().await.get(&session_id).cloned();
                let Some(session) = session else {
                    writer.respond_error(&id, -32602, "unknown sessionId; load or resume it first").await;
                    continue;
                };
                let writer_for_task = writer.clone();
                let accepted = spawn_request(&request_tasks, &prompt_tasks, id.clone(), writer.clone(),
                    move |mut cancelled| async move {
                        tokio::select! {
                            biased;
                            _ = wait_cancelled(&mut cancelled) => Err(RequestError::Cancelled),
                            result = crate::history::page(&session, &session_id, cursor, limit, writer_for_task.supports_v2()) => result.map_err(RequestError::InvalidInput),
                        }
                    }
                ).await;
                if !accepted { writer.respond_error(&id, -32600, "duplicate request id").await; }
            }
            (Some(id), method @ ("session/load" | "session/resume")) => {
                let params = match request_params(&params, method) {
                    Ok(params) => params,
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                if writer.supports_v2() && method == "session/load" {
                    writer.respond_error(&id, -32601, "session/load is not defined in ACP v2; use session/resume").await;
                    continue;
                }
                let system_prompt = match crate::system_prompt::parse(&Value::Object(params.clone()), experimental_sessions) {
                    Ok(prompt) => prompt,
                    Err(error) => { writer.respond_error(&id, -32602, &error).await; continue; }
                };
                let replay_requested = if writer.supports_v2() {
                    match resume_replay_requested(params.get("replayFrom")) {
                        Ok(replay) => replay,
                        Err(error) => {
                            writer.respond_error(&id, -32602, &error).await;
                            continue;
                        }
                    }
                } else { false };
                if writer.supports_v2() {
                    // Replay is implemented here; leave all other setup checks
                    // with the v2 owner rather than weakening that validator.
                    let mut setup = params.clone();
                    setup.remove("replayFrom");
                    if let Err(error) = crate::v2::valid_session_setup_with_extensions(&Value::Object(setup), experimental_sessions) {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                }
                let session_id = match required_string(params, "sessionId") {
                    Ok(session_id) => session_id.to_owned(),
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let cwd = match required_string(params, "cwd") {
                    Ok(cwd) => cwd.to_owned(),
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let mcp_servers = match crate::mcp::parse(&Value::Object(params.clone()), writer.supports_v2(), experimental_sessions) {
                    Ok(servers) => servers,
                    Err(error) => { writer.respond_error(&id, -32602, &error).await; continue; }
                };
                let sessions_for_task = sessions.clone();
                let watchers_for_task = durable_watchers.clone();
                let prompt_tasks_for_task = prompt_tasks.clone();
                let request_tasks_for_task = request_tasks.clone();
                let writer_for_task = writer.clone();
                let opening_lock = {
                    let mut locks = opening_locks.lock().await;
                    locks.retain(|_, lock| lock.strong_count() > 0);
                    let lock = locks.get(&session_id).and_then(std::sync::Weak::upgrade)
                        .unwrap_or_else(|| Arc::new(Mutex::new(())));
                    locks.insert(session_id.clone(), Arc::downgrade(&lock));
                    lock
                };
                let method_name = method.to_owned();
                let session_id_for_task = session_id.clone();
                let accepted = spawn_request_reply(
                    &request_tasks,
                    &prompt_tasks,
                    id.clone(),
                    writer.clone(),
                    move |mut cancelled| async move {
                        let _opening_guard = tokio::select! {
                            biased;
                            _ = wait_cancelled(&mut cancelled) => return Err(RequestError::Cancelled),
                            guard = opening_lock.lock() => guard,
                        };
                        let existing = sessions_for_task
                            .lock()
                            .await
                            .get(&session_id_for_task)
                            .cloned();
                        let mut child_owner = OpeningSession::reserve(&writer_for_task)?;
                        let newly_opened = existing.is_none();
                        let opening_id = session_id_for_task.clone();
                        let mcp_writer = writer_for_task.clone();
                        let mut opening = Box::pin(async move {
                            if let Some(session) = existing {
                                // Reading configuration for a no-replay attach is
                                // safe during a run; state() would take its lease.
                                session.configure_mcp(mcp_servers, &opening_id, std::path::Path::new(&cwd), &mcp_writer).await?;
                                session.state_unlocked().await.map(|state| (session, state))
                            } else {
                                let path = crate::history::saved_session(&opening_id)?;
                                let additional_directories =
                                    crate::history::load_additional_directories(&opening_id)?;
                                open_session(
                                    &opening_id,
                                    PathBuf::from(cwd),
                                    additional_directories,
                                    Some(&path),
                                    client_tools,
                                    mcp_servers,
                                    &mcp_writer,
                                )
                                .await
                            }
                        });
                        let (session, state) = tokio::select! {
                            biased;
                            _ = wait_cancelled(&mut cancelled) => {
                                return Err(RequestError::Cancelled);
                            }
                            result = &mut opening => result?,
                        };
                        child_owner.0 = newly_opened.then(|| session.clone());
                        if let Some(prompt) = system_prompt.as_ref() {
                            tokio::select! {
                                biased;
                                _ = wait_cancelled(&mut cancelled) => return Err(RequestError::Cancelled),
                                result = session.configure_system_prompt(prompt) => result?,
                            }
                        }
                        if *cancelled.borrow() {
                            return Err(RequestError::Cancelled);
                        }
                        let mut replay = if method_name == "session/load" || replay_requested {
                            Some(tokio::select! {
                                biased;
                                _ = wait_cancelled(&mut cancelled) => return Err(RequestError::Cancelled),
                                result = crate::history::replay(&session) => result?,
                            })
                        } else {
                            // A no-replay resume may observe an active session;
                            // it must not compete for the foreground run lease.
                            None
                        };
                        let mut result = json!({});
                        match config_options_for_capability(
                            &session,
                            &state,
                            writer_for_task.supports_boolean_config()
                                && session.supports_command("set_compaction_enabled"),
                        )
                        .await {
                            Ok(config) => result["configOptions"] = config["configOptions"].clone(),
                            Err(error) => diagln!(
                                "[rpi-acp] config options unavailable for {session_id_for_task}: {error}"
                            ),
                        }
                        if !writer_for_task.supports_v2() {
                            if let Some(modes) = session_modes(&session).await {
                                result["modes"] = modes;
                            }
                        }
                        if let Some(watcher) = watchers_for_task.lock().await.remove(&session_id_for_task) {
                            watcher.abort();
                        }
                        let watcher = spawn_durable_update_watcher(
                            session.clone(),
                            writer_for_task.clone(),
                            session_id_for_task.clone(),
                        );
                        sessions_for_task
                            .lock()
                            .await
                            .insert(session_id_for_task.clone(), session.clone());
                        child_owner.0.take();
                        if let Some(watcher) = watcher {
                            watchers_for_task.lock().await.insert(session_id_for_task.clone(), watcher);
                        }
                        if let Some((_read, spool)) = replay.as_mut() {
                            while let Some(messages) = spool.pop().await? {
                                for message in messages {
                                    for update in crate::history::message_updates(&message) {
                                        if *cancelled.borrow() { return Err(RequestError::Cancelled); }
                                        let update = if writer_for_task.supports_v2() {
                                            crate::v2::map_session_update(update)
                                        } else { update };
                                        tokio::select! {
                                            biased;
                                            _ = wait_cancelled(&mut cancelled) => return Err(RequestError::Cancelled),
                                            _ = writer_for_task.notify("session/update", json!({"sessionId":session_id_for_task,"update":update})) => {},
                                        }
                                    }
                                }
                            }
                        }
                        let commands = prepare_session_commands(&session, &writer_for_task, &session_id_for_task).await;
                        if writer_for_task.supports_v2() && method_name == "session/resume" {
                            // v2 resume is a synchronization boundary: report the
                            // current foreground state before acknowledging it.
                            let snapshot = session.runtime_snapshot().await;
                            let foreground_state = match snapshot.status.as_str() {
                                "running" | "awaiting_input" => "running",
                                _ => "idle",
                            };
                            writer_for_task
                                .notify(
                                    "session/update",
                                    json!({"sessionId":session_id_for_task,"update":{"sessionUpdate":"state_update","state":foreground_state}}),
                                )
                                .await;
                        }
                        if *cancelled.borrow() {
                            stop_live_session(
                                &session_id_for_task,
                                &sessions_for_task,
                                &watchers_for_task,
                                &prompt_tasks_for_task,
                                &request_tasks_for_task,
                            )
                            .await;
                            return Err(RequestError::Cancelled);
                        }
                        Ok(RequestReply { result, commands })
                    },
                )
                .await;
                if !accepted {
                    writer
                        .respond_error(&id, -32600, "duplicate request id")
                        .await;
                }
            }
            (Some(id), "session/set_mode") => {
                let params = match request_params(&params, "session/set_mode") {
                    Ok(params) => params,
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let session_id = match required_string(params, "sessionId") {
                    Ok(value) => value.to_owned(),
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let mode_id = match required_string(params, "modeId") {
                    Ok(value) => value.to_owned(),
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let session = sessions.lock().await.get(&session_id).cloned();
                let Some(session) = session else {
                    writer.respond_error(&id, -32602, "unknown sessionId").await;
                    continue;
                };
                let update_writer = writer.clone();
                let accepted = spawn_request(
                    &request_tasks,
                    &prompt_tasks,
                    id.clone(),
                    writer.clone(),
                    move |_cancelled| async move {
                        let modes = set_session_mode(&session, &mode_id).await?;
                        notify_current_mode(&update_writer, &session_id, &modes).await;
                        // Legacy clients still receive current_mode_update;
                        // configOptions-first clients must see the same change.
                        let state = session.state_unlocked().await?;
                        match config_options_for_capability(
                            &session, &state, update_writer.supports_boolean_config()
                                && session.supports_command("set_compaction_enabled"),
                        ).await {
                            Ok(config) => notify_config_options(&update_writer, &session_id, &config).await,
                            Err(error) => diagln!("[rpi-acp] mode config refresh unavailable: {error}"),
                        }
                        Ok(json!({}))
                    },
                )
                .await;
                if !accepted {
                    writer
                        .respond_error(&id, -32600, "duplicate request id")
                        .await;
                }
            }
            (Some(id), "session/set_config_option") => {
                let params = match request_params(&params, "session/set_config_option") {
                    Ok(params) => params,
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let session_id = match required_string(params, "sessionId") {
                    Ok(value) => value.to_owned(),
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let config_id = match required_string(params, "configId") {
                    Ok(value) => value.to_owned(),
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let value = params.get("value").cloned().unwrap_or(Value::Null);
                let value_type = params
                    .get("type")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let session = sessions.lock().await.get(&session_id).cloned();
                let Some(session) = session else {
                    writer.respond_error(&id, -32602, "unknown sessionId").await;
                    continue;
                };
                let boolean_config = writer.supports_boolean_config()
                    && session.supports_command("set_compaction_enabled");
                let update_writer = writer.clone();
                let accepted = spawn_request(
                    &request_tasks,
                    &prompt_tasks,
                    id.clone(),
                    writer.clone(),
                    move |_cancelled| async move {
                        let mut changed_mode = None;
                        match config_id.as_str() {
                            "model" => match value.as_str() {
                                Some(selected) => {
                                    let (provider, model_id) = split_model_value(selected);
                                    session.set_model(model_id, provider).await?;
                                }
                                None => {
                                    return Err(RequestError::InvalidInput(
                                        "model value must be a string".into(),
                                    ))
                                }
                            },
                            "thinking" => match value.as_str() {
                                Some(level) => session.set_thinking_level(level).await?,
                                None => {
                                    return Err(RequestError::InvalidInput(
                                        "thinking value must be a string".into(),
                                    ))
                                }
                            },
                            "mode" => {
                                let mode_id = value.as_str().filter(|value| !value.is_empty())
                                    .filter(|_| value_type.as_deref().is_none_or(|kind| kind == "select"))
                                    .ok_or_else(|| RequestError::InvalidInput("mode value must be a select string".into()))?;
                                changed_mode = Some(set_session_mode(&session, mode_id).await?);
                            },
                            "auto_compaction" => {
                                if !boolean_config {
                                    return Err(RequestError::InvalidInput(
                                        "boolean config options were not negotiated".into(),
                                    ));
                                }
                                let enabled = if value_type.as_deref() == Some("boolean") {
                                    value.as_bool()
                                } else {
                                    None
                                }
                                .ok_or_else(|| {
                                    RequestError::InvalidInput(
                                        "boolean config value must use top-level {type: boolean, value: boolean}".into(),
                                    )
                                })?;
                                session.set_compaction_enabled(enabled).await?;
                            },
                            other => {
                                return Err(RequestError::InvalidInput(format!(
                                    "unknown config option: {other}"
                                )))
                            }
                        }
                        let state = session.state_unlocked().await?;
                        let config = config_options_for_capability(
                            &session,
                            &state,
                            boolean_config,
                        )
                        .await
                        .map_err(RequestError::Internal)?;
                        if let Some(modes) = changed_mode {
                            notify_current_mode(&update_writer, &session_id, &modes).await;
                        }
                        notify_config_options(&update_writer, &session_id, &config).await;
                        Ok(config)
                    },
                )
                .await;
                if !accepted {
                    writer
                        .respond_error(&id, -32600, "duplicate request id")
                        .await;
                }
            }
            (Some(id), method @ ("session/close" | "session/delete")) => {
                let params = match request_params(&params, method) {
                    Ok(params) => params,
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let session_id = match required_string(params, "sessionId") {
                    Ok(value) => value.to_owned(),
                    Err(error) => {
                        writer.respond_error(&id, -32602, &error).await;
                        continue;
                    }
                };
                let is_close = method == "session/close";
                let opening_lock = {
                    let mut locks = opening_locks.lock().await;
                    locks.retain(|_, lock| lock.strong_count() > 0);
                    let lock = locks.get(&session_id).and_then(std::sync::Weak::upgrade)
                        .unwrap_or_else(|| Arc::new(Mutex::new(())));
                    locks.insert(session_id.clone(), Arc::downgrade(&lock));
                    lock
                };
                let sessions_for_task = sessions.clone();
                let watchers_for_task = durable_watchers.clone();
                let prompt_tasks_for_task = prompt_tasks.clone();
                let request_tasks_for_task = request_tasks.clone();
                let session_id_for_task = session_id.clone();
                let accepted = spawn_request(
                    &request_tasks,
                    &prompt_tasks,
                    id.clone(),
                    writer.clone(),
                    move |_cancelled| async move {
                        // Serialize removal with load/resume's pre-insertion work.
                        let _opening_guard = opening_lock.lock().await;
                        let live = stop_live_session(
                            &session_id_for_task,
                            &sessions_for_task,
                            &watchers_for_task,
                            &prompt_tasks_for_task,
                            &request_tasks_for_task,
                        )
                        .await;
                        if is_close {
                            // Close deliberately preserves the native transcript so a
                            // later load/resume can reopen it. Repeating close is safe.
                            let saved = crate::history::saved_session(&session_id_for_task).is_ok();
                            if live || saved {
                                return Ok(json!({}));
                            }
                            return Err(RequestError::InvalidInput("unknown sessionId".into()));
                        }
                        // Delete shares the same graceful stop path, then removes the
                        // persisted source of truth. It is idempotent for valid UUIDs.
                        let directory = crate::history::session_directory(&session_id_for_task)?;
                        match std::fs::remove_dir_all(&directory) {
                            Ok(()) => Ok(json!({})),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                Ok(json!({}))
                            }
                            Err(error) => {
                                Err(RequestError::Internal(format!("delete session: {error}")))
                            }
                        }
                    },
                )
                .await;
                if !accepted {
                    writer
                        .respond_error(&id, -32600, "duplicate request id")
                        .await;
                }
            }
            (id, "$/cancel_request") => {
                let request_id = match params
                    .as_object()
                    .and_then(|params| params.get("requestId"))
                    .filter(|id| valid_rpc_id(id))
                    .cloned()
                {
                    Some(request_id) => request_id,
                    None => {
                        if let Some(id) = id {
                            writer
                                .respond_error(&id, -32602, "requestId must be a string or number")
                                .await;
                        }
                        continue;
                    }
                };
                let key = request_key(&request_id);
                let _ = cancel_request_task(&request_tasks, &key).await;
                let prompt_session = {
                    prompt_tasks
                        .lock()
                        .await
                        .get(&key)
                        .map(|task| task.session.clone())
                };
                if let Some(session) = prompt_session {
                    let _ = session.abort().await;
                }
                if let Some(id) = id {
                    writer.respond(&id, json!({})).await;
                }
            }
            (id, "session/cancel") => {
                let session_id = match params
                    .as_object()
                    .and_then(|params| params.get("sessionId"))
                    .and_then(Value::as_str)
                    .filter(|id| !id.trim().is_empty())
                {
                    Some(session_id) => session_id.to_owned(),
                    None => {
                        if let Some(id) = id {
                            writer
                                .respond_error(&id, -32602, "sessionId must be a non-empty string")
                                .await;
                        }
                        continue;
                    }
                };
                let sessions_session = sessions.lock().await.get(&session_id).cloned();
                let mut prompt_sessions: Vec<_> = prompt_tasks
                    .lock()
                    .await
                    .values()
                    .filter(|task| task.session_id == session_id)
                    .map(|task| task.session.clone())
                    .collect();
                if let Some(session) = sessions_session {
                    prompt_sessions.push(session);
                }
                // A live prompt is also present in the session registry. Send
                // one abort per actual child, not one per registry reference.
                let mut aborted: Vec<Arc<Session>> = Vec::new();
                for session in prompt_sessions {
                    if !aborted.iter().any(|other| Arc::ptr_eq(other, &session)) {
                        let _ = session.abort().await;
                        aborted.push(session);
                    }
                }
                if let Some(id) = id {
                    writer.respond(&id, json!({})).await;
                }
            }
            (Some(id), other) => {
                writer
                    .respond_error(&id, -32601, &format!("method not found: {other}"))
                    .await
            }
            (None, _) => {}
        }
    }

    // Input EOF is not output EOF. Wake unavailable replies, cancel native
    // owners, and keep forwarding their teardown until authoritative settlement.
    writer.close_input();
    stop_all_prompt_tasks(&prompt_tasks, &request_tasks).await;
    wait_request_tasks(&request_tasks).await;
    // Client closed stdin or host requested shutdown.
    let watchers: Vec<_> = {
        durable_watchers
            .lock()
            .await
            .drain()
            .map(|(_, watcher)| watcher)
            .collect()
    };
    for watcher in watchers {
        watcher.abort();
    }
    let sessions: Vec<_> = {
        sessions
            .lock()
            .await
            .drain()
            .map(|(_, session)| session)
            .collect()
    };
    for session in sessions {
        kill_session(session).await;
    }
    writer.join_oauth_cleanup().await;
    writer.close();
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
#[cfg(test)]
fn config_options(models: &Value, thinking: &Value, state: &Value) -> Value {
    config_options_with_boolean(models, thinking, state, false)
}

fn config_options_with_boolean(
    models: &Value,
    thinking: &Value,
    state: &Value,
    boolean_capability: bool,
) -> Value {
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
    if boolean_capability {
        if let Some(enabled) = state.get("compactionEnabled").and_then(Value::as_bool) {
            options.push(json!({
                "id": "auto_compaction",
                "name": "Automatic compaction",
                "description": "Allow native rpi to compact the context when it reaches its budget",
                "category": "other",
                "type": "boolean",
                "currentValue": enabled,
            }));
        }
    }
    json!({ "configOptions": options })
}

/// Project only native-advertised, internally consistent modes. Never invent a
/// UI-only mode: the native setter owns tool policy and persistence.
fn mode_config_option(modes: &Value) -> Option<Value> {
    let current = modes.get("currentModeId")?.as_str()?;
    let mut options = Vec::new();
    for mode in modes.get("availableModes")?.as_array()? {
        let Some(id) = mode.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) else { continue };
        if options.iter().any(|option: &Value| option["value"].as_str() == Some(id)) { continue; }
        let name = mode.get("name").and_then(Value::as_str).filter(|name| !name.is_empty()).unwrap_or(id);
        let mut option = json!({"value":id,"name":name});
        if let Some(description) = mode.get("description").and_then(Value::as_str) {
            option["description"] = json!(description);
        }
        options.push(option);
    }
    if !options.iter().any(|option| option["value"].as_str() == Some(current)) { return None; }
    Some(json!({"id":"mode","name":"Mode","category":"other","type":"select",
        "currentValue":current,"options":options}))
}

async fn session_modes(session: &Arc<Session>) -> Option<Value> {
    if !session.supports_command("get_modes") || !session.supports_command("set_mode") {
        return None;
    }
    match session.modes().await {
        Ok(modes) if mode_config_option(&modes).is_some() => Some(modes),
        Ok(_) => None,
        Err(error) => {
            diagln!("[rpi-acp] mode discovery unavailable: {error}");
            None
        }
    }
}

async fn set_session_mode(session: &Arc<Session>, mode_id: &str) -> Result<Value, RequestError> {
    if !session.supports_command("get_modes") || !session.supports_command("set_mode") {
        return Err(RequestError::InvalidInput("native host does not support mode configuration".into()));
    }
    let modes = session.modes().await?;
    let available = mode_config_option(&modes).is_some_and(|option|
        option["options"].as_array().is_some_and(|options|
            options.iter().any(|option| option["value"].as_str() == Some(mode_id))));
    if !available { return Err(RequestError::InvalidInput(format!("unknown mode: {mode_id}"))); }
    session.set_mode(mode_id).await.map_err(RequestError::Internal)
}

async fn notify_current_mode(writer: &Arc<Writer>, session_id: &str, modes: &Value) {
    if !writer.supports_v2() {
        writer.notify("session/update", json!({"sessionId":session_id,"update":{
            "sessionUpdate":"current_mode_update","currentModeId":modes["currentModeId"]
        }})).await;
    }
}

async fn notify_config_options(writer: &Arc<Writer>, session_id: &str, config: &Value) {
    writer.notify("session/update", json!({"sessionId":session_id,"update":{
        "sessionUpdate":"config_option_update","configOptions":config["configOptions"]
    }})).await;
}

fn session_commands(native_commands: &Value, compact: bool) -> Vec<Value> {
    let mut commands = native_commands.get("commands").and_then(Value::as_array).cloned().unwrap_or_default();
    if compact {
        // v1's local handler takes precedence over an extension named compact.
        commands.retain(|command| command.get("name").and_then(Value::as_str)
            .is_none_or(|name| name.trim_start_matches('/') != "compact"));
        commands.insert(0, json!({"name":"compact","description":"Compact the conversation context",
            "input":{"hint":"Optional summary instructions"}}));
    }
    commands
}

async fn prepare_session_commands(session: &Arc<Session>, writer: &Arc<Writer>, session_id: &str) -> Option<Value> {
    // v2 prompts require canonical native insertion; do not advertise the v1
    // compact shortcut there. Extension commands keep their existing opt-in.
    let compact = !writer.supports_v2() && session.supports_command("compact");
    let extensions = std::env::var_os("RPI_EXTENSIONS_DIR").is_some()
        && session.supports_command("get_commands") && session.supports_command("execute_command");
    if !compact && !extensions { return None; }
    let commands = if extensions {
        match session.available_commands().await {
            Ok(commands) => commands,
            Err(error) => {
                diagln!("[rpi-acp] command discovery unavailable: {error}");
                if !compact { return None; }
                json!({})
            }
        }
    } else { json!({}) };
    let update = json!({"sessionUpdate":"available_commands_update",
        "availableCommands":session_commands(&commands, compact)});
    let update = if writer.supports_v2() { crate::v2::map_session_update(update) } else { update };
    Some(json!({"sessionId":session_id,"update":update}))
}

async fn config_options_for_capability(
    session: &Arc<Session>,
    state: &Value,
    boolean_capability: bool,
) -> Result<Value, String> {
    let models = session.available_models().await?;
    // Model switching must remain available if an older host lacks this optional query.
    let thinking = match session.available_thinking_levels().await {
        Ok(levels) => levels,
        Err(error) => {
            diagln!("[rpi-acp] thinking-level discovery unavailable: {error}");
            json!([])
        }
    };
    let mut config = config_options_with_boolean(
        &models,
        &thinking,
        state,
        boolean_capability,
    );
    if let Some(modes) = session_modes(session).await {
        // session_modes has already checked the native descriptor shape.
        config["configOptions"].as_array_mut().unwrap().push(mode_config_option(&modes).unwrap());
    }
    Ok(config)
}

async fn open_session(
    id: &str,
    cwd: PathBuf,
    additional_directories: Vec<PathBuf>,
    saved: Option<&std::path::Path>,
    client_tools: rpi_extcore::host::ClientToolOptions,
    mcp_servers: crate::mcp::Servers,
    writer: &Arc<Writer>,
) -> Result<(Arc<Session>, Value), String> {
    if !cwd.is_absolute() || !cwd.is_dir() {
        return Err("cwd must be an existing absolute directory".into());
    }
    let mut owner = OpeningSession::reserve(writer)?;
    let session = Session::spawn_with_features(id.to_owned(), cwd.clone(), additional_directories, saved, client_tools, Some(writer.clone())).await?;
    owner.0 = Some(session.clone());
    // Reconcile the provisional root with native state BEFORE discovery can
    // request URL elicitation. Never route OAuth to a fallback or child ID.
    match session.state().await {
        Ok(state) if state["sessionId"] == id => {
            session.configure_mcp(mcp_servers, id, &cwd, writer).await?;
            owner.0.take();
            Ok((session, state))
        },
        result => {
            kill_session(session).await;
            owner.0.take();
            Err(result
                .err()
                .unwrap_or_else(|| "backend opened a different sessionId".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("../tests/fixtures/eof_cleanup_writer.rs");

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
    fn additional_directories_require_existing_absolute_dirs_and_deduplicate() {
        let directory =
            std::env::temp_dir().join(format!("rpi-acp-additional-root-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).expect("temporary additional root");
        let requested = json!({
            "additionalDirectories": [
                directory.to_string_lossy(),
                directory.to_string_lossy()
            ]
        });
        let roots = parse_additional_directories(&requested).expect("valid roots");
        assert_eq!(roots, vec![std::fs::canonicalize(&directory).unwrap()]);
        assert!(parse_additional_directories(&json!({
            "additionalDirectories": [directory.join("missing").to_string_lossy()]
        }))
        .is_err());
        let _ = std::fs::remove_dir_all(directory);
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

    #[test]
    fn native_modes_project_to_a_select_without_inventing_values() {
        let modes = json!({"currentModeId":"plan", "availableModes":[
            {"id":"default","name":"Default"}, {"id":"plan","name":"Plan","description":"Read only"},
            {"id":"plan","name":"Duplicate"}, {"id":""}, {}
        ]});
        let option = mode_config_option(&modes).unwrap();
        assert_eq!(option["id"], "mode");
        assert_eq!(option["currentValue"], "plan");
        assert_eq!(option["options"], json!([
            {"value":"default","name":"Default"},
            {"value":"plan","name":"Plan","description":"Read only"}
        ]));
        for modes in [json!({}), json!({"currentModeId":"missing","availableModes":[{"id":"plan"}]}),
            json!({"currentModeId":"plan","availableModes":[]})] {
            assert!(mode_config_option(&modes).is_none());
        }
        let mut config = json!({"configOptions":[option]});
        crate::v2::map_config_options(&mut config);
        assert_eq!(config["configOptions"][0]["configId"], "mode");
        assert_eq!(config["configOptions"][0]["currentValue"], "plan");
    }

    #[test]
    fn compact_is_advertised_without_plugins_and_cannot_be_shadowed() {
        assert_eq!(session_commands(&json!({}), false), Vec::<Value>::new());
        let commands = session_commands(&json!({}), true);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0]["name"], "compact");
        assert!(commands[0]["input"]["hint"].is_string());
        let native = json!({"commands":[{"name":"/compact","description":"shadow"},
            {"name":"compact","description":"duplicate"},{"name":"example","description":"extension"}]});
        let merged = session_commands(&native, true);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[1]["name"], "example");
        // Without the supported v1 shortcut, preserve the extension contract.
        assert_eq!(session_commands(&native, false), native["commands"].as_array().unwrap().clone());
    }

    #[tokio::test]
    async fn config_writer_maps_v2_controls_only_and_preserves_v1_bytes() {
        for version in [1, 2] {
            let (output, peer) = tokio::io::duplex(16384);
            let writer = Writer::with_output(output);
            writer.set_protocol_version(version);
            // Boolean is schema-supported, but this test does not enable its
            // runtime capability gate. Exercise a future enabled payload too.
            let mut config = config_options_with_boolean(
                &json!([{"id":"m","provider":"a"},{"id":"m","provider":"b"}]),
                &json!(["off", "high"]),
                &json!({"modelId":"m","provider":"a","thinkingLevel":"high",
                    "compactionEnabled":true}),
                true,
            );
            let opaque = json!({"id":"opaque", "configOptions":[{"id":"not-a-control"}]});
            config["_meta"] = opaque.clone();
            config["data"] = opaque.clone();
            config["configOptions"][0]["_meta"] = opaque.clone();
            let update = json!({"sessionUpdate":"config_option_update",
                "configOptions":config["configOptions"], "_meta":opaque});
            writer.respond(&json!(7), config.clone()).await;
            writer.notify("session/update", json!({"sessionId":"s","update":update})).await;
            let tool = json!({"sessionId":"s","update":{"sessionUpdate":"tool_call_update",
                "toolCallId":"t","rawOutput":opaque}});
            writer.notify("session/update", tool.clone()).await;
            let mut lines = BufReader::new(peer).lines();
            let response_line = lines.next_line().await.unwrap().unwrap();
            let notification_line = lines.next_line().await.unwrap().unwrap();
            if version == 1 {
                assert_eq!(response_line, json!({"jsonrpc":"2.0","id":7,"result":config}).to_string());
                assert_eq!(notification_line, json!({"jsonrpc":"2.0","method":"session/update",
                    "params":{"sessionId":"s","update":update}}).to_string());
            } else {
                let response: Value = serde_json::from_str(&response_line).unwrap();
                let notification: Value = serde_json::from_str(&notification_line).unwrap();
                for mapped in [&response["result"], &notification["params"]["update"]] {
                    for (index, id) in ["model", "thinking", "auto_compaction"].iter().enumerate() {
                        assert_eq!(mapped["configOptions"][index]["configId"], *id);
                        assert!(mapped["configOptions"][index].get("id").is_none());
                        let mut expected = config["configOptions"][index].clone();
                        expected.as_object_mut().unwrap().remove("id");
                        expected["configId"] = json!(id);
                        if index == 0 {
                            for group in expected["options"].as_array_mut().unwrap() {
                                let id = group.as_object_mut().unwrap().remove("group").unwrap();
                                group["groupId"] = id;
                            }
                        }
                        assert_eq!(mapped["configOptions"][index], expected);
                    }
                    assert_eq!(mapped["_meta"], opaque);
                }
                assert_eq!(response["result"]["data"], opaque);
            }
            let tool_line = lines.next_line().await.unwrap().unwrap();
            assert_eq!(tool_line, json!({"jsonrpc":"2.0","method":"session/update","params":tool}).to_string());
        }
    }

    #[tokio::test]
    async fn permission_writer_maps_only_negotiated_v2_and_preserves_v1_bytes() {
        for version in [1, 2] {
            let (output, peer) = tokio::io::duplex(4096);
            let writer = Arc::new(Writer::with_output(output));
            writer.set_protocol_version(version);
            let params = json!({"sessionId":"s", "toolCall":{"toolCallId":"t", "title":"Real question?"},
                "options":[{"optionId":"0","name":"Allow","kind":"allow_once"}],
                "_meta":{"pi-rust/dialog":{"requestId":"q"}}});
            let original = params.clone();
            let request = tokio::spawn({
                let writer = writer.clone();
                async move { writer.request("session/request_permission", params).await }
            });
            let mut lines = BufReader::new(peer).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            if version == 1 {
                assert_eq!(line, serde_json::to_string(&json!({"jsonrpc":"2.0", "id":1,
                    "method":"session/request_permission", "params":original})).unwrap());
            } else {
                let frame: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(frame["params"]["title"], "Real question?");
                assert_eq!(frame["params"]["subject"]["toolCall"]["toolCallId"], "t");
                assert_eq!(frame["params"]["_meta"], original["_meta"]);
                assert!(frame["params"].get("toolCall").is_none());
            }
            writer.resolve(1, json!({"outcome":{"outcome":"cancelled"}})).await;
            request.await.unwrap();
        }
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

    #[tokio::test]
    async fn cancellable_client_request_sends_json_rpc_cancel_and_cleans_pending() {
        let (output, peer) = tokio::io::duplex(2048);
        let writer = Arc::new(Writer::with_output(output));
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        let request = tokio::spawn({
            let writer = writer.clone();
            async move {
                writer
                    .request_cancellable(
                        "fs/read_text_file",
                        json!({"sessionId":"s","path":"x"}),
                        &mut cancel_rx,
                    )
                    .await
            }
        });
        let mut lines = BufReader::new(peer).lines();
        let first = lines.next_line().await.unwrap().unwrap();
        assert!(first.contains("fs/read_text_file"));
        assert_eq!(writer.pending_count(), 1);
        cancel_tx.send(true).unwrap();
        let cancel = lines.next_line().await.unwrap().unwrap();
        assert!(cancel.contains("$/cancel_request"));
        assert_eq!(request.await.unwrap(), Value::Null);
        assert_eq!(writer.pending_count(), 0);
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
            result["agentCapabilities"]["sessionCapabilities"]["additionalDirectories"],
            json!({})
        );
        assert_eq!(
            result["agentCapabilities"]["promptCapabilities"]["image"],
            true
        );
        assert_eq!(
            result["agentCapabilities"]["promptCapabilities"]["audio"],
            true
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
    fn standard_elicitation_requires_advertised_mode() {
        let writer = Writer::new();
        for params in [json!({}), json!({"clientCapabilities":{"elicitation":{}}})] {
            writer.negotiate_questions(&params);
            assert!(!writer.supports_elicitation(), "{params}");
            assert!(!writer.supports_elicitation_url(), "{params}");
        }
        writer.negotiate_questions(&json!({
            "clientCapabilities":{"elicitation":{"url":{}}}
        }));
        assert!(writer.supports_elicitation());
        assert!(!writer.supports_elicitation_form());
        assert!(writer.supports_elicitation_url());
        writer.negotiate_questions(&json!({
            "clientCapabilities":{"elicitation":{"form":{}}}
        }));
        assert!(writer.supports_elicitation());
        assert!(writer.supports_elicitation_form());
        assert!(!writer.supports_elicitation_url());
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
    fn media_content_blocks_parse() {
        let image: ContentBlock =
            serde_json::from_value(json!({"type":"image","data":"AAEC","mimeType":"image/png"}))
                .unwrap();
        assert!(matches!(image, ContentBlock::Image { .. }));
        let audio: ContentBlock =
            serde_json::from_value(json!({"type":"audio","data":"AAEC","mimeType":"audio/wav"}))
                .unwrap();
        assert!(matches!(audio, ContentBlock::Audio { .. }));
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

    #[test]
    fn json_rpc_envelope_rejects_malformed_requests_without_losing_notifications() {
        let parsed = parse_rpc_message(json!({"jsonrpc":"1.0","id":1,"method":"x"}));
        assert_eq!(parsed.unwrap_err()["error"]["code"], -32600);
        let parsed = parse_rpc_message(json!({"jsonrpc":"2.0","method":"x","params":1}));
        assert!(matches!(
            parsed.unwrap(),
            RpcMessage::Request { id: None, .. }
        ));
        assert!(request_params(&json!(1), "x").is_err());
        let request = parse_rpc_message(json!({"jsonrpc":"2.0","id":1,"method":"x"})).unwrap();
        assert!(
            matches!(request, RpcMessage::Request { id: Some(_), method, .. } if method == "x")
        );
        let response = parse_rpc_message(json!({"jsonrpc":"2.0","id":1,"result":{}})).unwrap();
        assert!(matches!(response, RpcMessage::Response { id, .. } if id == json!(1)));
        let null_request =
            parse_rpc_message(json!({"jsonrpc":"2.0","id":null,"method":"x"})).unwrap();
        assert!(
            matches!(null_request, RpcMessage::Request { id: Some(id), .. } if id == Value::Null)
        );
    }

    #[test]
    fn prompt_validation_does_not_silently_drop_bad_or_unsupported_blocks() {
        let error = prompt_params(&json!({
            "sessionId":"s",
            "prompt":[{"type":"text","text":"ok"},{"type":"not-a-block"}]
        }))
        .unwrap_err();
        assert!(error.contains("prompt[1]"));
        let error = prompt_params(&json!({"sessionId":"s","prompt":[]})).unwrap();
        assert!(error.is_empty());
    }

    #[test]
    fn initialize_selects_explicit_v2_for_v2_clients() {
        let request = parse_rpc_message(json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize",
            "params":{"protocolVersion":2,"info":{"name":"client","version":"1"},"capabilities":{}}
        }))
        .unwrap();
        assert!(matches!(request, RpcMessage::Request { .. }));
        assert_eq!(crate::v2::PROTOCOL_VERSION, 2);
        assert_eq!(
            crate::v2::initialize_result(&initialize_result())["protocolVersion"],
            2
        );
    }
}

#[cfg(test)]
mod child_interaction_tests {
    use super::*;

    #[tokio::test]
    async fn trusted_source_is_root_or_announced_scoped_child_only() {
        let writer = Writer::with_output(tokio::io::sink());
        writer.negotiate_subagents(&json!({"clientCapabilities":{"subagents":{}}}));
        for root in ["a", "b"] {
            assert_eq!(writer.interaction_target(root, &json!({})).await.unwrap(), root);
            assert_eq!(writer.interaction_target(root, &json!({"sessionId":root})).await.unwrap(), root);
            writer.register_child(format!("child:1:{root}:worker"));
            assert_eq!(writer.interaction_target(root, &json!({"sessionId":"worker", "params":{"sessionId":"worker"}})).await.unwrap(), format!("child:1:{root}:worker"));
        }
        for event in [json!({"sessionId":null}), json!({"sessionId":""}),
            json!({"sessionId":"worker","params":{"sessionId":"a"}}),
            json!({"params":{"sessionId":"worker"}})] {
            assert!(writer.interaction_target("a", &event).await.is_err(), "{event}");
        }
        writer.close();
        assert!(writer.interaction_target("a", &json!({"sessionId":"worker"})).await.is_err());
    }

    #[tokio::test]
    async fn publication_race_waits_and_other_roots_do_not_authorize() {
        let writer = Arc::new(Writer::with_output(tokio::io::sink()));
        writer.negotiate_subagents(&json!({"protocolVersion":2}));
        writer.register_child("child:1:b:worker".into());
        let event = json!({"sessionId":"worker"});
        let resolve = writer.interaction_target("a", &event);
        let publish = async {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            writer.notify("session/update", json!({"sessionId":"a","update":{"sessionUpdate":"subagent_update","sessionId":"child:1:a:worker","state":{"state":"working"}}})).await;
            writer.register_child("child:1:a:worker".into());
        };
        let (target, _) = tokio::join!(resolve, publish);
        assert_eq!(target.unwrap(), "child:1:a:worker");
        let start = tokio::time::Instant::now();
        assert!(writer.interaction_target("c", &json!({"sessionId":"worker"})).await.unwrap_err().contains("unannounced"));
        assert!(start.elapsed() < std::time::Duration::from_secs(3));
        let legacy = Writer::with_output(tokio::io::sink());
        legacy.register_child("child:1:a:worker".into());
        assert!(legacy.interaction_target("a", &json!({"sessionId":"worker"})).await.unwrap_err().contains("negotiated"));
    }
}

#[cfg(test)]
mod mcp_oauth_writer_tests {
    use super::*;
    use rpi_extcore::mcp::{oauth::*, AuthError, McpControl};
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[tokio::test]
    async fn oauth_ui_real_root_actions_and_owned_control_cleanup() {
        for action in ["accept", "decline", "cancel", "invalid", "control", "timeout", "close"] {
            let (output, input) = tokio::io::duplex(8192);
            let writer = Arc::new(Writer::with_output(output));
            writer.negotiate_questions(&json!({"clientCapabilities":{"elicitation":{"url":{}}}}));
            let mut lines = BufReader::new(input).lines();
            let control = McpControl::default();
            let c = control.clone();
            let w = writer.clone();
            let task = tokio::spawn(async move {
                w.request_mcp_oauth(json!({"sessionId":"native-root","mode":"url","url":"https://issuer.example/authorize?state=private","elicitationId":"not-request-id"}), &c,
                    std::time::Instant::now() + std::time::Duration::from_millis(if action == "timeout" { 80 } else { 2000 })).await
            });
            let frame: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(frame["params"]["sessionId"], "native-root");
            assert_eq!(frame["method"], "elicitation/create");
            let id = frame["id"].as_u64().unwrap();
            match action {
                "control" => control.cancel(),
                "close" => writer.close_input(),
                "timeout" => {},
                _ => { writer.checked_pending.lock().unwrap().remove(&id).unwrap().send(Ok(json!({"action":action}))).unwrap(); },
            }
            let result = task.await.unwrap();
            match action {
                "control" | "close" | "timeout" => {
                    assert_eq!(result.unwrap_err(), if action == "timeout" { AuthError::Timeout } else { AuthError::Cancelled });
                    let cancel: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                    assert_eq!(cancel["method"], "$/cancel_request");
                    assert_eq!(cancel["params"]["requestId"], id);
                },
                _ => assert_eq!(result.unwrap()["action"], action),
            }
            assert!(writer.checked_pending.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn standard_agent_auth_capabilities_unchanged_by_policy() {
        let before = initialize_result();
        let mut writer = Writer::with_output(tokio::io::sink());
        writer.mcp_oauth_policy = Err("fixed-policy-error");
        assert_eq!(initialize_result(), before);
        assert!(!writer.supports_elicitation_url());
        // Refer to the actual provider outcomes: acceptance is not a credential.
        assert_ne!(AuthorizationOutcome::Accepted, AuthorizationOutcome::Declined);
    }
}

#[cfg(test)]
mod history_cursor_tests {
    use super::*;

    #[test]
    fn remote_tools_require_explicit_version_specific_client_capabilities() {
        for params in [json!({}), json!({"clientCapabilities":{"fs":{"readTextFile":false},"terminal":{}}})] {
            assert!(!client_tool_options(&params, false).enabled());
        }
        let v1 = json!({"clientCapabilities":{"fs":{"readTextFile":true,"writeTextFile":true},"terminal":true}});
        let tools = client_tool_options(&v1, false);
        assert!(tools.read_text && tools.write_text && tools.terminal);
        assert!(!client_tool_options(&v1, true).enabled());
        let v2 = json!({"clientCapabilities":{"_meta":{"pi-rust/client-tools":{"readText":true,"terminal":true}}}});
        let tools = client_tool_options(&v2, true);
        assert!(tools.read_text && tools.terminal && !tools.write_text);
        assert!(!client_tool_options(&v2, false).enabled());
    }

    #[test]
    fn pinned_v2_start_replays_and_unknown_or_malformed_cursor_rejects() {
        assert!(!resume_replay_requested(None).unwrap());
        assert!(!resume_replay_requested(Some(&Value::Null)).unwrap());
        assert!(resume_replay_requested(Some(&json!({"type":"start"}))).unwrap());
        assert!(resume_replay_requested(Some(&json!({"type":"start","_meta":{}}))).unwrap());
        for value in [json!("start"), json!({}), json!({"type":"future"}), json!({"type":"_unknown"}), json!({"type":false})] {
            assert!(resume_replay_requested(Some(&value)).is_err(), "{value}");
        }
    }
}

#[cfg(test)]
mod oauth_cleanup_regressions {
    use super::*;
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[tokio::test]
    async fn dropped_during_cancel_output_keeps_canonical_notification_owned() {
        let (output, input) = tokio::io::duplex(8192);
        let writer = Arc::new(Writer::with_output(output));
        let mut lines = BufReader::new(input).lines();
        let control = rpi_extcore::mcp::McpControl::default();
        let task = tokio::spawn({
            let writer = writer.clone(); let control = control.clone();
            async move { writer.request_mcp_oauth(json!({"sessionId":"root"}), &control,
                Instant::now() + Duration::from_secs(30)).await }
        });
        let frame: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let blocked_output = writer.out.lock().await;
        control.cancel();
        // Cancellation notification cannot obtain the output lock.
        tokio::task::yield_now().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(writer.oauth_cleanup.lock().unwrap().len(), 1);
        drop(blocked_output);
        writer.join_oauth_cleanup().await;
        let cancel: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(cancel["method"], "$/cancel_request");
        assert_eq!(cancel["params"]["requestId"], frame["id"]);
        assert_eq!(writer.cleanup_admission.available_permits(), 128);
    }

    #[tokio::test]
    async fn aborted_ui_admission_is_bounded_recovers_and_does_not_wait_live_sibling() {
        let writer = Arc::new(Writer::with_output(tokio::io::sink()));
        let sibling = tokio::spawn({
            let writer = writer.clone();
            async move { writer.request_mcp_oauth(json!({"sessionId":"sibling"}),
                &rpi_extcore::mcp::McpControl::default(), Instant::now() + Duration::from_secs(30)).await }
        });
        while writer.checked_pending.lock().unwrap().is_empty() { tokio::task::yield_now().await; }
        let sibling_id = *writer.checked_pending.lock().unwrap().keys().next().unwrap();
        for _ in 0..3 {
            let blocked_output = writer.out.lock().await;
            let mut tasks = Vec::new();
            for _ in 0..127 {
                let writer = writer.clone();
                tasks.push(tokio::spawn(async move { writer.request_mcp_oauth(json!({}),
                    &rpi_extcore::mcp::McpControl::default(), Instant::now() + Duration::from_secs(30)).await }));
            }
            while writer.cleanup_admission.available_permits() != 0 { tokio::task::yield_now().await; }
            assert!(writer.request_mcp_oauth(json!({}), &rpi_extcore::mcp::McpControl::default(),
                Instant::now() + Duration::from_secs(30)).await.is_err());
            for task in tasks { task.abort(); assert!(task.await.unwrap_err().is_cancelled()); }
            assert!(writer.oauth_cleanup.lock().unwrap().len() <= 128);
            drop(blocked_output);
            tokio::time::timeout(Duration::from_secs(2), writer.join_oauth_cleanup()).await.unwrap();
            assert!(!sibling.is_finished());
            assert_eq!(writer.cleanup_admission.available_permits(), 127);
        }
        writer.checked_pending.lock().unwrap().remove(&sibling_id).unwrap().send(Ok(json!({"action":"accept"}))).unwrap();
        assert_eq!(sibling.await.unwrap().unwrap()["action"], "accept");
        assert_eq!(writer.cleanup_admission.available_permits(), 128);
    }

    #[tokio::test]
    async fn cancelled_drain_keeps_live_handles_for_retry() {
        let writer = Arc::new(Writer::with_output(tokio::io::sink()));
        let (release, wait) = oneshot::channel();
        writer.track_cleanup(writer.reserve_cleanup().unwrap(), async move { let _ = wait.await; });
        let drain = tokio::spawn({ let writer = writer.clone(); async move { writer.join_oauth_cleanup().await } });
        tokio::task::yield_now().await;
        drain.abort();
        assert!(drain.await.unwrap_err().is_cancelled());
        assert_eq!(writer.oauth_cleanup.lock().unwrap().len(), 1);
        assert_eq!(writer.cleanup_admission.available_permits(), 127);
        release.send(()).unwrap();
        writer.join_oauth_cleanup().await;
        assert!(writer.oauth_cleanup.lock().unwrap().is_empty());
        assert_eq!(writer.cleanup_admission.available_permits(), 128);
    }

    #[tokio::test]
    async fn concurrent_error_boundaries_join_late_producer_cleanup() {
        let (output, input) = tokio::io::duplex(8192);
        let writer = Arc::new(Writer::with_output(output));
        let (release, wait) = oneshot::channel();
        let producer_permit = writer.reserve_cleanup().unwrap();
        let child_permit = writer.reserve_cleanup().unwrap();
        let (child_release, child_wait) = oneshot::channel();
        writer.track_cleanup(producer_permit, {
            let writer = writer.clone();
            async move {
                wait.await.unwrap();
                writer.track_cleanup(child_permit, async move { child_wait.await.unwrap(); });
            }
        });
        let mut boundaries = Vec::new();
        for id in [1, 2] {
            let writer = writer.clone();
            boundaries.push(tokio::spawn(async move { writer.respond_error(&json!(id), -32800, "cancelled").await }));
        }
        tokio::task::yield_now().await;
        release.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(boundaries.iter().all(|task| !task.is_finished()));
        child_release.send(()).unwrap();
        for boundary in boundaries { boundary.await.unwrap(); }
        let mut lines = BufReader::new(input).lines();
        for _ in 0..2 { assert!(lines.next_line().await.unwrap().unwrap().contains("cancelled")); }
        assert!(writer.oauth_cleanup.lock().unwrap().is_empty());
        assert_eq!(writer.cleanup_admission.available_permits(), 128);
    }
}
