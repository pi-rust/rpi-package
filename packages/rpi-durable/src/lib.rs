#![forbid(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

//! Durable, session-scoped workflow checkpoints for rpi.
//!
//! The native session branch is the source of truth. This extension only keeps
//! a decoded copy in memory and appends a compact custom entry after each
//! mutation; it does not create a second transcript or project database.

use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{
    EventHandlerFn, EventTag, FreeStringFn, PluginApi, RuntimeActionId, StableToolSchema,
    StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

const TOOL: &str = "durable";
const ENTRY_TYPE: &str = "durable-state";
const MAX_STEPS: usize = 128;
const MAX_NAME_CHARS: usize = 500;
const MAX_STEP_ID_CHARS: usize = 128;
const MAX_STEP_TITLE_CHARS: usize = 1_000;
const MAX_NOTE_CHARS: usize = 4_000;
const MAX_JOURNAL_ENTRIES: usize = 64;
const MAX_SUMMARY_CHARS: usize = 1_000;
const MAX_QUEUE_ITEMS: usize = 32;
const MAX_QUEUE_TEXT_CHARS: usize = 4_000;

type Runtime = PluginApi;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static STATE: OnceLock<Mutex<DurableState>> = OnceLock::new();

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DurableState {
    workflow: Option<Workflow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run: Option<RunState>,
    #[serde(default)]
    next_run_id: u64,
    #[serde(default)]
    next_queue_id: u64,
    #[serde(default)]
    queue: Vec<QueueItem>,
    #[serde(default)]
    revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Workflow {
    name: String,
    status: WorkflowStatus,
    steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WorkflowStatus {
    Running,
    Paused,
    Completed,
    Failed,
}

impl WorkflowStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RunStatus {
    Queued,
    Running,
    Settled,
    Failed,
    Interrupted,
}

impl RunStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Settled => "settled",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ToolExecutionStatus {
    Started,
    Completed,
    Failed,
    Interrupted,
    Unknown,
}

impl ToolExecutionStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolExecution {
    call_id: String,
    name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    params: String,
    status: ToolExecutionStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    result: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunState {
    id: String,
    status: RunStatus,
    started_revision: u64,
    updated_revision: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    outcome: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    recovery_of: String,
    #[serde(default)]
    tools: Vec<ToolExecution>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum QueueItemStatus {
    Queued,
    Claimed,
    Completed,
    Failed,
    Cancelled,
}

impl QueueItemStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Claimed => "claimed",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueItem {
    id: String,
    text: String,
    status: QueueItemStatus,
    created_revision: u64,
    updated_revision: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    note: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Step {
    id: String,
    title: String,
    status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StepStatus {
    Pending,
    InProgress,
    Completed,
}

impl StepStatus {
    fn marker(&self) -> &'static str {
        match self {
            Self::Pending => " ",
            Self::InProgress => "•",
            Self::Completed => "✓",
        }
    }
}

fn state() -> &'static Mutex<DurableState> {
    STATE.get_or_init(|| Mutex::new(DurableState::default()))
}

fn call_runtime(id: RuntimeActionId, args: Value) -> Result<Value, String> {
    let runtime = *RUNTIME.get().ok_or("durable runtime is unavailable")?;
    let input = args.to_string();
    let mut output = StbString::empty();
    let rc = (runtime.runtime_action)(
        id as u32,
        StbStringRef::from_str(&input),
        &mut output,
        runtime.user_data,
    );
    let text = output.to_string_lossy();
    (runtime.free_string)(output);
    if rc != 0 {
        return Err(if text.is_empty() {
            format!("runtime action {id:?} failed")
        } else {
            text
        });
    }
    if text.trim().is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_str(&text).map_err(|error| format!("invalid runtime response: {error}"))
    }
}

fn restore_state() {
    let Ok(value) = call_runtime(RuntimeActionId::GetSessionBranch, json!({})) else {
        return;
    };
    let Some(entries) = value.get("entries").and_then(Value::as_array) else {
        return;
    };

    let mut restored = DurableState::default();
    for entry in entries {
        if entry.get("type").and_then(Value::as_str) != Some("custom")
            || entry.get("customType").and_then(Value::as_str) != Some(ENTRY_TYPE)
        {
            continue;
        }
        if let Some(data) = entry.get("data") {
            if let Ok(next) = serde_json::from_value::<DurableState>(data.clone()) {
                restored = next;
            }
        }
    }
    let restored = recover_from_branch(restored);
    *state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = restored;
}

fn persist_state(next: &DurableState) -> Result<(), String> {
    call_runtime(
        RuntimeActionId::AppendEntry,
        json!({"customType": ENTRY_TYPE, "data": next}),
    )?;
    Ok(())
}

fn summary(value: &str) -> String {
    let mut result: String = value.chars().take(MAX_SUMMARY_CHARS).collect();
    if value.chars().count() > MAX_SUMMARY_CHARS {
        result.push('…');
    }
    result
}

fn commit_state(current: &DurableState, mut next: DurableState) -> Result<(), String> {
    if next == *current {
        return Ok(());
    }
    let mut guard = state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if *guard != *current {
        return Err("durable state changed concurrently; retry the operation".into());
    }
    next.revision = current.revision.saturating_add(1);
    if let Some(run) = next.run.as_mut() {
        run.updated_revision = next.revision;
    }
    persist_state(&next)?;
    *guard = next;
    Ok(())
}

fn mutate_state<F>(mutator: F) -> Result<(), String>
where
    F: FnOnce(&mut DurableState),
{
    let current = state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let mut next = current.clone();
    mutator(&mut next);
    commit_state(&current, next)
}

fn mark_interrupted(mut next: DurableState) -> DurableState {
    if let Some(run) = next.run.as_mut() {
        if run.status == RunStatus::Running {
            run.status = RunStatus::Interrupted;
            run.outcome = "interrupted".into();
            for tool in &mut run.tools {
                if tool.status == ToolExecutionStatus::Started {
                    tool.status = ToolExecutionStatus::Interrupted;
                    if tool.result.is_empty() {
                        tool.result = "execution did not reach a durable result".into();
                    }
                }
            }
        }
    }
    next
}

fn recover_from_branch(mut restored: DurableState) -> DurableState {
    let recovered = mark_interrupted(restored.clone());
    if recovered != restored && persist_state(&recovered).is_ok() {
        restored = recovered;
    }
    restored
}

fn text(value: Option<&Value>, field: &str, max_chars: usize) -> Result<String, String> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{field} is required"))?
        .trim();
    if value.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if value.chars().count() > max_chars {
        return Err(format!("{field} exceeds {max_chars} characters"));
    }
    Ok(value.to_string())
}

fn optional_text(
    value: Option<&Value>,
    field: &str,
    max_chars: usize,
) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    text(Some(value), field, max_chars).map(Some)
}

fn workflow_mut(state: &mut DurableState) -> Result<&mut Workflow, String> {
    state
        .workflow
        .as_mut()
        .ok_or_else(|| "no durable workflow is active; call durable with action start".into())
}

fn parse_steps(value: Option<&Value>) -> Result<Vec<Step>, String> {
    let values = value
        .and_then(Value::as_array)
        .ok_or("steps is required and must be an array")?;
    if values.is_empty() {
        return Err("steps must contain at least one item".into());
    }
    if values.len() > MAX_STEPS {
        return Err(format!("steps cannot contain more than {MAX_STEPS} items"));
    }

    let mut steps = Vec::with_capacity(values.len());
    for value in values {
        let id = text(value.get("id"), "step id", MAX_STEP_ID_CHARS)?;
        let title = text(value.get("title"), "step title", MAX_STEP_TITLE_CHARS)?;
        if steps.iter().any(|step: &Step| step.id == id) {
            return Err(format!("duplicate step id: {id}"));
        }
        steps.push(Step {
            id,
            title,
            status: StepStatus::Pending,
            note: None,
        });
    }
    Ok(steps)
}

fn require_running(workflow: &Workflow) -> Result<(), String> {
    if workflow.status != WorkflowStatus::Running {
        return Err(format!(
            "workflow is {}; only a running workflow can be checkpointed",
            workflow.status.as_str()
        ));
    }
    Ok(())
}

fn allocate_run(next: &mut DurableState, status: RunStatus, recovery_of: String) -> RunState {
    next.next_run_id = next.next_run_id.saturating_add(1);
    RunState {
        id: format!("run-{}", next.next_run_id),
        status,
        started_revision: next.revision.saturating_add(1),
        updated_revision: next.revision.saturating_add(1),
        outcome: String::new(),
        recovery_of,
        tools: Vec::new(),
    }
}

fn start_run(next: &mut DurableState) {
    let recovery_of = next
        .run
        .as_ref()
        .filter(|run| matches!(run.status, RunStatus::Queued | RunStatus::Running))
        .map(|run| run.id.clone())
        .unwrap_or_default();
    next.run = Some(allocate_run(next, RunStatus::Running, recovery_of));
}

fn recover_run(next: &mut DurableState) -> Result<(), String> {
    let previous = next
        .run
        .as_ref()
        .ok_or_else(|| "no durable run is available to recover".to_string())?;
    if previous.status != RunStatus::Interrupted {
        return Err(format!(
            "run is {}; only an interrupted run can be recovered",
            previous.status.as_str()
        ));
    }
    let previous_id = previous.id.clone();
    let tools = previous.tools.clone();
    let mut run = allocate_run(next, RunStatus::Queued, previous_id.clone());
    run.tools = tools;
    next.run = Some(run);
    Ok(())
}

fn forget_run(next: &mut DurableState) -> Result<(), String> {
    if next.run.is_none() {
        return Err("no durable run is recorded".into());
    }
    next.run = None;
    Ok(())
}

fn queue_item_id(params: &Value) -> Result<String, String> {
    text(params.get("item"), "item", MAX_STEP_ID_CHARS)
}

fn enqueue_item(next: &mut DurableState, params: &Value) -> Result<(), String> {
    if next.queue.len() >= MAX_QUEUE_ITEMS {
        return Err(format!(
            "queue cannot contain more than {MAX_QUEUE_ITEMS} items"
        ));
    }
    let item_text = text(params.get("text"), "text", MAX_QUEUE_TEXT_CHARS)?;
    next.next_queue_id = next.next_queue_id.saturating_add(1);
    let revision = next.revision.saturating_add(1);
    next.queue.push(QueueItem {
        id: format!("item-{}", next.next_queue_id),
        text: item_text,
        status: QueueItemStatus::Queued,
        created_revision: revision,
        updated_revision: revision,
        note: String::new(),
    });
    Ok(())
}

fn claim_item(next: &mut DurableState, params: &Value) -> Result<(), String> {
    let id = queue_item_id(params)?;
    let item = next
        .queue
        .iter_mut()
        .find(|item| item.id == id)
        .ok_or_else(|| format!("queue item not found: {id}"))?;
    if item.status != QueueItemStatus::Queued {
        return Err(format!(
            "queue item {id} is {}; only a queued item can be claimed",
            item.status.as_str()
        ));
    }
    item.status = QueueItemStatus::Claimed;
    item.updated_revision = next.revision.saturating_add(1);
    Ok(())
}

fn finish_item(next: &mut DurableState, params: &Value) -> Result<(), String> {
    let id = queue_item_id(params)?;
    let status = match params.get("status").and_then(Value::as_str) {
        Some("completed") => QueueItemStatus::Completed,
        Some("failed") => QueueItemStatus::Failed,
        _ => return Err("status must be completed or failed for queue finish".into()),
    };
    let note = optional_text(params.get("note"), "note", MAX_NOTE_CHARS)?.unwrap_or_default();
    let revision = next.revision.saturating_add(1);
    let item = next
        .queue
        .iter_mut()
        .find(|item| item.id == id)
        .ok_or_else(|| format!("queue item not found: {id}"))?;
    if item.status != QueueItemStatus::Claimed {
        return Err(format!(
            "queue item {id} is {}; only a claimed item can be finished",
            item.status.as_str()
        ));
    }
    item.status = status;
    item.updated_revision = revision;
    item.note = note;
    Ok(())
}

fn cancel_item(next: &mut DurableState, params: &Value) -> Result<(), String> {
    let id = queue_item_id(params)?;
    let note = optional_text(params.get("note"), "note", MAX_NOTE_CHARS)?
        .unwrap_or_else(|| "cancelled".into());
    let revision = next.revision.saturating_add(1);
    let item = next
        .queue
        .iter_mut()
        .find(|item| item.id == id)
        .ok_or_else(|| format!("queue item not found: {id}"))?;
    if !matches!(
        item.status,
        QueueItemStatus::Queued | QueueItemStatus::Claimed
    ) {
        return Err(format!(
            "queue item {id} is {}; only a queued or claimed item can be cancelled",
            item.status.as_str()
        ));
    }
    item.status = QueueItemStatus::Cancelled;
    item.updated_revision = revision;
    item.note = note;
    Ok(())
}

fn record_tool_start(next: &mut DurableState, call_id: &str, name: &str, params: &str) {
    let Some(run) = next.run.as_mut() else {
        return;
    };
    if run
        .tools
        .iter()
        .any(|tool| tool.call_id == call_id && tool.status != ToolExecutionStatus::Unknown)
    {
        return;
    }
    run.tools.push(ToolExecution {
        call_id: summary(call_id),
        name: summary(name),
        params: summary(params),
        status: ToolExecutionStatus::Started,
        result: String::new(),
    });
    if run.tools.len() > MAX_JOURNAL_ENTRIES {
        let overflow = run.tools.len() - MAX_JOURNAL_ENTRIES;
        run.tools.drain(..overflow);
    }
}

fn record_tool_end(
    next: &mut DurableState,
    call_id: &str,
    name: &str,
    result: &str,
    is_error: bool,
) {
    let Some(run) = next.run.as_mut() else {
        return;
    };
    let call_id = summary(call_id);
    if let Some(tool) = run.tools.iter_mut().find(|tool| tool.call_id == call_id) {
        if matches!(
            tool.status,
            ToolExecutionStatus::Completed
                | ToolExecutionStatus::Failed
                | ToolExecutionStatus::Interrupted
                | ToolExecutionStatus::Unknown
        ) {
            return;
        }
        tool.status = if is_error {
            ToolExecutionStatus::Failed
        } else {
            ToolExecutionStatus::Completed
        };
        tool.result = summary(result);
        return;
    }
    run.tools.push(ToolExecution {
        call_id,
        name: summary(name),
        params: String::new(),
        status: ToolExecutionStatus::Unknown,
        result: summary(result),
    });
    if run.tools.len() > MAX_JOURNAL_ENTRIES {
        let overflow = run.tools.len() - MAX_JOURNAL_ENTRIES;
        run.tools.drain(..overflow);
    }
}

/// Apply one action without touching the host. Keeping this state machine pure
/// makes transition and validation tests independent of the native ABI.
fn apply_action(current: &DurableState, params: &Value) -> Result<DurableState, String> {
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status");
    let mut next = current.clone();

    match action {
        "status" => {}
        "start" => {
            let name = text(params.get("name"), "name", MAX_NAME_CHARS)?;
            next.workflow = Some(Workflow {
                name,
                status: WorkflowStatus::Running,
                steps: parse_steps(params.get("steps"))?,
                reason: None,
            });
        }
        "checkpoint" => {
            let step_id = text(params.get("step"), "step", MAX_STEP_ID_CHARS)?;
            let status = match params.get("status").and_then(Value::as_str).unwrap_or("in_progress") {
                "pending" => StepStatus::Pending,
                "in_progress" => StepStatus::InProgress,
                "completed" => StepStatus::Completed,
                _ => return Err("status must be pending, in_progress, or completed".into()),
            };
            let note = optional_text(params.get("note"), "note", MAX_NOTE_CHARS)?;
            let workflow = workflow_mut(&mut next)?;
            require_running(workflow)?;
            let step = workflow
                .steps
                .iter_mut()
                .find(|step| step.id == step_id)
                .ok_or_else(|| format!("step not found: {step_id}"))?;
            step.status = status;
            if note.is_some() {
                step.note = note;
            }
        }
        "pause" => {
            let reason = optional_text(params.get("reason"), "reason", MAX_NOTE_CHARS)?;
            let workflow = workflow_mut(&mut next)?;
            if workflow.status != WorkflowStatus::Running {
                return Err(format!(
                    "workflow is {}; only a running workflow can be paused",
                    workflow.status.as_str()
                ));
            }
            workflow.status = WorkflowStatus::Paused;
            workflow.reason = reason;
        }
        "resume" => {
            let workflow = workflow_mut(&mut next)?;
            if workflow.status != WorkflowStatus::Paused {
                return Err(format!(
                    "workflow is {}; only a paused workflow can be resumed",
                    workflow.status.as_str()
                ));
            }
            workflow.status = WorkflowStatus::Running;
            workflow.reason = None;
        }
        "complete" => {
            let workflow = workflow_mut(&mut next)?;
            require_running(workflow)?;
            if workflow
                .steps
                .iter()
                .any(|step| step.status != StepStatus::Completed)
            {
                return Err("all steps must be completed before completing the workflow".into());
            }
            workflow.status = WorkflowStatus::Completed;
            workflow.reason = None;
        }
        "fail" => {
            let reason = optional_text(params.get("reason"), "reason", MAX_NOTE_CHARS)?
                .or_else(|| Some("workflow marked as failed".into()));
            let workflow = workflow_mut(&mut next)?;
            if !matches!(workflow.status, WorkflowStatus::Running | WorkflowStatus::Paused) {
                return Err(format!(
                    "workflow is {}; only a running or paused workflow can fail",
                    workflow.status.as_str()
                ));
            }
            workflow.status = WorkflowStatus::Failed;
            workflow.reason = reason;
        }
        "reset" => next.workflow = None,
        "recover" => recover_run(&mut next)?,
        "forget_run" => forget_run(&mut next)?,
        "enqueue" => enqueue_item(&mut next, params)?,
        "claim" => claim_item(&mut next, params)?,
        "finish" => finish_item(&mut next, params)?,
        "cancel" => cancel_item(&mut next, params)?,
        "queue" => {}
        _ => return Err("action must be one of: status, start, checkpoint, pause, resume, complete, fail, reset, recover, forget_run, enqueue, queue, claim, finish, cancel".into()),
    }
    Ok(next)
}

fn render(state: &DurableState) -> String {
    let mut lines = Vec::new();
    if let Some(workflow) = &state.workflow {
        let completed = workflow
            .steps
            .iter()
            .filter(|step| step.status == StepStatus::Completed)
            .count();
        lines.push(format!(
            "{} [{}] — {completed}/{} completed",
            workflow.name,
            workflow.status.as_str(),
            workflow.steps.len()
        ));
        if let Some(reason) = &workflow.reason {
            lines.push(format!("Reason: {reason}"));
        }
        for step in &workflow.steps {
            let marker = step.status.marker();
            lines.push(format!("[{marker}] {} — {}", step.id, step.title));
        }
    } else {
        lines.push("No durable workflow is active".into());
    }
    if let Some(run) = &state.run {
        let tool_summary = run
            .tools
            .iter()
            .rev()
            .take(3)
            .map(|tool| format!("{}:{}", tool.name, tool.status.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "Run {} [{}]{}",
            run.id,
            run.status.as_str(),
            if tool_summary.is_empty() {
                String::new()
            } else {
                format!(" — tools: {tool_summary}")
            }
        ));
        if run.status == RunStatus::Interrupted {
            lines
                .push("Recovery required: inspect the journal before retrying side effects".into());
        }
    }
    if !state.queue.is_empty() {
        let queued = state
            .queue
            .iter()
            .filter(|item| item.status == QueueItemStatus::Queued)
            .count();
        lines.push(format!(
            "Queue: {queued}/{} items queued",
            state.queue.len()
        ));
        for item in state.queue.iter().rev().take(3) {
            lines.push(format!(
                "[{}] {} — {}",
                item.status.as_str(),
                item.id,
                item.text
            ));
        }
    }
    lines.join("\n")
}

fn result(action: &str, state: &DurableState, error: Option<&str>) -> String {
    let mut details = serde_json::to_value(state).unwrap_or_else(|_| json!({"workflow": null}));
    details["action"] = json!(action);
    if let Some(error) = error {
        details["error"] = json!(error);
    }
    json!({
        "content": [{"type": "text", "text": if let Some(error) = error { format!("Error: {error}") } else { render(state) }}],
        "details": details
    })
    .to_string()
}

fn durable(params: &Value) -> Result<String, String> {
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status");
    let current = state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let next = match apply_action(&current, params) {
        Ok(next) => next,
        Err(error) => return Ok(result(action, &current, Some(&error))),
    };

    commit_state(&current, next.clone())?;
    Ok(result(action, &next, None))
}

static HANDLES: ToolHandles<Drive> = ToolHandles::new();

struct Drive {
    params: Value,
    done: bool,
}

extern "C" fn execute(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let input = params.to_string_lossy();
    params.free_with(free);
    HANDLES.insert(Drive {
        params: serde_json::from_str(&input).unwrap_or(Value::Null),
        done: false,
    })
}

extern "C" fn poll(handle: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null durable handle".into()));
    }
    let Some(entry) = HANDLES.get(handle) else {
        return StepResult::err(StbString::from_string(
            "invalid or destroyed durable tool handle".into(),
        ));
    };
    let mut drive = entry.lock();
    if entry.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("durable cancelled".into()));
    }
    if drive.done {
        return StepResult::err(StbString::from_string(
            "durable polled after completion".into(),
        ));
    }
    drive.done = true;
    match durable(&drive.params) {
        Ok(value) => StepResult::done(StbString::from_string(value)),
        Err(error) => StepResult::err(StbString::from_string(error)),
    }
}

extern "C" fn cancel(handle: StepHandle) {
    HANDLES.cancel(handle);
}

extern "C" fn destroy(handle: StepHandle) {
    drop(HANDLES.remove(handle));
}

use rpi_plugin_sdk::callbacks::free_string;

rpi_plugin_sdk::string_handler! {
fn command(args, out, _user_data) {
    if !out.is_available() {
        return 1;
    }
    let _ = args;
    let snapshot = state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    out.write(json!({"kind":"message","text":render(&snapshot)}).to_string());
    0
}
}

rpi_plugin_sdk::event_handler! {
fn handle_event(event, _user_data) {
    match event.tag() {
        EventTag::SessionStart | EventTag::SessionTree => restore_state(),
        EventTag::AgentStart => {
            if mutate_state(start_run).is_err() {
                return 1;
            }
        }
        EventTag::ToolExecutionStart => {
            let Some(call) = event.tool_call() else {
                return 1;
            };
            let result = mutate_state(|next| {
                if next.run.is_none() {
                    start_run(next);
                }
                if next
                    .run
                    .as_ref()
                    .is_some_and(|run| run.status == RunStatus::Running)
                {
                    record_tool_start(next, &call.tool_call_id, &call.tool_name, &call.params);
                }
            });
            if result.is_err() {
                return 1;
            }
        }
        EventTag::ToolExecutionEnd | EventTag::ToolResult => {
            let Some(result_event) = event.tool_result() else {
                return 1;
            };
            let result = mutate_state(|next| {
                if next.run.is_none() {
                    start_run(next);
                }
                record_tool_end(
                    next,
                    &result_event.tool_call_id,
                    &result_event.tool_name,
                    &result_event.result,
                    result_event.is_error,
                );
            });
            if result.is_err() {
                return 1;
            }
        }
        EventTag::AgentSettled => {
            let result = mutate_state(|next| {
                if let Some(run) = next.run.as_mut() {
                    if run.status == RunStatus::Running {
                        run.status = RunStatus::Settled;
                        run.outcome = "settled".into();
                        for tool in &mut run.tools {
                            if tool.status == ToolExecutionStatus::Started {
                                tool.status = ToolExecutionStatus::Interrupted;
                                tool.result =
                                    "run settled without a durable tool result".into();
                            }
                        }
                    }
                }
            });
            if result.is_err() {
                return 1;
            }
        }
        EventTag::SessionShutdown => {
            let result = mutate_state(|next| {
                let recovered = mark_interrupted(next.clone());
                *next = recovered;
            });
            if result.is_err() {
                return 1;
            }
        }
        _ => {}
    }
    0
}
}

rpi_plugin_sdk::export_plugin!(|api| {
    let _ = RUNTIME.set(*api);
    if let Some(register_event) = api.register_event_handler {
        for tag in [
            EventTag::SessionStart,
            EventTag::SessionTree,
            EventTag::AgentStart,
            EventTag::ToolExecutionStart,
            EventTag::ToolExecutionEnd,
            EventTag::ToolResult,
            EventTag::AgentSettled,
            EventTag::SessionShutdown,
        ] {
            if register_event(tag, handle_event as EventHandlerFn, std::ptr::null_mut()) != 0 {
                return 1;
            }
        }
    }
    if let Some(register_command) = api.register_command {
        let _ = register_command(
            StbStringRef::from_str("durable"),
            StbStringRef::from_str(
                "Show the current durable workflow, recovery journal, and work queue",
            ),
            command,
        );
    }
    let Some(register) = api.register_tool else {
        return 1;
    };
    let schema = Box::new(StableToolSchema {
        name: StbString::from_string(TOOL.into()),
        description: StbString::from_string(
            "Maintain a durable workflow, recovery journal, and explicit work queue in the current session branch. Use start/checkpoint for workflow state, inspect interrupted runs before retrying side effects, and use enqueue/claim/finish for manually coordinated work items.".into(),
        ),
        parameters: StbString::from_string(
            r#"{"type":"object","properties":{"action":{"type":"string","enum":["status","start","checkpoint","pause","resume","complete","fail","reset","recover","forget_run","enqueue","queue","claim","finish","cancel"]},"name":{"type":"string","description":"Workflow name for start"},"steps":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"title":{"type":"string"}},"required":["id","title"]},"minItems":1,"maxItems":128},"step":{"type":"string","description":"Step id for checkpoint"},"status":{"type":"string","enum":["pending","in_progress","completed","failed"]},"note":{"type":"string"},"reason":{"type":"string"},"text":{"type":"string","description":"Work item text for enqueue"},"item":{"type":"string","description":"Queue item id returned by enqueue or queue"}},"required":["action"]}"#.into(),
        ),
    });
    let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
    drop(schema);
    rc
});

#[cfg(test)]
mod tests {
    use super::*;

    fn started() -> DurableState {
        apply_action(
            &DurableState::default(),
            &json!({
                "action":"start",
                "name":"Release",
                "steps":[{"id":"build","title":"Build"},{"id":"test","title":"Test"}]
            }),
        )
        .unwrap()
    }

    #[test]
    fn start_creates_pending_steps() {
        let state = started();
        let workflow = state.workflow.unwrap();
        assert_eq!(workflow.status, WorkflowStatus::Running);
        assert_eq!(workflow.steps[0].status, StepStatus::Pending);
    }

    #[test]
    fn checkpoint_and_complete_follow_the_workflow() {
        let state = started();
        let state = apply_action(
            &state,
            &json!({"action":"checkpoint","step":"build","status":"completed","note":"ok"}),
        )
        .unwrap();
        assert_eq!(
            state.workflow.as_ref().unwrap().steps[0].status,
            StepStatus::Completed
        );
        assert!(apply_action(&state, &json!({"action":"complete"})).is_err());
        let state = apply_action(
            &state,
            &json!({"action":"checkpoint","step":"test","status":"completed"}),
        )
        .unwrap();
        let state = apply_action(&state, &json!({"action":"complete"})).unwrap();
        assert_eq!(state.workflow.unwrap().status, WorkflowStatus::Completed);
    }

    #[test]
    fn pause_resume_and_fail_validate_status() {
        let state = started();
        let state = apply_action(&state, &json!({"action":"pause","reason":"waiting"})).unwrap();
        assert_eq!(
            state.workflow.as_ref().unwrap().status,
            WorkflowStatus::Paused
        );
        let state = apply_action(&state, &json!({"action":"resume"})).unwrap();
        let state = apply_action(&state, &json!({"action":"fail","reason":"blocked"})).unwrap();
        assert_eq!(
            state.workflow.as_ref().unwrap().status,
            WorkflowStatus::Failed
        );
        assert!(apply_action(&state, &json!({"action":"resume"})).is_err());
    }

    #[test]
    fn rejects_duplicate_steps_and_unknown_step() {
        assert!(apply_action(
            &DurableState::default(),
            &json!({"action":"start","name":"x","steps":[{"id":"a","title":"A"},{"id":"a","title":"Again"}]})
        )
        .is_err());
        assert!(
            apply_action(&started(), &json!({"action":"checkpoint","step":"missing"})).is_err()
        );
    }

    #[test]
    fn state_round_trips_as_session_entry_data() {
        let state = started();
        let value = serde_json::to_value(&state).unwrap();
        let restored: DurableState = serde_json::from_value(value).unwrap();
        assert_eq!(restored, state);
    }

    #[test]
    fn reset_removes_workflow() {
        let state = apply_action(&started(), &json!({"action":"reset"})).unwrap();
        assert_eq!(state, DurableState::default());
    }

    #[test]
    fn interrupted_run_is_explicitly_recoverable_without_replaying_tools() {
        let mut state = DurableState::default();
        start_run(&mut state);
        assert_eq!(state.run.as_ref().unwrap().status, RunStatus::Running);
        record_tool_start(&mut state, "call-1", "shell", "{\"cmd\":\"make test\"}");

        let interrupted = mark_interrupted(state);
        let run = interrupted.run.as_ref().unwrap();
        assert_eq!(run.status, RunStatus::Interrupted);
        assert_eq!(run.tools[0].status, ToolExecutionStatus::Interrupted);

        let mut recovered = interrupted;
        recover_run(&mut recovered).unwrap();
        let run = recovered.run.as_ref().unwrap();
        assert_eq!(run.status, RunStatus::Queued);
        assert_eq!(run.recovery_of, "run-1");
        assert_eq!(run.tools[0].status, ToolExecutionStatus::Interrupted);
    }

    #[test]
    fn tool_journal_is_bounded_and_terminal_results_are_idempotent() {
        let mut state = DurableState::default();
        start_run(&mut state);
        for index in 0..(MAX_JOURNAL_ENTRIES + 3) {
            let id = format!("call-{index}");
            record_tool_start(&mut state, &id, "tool", "{}");
            record_tool_end(&mut state, &id, "tool", "ok", false);
        }
        let run = state.run.as_ref().unwrap();
        assert_eq!(run.tools.len(), MAX_JOURNAL_ENTRIES);
        assert_eq!(run.tools[0].call_id, "call-3");
        assert_eq!(run.tools[0].status, ToolExecutionStatus::Completed);

        record_tool_end(&mut state, "call-3", "tool", "different", true);
        let tool = &state.run.as_ref().unwrap().tools[0];
        assert_eq!(tool.status, ToolExecutionStatus::Completed);
        assert_eq!(tool.result, "ok");
    }

    #[test]
    fn queue_items_are_claimed_and_finished_once() {
        let state = apply_action(
            &DurableState::default(),
            &json!({"action":"enqueue","text":"Deploy the release"}),
        )
        .unwrap();
        assert_eq!(state.queue[0].id, "item-1");
        assert_eq!(state.queue[0].status, QueueItemStatus::Queued);

        let state = apply_action(&state, &json!({"action":"claim","item":"item-1"})).unwrap();
        assert_eq!(state.queue[0].status, QueueItemStatus::Claimed);
        let state = apply_action(
            &state,
            &json!({"action":"finish","item":"item-1","status":"completed","note":"verified"}),
        )
        .unwrap();
        assert_eq!(state.queue[0].status, QueueItemStatus::Completed);
        assert_eq!(state.queue[0].note, "verified");
        assert!(apply_action(&state, &json!({"action":"claim","item":"item-1"})).is_err());
    }

    #[test]
    fn queue_rejects_invalid_transitions_and_is_bounded() {
        let mut state = DurableState::default();
        for index in 0..MAX_QUEUE_ITEMS {
            state = apply_action(
                &state,
                &json!({"action":"enqueue","text":format!("task-{index}")}),
            )
            .unwrap();
        }
        assert!(apply_action(&state, &json!({"action":"enqueue","text":"overflow"})).is_err());
        assert!(apply_action(
            &state,
            &json!({"action":"finish","item":"item-1","status":"completed"})
        )
        .is_err());
        assert!(apply_action(&state, &json!({"action":"cancel","item":"missing"})).is_err());
    }

    #[test]
    fn old_checkpoint_data_defaults_the_run_journal_and_queue() {
        let restored: DurableState = serde_json::from_value(json!({
            "workflow": null
        }))
        .unwrap();
        assert_eq!(restored, DurableState::default());
    }
}
