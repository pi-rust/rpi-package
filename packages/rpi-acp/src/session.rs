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
//! `tool_execution_start`, …) plus
//! `{"type":"response","id":N,"success":true,"data":{…}}` per command.
//! Prompt responses acknowledge acceptance; `agent_settled` ends the run.
//!
//! We spawn one child per ACP session with `current_dir = session/new.cwd`, so
//! the agent's tools run in the client's workspace.

#[cfg(test)]
use rpi_extcore::durable::ENTRY_TYPE;
use rpi_extcore::durable::{DurableMutation, DurableRuntime};
use rpi_extcore::host::{HostSession, SessionOptions};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(test)]
use tokio::io::AsyncWrite;
#[cfg(test)]
use tokio::io::{AsyncBufReadExt, BufReader};
#[cfg(test)]
use tokio::process::Command;
use tokio::sync::broadcast;
use tokio::task::{AbortHandle, JoinSet};

use crate::protocol::{ContentBlock, Writer};

static PERMISSION_BYPASS: AtomicBool = AtomicBool::new(false);

/// Set the process-wide policy before starting the ACP server. Existing child
/// processes retain the policy with which they were launched.
pub fn set_permission_bypass(enabled: bool) {
    PERMISSION_BYPASS.store(enabled, Ordering::Release);
}

#[cfg(test)]
use rpi_extcore::process::configure_permission_gate;

/// Read `--model` / `--provider` from this process's own argv. A Zed launch
/// such as `rpi --acp --model gateway/model` carries the model in the host's
/// command line; the child `rpi --mode rpc` does not inherit it automatically,
/// so it is forwarded here. Supports both `--flag value` and `--flag=value`.
fn forwarded_model_provider() -> (Option<String>, Option<String>) {
    forwarded_model_provider_from(&std::env::args().collect::<Vec<_>>())
}

/// Parse `--model` / `--provider` from an argv slice (testable, no env access).
fn forwarded_model_provider_from(args: &[String]) -> (Option<String>, Option<String>) {
    let mut model = None;
    let mut provider = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix("--model=") {
            model = Some(value.to_string());
        } else if arg == "--model" {
            model = args.get(index + 1).cloned();
            index += 1;
        } else if let Some(value) = arg.strip_prefix("--provider=") {
            provider = Some(value.to_string());
        } else if arg == "--provider" {
            provider = args.get(index + 1).cloned();
            index += 1;
        }
        index += 1;
    }
    (model, provider)
}

/// A live ACP session backed by one `rpi --mode rpc` child process.
pub struct Session {
    rpc: Arc<HostSession>,
    durable: Option<Arc<DurableRuntime>>,
    #[cfg(test)]
    events: Option<rpi_extcore::host::TestEvents>,
}

impl Session {
    pub async fn spawn(
        id: String,
        cwd: PathBuf,
        saved: Option<&Path>,
    ) -> Result<Arc<Self>, String> {
        let directory = crate::history::session_directory(&id)?;
        let (model, provider) = forwarded_model_provider();
        let rpc = HostSession::spawn_options(SessionOptions {
            executable: resolve_rpi_executable(),
            project: cwd,
            session_id: id,
            session_dir: Some(directory.clone()),
            saved_session: saved.map(Path::to_path_buf),
            fork_source: None,
            timings_dir: directory.join("tool-timings"),
            model,
            provider,
            permission_gate: !env_flag("RPI_ACP_NO_PERMISSION_GATE"),
            bypass: PERMISSION_BYPASS.load(Ordering::Acquire),
        })
        .await?;
        Self::attach(rpc).await
    }

    async fn attach(rpc: Arc<HostSession>) -> Result<Arc<Self>, String> {
        let durable = match DurableRuntime::try_attach(rpc.clone()).await {
            Ok(runtime) => runtime,
            Err(error) => {
                // Only absent capabilities permit a non-durable session. Once
                // advertised, branch RPC and replay failures must fail closed.
                rpc.terminate().await;
                return Err(error);
            }
        };
        Ok(Arc::new(Self {
            rpc,
            durable,
            #[cfg(test)]
            events: None,
        }))
    }

    #[cfg(test)]
    pub(crate) async fn attach_for_test(rpc: Arc<HostSession>) -> Result<Arc<Self>, String> {
        Self::attach(rpc).await
    }

    async fn send(&self, value: Value) -> Result<(), String> {
        self.rpc.notify(value).await
    }

    /// Wait for backend startup and obtain the authoritative active branch.
    pub async fn state(&self) -> Result<Value, String> {
        self.rpc.state().await
    }

    /// Thin neutral-core mapping; extcore owns discovery, registration and cleanup.
    pub async fn configure_stdio_tools(
        &self,
        servers: Vec<rpi_extcore::external_tools::StdioToolServer>,
        cwd: PathBuf,
        scope: String,
    ) -> Result<(), String> {
        self.rpc.configure_stdio_tools(servers, cwd, scope).await
    }

    pub async fn history_read(&self) -> Result<rpi_extcore::history::page::HistoryRead, String> {
        rpi_extcore::history::page::HistoryRead::begin(self.rpc.clone()).await
    }

    pub async fn available_models(&self) -> Result<Value, String> {
        self.rpc.available_models().await
    }
    pub async fn state_unlocked(&self) -> Result<Value, String> {
        self.rpc.state_unlocked().await
    }
    pub async fn set_model(&self, model_id: &str, provider: Option<&str>) -> Result<(), String> {
        self.rpc.set_model(model_id, provider).await
    }
    pub async fn available_thinking_levels(&self) -> Result<Value, String> {
        self.rpc.available_thinking_levels().await
    }
    pub async fn set_thinking_level(&self, level: &str) -> Result<(), String> {
        self.rpc.set_thinking_level(level).await
    }

    pub async fn durable_state(&self) -> Result<Value, String> {
        let runtime = self.durable.as_ref().ok_or_else(|| {
            "durable_unavailable: native host does not support durable branch operations"
                .to_string()
        })?;
        runtime.refresh_branch().await?;
        Ok(serde_json::to_value(runtime.state().await).unwrap_or(Value::Null))
    }

    pub async fn mutate_durable(&self, mutation: DurableMutation) -> Result<Value, String> {
        let runtime = self.durable.as_ref().ok_or_else(|| {
            "durable_unavailable: native host does not support durable branch operations"
                .to_string()
        })?;
        let result = runtime.mutate(mutation).await?;
        Ok(json!({"result": result, "state": runtime.state().await}))
    }

    pub async fn refresh_durable(&self) -> Result<bool, String> {
        let runtime = self.durable.as_ref().ok_or_else(|| {
            "durable_unavailable: native host does not support durable branch operations"
                .to_string()
        })?;
        runtime.refresh_branch().await
    }

    pub fn compact_available(&self) -> bool {
        self.rpc.supports_command("compact")
    }

    pub fn durable_available(&self) -> bool {
        self.durable.is_some()
    }
    pub fn subscribe_durable_events(&self) -> Option<broadcast::Receiver<Value>> {
        self.durable
            .as_ref()
            .filter(|_| self.rpc.supports_event("session_entry_committed"))?;
        Some(self.rpc.subscribe_raw())
    }

    /// Build the optional ACP notification payload from the runtime's
    /// authoritative persisted state. ACP does not maintain a second ledger.
    pub async fn durable_update(&self, session_id: &str) -> Option<Value> {
        let runtime = self.durable.as_ref()?;
        let state = runtime.state().await;
        Some(json!({
            "sessionId": session_id,
            "revision": state.revision,
            "state": state,
        }))
    }

    /// Ask the child to abort the current run (`session/cancel`).
    pub async fn abort(&self) -> Result<(), String> {
        self.rpc.abort_run().await
    }

    /// Run one prompt turn to completion, streaming `session/update`
    /// notifications to the client. Returns the ACP `stopReason` or a run error.
    pub async fn prompt(
        self: &Arc<Self>,
        writer: &Arc<Writer>,
        acp_session_id: &str,
        blocks: &[ContentBlock],
    ) -> Result<&'static str, String> {
        let mut run = match compact_instructions(blocks) {
            Some(instructions) => self.rpc.begin_compaction(instructions).await?,
            None => self.rpc.begin_prompt(prompt_text(blocks)).await?,
        };
        let mut cancelled = false;
        let mut dialogs = DialogTasks::default();

        let stop_reason = 'prompt: loop {
            let event = tokio::select! {
                biased;
                _ = run.cancellation.changed(), if !cancelled => {
                    cancelled = true;
                    dialogs.cancel_all(writer, acp_session_id).await;
                    continue;
                }
                result = dialogs.tasks.join_next(), if !dialogs.tasks.is_empty() => {
                    if let Some(Ok(id)) = result { dialogs.pending.remove(&id); }
                    continue;
                }
                event = run.events.recv() => event,
            };
            match event {
                Ok(value) => {
                    let progress = run.tracker.observe(&value, cancelled);
                    for delta in progress.deltas {
                        writer
                            .notify(
                                "session/update",
                                json!({"sessionId":acp_session_id,"update":text_update(delta)}),
                            )
                            .await;
                    }
                    if let Some(outcome) = progress.outcome {
                        break 'prompt outcome.map(|outcome| match outcome {
                            rpi_extcore::run::RunOutcome::Completed => "end_turn",
                            rpi_extcore::run::RunOutcome::Cancelled => "cancelled",
                            rpi_extcore::run::RunOutcome::TokenLimit => "max_tokens",
                        });
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
                    let updates = if matches!(
                        value["type"].as_str(),
                        Some("message_start" | "message_update" | "message_end")
                    ) {
                        Vec::new()
                    } else {
                        translate_event(&value).into_iter().collect::<Vec<_>>()
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
                    if let Some(plan) = translate_todo_event(&value) {
                        writer
                            .notify(
                                "session/update",
                                json!({"sessionId": acp_session_id, "update": plan}),
                            )
                            .await;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    // The lost frames may contain acceptance or settlement.
                    // Stop this backend rather than report a truncated success
                    // or allow another prompt while its outcome is unknown.
                    self.kill().await;
                    break 'prompt Err(format!("rpi event stream lost {skipped} events; backend stopped. Restart the ACP connection to restore saved history."));
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
        self.rpc.terminate().await;
    }
}

#[cfg(test)]
#[derive(Default)]
struct StreamText(rpi_extcore::message::MessageProjection);
#[cfg(test)]
impl std::ops::Deref for StreamText {
    type Target = rpi_extcore::message::MessageProjection;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
#[cfg(test)]
impl StreamText {
    fn event(&mut self, event: &Value) -> Result<Vec<Value>, String> {
        let mut updates = self
            .0
            .event(event)?
            .into_iter()
            .map(text_update)
            .collect::<Vec<_>>();
        if !matches!(
            event["type"].as_str(),
            Some("message_start" | "message_update" | "message_end")
        ) {
            updates.extend(translate_event(event));
        }
        Ok(updates)
    }
    fn complete(&mut self, text: &str) -> Result<Option<Value>, String> {
        Ok(self.0.complete(text)?.map(text_update))
    }
}
fn text_update(delta: Value) -> Value {
    let kind = if delta["kind"] == "thinking" {
        "agent_thought_chunk"
    } else {
        "agent_message_chunk"
    };
    json!({"sessionUpdate":kind,"content":{"type":"text","text":delta["text"]}})
}

#[cfg(test)]
use rpi_extcore::process::sibling_rpi;
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
pub use rpi_extcore::process::{is_rpi_binary, resolve_rpi_executable};

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
            if writer.supports_questions() && !writer.supports_elicitation() {
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
    } else if writer.supports_elicitation() {
        if let Some(params) = crate::questions::elicitation_request(event, session_id) {
            let result = tokio::time::timeout(
                timeout,
                writer.request(crate::questions::ELICITATION_METHOD, params),
            )
            .await
            .unwrap_or(Value::Null);
            crate::questions::elicitation_answer(request_id, &event["ui"], &result)
        } else {
            json!({"type":"ui_dialog_response", "requestId":request_id, "cancelled":true})
        }
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
        // Clients without standard elicitation or the legacy extension cannot answer.
        json!({"type":"ui_dialog_response", "requestId":request_id, "cancelled":true})
    };
    if writer.supports_questions() && !writer.supports_elicitation() {
        writer
            .notify(
                crate::questions::CLOSED_METHOD,
                json!({"sessionId":session_id,"requestId":request_id}),
            )
            .await;
    }
    let _ = session.send(response).await;
}

/// Only pure text can invoke the adapter command, never resource contents.
/// Keep the full (possibly multiline) instruction body, trimming its edges.
fn compact_instructions(blocks: &[ContentBlock]) -> Option<Option<String>> {
    if !blocks
        .iter()
        .all(|block| matches!(block, ContentBlock::Text { .. }))
    {
        return None;
    }
    let text = prompt_text(blocks);
    let tail = text.trim().strip_prefix("/compact")?;
    if !tail.is_empty() && !tail.starts_with(char::is_whitespace) {
        return None;
    }
    let instructions = tail.trim();
    Some((!instructions.is_empty()).then(|| instructions.to_owned()))
}

/// Flatten ACP prompt content blocks into the text `rpi --mode rpc` accepts.
///
/// Baseline ACP requires Text and ResourceLink support; we also accept embedded
/// `resource` blocks (advertised via `promptCapabilities.embeddedContext`) and
/// inline them as `<file>` blocks, mirroring rpi's `@file` expansion.
/// Resource references retain their exact URI alongside any display name.
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
            return format!("{name} ({uri})");
        }
    }
    uri.to_string()
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
            });
            if tool != "todo" {
                update["rawInput"] = args.clone();
            }
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
            let tool = event
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let is_error = event
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let text = tool_result_text(event.get("result"));
            let mut update = json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": tool_call_id(event),
                "status": if is_error { "failed" } else { "completed" },
            });
            if tool != "todo" {
                update["rawOutput"] = event.get("result").cloned().unwrap_or(Value::Null);
            }
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

fn translate_todo_event(event: &Value) -> Option<Value> {
    if event.get("type").and_then(Value::as_str) != Some("tool_execution_end")
        || event.get("toolName").and_then(Value::as_str) != Some("todo")
    {
        return None;
    }
    let todos = event.pointer("/result/details/todos")?.as_array()?;
    let entries = todos
        .iter()
        .filter_map(|todo| {
            let content = todo.get("text").and_then(Value::as_str)?.trim();
            if content.is_empty() {
                return None;
            }
            let status = todo
                .get("status")
                .and_then(Value::as_str)
                .or_else(|| {
                    todo.get("done")
                        .and_then(Value::as_bool)
                        .filter(|done| *done)
                        .map(|_| "completed")
                })
                .unwrap_or("pending");
            Some(json!({
                "content": content,
                "priority": "medium",
                "status": status,
            }))
        })
        .collect::<Vec<_>>();
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
        "bash" => "other",
        "grep" | "glob" | "search" | "codegraph" | "codegraph_search" => "search",
        "webfetch" | "websearch" => "fetch",
        _ => "other",
    }
}

/// A short human title for a tool call, using the raw input when useful.
fn title_for_tool(tool: &str, args: &Value) -> String {
    if tool == "todo" {
        return format!(
            "todo: {}",
            args.get("action").and_then(Value::as_str).unwrap_or("list")
        );
    }
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

    include!("session_compact_tests.rs");
    include!("session_compact_lifecycle_tests.rs");

    #[test]
    fn bypass_reaches_children_and_overrides_environment_activation() {
        for no_auto_gate in [false, true] {
            let mut command = Command::new("rpi");
            configure_permission_gate(&mut command, true, no_auto_gate);
            let command = command.as_std();
            assert_eq!(command.get_args().collect::<Vec<_>>(), ["--bypass"]);
            assert!(command.get_envs().any(|(name, value)| {
                name == "RPI_PERMISSION_GATE" && value == Some(std::ffi::OsStr::new("0"))
            }));
        }
    }

    #[test]
    fn default_and_legacy_permission_policies_are_preserved() {
        let mut command = Command::new("rpi");
        configure_permission_gate(&mut command, false, false);
        assert_eq!(
            command.as_std().get_args().collect::<Vec<_>>(),
            ["--permission-gate"]
        );
        let mut command = Command::new("rpi");
        configure_permission_gate(&mut command, false, true);
        assert_eq!(command.as_std().get_args().count(), 0);
        assert_eq!(command.as_std().get_envs().count(), 0);
    }

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
        let (rpc, events) = HostSession::test_connection(stdin, 64);
        let session = Arc::new(Session {
            rpc: rpc.clone(),
            durable: Some(DurableRuntime::empty(rpc)),
            events: Some(events),
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

    fn settle_prompt(session: &Session, command: &Value, mut result: Value) {
        session.events.as_ref().unwrap().send(json!({"type":"response","id":command["id"],"command":"prompt","success":true,"data":{"accepted":true}})).unwrap();
        result["type"] = json!("agent_settled");
        result["runId"] = json!("test-run");
        session.events.as_ref().unwrap().send(result).unwrap();
    }

    #[tokio::test]
    async fn attach_without_capabilities_keeps_the_backend_alive() {
        for commands in [
            json!([]),
            json!(["get_session_branch"]),
            json!(["execute_command"]),
        ] {
            let (stdin, _commands) = tokio::io::duplex(8192);
            let (rpc, _events) = HostSession::test_connection_with_ready(
                stdin,
                64,
                json!({"type":"ready","protocol":"rpi","supportedCommands":commands}),
            );
            let session = Session::attach(rpc.clone()).await.unwrap();
            assert!(!session.durable_available());
            assert_eq!(rpc.snapshot().await.status, "ready");
            session.kill().await;
        }
    }

    #[tokio::test]
    async fn attach_errors_preserve_the_cause_and_close_the_backend() {
        for (response, expected) in [
            (
                json!({"success":true,"data":{"entries":[]}}),
                "get_session_branch: host must return filtered custom entries",
            ),
            (
                json!({"success":true,"data":{"filtered":false,"entries":[]}}),
                "get_session_branch: host must return filtered custom entries",
            ),
            (
                json!({"success":false,"error":"fixture branch RPC failed"}),
                "get_session_branch: fixture branch RPC failed",
            ),
            (
                json!({"success":true,"data":{"filtered":true,"entries":[{
                    "id":"broken","type":"custom","customType":ENTRY_TYPE,
                    "data":{"format":"rpi-durable-journal","version":999}
                }]}}),
                "durable branch replay:",
            ),
        ] {
            let (stdin, commands) = tokio::io::duplex(8192);
            let (rpc, events) = HostSession::test_connection(stdin, 64);
            let mut raw = rpc.subscribe_raw();
            let attach = tokio::spawn(Session::attach(rpc.clone()));
            let mut commands = BufReader::new(commands).lines();
            let branch = read_json(&mut commands).await;
            assert_eq!(branch["type"], "get_session_branch");
            assert_eq!(branch["customTypes"], json!([ENTRY_TYPE]));
            let mut response = response;
            response["type"] = json!("response");
            response["id"] = branch["id"].clone();
            events.send(response).unwrap();
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), attach)
                .await
                .unwrap()
                .unwrap();
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("invalid durable backend was accepted"),
            };
            assert!(error.starts_with(expected), "{error}");
            // Cleanup completes before the error is returned, even though we
            // still retain references to the failed host here.
            loop {
                let event = tokio::time::timeout(std::time::Duration::from_secs(2), raw.recv())
                    .await
                    .unwrap()
                    .unwrap();
                if event["type"] == "host_closed" {
                    break;
                }
            }
            assert!(rpc.state().await.is_err());
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(2), commands.next_line())
                    .await
                    .unwrap()
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn durable_lifecycle_event_does_not_mutate_the_projection() {
        let (session, _commands) = test_session();
        let update = session.durable_update("acp-session").await.unwrap();
        assert_eq!(update["revision"], 0);
        assert!(update["state"]["run"].is_null());
    }

    #[tokio::test]
    async fn durable_unavailable_does_not_block_the_session() {
        let (session, _commands) = test_session();
        let unavailable = Session {
            rpc: session.rpc.clone(),
            durable: None,
            events: None,
        };
        let error = unavailable.durable_state().await.unwrap_err();
        assert!(error.starts_with("durable_unavailable:"));
        let error = unavailable
            .mutate_durable(DurableMutation::StartRun)
            .await
            .unwrap_err();
        assert!(error.starts_with("durable_unavailable:"));
    }

    #[tokio::test]
    async fn durable_mutation_uses_the_extension_command_bridge() {
        let (session, commands) = test_session();
        let mut commands = BufReader::new(commands).lines();
        let mutation = tokio::spawn({
            let session = session.clone();
            async move { session.mutate_durable(DurableMutation::StartRun).await }
        });
        let command = read_json(&mut commands).await;
        assert_eq!(command["type"], "execute_command");
        assert_eq!(command["command"], "durable");
        session
            .events
            .as_ref()
            .unwrap()
            .send(json!({
                "type": "response",
                "id": command["id"],
                "success": true,
                "data": {"changed": true, "run": {"id":"run-1", "status":"running", "tools":[]}}
            }))
            .unwrap();
        let branch = read_json(&mut commands).await;
        assert_eq!(branch["type"], "get_session_branch");
        assert_eq!(branch["customTypes"], json!([ENTRY_TYPE]));
        assert!(branch.get("customType").is_none());
        session
            .events
            .as_ref()
            .unwrap()
            .send(json!({
                "type": "response",
                "id": branch["id"],
                "success": true,
                "data": {"filtered":true,"entries": [{"id":"entry-1","type":"custom","customType":ENTRY_TYPE,"data":{
                    "format":"rpi-durable-journal","version":1,
                    "kind":"snapshot","baseRevision":0,"revision":1,
                    "state": {
                        "revision": 1, "nextRunId": 1,
                        "run": {"id":"run-1", "status":"running", "tools":[]},
                        "queue": []
                    }
                }}]}
            }))
            .unwrap();
        let result = mutation.await.unwrap().unwrap();
        assert_eq!(result["result"]["changed"], true);
        assert_eq!(result["state"]["run"]["status"], "running");
        let update = session.durable_update("acp-session").await.unwrap();
        assert_eq!(update["sessionId"], "acp-session");
        assert_eq!(update["revision"], 1);
        assert_eq!(update["state"]["run"]["status"], "running");
    }

    #[tokio::test]
    async fn state_reads_native_data_and_reports_rejections() {
        for (response, expected) in [
            (
                json!({"success":true,"data":{"sessionId":"native","leafId":"leaf"}}),
                Ok(json!({"sessionId":"native","leafId":"leaf"})),
            ),
            (
                json!({"success":false,"error":"storage unavailable"}),
                Err("open session: storage unavailable".to_owned()),
            ),
            (
                json!({"success":true}),
                Err("open session: invalid RPC state data".to_owned()),
            ),
        ] {
            let (session, commands) = test_session();
            let mut commands = BufReader::new(commands).lines();
            let state = tokio::spawn({
                let session = session.clone();
                async move { session.state().await }
            });
            let command = read_json(&mut commands).await;
            let mut response = response;
            response["type"] = json!("response");
            response["id"] = command["id"].clone();
            session.events.as_ref().unwrap().send(response).unwrap();
            assert_eq!(state.await.unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn accepted_prompt_waits_for_settlement_and_preserves_final_snapshot() {
        let (session, commands) = test_session();
        let mut commands = BufReader::new(commands).lines();
        let (output, peer) = tokio::io::duplex(8192);
        let writer = Arc::new(Writer::with_output(output));
        let mut replies = BufReader::new(peer).lines();
        let prompt = tokio::spawn({
            let session = session.clone();
            let writer = writer.clone();
            async move {
                session
                    .prompt(
                        &writer,
                        "s",
                        &[ContentBlock::Text {
                            text: "hello".into(),
                        }],
                    )
                    .await
            }
        });
        let command = read_json(&mut commands).await;
        assert_eq!(command["message"], "hello");
        assert!(command.get("content").is_none());
        // A previous run's settlement before this acknowledgement is unrelated.
        session
            .events
            .as_ref()
            .unwrap()
            .send(json!({"type":"agent_settled","runId":"old","outcome":"completed"}))
            .unwrap();
        session.events.as_ref().unwrap().send(json!({"type":"response","id":command["id"],"success":true,"data":{"accepted":true}})).unwrap();
        session
            .events
            .as_ref()
            .unwrap()
            .send(json!({"type":"message_update","eventType":"text_delta","delta":"前半段🌊"}))
            .unwrap();
        assert_eq!(
            read_json(&mut replies).await["params"]["update"]["content"]["text"],
            "前半段🌊"
        );
        assert!(!prompt.is_finished());
        assert_eq!(
            session.prompt(&writer, "s", &[]).await,
            Err("a prompt is already running".into())
        );
        session.events.as_ref().unwrap().send(json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"前半段🌊，后半段。"}],"stopReason":"stop"}})).unwrap();
        assert_eq!(
            read_json(&mut replies).await["params"]["update"]["content"]["text"],
            "，后半段。"
        );
        assert!(!prompt.is_finished());
        session
            .events
            .as_ref()
            .unwrap()
            .send(json!({"type":"agent_settled","runId":"current","outcome":"completed"}))
            .unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), prompt)
                .await
                .unwrap()
                .unwrap(),
            Ok("end_turn")
        );
    }

    #[tokio::test]
    async fn native_prompt_errors_and_terminal_outcomes_are_not_successes() {
        for (response, settlement, expected) in [
            (
                json!({"success":false,"error":"model unavailable"}),
                None,
                Err("model unavailable".to_owned()),
            ),
            (
                json!({"success":true,"data":{}}),
                None,
                Err("rpi did not acknowledge prompt acceptance".to_owned()),
            ),
            (
                json!({"success":true,"data":{"accepted":true}}),
                Some(json!({"outcome":"failed","error":"provider offline"})),
                Err("provider offline".to_owned()),
            ),
            (
                json!({"success":true,"data":{"accepted":true}}),
                Some(json!({"outcome":"suspended"})),
                Err("rpi run did not complete: suspended".to_owned()),
            ),
            (
                json!({"success":true,"data":{"accepted":true}}),
                Some(json!({"outcome":"aborted"})),
                Ok("cancelled"),
            ),
        ] {
            let (session, commands) = test_session();
            let mut commands = BufReader::new(commands).lines();
            let writer = Arc::new(Writer::with_output(tokio::io::sink()));
            let prompt = tokio::spawn({
                let session = session.clone();
                async move { session.prompt(&writer, "s", &[]).await }
            });
            let command = read_json(&mut commands).await;
            let mut response = response;
            response["type"] = json!("response");
            response["id"] = command["id"].clone();
            session.events.as_ref().unwrap().send(response).unwrap();
            if let Some(mut settlement) = settlement {
                settlement["type"] = json!("agent_settled");
                session.events.as_ref().unwrap().send(settlement).unwrap();
            }
            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(2), prompt)
                    .await
                    .unwrap()
                    .unwrap(),
                expected
            );
        }
    }

    #[tokio::test]
    async fn event_overflow_cannot_leave_a_prompt_waiting_for_a_lost_acknowledgement() {
        let (session, commands) = test_session();
        let mut commands = BufReader::new(commands).lines();
        let writer = Arc::new(Writer::with_output(tokio::io::sink()));
        let prompt = tokio::spawn({
            let session = session.clone();
            async move { session.prompt(&writer, "s", &[]).await }
        });
        let command = read_json(&mut commands).await;
        session.events.as_ref().unwrap().send(json!({"type":"response","id":command["id"],"success":true,"data":{"accepted":true}})).unwrap();
        // Do not yield: deterministically evict the acknowledgement from the
        // small test channel before the prompt reads it.
        for _ in 0..100 {
            session
                .events
                .as_ref()
                .unwrap()
                .send(json!({"type":"message_update","eventType":"text_delta","delta":"x"}))
                .unwrap();
        }
        session
            .events
            .as_ref()
            .unwrap()
            .send(json!({"type":"agent_settled","outcome":"completed"}))
            .unwrap();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), prompt)
            .await
            .expect("event loss must not leave the prompt waiting")
            .unwrap();
        assert!(outcome.unwrap_err().contains("event stream lost"));
        assert!(session
            .state()
            .await
            .unwrap_err()
            .contains("backend stopped"));
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
            .as_ref()
            .unwrap()
            .send(json!({"type":"message_update","eventType":"text_delta","delta":"前半段🌊"}))
            .unwrap();
        settle_prompt(
            &session,
            &command,
            json!({"outcome":"completed","finalText":"前半段🌊，后半段。"}),
        );
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
    fn compact_fixture_maps_initial_and_final_content_without_duplicate_acp_chunks() {
        let frames = [
            json!({"type":"message_start","message":{"role":"assistant","content":[{"type":"thinking","thinking":"初"},{"type":"text","text":"Hello"}]}}),
            json!({"type":"message_update","eventType":"thinking_delta","contentIndex":0,"delta":"想🌊"}),
            json!({"type":"message_update","eventType":"text_delta","contentIndex":1,"delta":" world!"}),
            json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"初想🌊好了"},{"type":"text","text":"Hello world! Saved."}]}}),
        ];
        let mut stream = StreamText::default();
        let mut text = String::new();
        let mut thinking = String::new();
        for event in frames {
            for update in stream.event(&event).unwrap() {
                let target = match update["sessionUpdate"].as_str() {
                    Some("agent_message_chunk") => &mut text,
                    Some("agent_thought_chunk") => &mut thinking,
                    other => panic!("unexpected update: {other:?}"),
                };
                target.push_str(update["content"]["text"].as_str().unwrap());
            }
        }
        assert_eq!(text, "Hello world! Saved.");
        assert_eq!(thinking, "初想🌊好了");
        assert!(stream.complete(&text).unwrap().is_none());
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
            session.events.as_ref().unwrap().send(json!({"type":"message_end","message":{"role":"assistant", "content":[{"type":"text","text":"partial"}],"stopReason":reason,"errorMessage":"provider disconnected"}})).unwrap();
            settle_prompt(
                &session,
                &command,
                json!({"outcome":outcome,"finalText":"partial"}),
            );
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
        session
            .events
            .as_ref()
            .unwrap()
            .send(permission_event("q"))
            .unwrap();
        assert_eq!(
            read_json(&mut replies).await["method"],
            "session/request_permission"
        );
        session
            .events
            .as_ref()
            .unwrap()
            .send(json!({"type":"ui_dialog_closed","requestId":"q"}))
            .unwrap();
        settle_prompt(&session, &command, json!({"outcome":"completed"}));
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
        session
            .events
            .as_ref()
            .unwrap()
            .send(permission_event("q1"))
            .unwrap();
        let first = read_json(&mut replies).await;
        session
            .events
            .as_ref()
            .unwrap()
            .send(json!({"type":"ui_dialog_closed","requestId":"q1"}))
            .unwrap();
        session
            .events
            .as_ref()
            .unwrap()
            .send(permission_event("q2"))
            .unwrap();
        let second = read_json(&mut replies).await;
        let answer = json!({"outcome":{"outcome":"selected","optionId":"0"}});
        writer
            .resolve(first["id"].as_u64().unwrap(), answer.clone())
            .await;
        writer.resolve(second["id"].as_u64().unwrap(), answer).await;
        let response = read_json(&mut commands).await;
        assert_eq!(response["requestId"], "q2");
        assert_eq!(response["answer"]["values"][0], "允许一次");
        settle_prompt(&session, &command, json!({"outcome":"completed"}));
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
        session
            .events
            .as_ref()
            .unwrap()
            .send(permission_event("q"))
            .unwrap();
        read_json(&mut replies).await;
        session.abort().await.unwrap();
        assert_eq!(read_json(&mut commands).await["type"], "abort");
        settle_prompt(&session, &command, json!({"outcome":"aborted"}));
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

    fn elicitation_writer(output: impl AsyncWrite + Unpin + Send + 'static) -> Arc<Writer> {
        let writer = Arc::new(Writer::with_output(output));
        writer.negotiate_questions(&json!({
            "clientCapabilities":{"elicitation":{"form":{}}}
        }));
        writer
    }

    fn question_event(id: &str) -> Value {
        json!({"type":"ui_dialog_request","requestId":id,"toolCallId":"ask-1", "ui":{
            "kind":"selector","question":"使用哪些语言？","options":[{"title":"Rust"},{"title":"Python"}],
            "allowMultiple":true,"allowComment":true,"timeout":30}})
    }

    #[tokio::test]
    async fn standard_elicitation_round_trip_uses_zed_form_protocol() {
        let (session, commands) = test_session();
        let mut commands = BufReader::new(commands).lines();
        let (output, peer) = tokio::io::duplex(8192);
        let writer = elicitation_writer(output);
        let mut replies = BufReader::new(peer).lines();
        let task = tokio::spawn({
            let session = session.clone();
            let writer = writer.clone();
            async move { handle_ui_dialog(&session, &writer, "s", &question_event("q")).await }
        });
        let request = read_json(&mut replies).await;
        assert_eq!(request["method"], crate::questions::ELICITATION_METHOD);
        assert_eq!(request["params"]["mode"], "form");
        assert_eq!(
            request["params"]["requestedSchema"]["properties"]["answers"]["type"],
            "array"
        );
        writer
            .resolve(
                request["id"].as_u64().unwrap(),
                json!({"action":"accept","content":{"answers":["Rust","Python"],"comment":"一起用"}}),
            )
            .await;
        let answer = read_json(&mut commands).await;
        assert_eq!(
            answer["answer"],
            json!({"values":["Rust","Python"],"text":"一起用"})
        );
        assert_eq!(answer["requestId"], "q");
        task.await.unwrap();
        assert_eq!(writer.pending_count(), 0);
    }

    #[tokio::test]
    async fn standard_elicitation_timeout_cancels_without_legacy_close_notification() {
        let (session, commands) = test_session();
        let mut commands = BufReader::new(commands).lines();
        let (output, peer) = tokio::io::duplex(8192);
        let writer = elicitation_writer(output);
        let mut replies = BufReader::new(peer).lines();
        let mut event = question_event("q-timeout");
        event["ui"]["timeout"] = json!(1);
        let task = tokio::spawn({
            let session = session.clone();
            let writer = writer.clone();
            async move { handle_ui_dialog(&session, &writer, "s", &event).await }
        });
        let request = read_json(&mut replies).await;
        assert_eq!(request["method"], crate::questions::ELICITATION_METHOD);
        assert_eq!(
            read_json(&mut commands).await,
            json!({"type":"ui_dialog_response","requestId":"q-timeout","cancelled":true})
        );
        task.await.unwrap();
        assert_eq!(writer.pending_count(), 0);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), replies.next_line())
                .await
                .is_err()
        );
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
        session
            .events
            .as_ref()
            .unwrap()
            .send(question_event("q1"))
            .unwrap();
        let first = read_json(&mut replies).await;
        session
            .events
            .as_ref()
            .unwrap()
            .send(json!({"type":"ui_dialog_closed","requestId":"q1"}))
            .unwrap();
        let closed = read_json(&mut replies).await;
        assert_eq!(closed["method"], crate::questions::CLOSED_METHOD);
        assert_eq!(closed["params"]["requestId"], "q1");
        session
            .events
            .as_ref()
            .unwrap()
            .send(question_event("q2"))
            .unwrap();
        let second = read_json(&mut replies).await;
        let answer = json!({"answer":{"values":["Rust"]}});
        writer
            .resolve(first["id"].as_u64().unwrap(), answer.clone())
            .await;
        writer.resolve(second["id"].as_u64().unwrap(), answer).await;
        assert_eq!(read_json(&mut commands).await["requestId"], "q2");
        settle_prompt(&session, &command, json!({"outcome":"completed"}));
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
    fn bash_tool_call_uses_other_kind_for_zed_progress() {
        let event = json!({
            "type": "tool_execution_start",
            "toolCallId": "bash-call",
            "toolName": "bash",
            "args": {"command": "sleep 1 && echo done"},
        });
        let update = translate_event(&event).unwrap();
        assert_eq!(update["sessionUpdate"], "tool_call");
        assert_eq!(update["kind"], "other");
        assert_eq!(update["title"], "bash: sleep 1 && echo done");
    }

    #[test]
    fn todo_tool_call_uses_a_human_title_instead_of_raw_json() {
        let event = json!({
            "type": "tool_execution_start",
            "toolCallId": "todo-call",
            "toolName": "todo",
            "args": {"action": "list", "id": 0, "includeDone": true, "status": "pending", "text": ""},
        });
        let update = translate_event(&event).unwrap();
        assert_eq!(update["title"], "todo: list");
        assert!(update.get("rawInput").is_none());

        let completed = translate_event(&json!({
            "type": "tool_execution_end",
            "toolCallId": "todo-call",
            "toolName": "todo",
            "isError": false,
            "result": {
                "content": [{"type": "text", "text": "0/0 completed"}],
                "details": {"todos": []},
            },
        }))
        .unwrap();
        assert!(completed.get("rawOutput").is_none());
        assert_eq!(completed["content"][0]["content"]["text"], "0/0 completed");
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
    fn forwarded_model_provider_parses_both_forms() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            forwarded_model_provider_from(&args(&["--model", "gateway/model"])),
            (Some("gateway/model".into()), None)
        );
        assert_eq!(
            forwarded_model_provider_from(&args(&["--model=claude-sonnet-5"])),
            (Some("claude-sonnet-5".into()), None)
        );
        assert_eq!(
            forwarded_model_provider_from(&args(&["--provider", "openai"])),
            (None, Some("openai".into()))
        );
        assert_eq!(
            forwarded_model_provider_from(&args(&["--provider=anthropic", "--model", "opus"])),
            (Some("opus".into()), Some("anthropic".into()))
        );
        // A trailing `--model` without a value is ignored, not taken from argv.
        assert_eq!(
            forwarded_model_provider_from(&args(&["--acp", "--model"])),
            (None, None)
        );
        assert_eq!(forwarded_model_provider_from(&[]), (None, None));
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
        assert_eq!(
            prompt_text(&blocks),
            "explain\n@main.rs (file:///a/main.rs)\n<file uri=\"file:///a/lib.rs\">\nfn main() {}\n</file>"
        );
    }

    #[test]
    fn prompt_text_distinguishes_same_name_resource_links() {
        let blocks = vec![
            ContentBlock::ResourceLink {
                name: Some("src".into()),
                uri: "file:///project-a/src/".into(),
            },
            ContentBlock::ResourceLink {
                name: Some("src".into()),
                uri: "file:///project-b/src/".into(),
            },
        ];
        assert_eq!(
            prompt_text(&blocks),
            "@src (file:///project-a/src/)\n@src (file:///project-b/src/)"
        );
    }

    #[test]
    fn prompt_text_preserves_exact_resource_link_uris() {
        for uri in [
            "file:///project/my folder/a%20b.rs?revision=one%2Ftwo&view=raw#L10",
            "file:///C:/Users/Big%20Fish/project/?filter=a%26b#directory",
            "file://server/share/my folder/file.rs?version=2#L3",
            "https://example.test/a%2Fb?q=hello%20world#section%201",
            "custom://workspace/resource%23name?key=value#part",
        ] {
            for name in [None, Some(""), Some("my label")] {
                let block = ContentBlock::ResourceLink {
                    name: name.map(str::to_string),
                    uri: uri.into(),
                };
                let expected = match name {
                    Some("my label") => format!("@my label ({uri})"),
                    _ => format!("@{uri}"),
                };
                assert_eq!(prompt_text(&[block]), expected, "name={name:?}");
            }
        }
    }

    #[test]
    fn prompt_text_preserves_uris_for_resources_without_text() {
        for uri in [
            "file:///C:/my%20project/src/?revision=one%2Ftwo#L5",
            "custom://workspace/my folder?key=a%26b#part",
        ] {
            let block = ContentBlock::Resource {
                resource: crate::protocol::EmbeddedResource {
                    uri: uri.into(),
                    text: None,
                },
            };
            assert_eq!(prompt_text(&[block]), format!("@{uri}"));
        }
    }

    #[test]
    fn prompt_text_preserves_mixed_block_order_and_embedded_payloads() {
        let blocks = vec![
            ContentBlock::Text {
                text: "before".into(),
            },
            ContentBlock::ResourceLink {
                name: Some("source".into()),
                uri: "file:///a%20b.rs?revision=2#L4".into(),
            },
            ContentBlock::Resource {
                resource: crate::protocol::EmbeddedResource {
                    uri: "custom://context?key=a%2Fb#part".into(),
                    text: Some("first line\nsecond line".into()),
                },
            },
            ContentBlock::Text {
                text: String::new(),
            },
            ContentBlock::Other,
            ContentBlock::Resource {
                resource: crate::protocol::EmbeddedResource {
                    uri: "file:///C:/my%20project/?revision=2#directory".into(),
                    text: None,
                },
            },
            ContentBlock::Resource {
                resource: crate::protocol::EmbeddedResource {
                    uri: "custom://empty".into(),
                    text: Some(String::new()),
                },
            },
            ContentBlock::Text {
                text: "after".into(),
            },
        ];
        assert_eq!(
            prompt_text(&blocks),
            concat!(
                "before\n",
                "@source (file:///a%20b.rs?revision=2#L4)\n",
                "<file uri=\"custom://context?key=a%2Fb#part\">\nfirst line\nsecond line\n</file>\n",
                "@file:///C:/my%20project/?revision=2#directory\n",
                "<file uri=\"custom://empty\">\n\n</file>\n",
                "after"
            )
        );
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
    fn todo_completion_emits_live_plan_statuses() {
        let event = json!({
            "type": "tool_execution_end",
            "toolCallId": "todo-call",
            "toolName": "todo",
            "isError": false,
            "result": {
                "details": {
                    "todos": [
                        {"id": 0, "text": "Inspect", "status": "completed"},
                        {"id": 1, "text": "Implement", "status": "in_progress"},
                        {"id": 2, "text": "Test", "status": "pending"},
                    ],
                },
            },
        });
        let plan = translate_todo_event(&event).unwrap();
        assert_eq!(plan["sessionUpdate"], "plan");
        assert_eq!(plan["entries"][0]["content"], "Inspect");
        assert_eq!(plan["entries"][0]["status"], "completed");
        assert_eq!(plan["entries"][1]["status"], "in_progress");
        assert_eq!(plan["entries"][2]["status"], "pending");
    }

    #[test]
    fn todo_clear_emits_an_empty_plan_update() {
        let event = json!({
            "type": "tool_execution_end",
            "toolName": "todo",
            "result": {"details": {"todos": []}},
        });
        assert_eq!(translate_todo_event(&event).unwrap()["entries"], json!([]));
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
