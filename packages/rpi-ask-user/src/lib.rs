//! `ask_user` — structured questions backed by the host's interactive UI
//! dialog bridge (runtime action 17).
//!
//! The tool accepts the native Pi shape (`question`, `context`, option objects,
//! `allowMultiple`, `allowFreeform`, `allowComment`, `displayMode`, `timeout`,
//! `suggest`) and the earlier `questions[]` alias. Both normalize into
//! [`AskQuestion`] values, then each question is presented to the host TUI one
//! at a time through [`RUNTIME_ACTION_UI_DIALOG`]. The plugin parks in `poll`
//! until the host answers or cancels, so the model never receives a fabricated
//! answer.
//!
//! A headless host (no attached TUI) rejects `ui_request/open`, and the tool
//! fails with the explicit `ask_user requires an interactive UI` error instead
//! of returning a selector payload that the model could mistake for an answer.

#![forbid(unsafe_code)]

use rpi_plugin_sdk::handles::ToolHandles;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rpi_plugin_sdk::{
    PluginApi, StableToolSchema, StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};

/// Host runtime-action id for the interactive UI dialog bridge. The SDK pinned
/// by this workspace may predate `RuntimeActionId::UiDialog`, so the raw ABI id
/// is used; the host validates it (`TryFrom<u32>`).
const RUNTIME_ACTION_UI_DIALOG: u32 = 17;

/// How long a single `poll` parks before re-checking the host for an answer.
/// Keeps the driver's spin loop from burning a core while the user thinks.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// Host fn pointers captured once at register time. `runtime_action` lets the
/// tool drive the UI bridge; `user_data` is the host's opaque context.
// Reuse the SDK's host-context lifetime and thread-safety contract.
type HostRuntime = PluginApi;

static HOST_RUNTIME: OnceLock<HostRuntime> = OnceLock::new();

/// Monotonic suffix so concurrent questions never share a `requestId`.
static REQUEST_SEQ: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------------------
// Typed normalization
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct AskOption {
    title: String,
    description: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct AskQuestion {
    id: String,
    question: String,
    header: Option<String>,
    context: Option<String>,
    options: Vec<AskOption>,
    allow_multiple: bool,
    allow_freeform: bool,
    allow_comment: bool,
    suggest: Option<String>,
    display_mode: Option<String>,
    timeout: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
enum AskPlan {
    Confirm { summary: String },
    Questions { questions: Vec<AskQuestion> },
}

impl AskQuestion {
    /// UI kind the host should render for this question.
    fn ui_kind(&self) -> &'static str {
        if self.options.is_empty() {
            "input"
        } else {
            "selector"
        }
    }

    fn to_ui(&self, fallback_context: Option<&str>) -> Value {
        json!({
            "kind": self.ui_kind(),
            "id": self.id,
            "header": self.header,
            "question": self.question,
            "context": self.context.as_deref().or(fallback_context),
            "options": self.options.iter().map(|option| {
                let mut value = json!({"title": option.title});
                if let Some(description) = &option.description {
                    value["description"] = json!(description);
                }
                value
            }).collect::<Vec<_>>(),
            "allowMultiple": self.allow_multiple,
            "allowFreeform": self.allow_freeform,
            "allowComment": self.allow_comment,
            "suggest": self.suggest,
            "displayMode": self.display_mode,
            "timeout": self.timeout,
        })
    }
}

fn opt_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    })
}

fn opt_bool(value: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_bool))
}

fn parse_options(item: &Value) -> Result<Vec<AskOption>, String> {
    let Some(raw) = item.get("options").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    if raw.len() > 20 {
        return Err("a question may contain at most 20 options".into());
    }
    let mut options = Vec::new();
    for option in raw {
        let (title, description) = match option {
            Value::String(title) => (title.trim().to_string(), None),
            Value::Object(object) => {
                let title = ["title", "label", "text", "value", "name"]
                    .iter()
                    .find_map(|key| object.get(*key).and_then(Value::as_str))
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .ok_or("each option requires a non-empty `title`")?
                    .to_string();
                let description = match object.get("description") {
                    None => None,
                    Some(value) => Some(
                        value
                            .as_str()
                            .ok_or("option `description` must be a string")?
                            .trim()
                            .to_string(),
                    ),
                };
                (title, description)
            }
            other => (other.to_string(), None),
        };
        if title.is_empty() {
            return Err("each option requires a non-empty `title`".into());
        }
        options.push(AskOption { title, description });
    }
    Ok(options)
}

fn normalize_question(
    item: &Value,
    defaults: &Value,
    index: usize,
    top_context: Option<&str>,
) -> Result<AskQuestion, String> {
    let question = opt_string(item, &["question", "prompt"])
        .or_else(|| opt_string(defaults, &["question", "prompt"]))
        .unwrap_or_default();
    let question = question.trim();
    if question.is_empty() {
        return Err("each question must contain 1-500 characters".into());
    }
    if question.chars().count() > 500 {
        return Err("each question must contain 1-500 characters".into());
    }
    let display_mode =
        opt_string(item, &["displayMode"]).or_else(|| opt_string(defaults, &["displayMode"]));
    if let Some(mode) = display_mode.as_deref() {
        if mode != "overlay" && mode != "inline" {
            return Err("displayMode must be overlay or inline".into());
        }
    }
    let timeout = item
        .get("timeout")
        .and_then(Value::as_u64)
        .or_else(|| defaults.get("timeout").and_then(Value::as_u64));
    if let Some(timeout) = timeout {
        if timeout == 0 {
            return Err("timeout must be a positive integer".into());
        }
    }
    let options = parse_options(item)?;
    let allow_freeform = opt_bool(item, &["allowFreeform", "allow_freeform", "allow_custom"])
        .or_else(|| {
            opt_bool(
                defaults,
                &["allowFreeform", "allow_freeform", "allow_custom"],
            )
        })
        .unwrap_or(options.is_empty());
    Ok(AskQuestion {
        id: opt_string(item, &["id", "questionId"]).unwrap_or_else(|| format!("q{}", index + 1)),
        question: question.to_string(),
        header: opt_string(item, &["header"]).or_else(|| opt_string(defaults, &["header"])),
        context: opt_string(item, &["context"]).or_else(|| top_context.map(str::to_string)),
        options,
        allow_multiple: opt_bool(item, &["allowMultiple", "allow_multiple", "multiple"])
            .or_else(|| opt_bool(defaults, &["allowMultiple", "allow_multiple", "multiple"]))
            .unwrap_or(false),
        allow_freeform,
        allow_comment: opt_bool(item, &["allowComment", "allow_comment"])
            .or_else(|| opt_bool(defaults, &["allowComment", "allow_comment"]))
            .unwrap_or(false),
        suggest: opt_string(item, &["suggest", "placeholder"])
            .or_else(|| opt_string(defaults, &["suggest", "placeholder"])),
        display_mode,
        timeout,
    })
}

/// Normalize the tool arguments into an [`AskPlan`].
fn ask(params: &Value) -> Result<AskPlan, String> {
    let kind = params
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("question");
    if kind != "question" && kind != "confirm" {
        return Err("type must be question or confirm".into());
    }
    if kind == "confirm" {
        let summary = opt_string(params, &["summary", "question"])
            .ok_or("summary is required for confirm")?;
        return Ok(AskPlan::Confirm { summary });
    }

    let top_context = opt_string(params, &["context"]);
    let mut questions = Vec::new();
    if let Some(items) = params.get("questions").and_then(Value::as_array) {
        if items.is_empty() || items.len() > 3 {
            return Err("questions must contain 1-3 items".into());
        }
        for (index, item) in items.iter().enumerate() {
            questions.push(normalize_question(
                item,
                params,
                index,
                top_context.as_deref(),
            )?);
        }
    } else {
        questions.push(normalize_question(
            params,
            params,
            0,
            top_context.as_deref(),
        )?);
    }
    Ok(AskPlan::Questions { questions })
}

/// Human-readable transcript text for a plan (used for the `/ask_user` command
/// and as a fallback). Never serialized JSON.
#[cfg_attr(not(test), allow(dead_code))]
fn plan_display_text(plan: &AskPlan) -> String {
    match plan {
        AskPlan::Confirm { summary } => format!("Confirmation requested: {summary}"),
        AskPlan::Questions { questions } => {
            let mut lines = Vec::new();
            for question in questions {
                let mut line = question.question.clone();
                if let Some(header) = &question.header {
                    line = format!("{header}: {line}");
                }
                if !question.options.is_empty() {
                    let labels = question
                        .options
                        .iter()
                        .map(|option| option.title.clone())
                        .collect::<Vec<_>>()
                        .join(", ");
                    line.push_str(&format!("\nChoices: {labels}"));
                }
                lines.push(line);
            }
            lines.join("\n")
        }
    }
}

// ---------------------------------------------------------------------------
// Host UI bridge
// ---------------------------------------------------------------------------

fn next_request_id() -> String {
    let seq = REQUEST_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("ask-{}-{seq}-{nanos}", std::process::id())
}

fn invoke_ui_dialog(runtime: &HostRuntime, args: &Value) -> Result<Value, String> {
    let args_json = serde_json::to_string(args).map_err(|error| error.to_string())?;
    let mut output = StbString::empty();
    let rc = (runtime.runtime_action)(
        RUNTIME_ACTION_UI_DIALOG,
        StbStringRef::from_str(&args_json),
        &mut output,
        runtime.user_data,
    );
    let text = output.to_string_lossy();
    (runtime.free_string)(output);
    if rc != 0 {
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            if let Some(error) = value.get("error").and_then(Value::as_str) {
                return Err(error.to_owned());
            }
        }
        return Err(if text.is_empty() {
            format!("ask_user UI dialog failed with status {rc}")
        } else {
            text
        });
    }
    serde_json::from_str(&text)
        .map_err(|error| format!("ask_user UI dialog returned invalid JSON: {error}"))
}

fn cancel_ui_dialog(runtime: &HostRuntime, request_id: &str) {
    let _ = invoke_ui_dialog(runtime, &json!({"op": "cancel", "requestId": request_id}));
}

// ---------------------------------------------------------------------------
// Tool drive
// ---------------------------------------------------------------------------

struct PendingQuestion {
    request_id: String,
    question: AskQuestion,
    deadline: Option<Instant>,
}

struct AskAnswer {
    id: String,
    question: String,
    values: Vec<String>,
    text: Option<String>,
    confirmed: Option<bool>,
}

static HANDLES: ToolHandles<Drive> = ToolHandles::new();

struct Drive {
    params: Value,
    tool_call_id: String,
    started: bool,
    finished: bool,
    pending: Option<PendingQuestion>,
    remaining: std::collections::VecDeque<AskQuestion>,
    answers: Vec<AskAnswer>,
    confirm: Option<String>,
}

fn answer_values(answer: &Value) -> Vec<String> {
    if let Some(values) = answer.get("values").and_then(Value::as_array) {
        return values
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect();
    }
    if let Some(value) = answer.get("value").and_then(Value::as_str) {
        return vec![value.to_string()];
    }
    Vec::new()
}

fn answer_summary(plan: &AskPlan, answers: &[AskAnswer]) -> String {
    match plan {
        AskPlan::Confirm { summary } => {
            let confirmed = answers
                .first()
                .and_then(|answer| answer.confirmed)
                .unwrap_or(false);
            format!(
                "{}: {summary}",
                if confirmed { "Confirmed" } else { "Declined" }
            )
        }
        AskPlan::Questions { .. } => {
            if answers.len() == 1 {
                let answer = &answers[0];
                let value = answer
                    .text
                    .clone()
                    .filter(|text| !text.is_empty())
                    .or_else(|| (!answer.values.is_empty()).then(|| answer.values.join(", ")))
                    .unwrap_or_else(|| "(no answer)".to_string());
                format!("{}: {value}", answer.question)
            } else {
                answers
                    .iter()
                    .map(|answer| {
                        let value = answer
                            .text
                            .clone()
                            .filter(|text| !text.is_empty())
                            .or_else(|| {
                                (!answer.values.is_empty()).then(|| answer.values.join(", "))
                            })
                            .unwrap_or_else(|| "(no answer)".to_string());
                        format!("{}: {value}", answer.question)
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
    }
}

fn answer_result(answers: &[AskAnswer]) -> Value {
    json!(answers
        .iter()
        .map(|answer| {
            json!({
                "id": answer.id,
                "question": answer.question,
                "values": answer.values,
                "text": answer.text,
                "confirmed": answer.confirmed,
            })
        })
        .collect::<Vec<_>>())
}

fn pending_progress(question: &AskQuestion) -> Value {
    json!({
        "content": [{"type": "text", "text": format!("Waiting for your answer…\n{}", question.question)}],
        "details": {"ui": {"kind": question.ui_kind(), "question": question.question}},
    })
}

/// Open the next queued question, or return `None` when none remain.
fn open_next(drive: &mut Drive, runtime: &HostRuntime) -> Result<Option<Value>, String> {
    let Some(question) = drive.remaining.pop_front() else {
        return Ok(None);
    };
    let request_id = next_request_id();
    let request = json!({
        "op": "open",
        "requestId": request_id,
        "toolCallId": drive.tool_call_id,
        "ui": question.to_ui(None),
    });
    let response = invoke_ui_dialog(runtime, &request)?;
    let deadline = question
        .timeout
        .map(|seconds| Instant::now() + Duration::from_secs(seconds));
    drive.pending = Some(PendingQuestion {
        request_id,
        question,
        deadline,
    });
    Ok(Some(response))
}

/// Poll the currently open question. `Ok(Some(done))` when the whole plan is
/// answered; `Ok(None)` while still waiting.
fn poll_open_question(drive: &mut Drive, runtime: &HostRuntime) -> Result<Option<Value>, String> {
    let Some(pending) = drive.pending.as_ref() else {
        return Ok(None);
    };
    if pending
        .deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        let request_id = pending.request_id.clone();
        cancel_ui_dialog(runtime, &request_id);
        drive.pending = None;
        return Err("ask timed out".into());
    }
    let request_id = pending.request_id.clone();
    let response = invoke_ui_dialog(runtime, &json!({"op": "poll", "requestId": request_id}))?;
    match response
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending")
    {
        "answered" => {
            let answer = response.get("answer").cloned().unwrap_or(Value::Null);
            let pending = drive.pending.take().expect("checked above");
            let text = answer
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string);
            let values = answer_values(&answer);
            let confirmed = if pending.question.ui_kind() == "input" {
                None
            } else {
                Some(
                    values
                        .first()
                        .map(|value| value.eq_ignore_ascii_case("yes"))
                        .unwrap_or(false),
                )
            };
            drive.answers.push(AskAnswer {
                id: pending.question.id.clone(),
                question: pending.question.question.clone(),
                values,
                text,
                confirmed,
            });
            // Open the next question, if any.
            if open_next(drive, runtime)?.is_none() {
                return Ok(Some(build_done(drive)));
            }
            Ok(None)
        }
        "cancelled" => Err("ask cancelled".into()),
        _ => Ok(None),
    }
}

fn build_done(drive: &Drive) -> Value {
    let plan = match ask(&drive.params) {
        Ok(plan) => plan,
        Err(_) => AskPlan::Confirm {
            summary: String::new(),
        },
    };
    let summary = answer_summary(&plan, &drive.answers);
    json!({
        "content": [{"type": "text", "text": summary}],
        "details": {
            "answers": answer_result(&drive.answers),
            "ui": {"kind": "ask_user_result"},
        },
    })
}

fn start_drive(drive: &mut Drive, runtime: &HostRuntime) -> Result<Value, String> {
    let plan = ask(&drive.params)?;
    match plan {
        AskPlan::Confirm { summary } => {
            let question = AskQuestion {
                id: "confirm".to_string(),
                question: summary.clone(),
                header: None,
                context: None,
                options: vec![
                    AskOption {
                        title: "Yes".to_string(),
                        description: None,
                    },
                    AskOption {
                        title: "No".to_string(),
                        description: None,
                    },
                ],
                allow_multiple: false,
                allow_freeform: false,
                allow_comment: false,
                suggest: None,
                display_mode: None,
                timeout: None,
            };
            drive.confirm = Some(summary);
            drive.remaining.push_back(question);
        }
        AskPlan::Questions { questions } => {
            for question in questions {
                drive.remaining.push_back(question);
            }
        }
    }
    match open_next(drive, runtime)? {
        Some(response) if response.get("status").and_then(Value::as_str) == Some("answered") => {
            // Rare immediate answer (also covers replay of a duplicate open).
            match poll_open_question(drive, runtime)? {
                Some(done) => Ok(done),
                None => Ok(json!(null)),
            }
        }
        Some(_) | None => Ok(json!(null)),
    }
}

// ---------------------------------------------------------------------------
// FFI lifecycle
// ---------------------------------------------------------------------------

rpi_plugin_sdk::execute_handler! {
fn execute(tool_call_id, params, free) {
    let tool_call_id = tool_call_id.to_owned();
    let text = params.to_string_lossy();
    params.free_with(free);
    HANDLES.insert(Drive {
        params: serde_json::from_str(&text).unwrap_or(Value::Null),
        tool_call_id,
        started: false,
        finished: false,
        pending: None,
        remaining: std::collections::VecDeque::new(),
        answers: Vec::new(),
        confirm: None,
    })
}
}

extern "C" fn poll(handle: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null ask handle".into()));
    }
    let Some(entry) = HANDLES.get(handle) else {
        return StepResult::err(StbString::from_string(
            "invalid or destroyed tool handle".into(),
        ));
    };
    let mut drive = entry.lock();
    let Some(runtime) = HOST_RUNTIME.get().copied() else {
        return StepResult::err(StbString::from_string(
            "ask_user requires an interactive UI".into(),
        ));
    };
    if entry.cancelled.load(Ordering::SeqCst) {
        if let Some(pending) = drive.pending.as_ref() {
            cancel_ui_dialog(&runtime, &pending.request_id);
        }
        return StepResult::err(StbString::from_string("ask cancelled".into()));
    }
    if drive.finished {
        return StepResult::err(StbString::from_string(
            "ask_user polled after completion".into(),
        ));
    }

    if !drive.started {
        drive.started = true;
        return match start_drive(&mut drive, &runtime) {
            Ok(done) if !done.is_null() => {
                drive.finished = true;
                StepResult::done(StbString::from_string(done.to_string()))
            }
            Ok(_) => {
                let question = drive
                    .pending
                    .as_ref()
                    .map(|pending| pending.question.clone());
                let progress = question
                    .map(|question| pending_progress(&question))
                    .unwrap_or_else(
                        || json!({"content":[{"type":"text","text":"Waiting for your answer…"}]}),
                    );
                StepResult::pending(StbString::from_string(progress.to_string()))
            }
            Err(error) => StepResult::err(StbString::from_string(error)),
        };
    }

    match poll_open_question(&mut drive, &runtime) {
        Ok(Some(done)) => {
            drive.finished = true;
            StepResult::done(StbString::from_string(done.to_string()))
        }
        Ok(None) => {
            // Park briefly so the driver's spin loop does not burn a core.
            std::thread::sleep(POLL_INTERVAL);
            if entry.cancelled.load(Ordering::SeqCst) {
                if let Some(pending) = drive.pending.as_ref() {
                    cancel_ui_dialog(&runtime, &pending.request_id);
                }
                return StepResult::err(StbString::from_string("ask cancelled".into()));
            }
            StepResult::pending(StbString::from_string(String::new()))
        }
        Err(error) => StepResult::err(StbString::from_string(error)),
    }
}

extern "C" fn cancel(handle: StepHandle) {
    if let Some(entry) = HANDLES.get(handle) {
        entry.cancel();
        // Serialize access to pending UI state with poll, after signalling it.
        let drive = entry.lock();
        if let Some(runtime) = HOST_RUNTIME.get() {
            if let Some(pending) = drive.pending.as_ref() {
                cancel_ui_dialog(runtime, &pending.request_id);
            }
        }
    }
}

extern "C" fn destroy(handle: StepHandle) {
    drop(HANDLES.remove(handle));
}

use rpi_plugin_sdk::callbacks::free_string;

fn command_output(out: &mut rpi_plugin_sdk::callbacks::Output<'_>, value: Value) -> i32 {
    if !out.is_available() {
        return 1;
    }
    out.write(value.to_string());
    0
}
rpi_plugin_sdk::string_handler! {

/// `/ask_user` command bridge. Keeps the command-level selector/editor path for
/// hosts that drive UI through slash commands rather than tool calls.
fn ask_command(args_json, out, _user_data) {
    let request_text = args_json.to_owned();
    let request: Value = serde_json::from_str(&request_text).unwrap_or(Value::Null);
    let raw = request.get("args").and_then(Value::as_str).unwrap_or("");
    let params: Value = serde_json::from_str(raw).unwrap_or_else(|_| json!({"question": raw}));
    if params.get("action").and_then(Value::as_str) == Some("select")
        || params.get("action").and_then(Value::as_str) == Some("input")
    {
        let value = params.get("value").and_then(Value::as_str).unwrap_or("");
        return command_output(
            out,
            json!({"kind":"message","text":format!("Selected: {value}")}),
        );
    }
    let plan = match ask(&params) {
        Ok(plan) => plan,
        Err(error) => {
            return command_output(
                out,
                json!({"kind":"message","text":format!("ask_user failed: {error}")}),
            )
        }
    };
    match plan {
        AskPlan::Confirm { summary } => command_output(
            out,
            json!({"kind":"selector","items":[
                {"value":"Yes","label":"Yes","description":summary},
                {"value":"No","label":"No"}
            ]}),
        ),
        AskPlan::Questions { questions } => {
            let first = &questions[0];
            if first.options.is_empty() {
                return command_output(
                    out,
                    json!({"kind":"editor","initialText": first.suggest.clone().unwrap_or_default()}),
                );
            }
            let items = first
                .options
                .iter()
                .map(|option| {
                    let mut item = json!({"value": option.title, "label": option.title});
                    if let Some(description) = &option.description {
                        item["description"] = json!(description);
                    }
                    item
                })
                .collect::<Vec<_>>();
            command_output(out, json!({"kind":"selector","items":items}))
        }
    }
}
}
const TOOL_DESCRIPTION: &str = "Ask for a missing decision or explicit confirmation through an attached interactive UI. Use one question, 1-3 sequential questions, or type=confirm with summary. Options open a selector; otherwise an input editor. Optional timeout is seconds per question; confirm has no timeout. Timeout/cancellation closes the dialog and errors. Returns a summary and details.answers {id, question, values, text, confirmed}. Read values/text for selections; confirmed only indicates whether a selector's first value is Yes (null for input), not general approval.";

fn question_properties() -> Value {
    json!({
        "question":{"type":"string","description":"Nonblank question text, 1-500 trimmed characters; batch items may fall back to the top-level question."},
        "context":{"type":"string","description":"Optional background shown with the question."},
        "header":{"type":"string","description":"Optional short heading."},
        "id":{"type":"string","description":"Answer identifier; defaults to q1, q2, q3 by question position. Not inherited."},
        "options":{"type":"array","maxItems":20,"items":{
            "anyOf":[
                {"type":"string","minLength":1,"description":"Nonblank choice title."},
                {"type":"object","properties":{
                    "title":{"type":"string","minLength":1,"description":"Nonblank visible and returned choice title."},
                    "description":{"type":"string","description":"Optional choice explanation."}
                },"required":["title"],"additionalProperties":true}
            ]
        },"description":"Up to 20 strings or {title, description} choices. Omitted/empty opens an input editor. Not inherited by batch items."},
        "allowMultiple":{"type":"boolean","default":false,"description":"Allow multiple selector choices; default false."},
        "allowFreeform":{"type":"boolean","description":"Allow custom text; defaults to true without options, false with options."},
        "allowComment":{"type":"boolean","default":false,"description":"Allow an additional comment; default false."},
        "displayMode":{"type":"string","enum":["overlay","inline"],"description":"Optional UI presentation hint; otherwise the host chooses."},
        "timeout":{"type":"integer","minimum":1,"description":"Positive seconds per question; omitted means no deadline. Expiry closes the dialog and errors. Ignored for confirm (no timeout)."},
        "suggest":{"type":"string","description":"Optional input suggestion, not an answer."}
    })
}

fn tool_parameters() -> Value {
    // Inline all nested schemas: provider schema conversion can discard references.
    let batch = json!({
        "type":"object",
        "properties":question_properties(),
        "additionalProperties":true
    });
    let mut properties = question_properties();
    properties["type"] = json!({"type":"string","enum":["question","confirm"],"default":"question","description":"Question mode, or Yes/No confirmation using summary (fallback question). Confirm ignores question UI settings and timeout."});
    properties["summary"] = json!({"type":"string","description":"Nonblank confirmation text for type=confirm; falls back to question. Ignored in question mode."});
    properties["questions"] = json!({"type":"array","minItems":1,"maxItems":3,"items":batch,"description":"1-3 sequential questions instead of a single question. Items inherit top-level text and UI defaults, but not options or id. Ignored for confirm."});
    json!({"type":"object","properties":properties,"additionalProperties":true})
}

fn tool_schema() -> StableToolSchema {
    StableToolSchema {
        name: StbString::from_string("ask_user".into()),
        description: StbString::from_string(TOOL_DESCRIPTION.into()),
        parameters: StbString::from_string(tool_parameters().to_string()),
    }
}

rpi_plugin_sdk::export_plugin!(|api| {
    let Some(register) = api.register_tool else {
        return 1;
    };
    let runtime = *api;
    let _ = HOST_RUNTIME.set(runtime);
    let schema = Box::new(tool_schema());
    let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
    drop(schema);
    if let Some(register_command) = api.register_command {
        let name = StbStringRef::from_str("ask_user");
        let description = StbStringRef::from_str("Ask a question through the interactive host UI; requires a TUI, accepts question text or ask_user JSON, and presents an editor, selector, or Yes/No confirmation");
        let _ = register_command(name, description, ask_command);
    }
    rc
});

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_property_descriptions(value: &Value) {
        if let Some(properties) = value.get("properties").and_then(Value::as_object) {
            for (name, property) in properties {
                assert!(
                    !property["description"].as_str().unwrap_or("").is_empty(),
                    "missing description: {name}"
                );
            }
        }
        match value {
            Value::Object(object) => object.values().for_each(assert_property_descriptions),
            Value::Array(array) => array.iter().for_each(assert_property_descriptions),
            _ => {}
        }
    }

    fn assert_inline_schema(value: &Value) {
        match value {
            Value::Object(object) => {
                assert!(
                    !object.contains_key("$ref"),
                    "provider may discard references"
                );
                assert!(!object.contains_key("$defs"), "schema must be fully inline");
                if let Some(items) = object.get("items") {
                    assert!(items.as_object().is_some_and(|items| !items.is_empty()));
                }
                object.values().for_each(assert_inline_schema);
            }
            Value::Array(array) => array.iter().for_each(assert_inline_schema),
            _ => {}
        }
    }

    #[test]
    fn registered_schema_has_real_nested_shapes_and_preserves_optional_fields() {
        // Exercise the constructor passed to register_tool, not a copied schema.
        let schema = tool_schema();
        assert_eq!(schema.name.to_string_lossy(), "ask_user");
        assert!(schema
            .description
            .to_string_lossy()
            .contains("interactive UI"));
        let params: Value = serde_json::from_str(&schema.parameters.to_string_lossy()).unwrap();
        assert_eq!(params["type"], "object");
        assert!(params.get("required").is_none());
        assert_eq!(params["additionalProperties"], true); // Host __rpi is accepted.
        assert_property_descriptions(&params);
        let properties = &params["properties"];
        assert_eq!(properties["type"]["default"], "question");
        assert_eq!(properties["questions"]["minItems"], 1);
        assert_eq!(properties["questions"]["maxItems"], 3);
        assert_eq!(properties["questions"]["items"]["type"], "object");
        assert_eq!(properties["options"]["maxItems"], 20);
        assert!(properties["options"]["items"]["anyOf"].is_array());
        assert_eq!(properties["timeout"]["minimum"], 1);
        assert!(properties["allowFreeform"].get("default").is_none()); // Depends on options.
        assert_eq!(properties["allowMultiple"]["default"], false);
        assert_eq!(properties["allowComment"]["default"], false);
        let question = &properties["questions"]["items"];
        assert_eq!(question["type"], "object");
        assert!(question.get("required").is_none()); // Text may come from top-level defaults.
        assert_eq!(question["properties"]["options"]["maxItems"], 20);
        assert_eq!(question["properties"]["timeout"]["minimum"], 1);
        assert_eq!(
            question["properties"]["displayMode"]["enum"],
            json!(["overlay", "inline"])
        );
        for fields in [properties, &question["properties"]] {
            // Advertise canonical arguments only, while parser compatibility stays intact.
            for alias in [
                "prompt",
                "questionId",
                "allow_multiple",
                "multiple",
                "allow_freeform",
                "allow_custom",
                "allow_comment",
                "placeholder",
            ] {
                assert!(fields.get(alias).is_none(), "advertised alias: {alias}");
            }
            let alternatives = fields["options"]["items"]["anyOf"].as_array().unwrap();
            assert_eq!(alternatives.len(), 2); // No legacy scalar/array choices.
            assert_eq!(alternatives[0]["type"], "string");
            let option = &alternatives[1];
            assert_eq!(option["type"], "object");
            assert_eq!(option["required"], json!(["title"]));
            assert_eq!(option["properties"].as_object().unwrap().len(), 2);
            assert_eq!(option["properties"]["title"]["type"], "string");
            assert_eq!(option["properties"]["description"]["type"], "string");
        }
        assert_inline_schema(&params);
        schema.name.free_with(Some(free_string));
        schema.description.free_with(Some(free_string));
        schema.parameters.free_with(Some(free_string));
    }

    #[test]
    fn documented_defaults_aliases_and_boundaries_match_parser() {
        let AskPlan::Questions { questions } = plan(json!({
            "question":"Fallback?", "options":["not inherited"], "__rpi":{"host":true},
            "questions":[{}, {"questionId":"second", "prompt":"Choose?", "options":[{"label":"A"}, {"value":"B"}]}]
        })) else {
            panic!("expected questions")
        };
        assert_eq!(questions[0].question, "Fallback?");
        assert_eq!(questions[0].id, "q1");
        assert!(questions[0].options.is_empty());
        assert!(questions[0].allow_freeform);
        assert!(!questions[0].allow_multiple);
        assert!(!questions[0].allow_comment);
        assert_eq!(questions[0].timeout, None);
        assert_eq!(questions[1].id, "second");
        assert!(!questions[1].allow_freeform);
        assert_eq!(questions[1].options[1].title, "B");
        assert!(ask(&json!({"question":"x".repeat(500)})).is_ok());
        assert!(ask(&json!({"question":"x".repeat(501)})).is_err());
        assert!(ask(&json!({"question":"x", "options":vec!["a";20]})).is_ok());
        assert!(ask(&json!({"question":"x", "options":vec!["a";21]})).is_err());
        assert!(
            ask(&json!({"question":"x", "options":[{"title":" ", "label":"not used"}]})).is_err()
        );
        // Confirm ignores question-mode limits and UI settings, including timeout.
        assert!(ask(
            &json!({"type":"confirm", "summary":"x".repeat(501), "questions":[], "timeout":0})
        )
        .is_ok());
        assert!(ask(&json!({"type":"confirm"})).is_err());
    }

    fn plan(params: Value) -> AskPlan {
        ask(&params).unwrap()
    }

    #[test]
    fn normalizes_native_single_question() {
        let AskPlan::Questions { questions } = plan(json!({
            "question": "Which target?",
            "options": [{"title": "Linux", "description": "glibc"}, "Windows"],
            "allowMultiple": true,
            "allowFreeform": false,
            "context": "release",
            "header": "Build",
            "suggest": "linux/amd64",
            "displayMode": "inline",
            "timeout": 30
        })) else {
            panic!("expected questions");
        };
        assert_eq!(questions.len(), 1);
        let question = &questions[0];
        assert_eq!(question.question, "Which target?");
        assert_eq!(question.header.as_deref(), Some("Build"));
        assert_eq!(question.context.as_deref(), Some("release"));
        assert_eq!(question.options[0].title, "Linux");
        assert_eq!(question.options[0].description.as_deref(), Some("glibc"));
        assert_eq!(question.options[1].title, "Windows");
        assert!(question.allow_multiple);
        assert!(!question.allow_freeform);
        assert_eq!(question.suggest.as_deref(), Some("linux/amd64"));
        assert_eq!(question.display_mode.as_deref(), Some("inline"));
        assert_eq!(question.timeout, Some(30));
    }

    #[test]
    fn normalizes_questions_alias_with_ids_and_per_question_defaults() {
        let AskPlan::Questions { questions } = plan(json!({
            "allowFreeform": true,
            "header": "Questions",
            "questions": [
                {"id": "proxy_port", "question": "端口?", "options": [], "suggest": "7890"},
                {"id": "level", "question": "Level?", "options": ["a", "b"], "allowMultiple": true}
            ]
        })) else {
            panic!("expected questions");
        };
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0].id, "proxy_port");
        assert!(questions[0].allow_freeform);
        assert_eq!(questions[0].suggest.as_deref(), Some("7890"));
        assert_eq!(questions[0].header.as_deref(), Some("Questions"));
        assert_eq!(questions[1].id, "level");
        assert!(questions[1].allow_multiple);
    }

    #[test]
    fn confirm_plan() {
        let plan = plan(json!({"type": "confirm", "summary": "Deploy now?"}));
        assert_eq!(
            plan,
            AskPlan::Confirm {
                summary: "Deploy now?".into()
            }
        );
    }

    #[test]
    fn rejects_invalid_input() {
        assert!(ask(&json!({"questions": []})).is_err());
        assert!(ask(&json!({"questions": [{"question": "ok"}, {"question": "ok"}, {"question": "ok"}, {"question": "ok"}]})).is_err());
        assert!(ask(&json!({"question": "x", "displayMode": "huge"})).is_err());
        assert!(ask(&json!({"question": "x", "timeout": 0})).is_err());
        assert!(ask(&json!({"question": "x", "options": [{"description": "no title"}]})).is_err());
        assert!(
            ask(&json!({"question": "x", "options": [{"title": "x", "description": 42}]})).is_err()
        );
    }

    #[test]
    fn display_text_is_human_readable() {
        let plan = plan(json!({
            "question": "Which target?",
            "options": ["Linux", {"title": "Windows"}]
        }));
        let text = plan_display_text(&plan);
        assert_eq!(text, "Which target?\nChoices: Linux, Windows");
        assert!(!text.trim_start().starts_with('{'));
    }

    #[test]
    fn answer_summary_uses_values_and_text() {
        let ask_plan = plan(json!({"question": "Port?", "suggest": "7890"}));
        let answers = vec![AskAnswer {
            id: "q1".into(),
            question: "Port?".into(),
            values: vec![],
            text: Some("7897".into()),
            confirmed: None,
        }];
        assert_eq!(answer_summary(&ask_plan, &answers), "Port?: 7897");

        let confirm = plan(json!({"type": "confirm", "summary": "Go?"}));
        let answers = vec![AskAnswer {
            id: "confirm".into(),
            question: "Go?".into(),
            values: vec!["No".into()],
            text: None,
            confirmed: Some(false),
        }];
        assert_eq!(answer_summary(&confirm, &answers), "Declined: Go?");
    }
}
