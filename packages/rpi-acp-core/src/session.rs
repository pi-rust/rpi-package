//! Per-ACP-session driver: a child `rpi --mode rpc` process whose JSONL event
//! stream is translated into ACP `session/update` notifications.
//!
//! Beyond streamed text/thinking/tool calls, two richer updates are derived from
//! the child's events: file mutations (`edit`/`write`) become ACP `diff` content
//! on the `tool_call`, and a completed `plan_mode_complete` becomes an ACP
//! `plan` update (`translate_plan_event`).
//!
//! `rpi --mode rpc` is the host's supported headless agent loop (see
//! `docs` / `crates/rpi-cli/src/modes.rs`): commands go in on stdin as
//! `{"id":N,"type":"prompt"|"abort"|...}`, and one JSON object per line comes
//! back on stdout — fine-grained lifecycle events (`message_update`,
//! `tool_execution_start`, …) plus a final
//! `{"type":"response","id":N,"status":"ok","result":{…}}` per command.
//!
//! We spawn one child per ACP session with `current_dir = session/new.cwd`, so
//! the agent's tools run in the client's workspace.

use crate::diagln;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, watch, Mutex as TokioMutex};
use tokio::task::{AbortHandle, JoinSet};

use crate::protocol::{ContentBlock, Writer};

/// A live ACP session backed by one `rpi --mode rpc` child process.
pub struct Session {
    /// The child process. `kill`-ed on shutdown.
    child: TokioMutex<Option<Child>>,
    /// The child's command channel.
    stdin: TokioMutex<Box<dyn AsyncWrite + Unpin + Send>>,
    cancellation: watch::Sender<u64>,
    prompt_lock: TokioMutex<()>,
    /// Every line the child writes to stdout, fanned out to whichever prompt
    /// turn is currently subscribed. Buffered generously: a prompt turn can
    /// emit many deltas between two reads.
    events: broadcast::Sender<Value>,
    /// Per-session command id counter (matches `response.id`).
    next_id: AtomicU64,
}

impl Session {
    /// Spawn `rpi --mode rpc` rooted at `cwd`.
    pub fn spawn(id: String, cwd: PathBuf, saved: Option<&Path>) -> Result<Arc<Self>, String> {
        let executable = resolve_rpi_executable();

        let mut command = Command::new(&executable);
        command
            .arg("--mode")
            .arg("rpc")
            .current_dir(&cwd)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let directory = crate::history::session_directory(&id)?;
        command.arg("--session-dir").arg(&directory);
        if let Some(path) = saved {
            // An exact file restore fails if missing rather than creating an empty session.
            command.arg("--session").arg(path);
        } else {
            command.arg("--session-id").arg(&id);
        }
        // Ask the child to load the (optional) permission-gate extension so the
        // ACP client's approval UI is used for dangerous tools. Harmless when
        // the extension is not installed; `RPI_ACP_NO_PERMISSION_GATE=1` opts
        // out.
        if !env_flag("RPI_ACP_NO_PERMISSION_GATE") {
            command.arg("--permission-gate");
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("spawn `rpi --mode rpc`: {e}"))?;
        // One line per session: the most common Zed-side failure is this path
        // pointing at a binary that no longer exists (e.g. a cleaned target dir).
        diagln!(
            "[rpi-acp] session {id}: child pid {} = {} --mode rpc",
            child.id().unwrap_or(0),
            executable.display()
        );

        let stdin = child.stdin.take().ok_or("child stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("child stdout unavailable")?;
        let stderr = child.stderr.take();

        // Fan every stdout line out to subscribers. `broadcast` (not mpsc)
        // because the prompt turn for this session subscribes *before* sending
        // the command, so no event between subscribe and completion is lost.
        let (events, _) = broadcast::channel::<Value>(16_384);
        let tx = events.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(line) {
                    Ok(value) => {
                        let _ = tx.send(value);
                    }
                    Err(error) => diagln!("[rpi-acp] child produced invalid json: {error}"),
                }
            }
            // Terminal marker so a pending prompt can finish if the child dies.
            let _ = tx.send(json!({"type": "session_end"}));
        });

        if let Some(stderr) = stderr {
            let session = id.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    diagln!("[rpi-acp:{session}] {line}");
                }
            });
        }

        Ok(Arc::new(Self {
            child: TokioMutex::new(Some(child)),
            stdin: TokioMutex::new(Box::new(stdin)),
            cancellation: watch::channel(0).0,
            prompt_lock: TokioMutex::new(()),
            events,
            next_id: AtomicU64::new(1),
        }))
    }

    async fn send(&self, value: Value) -> Result<(), String> {
        let mut stdin = self.stdin.lock().await;
        let mut buf = value.to_string();
        buf.push('\n');
        stdin
            .write_all(buf.as_bytes())
            .await
            .map_err(|e| format!("write to child: {e}"))?;
        stdin
            .flush()
            .await
            .map_err(|e| format!("flush child stdin: {e}"))
    }

    /// Wait for backend startup and obtain the authoritative active branch.
    pub async fn state(&self) -> Result<Value, String> {
        let _prompt = self
            .prompt_lock
            .try_lock()
            .map_err(|_| "a prompt is already running")?;
        let mut events = self.events.subscribe();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.send(json!({"id": id, "type": "get_state"})).await?;
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let event = events
                    .recv()
                    .await
                    .map_err(|e| format!("backend disconnected: {e}"))?;
                if event["type"] == "session_end" {
                    return Err(
                        "rpi exited while opening the session; check the agent stderr log".into(),
                    );
                }
                if event["type"] == "response" && event["id"] == id {
                    if event["status"] != "ok" {
                        return Err(format!("open session: {}", event["error"]));
                    }
                    return Ok(event["result"].clone());
                }
            }
        })
        .await
        .map_err(|_| "timed out waiting for rpi session startup".to_string())?
    }

    /// Ask the child to abort the current run (`session/cancel`).
    pub async fn abort(&self) -> Result<(), String> {
        self.cancellation
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        self.send(json!({"type": "abort"})).await
    }

    /// Run one prompt turn to completion, streaming `session/update`
    /// notifications to the client. Returns the ACP `stopReason` or a run error.
    pub async fn prompt(
        self: &Arc<Self>,
        writer: &Arc<Writer>,
        acp_session_id: &str,
        blocks: &[ContentBlock],
    ) -> Result<&'static str, String> {
        let Ok(_prompt) = self.prompt_lock.try_lock() else {
            return Err("a prompt is already running".into());
        };
        let mut cancellation = self.cancellation.subscribe();
        let mut cancelled = false;
        let mut dialogs = DialogTasks::default();
        let mut text = StreamText::default();
        let mut stream_error = None;
        let mut rx = self.events.subscribe();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let content = prompt_text(blocks);
        self.send(json!({"id": id, "type": "prompt", "content": content}))
            .await?;

        let stop_reason = 'prompt: loop {
            let event = tokio::select! {
                biased;
                _ = cancellation.changed(), if !cancelled => {
                    cancelled = true;
                    dialogs.cancel_all(writer, acp_session_id).await;
                    continue;
                }
                result = dialogs.tasks.join_next(), if !dialogs.tasks.is_empty() => {
                    if let Some(Ok(id)) = result { dialogs.pending.remove(&id); }
                    continue;
                }
                event = rx.recv() => event,
            };
            match event {
                Ok(value) => {
                    if value.get("type").and_then(Value::as_str) == Some("response")
                        && value.get("id").and_then(Value::as_u64) == Some(id)
                    {
                        if value.get("status").and_then(Value::as_str) == Some("error") {
                            let message = value
                                .get("error")
                                .and_then(Value::as_str)
                                .unwrap_or("agent run failed");
                            break 'prompt if cancelled {
                                Ok("cancelled")
                            } else {
                                Err(message.into())
                            };
                        }
                        let outcome = value
                            .pointer("/result/outcome")
                            .and_then(Value::as_str)
                            .unwrap_or("completed");
                        if let Some(error) = stream_error {
                            break 'prompt Err(error);
                        }
                        if let Some(final_text) =
                            value.pointer("/result/finalText").and_then(Value::as_str)
                        {
                            match text.complete(final_text) {
                                Ok(Some(update)) => {
                                    writer
                                        .notify(
                                            "session/update",
                                            json!({"sessionId":acp_session_id,"update":update}),
                                        )
                                        .await
                                }
                                Ok(None) => {}
                                Err(error) => break 'prompt Err(error),
                            }
                        }
                        diagln!("[rpi-acp] session {acp_session_id}: prompt {id} outcome {outcome}, text bytes {}, repaired bytes {}", text.text.len(), text.repaired);
                        break 'prompt match outcome {
                            "aborted" => Ok("cancelled"),
                            "failed" => Err(text.error.clone().unwrap_or_else(|| "rpi run failed before completing the reply; check the agent stderr log".into())),
                            _ if text.stop_reason.as_deref() == Some("length") => Ok("max_tokens"),
                            _ => Ok("end_turn"),
                        };
                    }
                    if value.get("type").and_then(Value::as_str) == Some("session_end") {
                        break 'prompt Err("rpi session ended before the prompt completed".into());
                    }
                    // A plugin opened a UI dialog (e.g. a permission gate). Ask
                    // the ACP client, then answer the plugin.
                    if value.get("type").and_then(Value::as_str) == Some("ui_dialog_request") {
                        if cancelled {
                            if let Some(request_id) = value.get("requestId").and_then(Value::as_str)
                            {
                                let _ = self.send(json!({"type":"ui_dialog_response","requestId":request_id,"cancelled":true})).await;
                            }
                        } else {
                            dialogs.open(
                                self.clone(),
                                writer.clone(),
                                acp_session_id.to_owned(),
                                value,
                            );
                        }
                        continue;
                    }
                    if value.get("type").and_then(Value::as_str) == Some("ui_dialog_closed") {
                        if let Some(id) = value.get("requestId").and_then(Value::as_str) {
                            dialogs.close(id, writer, acp_session_id).await;
                        }
                        continue;
                    }
                    let updates = match text.event(&value) {
                        Ok(updates) => updates,
                        // Keep draining until the backend completes; returning
                        // early would allow a new prompt while this run is active.
                        Err(error) => {
                            stream_error.get_or_insert(error);
                            continue;
                        }
                    };
                    for update in updates {
                        writer
                            .notify(
                                "session/update",
                                json!({"sessionId": acp_session_id, "update": update}),
                            )
                            .await;
                    }
                    if let Some(plan) = translate_plan_event(&value) {
                        writer
                            .notify(
                                "session/update",
                                json!({"sessionId": acp_session_id, "update": plan}),
                            )
                            .await;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    diagln!("[rpi-acp] dropped {skipped} child events (reader lagged)");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    break 'prompt Err("rpi event stream closed before the prompt completed".into())
                }
            }
        };
        dialogs.cancel_all(writer, acp_session_id).await;
        stop_reason
    }

    /// Kill the child (session/close, process shutdown).
    pub async fn kill(&self) {
        self.cancellation
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.kill().await;
        }
    }
}

/// ACP chunks append text. Reconcile with cumulative backend snapshots so a
/// dropped delta or an early RPC completion cannot silently truncate a reply.
#[derive(Default)]
struct StreamText {
    text: String,
    thinking: String,
    stop_reason: Option<String>,
    error: Option<String>,
    repaired: usize,
}

impl StreamText {
    fn event(&mut self, event: &Value) -> Result<Vec<Value>, String> {
        let kind = event["type"].as_str().unwrap_or("");
        let message = &event["message"];
        if kind == "message_start" && message["role"] == "assistant" {
            *self = Self {
                repaired: self.repaired,
                ..Self::default()
            };
            return Ok(Vec::new());
        }
        if matches!(kind, "message_update" | "message_end") && message["role"] == "assistant" {
            if kind == "message_end" {
                self.stop_reason = message["stopReason"].as_str().map(str::to_owned);
                self.error = message["errorMessage"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
            }
            if let Some(blocks) = message["content"].as_array() {
                let mut updates = Vec::new();
                for (block_type, field, variant, sent) in [
                    (
                        "thinking",
                        "thinking",
                        "agent_thought_chunk",
                        &mut self.thinking,
                    ),
                    ("text", "text", "agent_message_chunk", &mut self.text),
                ] {
                    let parts: Vec<&str> = blocks
                        .iter()
                        .filter(|b| b["type"] == block_type)
                        .filter_map(|b| b[field].as_str())
                        .collect();
                    if !parts.is_empty() {
                        if let Some(update) = append_snapshot(sent, &parts.concat(), variant)? {
                            if kind == "message_end" {
                                self.repaired +=
                                    update["content"]["text"].as_str().unwrap_or("").len();
                            }
                            updates.push(update);
                        }
                    }
                }
                // Older RPC senders may omit snapshots. Fall back only when the
                // delta's content kind is absent, never append it twice.
                let delta_type = match event["eventType"].as_str() {
                    Some("text_delta") => "text",
                    Some("thinking_delta") => "thinking",
                    _ => "",
                };
                if kind != "message_end" && !blocks.iter().any(|b| b["type"] == delta_type) {
                    if let Some(update) = self.delta(event) {
                        updates.push(update);
                    }
                }
                return Ok(updates);
            }
        }
        Ok(self.delta(event).into_iter().collect())
    }

    fn delta(&mut self, event: &Value) -> Option<Value> {
        let update = translate_event(event);
        if let Some(update) = &update {
            let sent = match update["sessionUpdate"].as_str() {
                Some("agent_message_chunk") => Some(&mut self.text),
                Some("agent_thought_chunk") => Some(&mut self.thinking),
                _ => None,
            };
            if let Some(sent) = sent {
                sent.push_str(update["content"]["text"].as_str().unwrap_or(""));
            }
        }
        update
    }

    fn complete(&mut self, final_text: &str) -> Result<Option<Value>, String> {
        let update = append_snapshot(&mut self.text, final_text, "agent_message_chunk")?;
        if let Some(update) = &update {
            self.repaired += update["content"]["text"].as_str().unwrap_or("").len();
        }
        Ok(update)
    }
}

fn append_snapshot(
    sent: &mut String,
    complete: &str,
    variant: &str,
) -> Result<Option<Value>, String> {
    if let Some(suffix) = complete.strip_prefix(sent.as_str()) {
        if suffix.is_empty() {
            return Ok(None);
        }
        let update = json!({"sessionUpdate":variant,"content":{"type":"text","text":suffix}});
        *sent = complete.to_owned();
        return Ok(Some(update));
    }
    // A delayed partial snapshot must not replace or duplicate newer text.
    if sent.starts_with(complete) {
        return Ok(None);
    }
    Err("rpi final message differs from the streamed text; reload this session to recover the saved reply".into())
}

/// Locate the `rpi` host binary to spawn in `--mode rpc`.
///
/// Resolution order:
/// 1. `RPI_BIN` (explicit override),
/// 2. this process itself when it *is* `rpi` — the cdylib-extension case, where
///    the extension runs inside the `rpi` host,
/// 3. a sibling `rpi` / `rpi.exe` next to this executable — the standalone
///    bridge shipped alongside rpi,
/// 4. bare `rpi`, resolved from `PATH` by the OS.
///
/// The standalone binary must NOT use `current_exe()` unconditionally: there it
/// is `rpi-acp`, so spawning it would recurse instead of starting an agent.
pub fn resolve_rpi_executable() -> PathBuf {
    if let Some(path) = env_non_empty("RPI_BIN") {
        return PathBuf::from(path);
    }
    if let Ok(current) = std::env::current_exe() {
        if is_rpi_binary(&current) {
            return current;
        }
        if let Some(sibling) = sibling_rpi(current.parent()) {
            return sibling;
        }
    }
    PathBuf::from("rpi")
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Whether an executable path looks like the `rpi` host (not `rpi-acp`).
pub(crate) fn is_rpi_binary(path: &Path) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.eq_ignore_ascii_case("rpi"))
        .unwrap_or(false)
}

fn sibling_rpi(dir: Option<&Path>) -> Option<PathBuf> {
    let dir = dir?;
    let candidate = dir.join(format!("rpi{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}

/// Owns each pending client request separately, so the event stream keeps advancing.
#[derive(Default)]
struct DialogTasks {
    tasks: JoinSet<String>,
    pending: HashMap<String, AbortHandle>,
}

impl DialogTasks {
    fn open(
        &mut self,
        session: Arc<Session>,
        writer: Arc<Writer>,
        session_id: String,
        event: Value,
    ) {
        let Some(id) = event
            .get("requestId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return;
        };
        if self.pending.contains_key(id) {
            return;
        }
        let id = id.to_owned();
        let completed_id = id.clone();
        let task = self.tasks.spawn(async move {
            handle_ui_dialog(&session, &writer, &session_id, &event).await;
            completed_id
        });
        self.pending.insert(id, task);
    }

    async fn close(&mut self, id: &str, writer: &Writer, session_id: &str) {
        if let Some(task) = self.pending.remove(id) {
            task.abort();
            if writer.supports_questions() {
                writer
                    .notify(
                        crate::questions::CLOSED_METHOD,
                        json!({"sessionId":session_id,"requestId":id}),
                    )
                    .await;
            }
        }
    }

    async fn cancel_all(&mut self, writer: &Writer, session_id: &str) {
        let ids: Vec<_> = self.pending.keys().cloned().collect();
        for id in ids {
            self.close(&id, writer, session_id).await;
        }
        self.tasks.abort_all();
        while self.tasks.join_next().await.is_some() {}
    }
}

/// Only explicitly declared permissions map to ACP's permission method.
fn permission_request(
    event: &Value,
    session_id: &str,
) -> Option<(Value, Vec<rpi_plugin_sdk::dialogs::PermissionOption>)> {
    let ui = event.get("ui")?;
    let tool_call_id = event
        .get("toolCallId")?
        .as_str()
        .filter(|id| !id.trim().is_empty())?;
    rpi_plugin_sdk::dialogs::validate(ui, Some(tool_call_id)).ok()?;
    let options = rpi_plugin_sdk::dialogs::permission_options(ui).ok()?;
    let title = ui
        .get("question")
        .or_else(|| ui.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("Permission required");
    let choices: Vec<Value> = options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            json!({
                "optionId": index.to_string(), "name": option.title, "kind": option.permission,
            })
        })
        .collect();
    Some((
        json!({"sessionId": session_id, "_meta":{"pi-rust/dialog":{"requestId":event.get("requestId")}},
        "toolCall": {"toolCallId": tool_call_id, "title": title}, "options": choices}),
        options,
    ))
}

fn permission_answer(
    request_id: &str,
    result: &Value,
    options: &[rpi_plugin_sdk::dialogs::PermissionOption],
) -> Value {
    let selected = result.pointer("/outcome/optionId").and_then(Value::as_str);
    if result.pointer("/outcome/outcome").and_then(Value::as_str) == Some("selected") {
        if let Some(option) = options.iter().enumerate().find_map(|(index, option)| {
            (selected == Some(index.to_string().as_str())).then_some(option)
        }) {
            return json!({"type":"ui_dialog_response", "requestId":request_id,
                "answer":{"values":[option.title]}});
        }
    }
    json!({"type":"ui_dialog_response", "requestId":request_id, "cancelled":true})
}

async fn handle_ui_dialog(
    session: &Session,
    writer: &Arc<Writer>,
    session_id: &str,
    event: &Value,
) {
    let request_id = event.get("requestId").and_then(Value::as_str).unwrap_or("");
    let timeout = crate::questions::timeout(&event["ui"]);
    let response = if let Some((params, options)) = permission_request(event, session_id) {
        let result = tokio::time::timeout(
            timeout,
            writer.request("session/request_permission", params),
        )
        .await
        .unwrap_or(Value::Null);
        permission_answer(request_id, &result, &options)
    } else if writer.supports_questions() {
        if let Some(params) = crate::questions::request(event, session_id) {
            let result = tokio::time::timeout(
                timeout,
                writer.request(crate::questions::REQUEST_METHOD, params),
            )
            .await
            .unwrap_or(Value::Null);
            crate::questions::answer(request_id, &event["ui"], &result)
        } else {
            json!({"type":"ui_dialog_response", "requestId":request_id, "cancelled":true})
        }
    } else {
        // Old clients have no question capability. Never relabel a question as permission.
        json!({"type":"ui_dialog_response", "requestId":request_id, "cancelled":true})
    };
    if writer.supports_questions() {
        writer
            .notify(
                crate::questions::CLOSED_METHOD,
                json!({"sessionId":session_id,"requestId":request_id}),
            )
            .await;
    }
    let _ = session.send(response).await;
}

/// Flatten ACP prompt content blocks into the text `rpi --mode rpc` accepts.
///
/// Baseline ACP requires Text and ResourceLink support; we also accept embedded
/// `resource` blocks (advertised via `promptCapabilities.embeddedContext`) and
/// inline them as `<file>` blocks, mirroring rpi's `@file` expansion.
pub fn prompt_text(blocks: &[ContentBlock]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text } => {
                if !text.is_empty() {
                    parts.push(text.clone());
                }
            }
            ContentBlock::ResourceLink { name, uri } => {
                parts.push(format!("@{}", resource_display_name(name.as_deref(), uri)));
            }
            ContentBlock::Resource { resource } => {
                if let Some(text) = &resource.text {
                    parts.push(format!("<file uri=\"{}\">\n{text}\n</file>", resource.uri));
                } else {
                    parts.push(format!("@{}", resource_display_name(None, &resource.uri)));
                }
            }
            ContentBlock::Other => {}
        }
    }
    parts.join("\n")
}

fn resource_display_name(name: Option<&str>, uri: &str) -> String {
    if let Some(name) = name {
        if !name.is_empty() {
            return name.to_string();
        }
    }
    uri.strip_prefix("file://").unwrap_or(uri).to_string()
}

/// Translate one `rpi --mode rpc` event line into an ACP `session/update`
/// object, or `None` if the line is not client-visible.
pub fn translate_event(event: &Value) -> Option<Value> {
    match event.get("type").and_then(Value::as_str)? {
        "message_update" => {
            let kind = event.get("eventType").and_then(Value::as_str)?;
            let delta = event.get("delta").and_then(Value::as_str)?;
            let variant = match kind {
                "text_delta" => "agent_message_chunk",
                "thinking_delta" => "agent_thought_chunk",
                _ => return None,
            };
            if delta.is_empty() {
                return None;
            }
            Some(json!({
                "sessionUpdate": variant,
                "content": {"type": "text", "text": delta},
            }))
        }
        "tool_execution_start" => {
            let tool = event
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let args = event.get("args").cloned().unwrap_or(Value::Null);
            let mut update = json!({
                "sessionUpdate": "tool_call",
                "toolCallId": tool_call_id(event),
                "name": tool,
                "title": title_for_tool(tool, &args),
                "kind": kind_for_tool(tool),
                "status": "in_progress",
                "rawInput": args.clone(),
            });
            // Surface file mutations as ACP `diff` content so the client can
            // render its native diff view.
            let diffs = diff_contents(tool, &args);
            if !diffs.is_empty() {
                update["content"] = Value::Array(diffs);
            }
            Some(update)
        }
        "tool_execution_update" => {
            let text = tool_result_text(event.get("partialResult"));
            if text.is_empty() {
                return None;
            }
            Some(json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": tool_call_id(event),
                "status": "in_progress",
                "content": [{"type": "content", "content": {"type": "text", "text": text}}],
            }))
        }
        "tool_execution_end" => {
            let is_error = event
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let text = tool_result_text(event.get("result"));
            let mut update = json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": tool_call_id(event),
                "status": if is_error { "failed" } else { "completed" },
                "rawOutput": event.get("result").cloned().unwrap_or(Value::Null),
            });
            if !text.is_empty() {
                update["content"] =
                    json!([{"type": "content", "content": {"type": "text", "text": text}}]);
            }
            Some(update)
        }
        _ => None,
    }
}

fn tool_call_id(event: &Value) -> Value {
    event
        .get("toolCallId")
        .cloned()
        .unwrap_or(Value::String("tool".into()))
}

/// ACP `diff` content blocks for the file-mutating tools (`write`, `edit`).
/// `edit` args are the model's raw arguments: either `edits: [{oldText,newText}]`
/// (the normal shape) or the legacy top-level `oldText`/`newText` pair.
fn diff_contents(tool: &str, args: &Value) -> Vec<Value> {
    match tool {
        "write" => {
            let (Some(path), Some(content)) = (
                args.get("path").and_then(Value::as_str),
                args.get("content").and_then(Value::as_str),
            ) else {
                return Vec::new();
            };
            vec![json!({"type": "diff", "path": path, "oldText": Value::Null, "newText": content})]
        }
        "edit" => {
            let Some(path) = args.get("path").and_then(Value::as_str) else {
                return Vec::new();
            };
            let mut diffs = Vec::new();
            if let Some(edits) = args.get("edits").and_then(Value::as_array) {
                for edit in edits {
                    let old = edit
                        .get("oldText")
                        .or_else(|| edit.get("old_text"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let new = edit
                        .get("newText")
                        .or_else(|| edit.get("new_text"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    diffs.push(
                        json!({"type": "diff", "path": path, "oldText": old, "newText": new}),
                    );
                }
            } else if let (Some(old), Some(new)) = (
                args.get("oldText").and_then(Value::as_str),
                args.get("newText").and_then(Value::as_str),
            ) {
                diffs.push(json!({"type": "diff", "path": path, "oldText": old, "newText": new}));
            }
            diffs
        }
        _ => Vec::new(),
    }
}

/// Translate a completed plan-mode tool result into an ACP `plan` update so the
/// client's plan/todo panel reflects `plan_mode_complete` (`rpi-plan-mode`
/// returns `details: {kind:"plan", plan, outline}`).
pub fn translate_plan_event(event: &Value) -> Option<Value> {
    if event.get("type").and_then(Value::as_str) != Some("tool_execution_end") {
        return None;
    }
    if event.get("toolName").and_then(Value::as_str) != Some("plan_mode_complete") {
        return None;
    }
    let details = event.pointer("/result/details")?;
    if details.get("kind").and_then(Value::as_str) != Some("plan") {
        return None;
    }
    let source = details
        .get("outline")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .or_else(|| details.get("plan").and_then(Value::as_str))?;
    let entries = plan_entries(source);
    if entries.is_empty() {
        return None;
    }
    Some(json!({"sessionUpdate": "plan", "entries": entries}))
}

/// Parse the `rpi-plan-mode` outline (`- ✓ 1. step` / `- ○ 2. step`, optionally
/// with `[x]` markers) into ACP plan entries.
fn plan_entries(outline: &str) -> Vec<Value> {
    let mut entries = Vec::new();
    for line in outline.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("- ") else {
            continue;
        };
        let completed = rest.contains('✓') || rest.contains("[x]") || rest.contains("[X]");
        let content = strip_plan_marker(rest.trim());
        if content.is_empty() {
            continue;
        }
        entries.push(json!({
            "content": content,
            "priority": "medium",
            "status": if completed { "completed" } else { "pending" },
        }));
    }
    entries
}

fn strip_plan_marker(rest: &str) -> &str {
    match rest.char_indices().find(|(_, ch)| *ch == '✓' || *ch == '○') {
        Some((index, ch)) => rest[index + ch.len_utf8()..].trim(),
        None => rest,
    }
}

/// Whether the tool name maps to a stable ACP `ToolKind`.
fn kind_for_tool(tool: &str) -> &'static str {
    match tool {
        "read" => "read",
        "write" | "edit" => "edit",
        "bash" => "execute",
        "grep" | "glob" | "search" | "codegraph" | "codegraph_search" => "search",
        "webfetch" | "websearch" => "fetch",
        _ => "other",
    }
}

/// A short human title for a tool call, using the raw input when useful.
fn title_for_tool(tool: &str, args: &Value) -> String {
    if let Some(path) = args.get("path").and_then(Value::as_str) {
        return format!("{tool} {}", truncate(path, 80));
    }
    if let Some(command) = args.get("command").and_then(Value::as_str) {
        return format!("{tool}: {}", truncate(command, 80));
    }
    if let Some(query) = args.get("query").and_then(Value::as_str) {
        return format!("{tool} {query}");
    }
    tool.to_string()
}

/// True when an environment variable is set to a truthy value.
fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            let value = value.trim();
            !value.is_empty() && !value.eq_ignore_ascii_case("false") && value != "0"
        })
        .unwrap_or(false)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}…")
}

/// Concatenate the text blocks of an rpi tool result (`{"content":[{...}]}`).
fn tool_result_text(result: Option<&Value>) -> String {
    let Some(result) = result else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for item in content {
            if item.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    parts.push(text.to_string());
                }
            }
        }
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permission_event(id: &str) -> Value {
        json!({"type":"ui_dialog_request", "requestId":id, "toolCallId":"call-1",
            "ui":{"purpose":"permission","kind":"selector","question":"Allow this tool?",
                "options":[{"title":"允许一次","permission":"allow_once"},
                    {"title":"拒绝","permission":"reject_once"}]}})
    }

    #[test]
    fn generic_questions_are_never_relabelled_as_permissions() {
        let mut event = permission_event("q");
        event["ui"]["purpose"] = json!("question");
        assert!(permission_request(&event, "s").is_none());
        event["ui"].as_object_mut().unwrap().remove("purpose");
        assert!(permission_request(&event, "s").is_none());
    }

    #[test]
    fn permission_mapping_preserves_labels_and_rejects_unknown_choices() {
        let (request, options) = permission_request(&permission_event("q"), "s").unwrap();
        assert_eq!(request["options"][0]["name"], "允许一次");
        assert_eq!(request["options"][0]["kind"], "allow_once");
        assert_eq!(
            permission_answer(
                "q",
                &json!({"outcome":{"outcome":"selected","optionId":"0"}}),
                &options
            )["answer"]["values"][0],
            "允许一次"
        );
        assert_eq!(
            permission_answer(
                "q",
                &json!({"outcome":{"outcome":"selected","optionId":"allow-made-up"}}),
                &options
            )["cancelled"],
            true
        );
    }

    fn test_session() -> (Arc<Session>, tokio::io::DuplexStream) {
        let (stdin, commands) = tokio::io::duplex(8192);
        let session = Arc::new(Session {
            child: TokioMutex::new(None),
            stdin: TokioMutex::new(Box::new(stdin)),
            cancellation: watch::channel(0).0,
            prompt_lock: TokioMutex::new(()),
            events: broadcast::channel(64).0,
            next_id: AtomicU64::new(1),
        });
        (session, commands)
    }

    async fn read_json(lines: &mut tokio::io::Lines<BufReader<tokio::io::DuplexStream>>) -> Value {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[tokio::test]
    async fn prompt_delivers_complete_text_before_the_acp_response() {
        let (session, commands) = test_session();
        let mut commands = BufReader::new(commands).lines();
        let (output, peer) = tokio::io::duplex(8192);
        let writer = Arc::new(Writer::with_output(output));
        let mut replies = BufReader::new(peer).lines();
        let prompt = tokio::spawn({
            let writer = writer.clone();
            let session = session.clone();
            async move {
                let stop = session.prompt(&writer, "s", &[]).await.unwrap();
                writer.respond(&json!(99), json!({"stopReason":stop})).await;
            }
        });
        let command = read_json(&mut commands).await;
        session
            .events
            .send(json!({"type":"message_update","eventType":"text_delta","delta":"前半段🌊"}))
            .unwrap();
        session.events.send(json!({"type":"response","id":command["id"],"status":"ok", "result":{"outcome":"completed","finalText":"前半段🌊，后半段。"}})).unwrap();
        let mut text = String::new();
        loop {
            let message = read_json(&mut replies).await;
            if message["id"] == 99 {
                break;
            }
            if let Some(chunk) = message
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
            {
                text.push_str(chunk);
            }
        }
        prompt.await.unwrap();
        assert_eq!(text, "前半段🌊，后半段。");
    }

    #[test]
    fn cumulative_snapshots_repair_missing_middle_deltas_without_duplication() {
        let mut stream = StreamText::default();
        let partial = json!({"type":"message_update","eventType":"text_delta", "delta":"中文", "message":{"role":"assistant","content":[{"type":"text","text":"中文"}]}});
        assert_eq!(
            stream.event(&partial).unwrap()[0]["content"]["text"],
            "中文"
        );
        // The middle delta was lost; the next snapshot still includes it.
        let later = json!({"type":"message_update","eventType":"text_delta", "delta":"尾部", "message":{"role":"assistant","content":[{"type":"text","text":"中文🌊中间尾部"}]}});
        assert_eq!(
            stream.event(&later).unwrap()[0]["content"]["text"],
            "🌊中间尾部"
        );
        assert!(stream.event(&later).unwrap().is_empty());
        assert!(stream.event(&partial).unwrap().is_empty());
        assert!(stream.complete("中文🌊中间尾部").unwrap().is_none());
    }

    #[test]
    fn final_snapshots_repair_thinking_and_text_and_reset_between_assistant_messages() {
        let mut stream = StreamText::default();
        stream
            .event(&json!({"type":"message_update","eventType":"thinking_delta","delta":"想"}))
            .unwrap();
        stream
            .event(&json!({"type":"message_update","eventType":"text_delta","delta":"调用工具"}))
            .unwrap();
        let updates = stream.event(&json!({"type":"message_end","message":{"role":"assistant", "stopReason":"toolUse", "content":[{"type":"thinking","thinking":"想好了"},{"type":"text","text":"调用工具。"}]}})).unwrap();
        assert_eq!(updates[0]["content"]["text"], "好了");
        assert_eq!(updates[1]["content"]["text"], "。");
        stream
            .event(&json!({"type":"message_start","message":{"role":"toolResult"}}))
            .unwrap();
        assert_eq!(stream.text, "调用工具。");
        stream
            .event(&json!({"type":"message_start","message":{"role":"assistant"}}))
            .unwrap();
        assert_eq!(
            stream.complete("最终答案").unwrap().unwrap()["content"]["text"],
            "最终答案"
        );
        assert!(stream.complete("最终答案").unwrap().is_none());
        assert!(stream.complete("不同的答案").is_err());
    }

    #[tokio::test]
    async fn incomplete_runs_report_errors_or_token_limits_instead_of_end_turn() {
        for (outcome, reason, expected) in [
            ("failed", "error", Err("provider disconnected".to_string())),
            ("completed", "length", Ok("max_tokens")),
        ] {
            let (session, commands) = test_session();
            let mut commands = BufReader::new(commands).lines();
            let writer = Arc::new(Writer::with_output(tokio::io::sink()));
            let prompt = tokio::spawn({
                let session = session.clone();
                async move { session.prompt(&writer, "s", &[]).await }
            });
            let command = read_json(&mut commands).await;
            session.events.send(json!({"type":"message_end","message":{"role":"assistant", "content":[{"type":"text","text":"partial"}],"stopReason":reason,"errorMessage":"provider disconnected"}})).unwrap();
            session.events.send(json!({"type":"response","id":command["id"],"status":"ok", "result":{"outcome":outcome,"finalText":"partial"}})).unwrap();
            assert_eq!(prompt.await.unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn closed_dialog_does_not_block_prompt_completion_or_leave_a_pending_request() {
        let (session, commands) = test_session();
        let mut commands = BufReader::new(commands).lines();
        let (output, peer) = tokio::io::duplex(8192);
        let writer = Arc::new(Writer::with_output(output));
        let mut replies = BufReader::new(peer).lines();
        let prompt = tokio::spawn({
            let session = session.clone();
            let writer = writer.clone();
            async move { session.prompt(&writer, "s", &[]).await }
        });
        let command = read_json(&mut commands).await;
        session.events.send(permission_event("q")).unwrap();
        assert_eq!(
            read_json(&mut replies).await["method"],
            "session/request_permission"
        );
        session
            .events
            .send(json!({"type":"ui_dialog_closed","requestId":"q"}))
            .unwrap();
        session.events.send(json!({"type":"response","id":command["id"],"status":"ok","result":{"outcome":"completed"}})).unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), prompt)
                .await
                .unwrap()
                .unwrap(),
            Ok("end_turn")
        );
        assert_eq!(writer.pending_count(), 0);
    }

    #[tokio::test]
    async fn late_answer_to_a_closed_dialog_cannot_answer_the_next_dialog() {
        let (session, commands) = test_session();
        let mut commands = BufReader::new(commands).lines();
        let (output, peer) = tokio::io::duplex(8192);
        let writer = Arc::new(Writer::with_output(output));
        let mut replies = BufReader::new(peer).lines();
        let prompt = tokio::spawn({
            let session = session.clone();
            let writer = writer.clone();
            async move { session.prompt(&writer, "s", &[]).await }
        });
        let command = read_json(&mut commands).await;
        session.events.send(permission_event("q1")).unwrap();
        let first = read_json(&mut replies).await;
        session
            .events
            .send(json!({"type":"ui_dialog_closed","requestId":"q1"}))
            .unwrap();
        session.events.send(permission_event("q2")).unwrap();
        let second = read_json(&mut replies).await;
        let answer = json!({"outcome":{"outcome":"selected","optionId":"0"}});
        writer
            .resolve(first["id"].as_u64().unwrap(), answer.clone())
            .await;
        writer.resolve(second["id"].as_u64().unwrap(), answer).await;
        let response = read_json(&mut commands).await;
        assert_eq!(response["requestId"], "q2");
        assert_eq!(response["answer"]["values"][0], "允许一次");
        session.events.send(json!({"type":"response","id":command["id"],"status":"ok","result":{"outcome":"completed"}})).unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), prompt)
                .await
                .unwrap()
                .unwrap(),
            Ok("end_turn")
        );
        assert_eq!(writer.pending_count(), 0);
    }

    #[tokio::test]
    async fn cancelling_a_prompt_clears_unanswered_client_permissions() {
        let (session, commands) = test_session();
        let mut commands = BufReader::new(commands).lines();
        let (output, peer) = tokio::io::duplex(8192);
        let writer = Arc::new(Writer::with_output(output));
        let mut replies = BufReader::new(peer).lines();
        let prompt = tokio::spawn({
            let session = session.clone();
            let writer = writer.clone();
            async move { session.prompt(&writer, "s", &[]).await }
        });
        let command = read_json(&mut commands).await;
        session.events.send(permission_event("q")).unwrap();
        read_json(&mut replies).await;
        session.abort().await.unwrap();
        assert_eq!(read_json(&mut commands).await["type"], "abort");
        session.events.send(json!({"type":"response","id":command["id"],"status":"ok","result":{"outcome":"aborted"}})).unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), prompt)
                .await
                .unwrap()
                .unwrap(),
            Ok("cancelled")
        );
        assert_eq!(writer.pending_count(), 0);
    }

    #[tokio::test]
    async fn unsupported_question_is_cancelled_with_its_original_request_id() {
        let (session, commands) = test_session();
        let mut commands = BufReader::new(commands).lines();
        let writer = Arc::new(Writer::with_output(tokio::io::sink()));
        let mut question = permission_event("language");
        question["ui"]["purpose"] = json!("question");
        handle_ui_dialog(&session, &writer, "s", &question).await;
        let response = read_json(&mut commands).await;
        assert_eq!(response["requestId"], "language");
        assert_eq!(response["cancelled"], true);
        assert_eq!(writer.pending_count(), 0);
    }

    fn question_writer(output: impl AsyncWrite + Unpin + Send + 'static) -> Arc<Writer> {
        let writer = Arc::new(Writer::with_output(output));
        writer
            .negotiate_questions(&json!({"clientCapabilities":{"_meta":{"pi-rust/questions":1}}}));
        writer
    }

    fn question_event(id: &str) -> Value {
        json!({"type":"ui_dialog_request","requestId":id,"toolCallId":"ask-1", "ui":{
            "kind":"selector","question":"使用哪些语言？","options":[{"title":"Rust"},{"title":"Python"}],
            "allowMultiple":true,"allowComment":true,"timeout":30}})
    }

    #[tokio::test]
    async fn negotiated_question_round_trip_preserves_multiple_answers_and_comment() {
        let (session, commands) = test_session();
        let mut commands = BufReader::new(commands).lines();
        let (output, peer) = tokio::io::duplex(8192);
        let writer = question_writer(output);
        let mut replies = BufReader::new(peer).lines();
        let task = tokio::spawn({
            let session = session.clone();
            let writer = writer.clone();
            async move { handle_ui_dialog(&session, &writer, "s", &question_event("q")).await }
        });
        let request = read_json(&mut replies).await;
        assert_eq!(request["method"], crate::questions::REQUEST_METHOD);
        assert_eq!(request["params"]["ui"]["allowMultiple"], true);
        writer
            .resolve(
                request["id"].as_u64().unwrap(),
                json!({"answer":{"values":["Rust","Python"],"text":"一起用"}}),
            )
            .await;
        let answer = read_json(&mut commands).await;
        assert_eq!(
            answer["answer"],
            json!({"values":["Rust","Python"],"text":"一起用"})
        );
        assert_eq!(answer["requestId"], "q");
        assert_eq!(
            read_json(&mut replies).await["method"],
            crate::questions::CLOSED_METHOD
        );
        task.await.unwrap();
        assert_eq!(writer.pending_count(), 0);
    }

    #[tokio::test]
    async fn question_timeout_cancels_and_releases_pending_request() {
        let (session, commands) = test_session();
        let mut commands = BufReader::new(commands).lines();
        let (output, peer) = tokio::io::duplex(8192);
        let writer = question_writer(output);
        let mut replies = BufReader::new(peer).lines();
        let mut event = question_event("q");
        event["ui"]["timeout"] = json!(1);
        let task = tokio::spawn({
            let session = session.clone();
            let writer = writer.clone();
            async move { handle_ui_dialog(&session, &writer, "s", &event).await }
        });
        read_json(&mut replies).await;
        assert_eq!(
            read_json(&mut commands).await,
            json!({"type":"ui_dialog_response","requestId":"q","cancelled":true})
        );
        task.await.unwrap();
        assert_eq!(writer.pending_count(), 0);
    }

    #[tokio::test]
    async fn closed_question_notifies_client_and_discards_late_reply() {
        let (session, commands) = test_session();
        let mut commands = BufReader::new(commands).lines();
        let (output, peer) = tokio::io::duplex(8192);
        let writer = question_writer(output);
        let mut replies = BufReader::new(peer).lines();
        let prompt = tokio::spawn({
            let session = session.clone();
            let writer = writer.clone();
            async move { session.prompt(&writer, "s", &[]).await }
        });
        let command = read_json(&mut commands).await;
        session.events.send(question_event("q1")).unwrap();
        let first = read_json(&mut replies).await;
        session
            .events
            .send(json!({"type":"ui_dialog_closed","requestId":"q1"}))
            .unwrap();
        let closed = read_json(&mut replies).await;
        assert_eq!(closed["method"], crate::questions::CLOSED_METHOD);
        assert_eq!(closed["params"]["requestId"], "q1");
        session.events.send(question_event("q2")).unwrap();
        let second = read_json(&mut replies).await;
        let answer = json!({"answer":{"values":["Rust"]}});
        writer
            .resolve(first["id"].as_u64().unwrap(), answer.clone())
            .await;
        writer.resolve(second["id"].as_u64().unwrap(), answer).await;
        assert_eq!(read_json(&mut commands).await["requestId"], "q2");
        session.events.send(json!({"type":"response","id":command["id"],"status":"ok","result":{"outcome":"completed"}})).unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), prompt)
                .await
                .unwrap()
                .unwrap(),
            Ok("end_turn")
        );
        assert_eq!(writer.pending_count(), 0);
    }

    #[test]
    fn text_delta_becomes_agent_message_chunk() {
        let event = json!({
            "type": "message_update",
            "eventType": "text_delta",
            "delta": "hello ",
        });
        assert_eq!(
            translate_event(&event),
            Some(json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "hello "},
            }))
        );
    }

    #[test]
    fn thinking_delta_becomes_agent_thought_chunk() {
        let event = json!({
            "type": "message_update",
            "eventType": "thinking_delta",
            "delta": "hmm",
        });
        assert_eq!(
            translate_event(&event),
            Some(json!({
                "sessionUpdate": "agent_thought_chunk",
                "content": {"type": "text", "text": "hmm"},
            }))
        );
    }

    #[test]
    fn toolcall_delta_is_not_client_visible() {
        let event = json!({
            "type": "message_update",
            "eventType": "toolcall_delta",
            "delta": "{\"path\"",
        });
        assert_eq!(translate_event(&event), None);
    }

    #[test]
    fn tool_start_maps_to_tool_call() {
        let event = json!({
            "type": "tool_execution_start",
            "toolCallId": "call_1",
            "toolName": "read",
            "args": {"path": "src/main.rs"},
        });
        let update = translate_event(&event).unwrap();
        assert_eq!(update["sessionUpdate"], "tool_call");
        assert_eq!(update["toolCallId"], "call_1");
        assert_eq!(update["kind"], "read");
        assert_eq!(update["status"], "in_progress");
        assert_eq!(update["title"], "read src/main.rs");
    }

    #[test]
    fn tool_end_maps_to_completed_with_text() {
        let event = json!({
            "type": "tool_execution_end",
            "toolCallId": "call_1",
            "toolName": "read",
            "isError": false,
            "result": {"content": [{"type": "text", "text": "file body"}]},
        });
        let update = translate_event(&event).unwrap();
        assert_eq!(update["sessionUpdate"], "tool_call_update");
        assert_eq!(update["status"], "completed");
        assert_eq!(
            update["content"][0]["content"]["text"],
            Value::String("file body".into())
        );
    }

    #[test]
    fn tool_end_error_maps_to_failed() {
        let event = json!({
            "type": "tool_execution_end",
            "toolCallId": "call_1",
            "toolName": "bash",
            "isError": true,
            "result": {"content": [{"type": "text", "text": "boom"}]},
        });
        let update = translate_event(&event).unwrap();
        assert_eq!(update["status"], "failed");
        assert_eq!(update["kind"], Value::Null);
    }

    #[test]
    fn is_rpi_binary_matches_stem_case_insensitively() {
        assert!(is_rpi_binary(Path::new("C:/x/rpi.exe")));
        assert!(is_rpi_binary(Path::new("/usr/local/bin/rpi")));
        assert!(is_rpi_binary(Path::new("C:/x/RPI.EXE")));
        assert!(!is_rpi_binary(Path::new("C:/x/rpi-acp.exe")));
    }

    #[test]
    fn sibling_rpi_is_none_without_a_directory_or_match() {
        assert!(sibling_rpi(None).is_none());
        assert!(sibling_rpi(Some(Path::new("C:/__rpi_acp_none__"))).is_none());
    }

    #[test]
    fn resolve_rpi_executable_honours_rpi_bin_override() {
        // NOTE: mutates process env; the crate's other tests do not read RPI_BIN.
        std::env::set_var("RPI_BIN", "C:/custom/rpi.exe");
        assert_eq!(resolve_rpi_executable(), PathBuf::from("C:/custom/rpi.exe"));
        std::env::remove_var("RPI_BIN");
    }

    #[test]
    fn prompt_text_embeds_resources_and_links() {
        let blocks = vec![
            ContentBlock::Text {
                text: "explain".into(),
            },
            ContentBlock::ResourceLink {
                name: Some("main.rs".into()),
                uri: "file:///a/main.rs".into(),
            },
            ContentBlock::Resource {
                resource: crate::protocol::EmbeddedResource {
                    uri: "file:///a/lib.rs".into(),
                    text: Some("fn main() {}".into()),
                },
            },
        ];
        let text = prompt_text(&blocks);
        assert!(text.starts_with("explain"));
        assert!(text.contains("@main.rs"));
        assert!(text.contains("<file uri=\"file:///a/lib.rs\">\nfn main() {}\n</file>"));
    }

    #[test]
    fn edit_start_emits_diff_content() {
        let event = json!({
            "type": "tool_execution_start",
            "toolCallId": "call_edit",
            "toolName": "edit",
            "args": {
                "path": "src/lib.rs",
                "edits": [{"oldText": "let a = 1;", "newText": "let a = 2;"}],
            },
        });
        let update = translate_event(&event).unwrap();
        assert_eq!(update["sessionUpdate"], "tool_call");
        assert_eq!(update["kind"], "edit");
        let diff = &update["content"][0];
        assert_eq!(diff["type"], "diff");
        assert_eq!(diff["path"], "src/lib.rs");
        assert_eq!(diff["oldText"], "let a = 1;");
        assert_eq!(diff["newText"], "let a = 2;");
    }

    #[test]
    fn write_start_emits_new_file_diff() {
        let event = json!({
            "type": "tool_execution_start",
            "toolCallId": "call_write",
            "toolName": "write",
            "args": {"path": "notes.md", "content": "hello"},
        });
        let update = translate_event(&event).unwrap();
        let diff = &update["content"][0];
        assert_eq!(diff["type"], "diff");
        assert_eq!(diff["path"], "notes.md");
        assert_eq!(diff["oldText"], Value::Null);
        assert_eq!(diff["newText"], "hello");
    }

    #[test]
    fn read_start_has_no_diff() {
        let event = json!({
            "type": "tool_execution_start",
            "toolCallId": "c",
            "toolName": "read",
            "args": {"path": "x"},
        });
        let update = translate_event(&event).unwrap();
        assert!(update.get("content").is_none());
    }

    #[test]
    fn plan_mode_complete_emits_plan_update() {
        let event = json!({
            "type": "tool_execution_end",
            "toolCallId": "c",
            "toolName": "plan_mode_complete",
            "isError": false,
            "result": {
                "content": [],
                "details": {
                    "kind": "plan",
                    "plan": "1. Inspect\n2. Implement",
                    "outline": "Progress: 1/2 complete\n- ✓ 1. Inspect\n- ○ 2. Implement",
                    "active": false,
                },
            },
        });
        let plan = translate_plan_event(&event).unwrap();
        assert_eq!(plan["sessionUpdate"], "plan");
        assert_eq!(plan["entries"][0]["content"], "1. Inspect");
        assert_eq!(plan["entries"][0]["status"], "completed");
        assert_eq!(plan["entries"][1]["content"], "2. Implement");
        assert_eq!(plan["entries"][1]["status"], "pending");
    }

    #[test]
    fn plan_mode_start_has_no_plan_entries() {
        let event = json!({
            "type": "tool_execution_end",
            "toolCallId": "c",
            "toolName": "plan_mode_start",
            "result": {"details": {"kind": "plan", "active": true}},
        });
        assert!(translate_plan_event(&event).is_none());
    }
}
