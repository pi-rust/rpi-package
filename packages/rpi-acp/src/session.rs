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
use tokio::sync::{broadcast, watch};
use tokio::task::{AbortHandle, JoinSet};

use crate::protocol::{ContentBlock, Writer};

static PERMISSION_BYPASS: AtomicBool = AtomicBool::new(false);

/// Set the process-wide policy before starting the ACP server. Existing child
/// processes retain the policy with which they were launched.
pub fn set_permission_bypass(enabled: bool) {
    PERMISSION_BYPASS.store(enabled, Ordering::Release);
}

pub(crate) fn permission_bypass() -> bool {
    PERMISSION_BYPASS.load(Ordering::Acquire)
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

/// The authoritative result of one prompt, including the canonical inserted
/// user message ID required by ACP v2. v1 callers continue to receive only
/// the stop reason through [`Session::prompt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptResult {
    pub stop_reason: &'static str,
    pub message_id: String,
}

/// A live ACP session backed by one `rpi --mode rpc` child process.
pub struct Session {
    rpc: Arc<HostSession>,
    durable: Option<Arc<DurableRuntime>>,
    mcp: tokio::sync::Mutex<Option<rpi_extcore::mcp::McpBinding>>,
    pub(crate) mcp_owner: rpi_extcore::mcp::McpOwner,
    child_watcher: tokio::sync::Mutex<Option<crate::subagents::Watcher>>,
    mcp_config: tokio::sync::Mutex<crate::mcp::Servers>,
    mcp_input_scope: crate::mcp_input::Scope,
    sampling_owner: tokio::sync::Mutex<Option<rpi_extcore::sampling::NativeSamplingOwner>>,
    #[cfg(test)]
    events: Option<rpi_extcore::host::TestEvents>,
}

impl Session {
    pub async fn spawn(
        id: String,
        cwd: PathBuf,
        additional_directories: Vec<PathBuf>,
        saved: Option<&Path>,
    ) -> Result<Arc<Self>, String> {
        Self::spawn_with_client_tools(id, cwd, additional_directories, saved, Default::default()).await
    }

    pub async fn spawn_with_client_tools(
        id: String,
        cwd: PathBuf,
        additional_directories: Vec<PathBuf>,
        saved: Option<&Path>,
        client_tools: rpi_extcore::host::ClientToolOptions,
    ) -> Result<Arc<Self>, String> {
        Self::spawn_with_features(id, cwd, additional_directories, saved, client_tools, None).await
    }

    pub(crate) async fn spawn_with_features(
        id: String,
        cwd: PathBuf,
        additional_directories: Vec<PathBuf>,
        saved: Option<&Path>,
        client_tools: rpi_extcore::host::ClientToolOptions,
        writer: Option<Arc<Writer>>,
    ) -> Result<Arc<Self>, String> {
        let child_events = writer.as_ref().is_some_and(|w| w.supports_subagents());
        let root = id.clone();
        let mcp_input_scope = crate::mcp_input::Scope {
            session_id: id.clone(),
            directories: std::iter::once(cwd.clone()).chain(additional_directories.iter().cloned()).collect(),
        };
        let directory = crate::history::session_directory(&id)?;
        let (model, provider) = forwarded_model_provider();
        let rpc = HostSession::spawn_options_with_features(SessionOptions {
            executable: resolve_rpi_executable(),
            project: cwd,
            session_id: id,
            session_dir: Some(directory.clone()),
            saved_session: saved.map(Path::to_path_buf),
            fork_source: None,
            timings_dir: directory.join("tool-timings"),
            model,
            provider,
            additional_directories,
            permission_gate: !env_flag("RPI_ACP_NO_PERMISSION_GATE"),
            bypass: PERMISSION_BYPASS.load(Ordering::Acquire),
        }, client_tools, rpi_extcore::host::RuntimeFeatures { child_events })
        .await?;
        // Subscribe before durable attachment or any other native request. A
        // legacy native binary may ignore the opt-in; do not assume support.
        let child_watcher = writer.filter(|_| child_events && rpc.ready()["childEventsVersion"] == 1)
            .map(|writer| crate::subagents::Watcher::start(rpc.clone(), writer, root));
        Self::attach_with_context(rpc, child_watcher, mcp_input_scope).await
    }

    #[cfg(test)]
    async fn attach(rpc: Arc<HostSession>) -> Result<Arc<Self>, String> {
        Self::attach_with_context(rpc, None, Default::default()).await
    }

    async fn attach_with_context(
        rpc: Arc<HostSession>,
        child_watcher: Option<crate::subagents::Watcher>,
        mcp_input_scope: crate::mcp_input::Scope,
    ) -> Result<Arc<Self>, String> {
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
            mcp: Default::default(), mcp_owner: Default::default(),
            child_watcher: tokio::sync::Mutex::new(child_watcher),
            sampling_owner: Default::default(),
            mcp_config: Default::default(),
            mcp_input_scope,
            #[cfg(test)]
            events: None,
        }))
    }

    /// Produce one native agent-to-client request and bridge the correlated
    /// response through ACP. This is used by native extensions/tools that need
    /// client-owned filesystem or terminal capabilities.
    pub async fn client_request(
        &self,
        writer: &Arc<Writer>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        if !CLIENT_REQUEST_METHODS.contains(&method) {
            return Err("unsupported client request method".to_owned());
        }
        validate_client_params(method, &params)?;
        let correlation = uuid::Uuid::new_v4().to_string();
        let mut events = self.rpc.subscribe_raw();
        let command = self.rpc.command(
            "client_request",
            json!({"method": method, "params": params, "correlationId": correlation}),
            std::time::Duration::from_secs(75),
        );
        tokio::pin!(command);
        loop {
            tokio::select! {
                result = &mut command => return result,
                event = events.recv() => match event {
                    Ok(event) if event.get("type").and_then(Value::as_str) == Some("client_request")
                        && event.get("method").and_then(Value::as_str) == Some(method)
                        && event.get("correlationId").and_then(Value::as_str) == Some(correlation.as_str()) => {
                        let request_id = event.get("requestId").and_then(Value::as_str).unwrap_or_default();
                        let target = match writer.interaction_target(&self.mcp_input_scope.session_id, &event).await {
                            Ok(target) => target,
                            Err(error) => {
                                self.rpc.terminate().await;
                                return Err(error);
                            }
                        };
                        let mut params = event.get("params").cloned().unwrap_or_else(|| json!({}));
                        params["sessionId"] = json!(target);
                        let response = writer.request(method, params).await;
                        let response = if response.is_null() {
                            json!({"type":"client_response","requestId":request_id,"cancelled":true})
                        } else {
                            match validate_client_response(method, &response) {
                                Ok(result) => json!({"type":"client_response","requestId":request_id,"result":result}),
                                Err(error) => json!({"type":"client_response","requestId":request_id,"error":error}),
                            }
                        };
                        self.send(response).await?;
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(skipped)) => return Err(format!("native client request event stream lost {skipped} events")),
                    Err(broadcast::error::RecvError::Closed) => return Err("native client request event stream closed".to_owned()),
                }
            }
        }
    }

    async fn send(&self, value: Value) -> Result<(), String> {
        self.rpc.notify(value).await
    }

    /// Acquire a neutral bounded history reader and its full-replay run lease.
    pub async fn history_read(&self) -> Result<rpi_extcore::history::page::HistoryRead, String> {
        rpi_extcore::history::page::HistoryRead::begin(self.rpc.clone()).await
    }

    /// Observe runtime state without taking the foreground run lease.
    pub async fn runtime_snapshot(&self) -> rpi_extcore::host::HostSnapshot {
        self.rpc.snapshot().await
    }

    /// Wait for backend startup and obtain the authoritative active branch.
    pub async fn state(&self) -> Result<Value, String> {
        self.rpc.state().await
    }

    pub fn supports_command(&self, command: &str) -> bool {
        self.rpc.supports_command(command)
    }

    pub async fn available_commands(&self) -> Result<Value, String> {
        self.rpc.available_commands().await
    }
    pub async fn context_usage(&self) -> Result<Value, String> {
        self.rpc.context_usage().await
    }

    pub async fn stats(&self) -> Result<Value, String> {
        self.rpc.stats().await
    }
    pub async fn set_compaction_enabled(&self, enabled: bool) -> Result<(), String> {
        self.rpc.set_compaction_enabled(enabled).await
    }
    pub async fn execute_command(&self, command: &str, args: &str) -> Result<Value, String> {
        self.rpc.execute_command(command, args).await
    }
    pub async fn compact(&self, instructions: Option<&str>) -> Result<Value, String> {
        self.rpc.compact(instructions).await
    }

    pub async fn modes(&self) -> Result<Value, String> {
        self.rpc.modes().await
    }
    pub async fn set_mode(&self, mode_id: &str) -> Result<Value, String> {
        self.rpc.set_mode(mode_id).await
    }
    /// Apply ACP lifecycle instructions through the neutral idle-only core API.
    pub async fn configure_system_prompt(&self, prompt: &crate::system_prompt::SystemPrompt) -> Result<(), String> {
        rpi_extcore::system_prompt::configure(&self.rpc, prompt.mode, &prompt.text).await
    }

    pub async fn state_unlocked(&self) -> Result<Value, String> {
        self.rpc.state_unlocked().await
    }
    pub async fn available_models(&self) -> Result<Value, String> {
        self.rpc.available_models().await
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

    /// v1-compatible prompt entry point. The v2-capable entry point below
    /// retains the canonical assistant message ID.
    pub async fn prompt(
        self: &Arc<Self>,
        writer: &Arc<Writer>,
        acp_session_id: &str,
        blocks: &[ContentBlock],
    ) -> Result<&'static str, String> {
        Ok(self
            .prompt_with_result(writer, acp_session_id, blocks)
            .await?
            .stop_reason)
    }

    pub async fn prompt_with_result(
        self: &Arc<Self>,
        writer: &Arc<Writer>,
        acp_session_id: &str,
        blocks: &[ContentBlock],
    ) -> Result<PromptResult, String> {
        self.prompt_with_responder(writer, acp_session_id, blocks, None).await
    }

    pub(crate) async fn prompt_with_responder(
        self: &Arc<Self>,
        writer: &Arc<Writer>,
        acp_session_id: &str,
        blocks: &[ContentBlock],
        mut responder: Option<&mut crate::protocol::PromptResponder>,
    ) -> Result<PromptResult, String> {
        let content = prompt_text(blocks);
        let parts = prompt_content(blocks);
        let has_media = blocks.iter().any(|block| matches!(block, ContentBlock::Image { .. } | ContentBlock::Audio { .. }));
        let mut user_message_id = String::new();
        // v2 requires native insertion, including for slash input. Local v1
        // shortcuts must not manufacture a retained conversation message.
        if let Some((command, args)) = slash_command(&content).filter(|_| !writer.supports_v2() && !has_media) {
            if command == "compact" {
                let compacted = self.run_compaction(writer, acp_session_id, &args).await;
                if writer.supports_v2() {
                    if let Ok(stop_reason) = &compacted {
                        emit_v2_idle(writer, acp_session_id, stop_reason).await;
                    }
                }
                return compacted.map(|stop_reason| PromptResult {
                    stop_reason,
                    message_id: user_message_id.clone(),
                });
            }
            if self.supports_command("get_commands") && self.supports_command("execute_command") {
                let known = self
                    .available_commands()
                    .await
                    .ok()
                    .and_then(|commands| commands.get("commands").cloned())
                    .and_then(|commands| commands.as_array().cloned())
                    .into_iter()
                    .flatten()
                    .filter_map(|item| item.get("name").and_then(Value::as_str).map(str::to_owned))
                    .any(|name| name.trim_start_matches('/') == command);
                if known {
                    let result = self.execute_command_and_wait(&command, &args).await?;
                    let text = command_result_text(&result);
                    if !text.is_empty() {
                        writer
                            .notify(
                                "session/update",
                                json!({
                                    "sessionId": acp_session_id,
                                    "update": text_update_with_id(
                                        json!({"kind":"text","text":text}),
                                        &format!("command:{acp_session_id}:{}", uuid::Uuid::new_v4()),
                                    ),
                                }),
                            )
                            .await;
                    }
                    self.emit_usage(writer, acp_session_id).await;
                    if writer.supports_v2() {
                        emit_v2_idle(writer, acp_session_id, "end_turn").await;
                    }
                    return Ok(PromptResult {
                        stop_reason: "end_turn",
                        message_id: user_message_id.clone(),
                    });
                }
            }
        }
        let mut run = self
            .rpc
            .begin_prompt_content(parts)
            .await?;
        let mut message_ids = MessageIds::new(acp_session_id);
        let mut cancelled = false;
        let mut dialogs = DialogTasks::default();
        let mut client_requests = ClientRequestTasks::default();

        let stop_reason = 'prompt: loop {
            let event = tokio::select! {
                biased;
                _ = run.cancellation.changed(), if !cancelled => {
                    cancelled = true;
                    dialogs.cancel_all(writer, acp_session_id).await;
                    client_requests.cancel_interactive();
                    continue;
                }
                result = client_requests.tasks.join_next(), if !client_requests.tasks.is_empty() => {
                    if let Some(Ok(id)) = result { client_requests.pending.remove(&id); }
                    continue;
                }
                event = run.events.recv() => event,
            };
            match event {
                Ok(mut value) => {
                    if matches!(value["type"].as_str(), Some("client_request" | "ui_dialog_request")) {
                        let target = match writer.interaction_target(acp_session_id, &value).await {
                            Ok(target) => target,
                            Err(error) => {
                                crate::diagln!("[rpi-acp] interaction routing failed: {error}; terminating native host");
                                self.rpc.terminate().await;
                                break 'prompt Err(error);
                            }
                        };
                        value["sessionId"] = json!(target);
                        if value["params"].is_object() {
                            value["params"]["sessionId"] = json!(target);
                        }
                    }
                    message_ids.observe(&value);
                    if user_message_id.is_empty() {
                        if let Some(id) = canonical_user_message_id(&value) {
                            user_message_id = id.to_owned();
                            if writer.supports_v2() {
                                emit_v2_prompt_start(writer, acp_session_id, &user_message_id, blocks).await;
                                if let Some(response) = responder.as_deref_mut() {
                                    response.inserted(writer, &user_message_id).await;
                                }
                            }
                        }
                    }
                    let progress = run.tracker.observe(&value, cancelled);
                    for delta in progress.deltas {
                        writer
                            .notify(
                                "session/update",
                                json!({"sessionId":acp_session_id,"update":text_update_with_ids(delta, &message_ids)}),
                            )
                            .await;
                    }
                    if let Some(outcome) = progress.outcome {
                        self.emit_usage(writer, acp_session_id).await;
                        break 'prompt outcome.map(|outcome| match outcome {
                            rpi_extcore::run::RunOutcome::Completed => "end_turn",
                            rpi_extcore::run::RunOutcome::Cancelled => "cancelled",
                            rpi_extcore::run::RunOutcome::TokenLimit => "max_tokens",
                        });
                    }
                    // A plugin opened a UI dialog (e.g. a permission gate). Ask
                    // the ACP client, then answer the plugin.
                    if value.get("type").and_then(Value::as_str) == Some("client_request") {
                        let target = value["sessionId"].as_str().unwrap();
                        let cleanup = is_terminal_cleanup(&value, target);
                        if (cancelled || writer.is_input_closed()) && !cleanup {
                            if let Some(request_id) = value.get("requestId").and_then(Value::as_str)
                            {
                                let _ = self.send(json!({"type":"client_response","requestId":request_id,"cancelled":true})).await;
                            }
                        } else {
                            client_requests.open(self.clone(), writer.clone(), value, cleanup);
                        }
                        continue;
                    }
                    if value.get("type").and_then(Value::as_str) == Some("client_request_closed") {
                        if let Some(id) = value.get("requestId").and_then(Value::as_str) {
                            client_requests.close(id).await;
                        }
                        continue;
                    }
                    if value.get("type").and_then(Value::as_str) == Some("ui_dialog_request") {
                        if cancelled || writer.is_input_closed() {
                            if let Some(request_id) = value.get("requestId").and_then(Value::as_str)
                            {
                                let _ = self.send(json!({"type":"ui_dialog_response","requestId":request_id,"cancelled":true})).await;
                            }
                        } else {
                            dialogs.open(
                                self.clone(),
                                writer.clone(),
                                value["sessionId"].as_str().unwrap().to_owned(),
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
                        let update = if writer.supports_v2() {
                            crate::v2::map_session_update(update)
                        } else {
                            update
                        };
                        writer
                            .notify(
                                "session/update",
                                json!({"sessionId": acp_session_id, "update": update}),
                            )
                            .await;
                    }
                    if let Some(compaction) = compaction_update(&value, self, acp_session_id).await
                    {
                        if writer.supports_compaction() {
                            writer
                                .notify(
                                    "session/update",
                                    json!({"sessionId":acp_session_id,"update":compaction}),
                                )
                                .await;
                        }
                    }
                    if let Some(notice) = notice_update(&value) {
                        if writer.supports_notices() {
                            writer
                                .notify(
                                    "session/update",
                                    json!({"sessionId":acp_session_id,"update":notice}),
                                )
                                .await;
                        }
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
        // Native settlement follows resource-owner cleanup. Do not cancel
        // already-forwarded kill/release calls while reporting an idle state.
        client_requests.cancel_interactive();
        client_requests.drain().await;
        dialogs.cancel_all(writer, acp_session_id).await;
        let child_failure = self.child_watcher.lock().await.as_ref().and_then(|w| w.failure());
        let stop_reason = if let Some(error) = child_failure {
            Err(format!("subagent forwarding failed: {error}"))
        } else if writer.supports_v2() && user_message_id.is_empty() {
            Err("native prompt settled without canonical user insertion".to_owned())
        } else if cancelled && stop_reason.is_err() {
            Ok("cancelled")
        } else {
            stop_reason
        };
        let result = stop_reason.map(|stop_reason| PromptResult {
            stop_reason,
            message_id: user_message_id,
        });
        if writer.supports_v2() {
            if let Ok(result) = &result {
                writer
                    .notify(
                        "session/update",
                        json!({
                            "sessionId": acp_session_id,
                            "update": {
                                "sessionUpdate": "state_update",
                                "state": "idle",
                                "stopReason": result.stop_reason
                            }
                        }),
                    )
                    .await;
            }
        }
        result
    }

    async fn execute_command_and_wait(&self, command: &str, args: &str) -> Result<Value, String> {
        // Subscribe before dispatch. An extension command may hand work to the
        // native run lane and return its run ID before the authoritative
        // settlement event; subscribing afterwards loses that boundary.
        let mut events = self.rpc.subscribe_raw();
        let result = self.execute_command(command, args).await?;
        let Some(run_id) = result
            .get("runId")
            .or_else(|| result.get("run_id"))
            .and_then(Value::as_str)
        else {
            return Ok(result);
        };
        let settled = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                let event = events
                    .recv()
                    .await
                    .map_err(|error| format!("command event stream failed: {error}"))?;
                if event.get("type").and_then(Value::as_str) != Some("agent_settled") {
                    continue;
                }
                if event.get("runId").and_then(Value::as_str) != Some(run_id) {
                    continue;
                }
                break Ok::<Value, String>(event);
            }
        })
        .await
        .map_err(|_| "command run settlement timed out".to_owned())??;
        match settled.get("outcome").and_then(Value::as_str) {
            Some("completed") => Ok(result),
            Some("aborted") | Some("cancelled") => Err("command run was cancelled".to_owned()),
            Some(outcome) => Err(settled
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("command run ended with {outcome}"))),
            None => Err("command run settled without an outcome".to_owned()),
        }
    }

    async fn emit_usage(&self, writer: &Arc<Writer>, session_id: &str) {
        // Optional RPC queries must be capability-gated. Older native hosts (and
        // the compatibility fixture) do not answer them; querying one after a
        // settled prompt would otherwise delay the ACP response until the RPC
        // timeout and make a successful run look hung.
        if !self.supports_command("get_context_usage") {
            return;
        }
        let Ok(context) =
            tokio::time::timeout(std::time::Duration::from_millis(750), self.context_usage())
                .await
                .unwrap_or_else(|_| Err("context usage query timed out".to_owned()))
        else {
            return;
        };
        let Some(used) = context.get("tokens").and_then(Value::as_u64) else {
            return;
        };
        let Some(size) = context.get("window").and_then(Value::as_u64) else {
            return;
        };
        let mut update = json!({
            "sessionUpdate": "usage_update",
            "used": used,
            "size": size,
        });
        if self.supports_command("get_stats") {
            if let Ok(Ok(stats)) =
                tokio::time::timeout(std::time::Duration::from_millis(750), self.stats()).await
            {
                if let Some(amount) = stats.get("costTotal").and_then(Value::as_f64) {
                    if amount.is_finite() {
                        update["cost"] = json!({"amount": amount, "currency": "USD"});
                    }
                }
            }
        }
        writer
            .notify(
                "session/update",
                json!({"sessionId": session_id, "update": update}),
            )
            .await;
    }

    async fn run_compaction(
        &self,
        writer: &Arc<Writer>,
        session_id: &str,
        instructions: &str,
    ) -> Result<&'static str, String> {
        if !self.supports_command("compact") {
            return Err("native host does not support compaction".to_owned());
        }
        let id = uuid::Uuid::new_v4().to_string();
        if writer.supports_compaction() {
            writer
                .notify(
                    "session/update",
                    json!({"sessionId":session_id,"update":{
                        "sessionUpdate":"compaction_update",
                        "compactionId":id,
                        "status":"in_progress"
                    }}),
                )
                .await;
        }
        let result = self
            .compact((!instructions.is_empty()).then_some(instructions))
            .await;
        let (status, summary, error, reason): (&str, Option<String>, Option<String>, &str) =
            match result {
                Ok(value) if value["outcome"] == "compacted" => (
                    "completed",
                    value
                        .get("summary")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    None,
                    "end_turn",
                ),
                Ok(value) if value["outcome"] == "aborted" => (
                    "cancelled",
                    value
                        .get("summary")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    None,
                    "cancelled",
                ),
                Ok(value) => {
                    let message = value
                        .get("summary")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| "Native compaction declined".to_owned());
                    ("failed", Some(message.clone()), Some(message), "end_turn")
                }
                Err(error) => ("failed", None, Some(error), "end_turn"),
            };
        if writer.supports_compaction() {
            let mut update = json!({
                "sessionUpdate":"compaction_update",
                "compactionId":id,
                "status":status,
            });
            if let Some(summary) = summary {
                update["summary"] = json!([{"type":"text","text":summary}]);
            }
            if let Some(error) = error.as_ref() {
                update["error"] = json!(error);
            }
            writer
                .notify(
                    "session/update",
                    json!({"sessionId":session_id,"update":update}),
                )
                .await;
        }
        self.emit_usage(writer, session_id).await;
        match status {
            "failed" => Err(error.unwrap_or_else(|| "native compaction failed".to_owned())),
            _ => Ok(reason),
        }
    }
    /// Replace runtime-only MCP resources while holding the native foreground lease.
    pub async fn configure_mcp(&self, servers: crate::mcp::Servers, id: &str, cwd: &Path, writer: &Arc<Writer>) -> Result<(), String> {
        crate::mcp_acp::register(writer, &servers.2)?;
        let mut current = self.mcp_config.lock().await;
        if *current == servers { return Ok(()); }
        let mut oauth = if servers.requires_managed_http_auth() {
            crate::mcp_oauth::options(writer, &self.mcp_input_scope.session_id)?
        } else {
            rpi_extcore::mcp::McpOptions::default()
        };
        oauth.owner = Some(self.mcp_owner.clone());
        oauth.catalog = writer.mcp_catalog;
        let _lease = self.rpc.try_run_lease()?;
        oauth.sampling = crate::mcp_sampling::options(writer, &self.rpc,
            &self.mcp_input_scope.session_id, cwd, &servers,
            &mut *self.sampling_owner.lock().await)?;
        let sampling = oauth.sampling.clone();
        let mut binding = self.mcp.lock().await;
        if let Some(old) = binding.as_mut() {
            if old.shutdown().await.is_err() {
                let _ = self.mcp_owner.shutdown().await;
                if let Some(owner) = self.sampling_owner.lock().await.as_ref() { let _ = owner.shutdown().await; }
                self.rpc.terminate().await;
                return Err("MCP shutdown failed; session closed".into());
            }
        }
        *binding = None;
        let config = servers.clone().config(id, cwd);
        let attached = if servers.1.is_empty() {
            rpi_extcore::mcp::attach_with_options(self.rpc.clone(), config, oauth).await
        } else {
            rpi_extcore::mcp_external::attach_with_runtime_options(self.rpc.clone(), config,
                crate::mcp_acp::external(writer, servers.1.clone()),
                crate::mcp_input::options(writer, &self.mcp_input_scope, sampling, crate::mcp_sampling::configured(&servers))?, oauth).await
        };
        match attached {
            Ok(new) => { *binding = Some(new); *current = servers; Ok(()) }
            Err(_) => {
                let _ = self.mcp_owner.shutdown().await;
                if let Some(owner) = self.sampling_owner.lock().await.as_ref() { let _ = owner.shutdown().await; }
                self.rpc.terminate().await;
                Err("MCP attachment failed; session closed".into())
            }
        }
    }

    /// Kill the child (session/close, process shutdown).
    pub async fn kill(&self) {
        if let Some(mut watcher) = self.child_watcher.lock().await.take() {
            watcher.stop().await;
        }
        // Cancel and join even unpublished discovery before terminating RPC.
        let _ = self.mcp_owner.shutdown().await;
        *self.mcp.lock().await = None;
        if let Some(owner) = self.sampling_owner.lock().await.as_ref() {
            let _ = owner.shutdown().await;
        }
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
async fn compaction_update(event: &Value, _session: &Session, _session_id: &str) -> Option<Value> {
    let kind = event.get("type").and_then(Value::as_str)?;
    let (status, id) = match kind {
        "compaction_started" => ("in_progress", event.get("compactionId")?.as_str()?),
        "compaction_finished" => (
            event
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("failed"),
            event.get("compactionId")?.as_str()?,
        ),
        _ => return None,
    };
    let mut update = json!({
        "sessionUpdate": "compaction_update",
        "compactionId": id,
        "status": status,
    });
    if let Some(summary) = event.get("summary").and_then(Value::as_str) {
        update["summary"] = json!([{"type":"text","text":summary}]);
    }
    if let Some(error) = event.get("error").and_then(Value::as_str) {
        update["error"] = json!(error);
    }
    Some(update)
}

fn notice_update(event: &Value) -> Option<Value> {
    if event.get("type").and_then(Value::as_str) != Some("retry_scheduled") {
        return None;
    }
    let attempt = event.get("attempt").and_then(Value::as_u64).unwrap_or(0);
    let max = event.get("maxRetries").and_then(Value::as_u64).unwrap_or(0);
    let delay = event.get("delayMs").and_then(Value::as_u64).unwrap_or(0);
    let detail = event
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("provider retry");
    Some(json!({
        "sessionUpdate": "notice",
        "severity": "warning",
        "title": "Retry scheduled",
        "description": format!("Attempt {attempt}/{max} in {delay} ms: {detail}"),
    }))
}
fn slash_command(text: &str) -> Option<(String, String)> {
    let line = text.lines().next()?.trim();
    let command = line.strip_prefix('/')?;
    let (name, args) = command.split_once(' ').unwrap_or((command, ""));
    if name.is_empty() || name.contains('/') {
        return None;
    }
    Some((name.to_owned(), args.trim().to_owned()))
}

fn command_result_text(value: &Value) -> String {
    for candidate in ["text", "message", "output"] {
        if let Some(text) = value.get(candidate).and_then(Value::as_str) {
            return text.to_owned();
        }
    }
    let result = value.get("result").unwrap_or(value);
    match result {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        _ => result.to_string(),
    }
}

struct MessageIds {
    assistant: String,
    thought: String,
}

impl MessageIds {
    fn new(session_id: &str) -> Self {
        let assistant = format!("assistant:{session_id}:fallback:{}", uuid::Uuid::new_v4());
        Self {
            thought: format!("{assistant}:thought"),
            assistant,
        }
    }

    fn observe(&mut self, event: &Value) {
        if !matches!(
            event["type"].as_str(),
            Some("message_start" | "message_update" | "message_end")
        ) || event["message"]["role"] != "assistant"
        {
            return;
        }
        let Some(id) = event["message"]["messageId"]
            .as_str()
            .or_else(|| event["message"]["id"].as_str())
            .or_else(|| event["messageId"].as_str())
            .filter(|id| !id.is_empty())
        else {
            return;
        };
        self.assistant = id.to_owned();
        self.thought = format!("{id}:thought");
    }

    fn for_kind(&self, kind: &str) -> &str {
        if kind == "thinking" {
            &self.thought
        } else {
            &self.assistant
        }
    }
}

fn text_update_with_id(delta: Value, message_id: &str) -> Value {
    let kind = if delta["kind"] == "thinking" {
        "agent_thought_chunk"
    } else {
        "agent_message_chunk"
    };
    json!({
        "sessionUpdate":kind,
        "content":{"type":"text","text":delta["text"]},
        "messageId":message_id,
    })
}

fn text_update_with_ids(delta: Value, ids: &MessageIds) -> Value {
    let kind = delta["kind"].as_str().unwrap_or("text").to_owned();
    text_update_with_id(delta, ids.for_kind(&kind))
}

#[cfg(test)]
fn text_update(delta: Value) -> Value {
    text_update_with_id(delta, "test:assistant")
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

/// Owns native-triggered agent → client filesystem and terminal requests.
/// Each request has a cancellation channel separate from the native event
/// stream, so a stalled client cannot stop event processing or survive session
/// cancellation.
#[derive(Default)]
struct ClientRequestTasks {
    tasks: JoinSet<String>,
    pending: HashMap<String, (watch::Sender<bool>, bool)>,
}

impl ClientRequestTasks {
    fn open(&mut self, session: Arc<Session>, writer: Arc<Writer>, event: Value, cleanup: bool) {
        let Some(request_id) = event
            .get("requestId")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        else {
            return;
        };
        if self.pending.contains_key(request_id) {
            return;
        }
        let request_id = request_id.to_owned();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let completed_id = request_id.clone();
        self.tasks.spawn(async move {
            handle_client_request(&session, &writer, &event, cancel_rx).await;
            completed_id
        });
        self.pending.insert(request_id, (cancel_tx, cleanup));
    }

    async fn close(&mut self, request_id: &str) {
        if let Some((cancel, _)) = self.pending.remove(request_id) {
            let _ = cancel.send(true);
        }
    }

    fn cancel_interactive(&mut self) {
        for (cancel, cleanup) in self.pending.values() {
            if !cleanup {
                let _ = cancel.send(true);
            }
        }
    }

    async fn drain(&mut self) {
        while let Some(result) = self.tasks.join_next().await {
            if let Ok(id) = result {
                self.pending.remove(&id);
            }
        }
    }
}

/// Only native resource-owner teardown may bypass prompt cancellation. Neither
/// terminal creation/wait nor arbitrary filesystem work is cleanup.
fn is_terminal_cleanup(event: &Value, session_id: &str) -> bool {
    event.get("requestId").and_then(Value::as_str).is_some_and(|id| !id.trim().is_empty())
        && client_request_spec(event).is_ok_and(|(method, params)| {
            matches!(method, "terminal/kill" | "terminal/release")
                && params.get("sessionId").and_then(Value::as_str) == Some(session_id)
        })
}

fn canonical_user_message_id(event: &Value) -> Option<String> {
    if !matches!(event["type"].as_str(), Some("message_start" | "message_end"))
        || (event["message"]["role"] != "user" && event["message"]["kind"] != "user")
    {
        return None;
    }
    let id = event["message"]["messageId"].as_str()
        .or_else(|| event["message"]["id"].as_str())
        .or_else(|| event["messageId"].as_str())
        .filter(|id| !id.trim().is_empty())?;
    // Native typed user events carry the committed entry identity. History
    // namespaces legacy user entries this way; keep insertion and load identical.
    Some(if event["message"]["kind"] == "user" && !id.starts_with("history:") {
        format!("history:{id}")
    } else {
        id.to_owned()
    })
}

const CLIENT_REQUEST_METHODS: &[&str] = &[
    "fs/read_text_file",
    "fs/write_text_file",
    "terminal/create",
    "terminal/output",
    "terminal/wait_for_exit",
    "terminal/kill",
    "terminal/release",
];

fn client_request_spec(event: &Value) -> Result<(&str, Value), String> {
    let method = event
        .get("method")
        .and_then(Value::as_str)
        .filter(|method| CLIENT_REQUEST_METHODS.contains(method))
        .ok_or_else(|| "unsupported or missing client request method".to_owned())?;
    let mut params = event
        .get("params")
        .cloned()
        .ok_or_else(|| "client request params are required".to_owned())?;
    if !params.is_object() {
        return Err("client request params must be an object".to_owned());
    }
    if let Some(session_id) = event.get("sessionId").and_then(Value::as_str) {
        if params.get("sessionId").is_some_and(|nested| nested.as_str() != Some(session_id)) {
            return Err("client request params.sessionId conflicts with trusted source".into());
        }
        params["sessionId"] = json!(session_id);
    }
    validate_client_params(method, &params)?;
    Ok((method, params))
}

fn validate_client_params(method: &str, params: &Value) -> Result<(), String> {
    let object = params
        .as_object()
        .ok_or_else(|| "client request params must be an object".to_owned())?;
    match method {
        "fs/read_text_file" => {
            let path = object
                .get("path")
                .and_then(Value::as_str)
                .filter(|path| !path.trim().is_empty())
                .ok_or_else(|| "fs/read_text_file path is required".to_owned())?;
            if !Path::new(path).is_absolute() {
                return Err("fs/read_text_file path must be absolute".to_owned());
            }
            for name in ["line", "limit"] {
                if let Some(value) = object.get(name) {
                    if !value.is_null() && value.as_u64().is_none() {
                        return Err(format!(
                            "fs/read_text_file {name} must be an unsigned integer"
                        ));
                    }
                }
            }
        }
        "fs/write_text_file" => {
            let path = object
                .get("path")
                .and_then(Value::as_str)
                .filter(|path| !path.trim().is_empty())
                .ok_or_else(|| "fs/write_text_file path is required".to_owned())?;
            if !Path::new(path).is_absolute() {
                return Err("fs/write_text_file path must be absolute".to_owned());
            }
            if !object.get("content").is_some_and(Value::is_string) {
                return Err("fs/write_text_file content must be a string".to_owned());
            }
        }
        "terminal/create" => {
            if !object
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| !command.trim().is_empty())
            {
                return Err("terminal/create command is required".to_owned());
            }
            if let Some(cwd) = object.get("cwd").and_then(Value::as_str) {
                if !Path::new(cwd).is_absolute() {
                    return Err("terminal/create cwd must be absolute".to_owned());
                }
            }
            if let Some(args) = object.get("args") {
                if !args.is_array()
                    || args
                        .as_array()
                        .is_some_and(|args| args.iter().any(|arg| !arg.is_string()))
                {
                    return Err("terminal/create args must be an array of strings".to_owned());
                }
            }
            if let Some(env) = object.get("env") {
                if !env.is_array() {
                    return Err("terminal/create env must be an array".to_owned());
                }
            }
        }
        "terminal/output" | "terminal/wait_for_exit" | "terminal/kill" | "terminal/release" => {
            if !object
                .get("terminalId")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.trim().is_empty())
            {
                return Err(format!("{method} terminalId is required"));
            }
        }
        _ => return Err("unsupported client request method".to_owned()),
    }
    Ok(())
}

fn validate_client_response(method: &str, result: &Value) -> Result<Value, String> {
    if let Some(error) = result.get("__acp_error") {
        return Err(format!("client rejected {method}: {error}"));
    }
    let object = result
        .as_object()
        .ok_or_else(|| format!("client {method} response must be an object"))?;
    match method {
        "fs/read_text_file" => {
            if !object.get("content").is_some_and(Value::is_string) {
                return Err("fs/read_text_file response requires string content".to_owned());
            }
        }
        "terminal/create" => {
            if object
                .get("terminalId")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                return Err("terminal/create response requires terminalId".to_owned());
            }
        }
        "terminal/output"
            if !object.get("output").is_some_and(Value::is_string)
                || !object.get("truncated").is_some_and(Value::is_boolean) =>
        {
            return Err("terminal/output response requires output and truncated".to_owned());
        }
        _ => {}
    }
    Ok(result.clone())
}

async fn handle_client_request(
    session: &Session,
    writer: &Arc<Writer>,
    event: &Value,
    mut cancelled: watch::Receiver<bool>,
) {
    let request_id = event
        .get("requestId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let response = match client_request_spec(event) {
        Ok((method, params)) => {
            let target = event["sessionId"].as_str().unwrap_or_default();
            let result = if is_terminal_cleanup(event, target) {
                writer.request_terminal_cleanup(method, params, &mut cancelled).await
            } else {
                writer.request_cancellable(method, params, &mut cancelled).await
            };
            if result.is_null() {
                json!({"type":"client_response","requestId":request_id,"cancelled":true})
            } else {
                match validate_client_response(method, &result) {
                    Ok(result) => {
                        json!({"type":"client_response","requestId":request_id,"result":result})
                    }
                    Err(error) => {
                        json!({"type":"client_response","requestId":request_id,"error":error})
                    }
                }
            }
        }
        Err(error) => json!({"type":"client_response","requestId":request_id,"error":error}),
    };
    let _ = session.send(response).await;
}

/// Owns each pending client request separately, so the event stream keeps advancing.
#[derive(Default)]
struct DialogTasks {
    tasks: JoinSet<String>,
    pending: HashMap<String, (AbortHandle, String)>,
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
        let target = session_id.clone();
        let task = self.tasks.spawn(async move {
            handle_ui_dialog(&session, &writer, &session_id, &event).await;
            completed_id
        });
        self.pending.insert(id, (task, target));
    }

    async fn close(&mut self, id: &str, writer: &Writer, _session_id: &str) {
        if let Some((task, session_id)) = self.pending.remove(id) {
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
    if ui.get("purpose").and_then(Value::as_str) != Some("permission") {
        return None;
    }
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
    } else if writer.supports_elicitation_url()
        && event.pointer("/ui/kind").and_then(Value::as_str) == Some("url")
    {
        if let Some(params) = crate::questions::elicitation_url_request(event, session_id) {
            let result = tokio::time::timeout(
                timeout,
                writer.request(crate::questions::ELICITATION_METHOD, params),
            )
            .await
            .unwrap_or(Value::Null);
            crate::questions::elicitation_url_answer(request_id, &result)
        } else {
            json!({"type":"ui_dialog_response", "requestId":request_id, "cancelled":true})
        }
    } else if writer.supports_elicitation_form() {
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

/// Convert ACP input in place; resources use the existing text rendering at
/// their original position. Adjacent text keeps the legacy newline semantics.
fn prompt_content(blocks: &[ContentBlock]) -> Vec<rpi_extcore::host::PromptContent> {
    use rpi_extcore::host::PromptContent;
    let mut parts = Vec::new();
    let mut text_blocks = Vec::new();
    fn flush(text_blocks: &mut Vec<ContentBlock>, parts: &mut Vec<PromptContent>) {
        let text = prompt_text(text_blocks);
        if !text.is_empty() { parts.push(PromptContent::Text { text }); }
        text_blocks.clear();
    }
    for block in blocks {
        match block {
            ContentBlock::Image { data, mime_type } => {
                flush(&mut text_blocks, &mut parts);
                parts.push(PromptContent::Image { data: data.clone(), mime_type: mime_type.clone() });
            }
            ContentBlock::Audio { data, mime_type } => {
                flush(&mut text_blocks, &mut parts);
                parts.push(PromptContent::Audio { data: data.clone(), mime_type: mime_type.clone() });
            }
            _ => text_blocks.push(block.clone()),
        }
    }
    flush(&mut text_blocks, &mut parts);
    parts
}

/// Convert ACP image blocks to the native provider-neutral image shape.
///
/// The native RPC protocol deliberately carries the provider-neutral
/// `ImageContent` fields (`data` and `mimeType`) as JSON, so the ACP adapter
/// does not depend on provider-specific types.
pub fn prompt_images(blocks: &[ContentBlock]) -> Vec<Value> {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image { data, mime_type } => Some(json!({
                "data": data,
                "mimeType": mime_type,
            })),
            _ => None,
        })
        .collect()
}

/// Convert ACP audio blocks to the native provider-neutral audio shape.
pub fn prompt_audio(blocks: &[ContentBlock]) -> Vec<Value> {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Audio { data, mime_type } => Some(json!({
                "data": data,
                "mimeType": mime_type,
            })),
            _ => None,
        })
        .collect()
}

async fn emit_v2_prompt_start(
    writer: &Arc<Writer>,
    session_id: &str,
    message_id: &str,
    blocks: &[ContentBlock],
) {
    writer
        .notify(
            "session/update",
            json!({
                "sessionId": session_id,
                "update": {"sessionUpdate": "state_update", "state": "running"}
            }),
        )
        .await;
    writer
        .notify(
            "session/update",
            json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "user_message",
                    "messageId": message_id,
                    "content": v2_prompt_content(blocks)
                }
            }),
        )
        .await;
}

async fn emit_v2_idle(writer: &Arc<Writer>, session_id: &str, stop_reason: &str) {
    writer
        .notify(
            "session/update",
            json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "state_update",
                    "state": "idle",
                    "stopReason": stop_reason
                }
            }),
        )
        .await;
}

fn v2_prompt_content(blocks: &[ContentBlock]) -> Vec<Value> {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(json!({"type": "text", "text": text})),
            ContentBlock::Image { data, mime_type } => Some(json!({
                "type": "image",
                "data": data,
                "mimeType": mime_type,
            })),
            ContentBlock::Audio { data, mime_type } => Some(json!({
                "type": "audio",
                "data": data,
                "mimeType": mime_type,
            })),
            ContentBlock::ResourceLink { name, uri } => Some(json!({
                "type": "resource_link",
                "name": name.as_deref().unwrap_or(uri),
                "uri": uri,
            })),
            ContentBlock::Resource { resource } => Some(crate::v2::prompt_resource(
                &resource.uri,
                resource.text.as_deref(),
            )),
            ContentBlock::Other => None,
        })
        .collect()
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
            ContentBlock::Image { .. } | ContentBlock::Audio { .. } => {}
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
    // A friendly label is not a locator: two attached roots can have the same
    // basename. Keep the exact URI in native prompts/history without fetching it.
    match name.filter(|name| !name.is_empty() && *name != uri) {
        Some(name) => format!("{name} ({uri})"),
        None => uri.to_owned(),
    }
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
    include!("../tests/fixtures/eof_cleanup_session.rs");

    #[test]
    fn canonical_user_identity_matches_native_history_without_fallbacks() {
        for kind in ["message_start", "message_end"] {
            assert_eq!(canonical_user_message_id(&json!({"type":kind,"message":{"kind":"user","messageId":"entry-a"}})), Some("history:entry-a".into()));
            assert_eq!(canonical_user_message_id(&json!({"type":kind,"message":{"role":"user","messageId":"legacy-id"}})), Some("legacy-id".into()));
            assert_eq!(canonical_user_message_id(&json!({"type":kind,"message":{"kind":"user","messageId":"history:entry-a"}})), Some("history:entry-a".into()));
            for message in [json!({"kind":"user"}), json!({"kind":"user","messageId":" "}), json!({"kind":"assistant","messageId":"assistant-uuid"})] {
                assert_eq!(canonical_user_message_id(&json!({"type":kind,"message":message})), None);
            }
        }
    }

    #[test]
    fn client_request_spec_accepts_native_fs_and_terminal_envelopes() {
        let event = json!({
            "type": "client_request",
            "requestId": "r1",
            "sessionId": "s1",
            "method": "fs/read_text_file",
            "params": {"path": "C:/workspace/a.txt"}
        });
        let (method, params) = client_request_spec(&event).unwrap();
        assert_eq!(method, "fs/read_text_file");
        assert_eq!(params["sessionId"], "s1");
        assert!(client_request_spec(&json!({
            "requestId": "r2", "method": "terminal/create", "params": {}
        }))
        .is_err());
        assert!(client_request_spec(&json!({
            "requestId": "r3", "method": "mcp/call", "sessionId": "s1", "params": {}
        }))
        .is_err());
    }

    #[test]
    fn client_response_validation_rejects_malformed_successes() {
        assert!(validate_client_response("fs/read_text_file", &json!({"content":"ok"})).is_ok());
        assert!(validate_client_response("fs/read_text_file", &json!({"content":1})).is_err());
        assert!(validate_client_response(
            "terminal/output",
            &json!({"output":"x","truncated":false})
        )
        .is_ok());
        assert!(validate_client_response("terminal/output", &json!({"output":"x"})).is_err());
        assert!(validate_client_response(
            "terminal/create",
            &json!({"__acp_error":{"code":-1,"message":"denied"}})
        )
        .is_err());
    }
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
            mcp: Default::default(), mcp_owner: Default::default(),
            child_watcher: Default::default(),
            sampling_owner: Default::default(),
            mcp_config: Default::default(),
            mcp_input_scope: Default::default(),
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
    async fn mcp_unchanged_and_unrelated_configs_ignore_invalid_oauth_policy() {
        let (session, _commands) = test_session();
        let mut writer = Writer::with_output(tokio::io::sink());
        writer.mcp_oauth_policy = Err("fixed-policy-error");
        let writer = Arc::new(writer);
        let cwd = std::env::current_dir().unwrap();
        // Initial empty config is a real no-op, even with a broken opt-in policy.
        session.configure_mcp(crate::mcp::Servers::default(), "root", &cwd, &writer).await.unwrap();
        for v2 in [false, true] {
            let params = json!({"mcpServers":[{"type":"http", "name":"managed", "url":"https://mcp.example/mcp", "headers":[]}]});
            let servers = crate::mcp::parse(&params, v2, false).unwrap();
            // Changed managed HTTP must still fail closed, before attachment.
            assert_eq!(session.configure_mcp(servers.clone(), "root", &cwd, &writer).await.unwrap_err(), "fixed-policy-error");
            // Already-configured fast return must not provision again.
            *session.mcp_config.lock().await = servers.clone();
            session.configure_mcp(servers, "root", &cwd, &writer).await.unwrap();
            *session.mcp_config.lock().await = crate::mcp::Servers::default();
            let static_params = json!({"mcpServers":[{"type":"http", "name":"static", "url":"https://mcp.example/mcp", "headers":[{"name":"aUtHoRiZaTiOn", "value":"Bearer static"}]}]});
            let servers = crate::mcp::parse(&static_params, v2, false).unwrap();
            // No native MCP capability in this fixture: reaching attachment proves
            // the unrelated invalid policy and missing URL capability were skipped.
            assert_eq!(session.configure_mcp(servers, "root", &cwd, &writer).await.unwrap_err(), "MCP attachment failed; session closed");
        }
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
    async fn system_prompt_maps_official_modes_under_native_lease() {
        let (input, output) = tokio::io::duplex(8192);
        let (rpc, events) = HostSession::test_connection_with_ready(input, 64,
            json!({"type":"ready","protocol":"rpi","supportedCommands":["set_system_prompt"]}));
        let session = Arc::new(Session {
            rpc: rpc.clone(), durable: None, mcp: Default::default(), mcp_owner: Default::default(),
            child_watcher: Default::default(),
            sampling_owner: Default::default(),
            mcp_config: Default::default(), mcp_input_scope: Default::default(), events: Some(events),
        });
        let mut lines = BufReader::new(output).lines();
        for (mode, native) in [("append", "append"), ("override", "replace")] {
            let prompt = crate::system_prompt::parse(&json!({"systemPrompt":{
                "mode":mode,"content":[{"type":"text","text":"first"},{"type":"text","text":"second"}]
            }}), true).unwrap().unwrap();
            let owner = session.clone();
            let call = tokio::spawn(async move { owner.configure_system_prompt(&prompt).await });
            let command = read_json(&mut lines).await;
            assert_eq!(command["type"], "set_system_prompt");
            assert_eq!(command["mode"], native);
            assert_eq!(command["prompt"], "first\nsecond");
            assert!(rpc.try_run_lease().is_err());
            session.events.as_ref().unwrap().send(json!({"type":"response","id":command["id"],"success":true})).unwrap();
            call.await.unwrap().unwrap();
            assert!(rpc.try_run_lease().is_ok());
        }
        let lease = rpc.try_run_lease().unwrap();
        let prompt = crate::system_prompt::SystemPrompt { mode: rpi_extcore::system_prompt::Mode::Append, text:"busy".into() };
        assert!(session.configure_system_prompt(&prompt).await.unwrap_err().contains("already running"));
        drop(lease);
        session.kill().await;
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
            mcp: Default::default(), mcp_owner: Default::default(),
            child_watcher: Default::default(),
            sampling_owner: Default::default(),
            mcp_config: Default::default(),
            mcp_input_scope: Default::default(),
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
        let text = prompt_text(&blocks);
        assert!(text.starts_with("explain"));
        assert!(text.contains("@main.rs"));
        assert!(text.contains("file:///a/main.rs"));
        assert!(text.contains("<file uri=\"file:///a/lib.rs\">\nfn main() {}\n</file>"));
    }

    #[test]
    fn resource_links_keep_full_uri_even_with_colliding_display_names() {
        let blocks = vec![
            ContentBlock::ResourceLink { name: Some("src".into()), uri: "file:///a/src/".into() },
            ContentBlock::ResourceLink { name: Some("src".into()), uri: "file:///b/src/".into() },
            ContentBlock::ResourceLink { name: None, uri: "https://example.invalid/not-fetched".into() },
            ContentBlock::Resource { resource: crate::protocol::EmbeddedResource {
                uri: "file:///c/uri-only".into(), text: None,
            } },
        ];
        let text = prompt_text(&blocks);
        for uri in ["file:///a/src/", "file:///b/src/", "https://example.invalid/not-fetched", "file:///c/uri-only"] {
            assert!(text.contains(uri), "URI lost: {uri}: {text}");
        }
        assert_ne!(prompt_text(&blocks[..1]), prompt_text(&blocks[1..2]));
    }

    #[test]
    fn prompt_content_keeps_text_resources_and_media_in_place() {
        use rpi_extcore::host::PromptContent;
        let blocks = vec![
            ContentBlock::Text { text: "before".into() },
            ContentBlock::Image { data: "AA==".into(), mime_type: "image/png".into() },
            ContentBlock::Resource { resource: crate::protocol::EmbeddedResource {
                uri: "file:///a/lib.rs".into(), text: Some("resource body".into()),
            } },
            ContentBlock::Audio { data: "AA==".into(), mime_type: "audio/wav".into() },
            ContentBlock::Text { text: "after".into() },
        ];
        let replay = v2_prompt_content(&blocks);
        assert_eq!(replay.len(), 5);
        assert_eq!(replay[0]["text"], "before");
        assert_eq!(replay[1]["type"], "image");
        assert_eq!(replay[2]["resource"]["text"], "resource body");
        assert_eq!(replay[3]["type"], "audio");
        assert_eq!(replay[4]["text"], "after");
        let uri_only = serde_json::from_value::<ContentBlock>(json!({
            "type":"resource", "resource":{"uri":"https://example.test/no-fetch"}
        })).unwrap();
        assert_eq!(v2_prompt_content(&[uri_only]), vec![json!({
            "type":"resource_link", "name":"https://example.test/no-fetch",
            "uri":"https://example.test/no-fetch"
        })]);
        let parts = prompt_content(&blocks);
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0], PromptContent::Text { text: "before".into() });
        assert!(matches!(&parts[1], PromptContent::Image { .. }));
        assert!(matches!(&parts[2], PromptContent::Text { text } if text.contains("resource body")));
        assert!(matches!(&parts[3], PromptContent::Audio { .. }));
        assert_eq!(parts[4], PromptContent::Text { text: "after".into() });
    }

    #[test]
    fn prompt_images_preserve_provider_neutral_payload() {
        let blocks = vec![ContentBlock::Image {
            data: "aGVsbG8=".into(),
            mime_type: "image/png".into(),
        }];
        assert_eq!(
            prompt_images(&blocks),
            vec![json!({"data": "aGVsbG8=", "mimeType": "image/png"})]
        );
        assert_eq!(prompt_text(&blocks), "");
    }

    #[test]
    fn prompt_audio_preserve_provider_neutral_payload() {
        let blocks = vec![ContentBlock::Audio {
            data: "UklGRg==".into(),
            mime_type: "audio/wav".into(),
        }];
        assert_eq!(
            prompt_audio(&blocks),
            vec![json!({"data": "UklGRg==", "mimeType": "audio/wav"})]
        );
        assert_eq!(prompt_text(&blocks), "");
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
