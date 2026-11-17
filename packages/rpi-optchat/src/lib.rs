//! `/optchat`: OptChat's persistent chat memory for the rpi Rust agent.
//!
//! The `pi-optchat` extension keeps one endless chat by storing the whole log
//! and handing the model a bounded summary view of it. A native rpi plugin
//! cannot rewrite a request's transcript, so this port keeps OptChat's memory
//! model — an append-only log, a binary summary tree, `zoom`/`search` — and
//! exposes it to the host instead of replacing the transcript:
//!
//! - messages and tool activity in the current session branch are logged as
//!   `optchat-entry` custom entries through `AppendEntry`;
//! - the tree folds them into a view bounded by [`memory::VIEW`];
//! - the model reads it with the `zoom` and `search` tools, the user with
//!   `/optchat view`;
//! - `register_before_agent_start` appends one short pointer line so the model
//!   knows the memory exists.
//!
//! Storage is the session branch itself, so memory follows a fork, disappears
//! with a discarded branch, and reloads with the session. No project file, no
//! background worker, no model call: summary lines are extractive (see
//! `memory.rs`).
#![forbid(unsafe_code)]
// The pinned SDK exports safe C entrypoints around its internal raw ABI reads.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod memory;
mod runtime;

use memory::{clip, flat, iso8601, Kind, Memory, Origin, MAX_ENTRIES, PAGE, VIEW};
use rpi_plugin_sdk::callbacks::free_string;
use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{
    EventHandlerFn, EventTag, FreeStringFn, PluginApi, RuntimeActionId, StableToolSchema,
    StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Session custom-entry type holding one whole message.
const ENTRY_TYPE: &str = "optchat-entry";
/// Marker that makes the injected pointer line idempotent.
const MEMORY_MARKER: &str = "[OptChat memory]";
/// The two tools this plugin owns; their own calls are not logged as memory,
/// because a search echo of memory inside memory is noise, not history.
const OWN_TOOLS: [&str; 2] = ["zoom", "search"];
/// Bounds for `/optchat import`.
const MAX_IMPORT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_IMPORT_LINES: usize = 2_000;
/// `GetSessionBranch` page size while hydrating the log.
const HYDRATE_PAGE: usize = 200;
/// How close two identical captures must be to count as one message.
const DUPLICATE_WINDOW_MS: i64 = 10_000;
/// Orchestrator marker: a subagent shares the parent's session environment but
/// must not write its own turns into the parent's memory.
const ORCHESTRATOR_SUBAGENT_ENV: &str = "RPI_ORCHESTRATOR_SUBAGENT";

type Runtime = PluginApi;

static PROCESS_IS_SUBAGENT: AtomicBool = AtomicBool::new(false);

fn capture_process_role() {
    PROCESS_IS_SUBAGENT.store(
        std::env::var_os(ORCHESTRATOR_SUBAGENT_ENV).is_some()
            || std::env::var_os(rpi_plugin_sdk::SUBAGENT_PARENT_TRACE_ID_ENV).is_some(),
        Ordering::Relaxed,
    );
}

fn is_subagent() -> bool {
    PROCESS_IS_SUBAGENT.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Host runtime actions
// ---------------------------------------------------------------------------

fn call_runtime(id: RuntimeActionId, args: Value) -> Result<Value, String> {
    let runtime = runtime::snapshot().ok_or("optchat runtime is unavailable")?;
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
        return Err(if text.trim().is_empty() {
            format!("runtime action {id:?} failed")
        } else {
            text
        });
    }
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).map_err(|e| format!("invalid runtime response: {e}"))
}

// ---------------------------------------------------------------------------
// Session-scoped state
// ---------------------------------------------------------------------------

struct State {
    session: String,
    memory: Memory,
    /// The last captured message, so one message delivered twice — a user
    /// prompt via `before_agent_start` and again via `message_end`, or a
    /// re-dispatched turn — is not logged twice. Two genuinely identical
    /// messages more than [`DUPLICATE_WINDOW_MS`] apart are both kept.
    last: Option<(Kind, String, i64)>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// Serialize local append preparation, host persistence, and local commit so
/// two callbacks cannot persist the same message index or reorder the log.
static APPEND_GATE: Mutex<()> = Mutex::new(());

fn current_session() -> String {
    std::env::var(rpi_plugin_sdk::SESSION_ID_ENV)
        .ok()
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| "default".to_string())
}

/// Reload the log from the current session branch. Only `optchat-entry` entries
/// are requested, and a host that cannot filter them is refused rather than
/// reading the whole branch.
fn hydrate() -> Result<Memory, String> {
    let mut entries: Vec<memory::Entry> = Vec::new();
    let mut after: Option<u64> = None;
    for _ in 0..(MAX_ENTRIES / HYDRATE_PAGE + 4) {
        let mut args = json!({
            "customTypes": [ENTRY_TYPE],
            "limit": HYDRATE_PAGE,
            "order": "oldestFirst",
        });
        if let Some(seq) = after {
            args["afterSeq"] = json!(seq);
        }
        let value = call_runtime(RuntimeActionId::GetSessionBranch, args)?;
        if value.get("filtered").and_then(Value::as_bool) != Some(true) {
            return Err(
                "This rpi host answered GetSessionBranch without `filtered: true`; upgrade rpi so \
                 OptChat reads only its own entries."
                    .into(),
            );
        }
        let page = value
            .get("entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if page.is_empty() {
            break;
        }
        for record in &page {
            if record.get("type").and_then(Value::as_str) != Some("custom") {
                continue;
            }
            if record.get("customType").and_then(Value::as_str) != Some(ENTRY_TYPE) {
                continue;
            }
            if let Some(data) = record.get("data") {
                if let Ok(entry) = serde_json::from_value(data.clone()) {
                    entries.push(entry);
                }
            }
        }
        let last = page
            .iter()
            .filter_map(|record| record.get("seq").and_then(Value::as_u64))
            .max();
        match last {
            Some(seq) if page.len() >= HYDRATE_PAGE => after = Some(seq),
            _ => break,
        }
    }
    Ok(Memory::load(entries))
}

/// Run `f` against the state of the session this process is serving, reloading
/// the log when the host switched sessions. Hydration happens outside the lock:
/// it calls the host, and a host call must never be made while holding a lock a
/// callback could re-enter.
fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> Result<T, String> {
    let session = current_session();
    let stale = {
        let guard = STATE.lock().unwrap_or_else(|error| error.into_inner());
        guard
            .as_ref()
            .map(|state| state.session != session)
            .unwrap_or(true)
    };
    if stale {
        let memory = hydrate()?;
        let mut guard = STATE.lock().unwrap_or_else(|error| error.into_inner());
        let still_stale = guard
            .as_ref()
            .map(|state| state.session != session)
            .unwrap_or(true);
        if still_stale {
            *guard = Some(State {
                session,
                memory,
                last: None,
            });
        }
    }
    let mut guard = STATE.lock().unwrap_or_else(|error| error.into_inner());
    let state = guard
        .as_mut()
        .ok_or("OptChat memory state is unavailable")?;
    Ok(f(state))
}

fn with_memory<T>(f: impl FnOnce(&mut Memory) -> T) -> Result<T, String> {
    with_state(|state| f(&mut state.memory))
}

fn persist(entry: &memory::Entry) -> Result<(), String> {
    call_runtime(
        RuntimeActionId::AppendEntry,
        json!({"customType": ENTRY_TYPE, "data": entry}),
    )
    .map(|_| ())
}

/// Diagnostics for a capture path that must never fail a turn. Enabled with
/// `RPI_OPTCHAT_DEBUG=1`; otherwise a failed write is silently skipped and the
/// next turn retries through normal capture.
fn debug(message: &str) {
    if std::env::var_os("RPI_OPTCHAT_DEBUG").is_some() {
        eprintln!("[optchat] {message}");
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Persist an entry before committing it to the in-memory log. The host call
/// intentionally happens outside `STATE`; the append gate serializes local
/// writers without holding the state lock across the foreign call.
fn persist_then_commit(
    entry: memory::Entry,
    session: &str,
    last: Option<(Kind, String, i64)>,
) -> Result<usize, String> {
    persist(&entry)?;
    with_state(|state| {
        if state.session != session {
            return Err("OptChat session changed while persisting an entry".to_string());
        }
        let committed = state.memory.append_prepared(entry)?;
        if let Some(last) = last {
            state.last = Some(last);
        }
        Ok(committed.i)
    })?
}

/// Log one message, then store it as a session custom entry. The entry is only
/// made visible in memory after `AppendEntry` succeeds, so a transient host
/// failure cannot leave a phantom entry or consume the duplicate guard.
fn capture(
    kind: Kind,
    text: String,
    stamp: Option<i64>,
    receipt: Option<String>,
    origin: Option<Origin>,
) -> Result<(), String> {
    if is_subagent() || text.trim().is_empty() {
        return Ok(());
    }
    let millis = stamp.unwrap_or_else(now_ms);
    let date = iso8601(millis);
    let dedupe_text = text.clone();
    let _gate = APPEND_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let session = current_session();
    let candidate = with_state(|state| {
        let duplicate = state
            .last
            .as_ref()
            .is_some_and(|(last_kind, last_text, last)| {
                *last_kind == kind
                    && last_text == &text
                    && (millis - last).abs() <= DUPLICATE_WINDOW_MS
            });
        if duplicate || state.memory.len() >= MAX_ENTRIES {
            return None;
        }
        Some(state.memory.prepare(kind, text, date, receipt, origin))
    })?;
    let Some(entry) = candidate else {
        return Ok(());
    };
    if let Err(error) = persist_then_commit(entry, &session, Some((kind, dedupe_text, millis))) {
        debug(&format!("could not persist message: {error}"));
    }
    Ok(())
}

/// Log an entry the user asked for (`/optchat note`, import): not deduplicated,
/// because two identical notes are two notes. It also uses the same append gate
/// as automatic capture, so its index cannot race a message event.
fn record_note(kind: Kind, text: String, origin: Option<Origin>) -> Result<usize, String> {
    let date = iso8601(now_ms());
    let _gate = APPEND_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let session = current_session();
    let candidate = with_state(|state| {
        (state.memory.len() < MAX_ENTRIES)
            .then(|| state.memory.prepare(kind, text, date, None, origin))
    })?;
    let Some(entry) = candidate else {
        return Err(format!(
            "This session's memory already holds {MAX_ENTRIES} messages; nothing was recorded."
        ));
    };
    persist_then_commit(entry, &session, None)
}

// ---------------------------------------------------------------------------
// Capture events
// ---------------------------------------------------------------------------

/// Text blocks of a message's `content`.
fn text_of(content: &Value) -> String {
    let Some(blocks) = content.as_array() else {
        return content.as_str().unwrap_or_default().to_string();
    };
    let mut parts: Vec<&str> = Vec::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        if let Some(text) = block.get("text").and_then(Value::as_str) {
            if !text.trim().is_empty() {
                parts.push(text);
            }
        }
    }
    parts.join("\n")
}

fn capture_message(raw: &str) {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return;
    };
    let kind = match value.get("kind").and_then(Value::as_str) {
        Some("user") => Kind::User,
        Some("assistant") => Kind::Talk,
        // `toolResult` duplicates the echo captured from tool execution, and a
        // custom message is another extension's business.
        _ => return,
    };
    let text = text_of(value.get("content").unwrap_or(&Value::Null));
    let stamp = value.get("timestamp").and_then(Value::as_i64);
    let _ = capture(kind, text, stamp, None, None);
}

fn capture_tool_call(call: &rpi_plugin_sdk::events::ToolCall) {
    let _ = capture(
        Kind::Tool,
        format!("{}({})", call.tool_name, clip(&flat(&call.params), 2_000)),
        None,
        Some(call.tool_call_id.clone()),
        None,
    );
}

fn capture_tool_result(result: &rpi_plugin_sdk::events::ToolResult) {
    let text = match serde_json::from_str::<Value>(&result.result) {
        Ok(value) => {
            let content = text_of(value.get("content").unwrap_or(&Value::Null));
            if content.trim().is_empty() {
                result.result.clone()
            } else {
                content
            }
        }
        Err(_) => result.result.clone(),
    };
    let _ = capture(
        Kind::Echo,
        text,
        None,
        Some(result.tool_call_id.clone()),
        None,
    );
}

rpi_plugin_sdk::event_handler! {
fn on_message_end(event, data) {
    let _scope = runtime::enter(runtime::callback(data));
    let Some(raw) = event.message_text() else {
        return 0;
    };
    capture_message(&raw);
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_tool_start(event, data) {
    let _scope = runtime::enter(runtime::callback(data));
    let Some(call) = event.tool_call() else {
        debug("tool_execution_start without a tool-call payload");
        return 0;
    };
    debug(&format!("tool_execution_start {}", call.tool_name));
    if !OWN_TOOLS.contains(&call.tool_name.as_str()) {
        capture_tool_call(&call);
    }
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_tool_end(event, data) {
    let _scope = runtime::enter(runtime::callback(data));
    let Some(result) = event.tool_result() else {
        debug("tool_execution_end without a tool-result payload");
        return 0;
    };
    debug(&format!("tool_execution_end {}", result.tool_name));
    if !OWN_TOOLS.contains(&result.tool_name.as_str()) {
        capture_tool_result(&result);
    }
    0
}
}

// ---------------------------------------------------------------------------
// Prompt pointer
// ---------------------------------------------------------------------------

/// One short line: what the memory holds and how to read it. The view itself is
/// not injected — the native transcript already carries recent turns, and
/// duplicating it here would spend the context budget twice.
fn memory_hint(memory: &Memory) -> Option<String> {
    let stats = memory.stats();
    if stats.messages == 0 {
        return None;
    }
    let messages = if stats.messages == 1 {
        "message"
    } else {
        "messages"
    };
    let lines = if stats.lines == 1 { "line" } else { "lines" };
    Some(format!(
        "\n\n{MEMORY_MARKER} This session's persistent memory holds {} {messages} folded into {} \
         summary {lines}. `zoom(id, n)` opens message `id` whole (n = 1) or the summary block of n \
         messages starting at id; `search(text)` finds older messages by content, and its \
         `before: id` continues older. Use them for details the visible transcript no longer \
         shows. The user can print the summary view with `/optchat view`.",
        stats.messages, stats.lines
    ))
}

rpi_plugin_sdk::string_handler! {
fn before_agent_start(input, out, data) {
    let _scope = runtime::enter(runtime::callback(data));
    let envelope: Value = serde_json::from_str(input).unwrap_or(Value::Null);
    let prompt = envelope
        .get("systemPrompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // The prompt that starts a run is appended to the transcript before the
    // loop runs, so it never produces a `message_end`; this payload is where
    // the user's words arrive.
    if let Some(text) = envelope.get("prompt").and_then(Value::as_str) {
        let _ = capture(Kind::User, text.to_string(), None, None, None);
    }
    let hint = if prompt.contains(MEMORY_MARKER) {
        None
    } else {
        with_memory(|memory| memory_hint(memory)).unwrap_or(None)
    };
    let Some(hint) = hint else {
        out.write("{}".to_string());
        return 0;
    };
    if !out.is_available() {
        return 1;
    }
    out.write(json!({"systemPrompt": format!("{prompt}{hint}")}).to_string());
    0
}
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

static HANDLES: ToolHandles<Drive> = ToolHandles::new();

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Zoom,
    Search,
}

struct Drive {
    runtime: Option<Runtime>,
    op: Op,
    params: Value,
    done: bool,
}

fn submit(op: Op, params: StbString, free: Option<FreeStringFn>) -> StepHandle {
    let text = params.to_string_lossy();
    params.free_with(free);
    HANDLES.insert(Drive {
        runtime: runtime::snapshot(),
        op,
        params: serde_json::from_str(&text).unwrap_or(Value::Null),
        done: false,
    })
}

rpi_plugin_sdk::execute_handler! { fn zoom_execute(_id, params, free) {
    submit(Op::Zoom, params, free)
} }

rpi_plugin_sdk::execute_handler! { fn search_execute(_id, params, free) {
    submit(Op::Search, params, free)
} }

extern "C" fn poll(handle: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null optchat handle".into()));
    }
    let Some(entry) = HANDLES.get(handle) else {
        return StepResult::err(StbString::from_string(
            "invalid or destroyed tool handle".into(),
        ));
    };
    let mut drive = entry.lock();
    let _scope = runtime::enter(drive.runtime);
    if entry.cancelled.load(Ordering::SeqCst) {
        drive.done = true;
        return StepResult::err(StbString::from_string("optchat cancelled".into()));
    }
    if drive.done {
        return StepResult::err(StbString::from_string(
            "optchat polled after completion".into(),
        ));
    }
    drive.done = true;
    let tool = match drive.op {
        Op::Zoom => "zoom",
        Op::Search => "search",
    };
    let result = match drive.op {
        Op::Zoom => run_zoom(&drive.params),
        Op::Search => run_search(&drive.params),
    };
    match result {
        Ok(text) => StepResult::done(StbString::from_string(
            json!({
                "content": [{"type": "text", "text": clip(&text, memory::CAP)}],
                "details": {"kind": "optchat", "tool": tool},
            })
            .to_string(),
        )),
        Err(error) => StepResult::err(StbString::from_string(error)),
    }
}

extern "C" fn cancel(handle: StepHandle) {
    HANDLES.cancel(handle);
}

extern "C" fn destroy(handle: StepHandle) {
    drop(HANDLES.remove(handle));
}

fn run_zoom(params: &Value) -> Result<String, String> {
    let id = params
        .get("id")
        .and_then(Value::as_u64)
        .ok_or("zoom needs an integer `id` (a message id or the start of a summary line)")?;
    let n = params.get("n").and_then(Value::as_u64).unwrap_or(1);
    let n = u32::try_from(n).map_err(|_| "`n` is too large".to_string())?;
    let offset = params.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(PAGE as u64);
    let offset = usize::try_from(offset).unwrap_or(0);
    let limit = usize::try_from(limit).unwrap_or(PAGE);
    with_memory(|memory| memory.zoom(id, n, offset, limit))?
}

fn run_search(params: &Value) -> Result<String, String> {
    let text = params
        .get("text")
        .and_then(Value::as_str)
        .ok_or("search needs `text` to look for")?
        .to_string();
    let before = params.get("before").and_then(Value::as_u64);
    with_memory(|memory| memory.search(&text, before))
}

fn tool_schema(name: &str, description: &str, parameters: Value) -> StableToolSchema {
    StableToolSchema {
        name: StbString::from_string(name.to_string()),
        description: StbString::from_string(description.to_string()),
        parameters: StbString::from_string(parameters.to_string()),
    }
}

fn zoom_schema() -> StableToolSchema {
    tool_schema(
        "zoom",
        "Open this session's OptChat memory. zoom(id, n) opens line id+n into the two lines of n/2 messages it was made of; n = 1 (the default) gives message id whole, from the log rather than the summary. A message over 25000 characters comes in pages; offset and limit (characters) read any part of it. id must be a multiple of n, and both must address a block memory holds. Use it before guessing or asking about something the visible transcript no longer shows.",
        json!({
            "type": "object",
            "additionalProperties": true,
            "properties": {
                "id": {"type": "integer", "minimum": 0, "description": "Message id (n = 1), or the start id of a summary line; must be a multiple of n."},
                "n": {"type": "integer", "minimum": 1, "maximum": 65536, "default": 1, "description": "Power of two: 1 opens one message, 2 the block of two starting at id, and so on."},
                "offset": {"type": "integer", "minimum": 0, "description": "Character offset for paging a message longer than 25000 characters."},
                "limit": {"type": "integer", "minimum": 1, "maximum": 25000, "description": "Characters per page (default and maximum 25000)."}
            },
            "required": ["id"]
        }),
    )
}

fn search_schema() -> StableToolSchema {
    tool_schema(
        "search",
        "Find the original messages in this session's OptChat memory that contain text (plain text, any case), newest first, 20 at a time; before: id continues with older ones. Use it for an exact name, number, path or error the visible transcript does not show, then zoom(id, 1) to read a hit whole.",
        json!({
            "type": "object",
            "additionalProperties": true,
            "properties": {
                "text": {"type": "string", "minLength": 1, "description": "Plain text to look for, matched case-insensitively."},
                "before": {"type": "integer", "minimum": 0, "description": "Continue with messages older than this id."}
            },
            "required": ["text"]
        }),
    )
}

// ---------------------------------------------------------------------------
// Slash command
// ---------------------------------------------------------------------------

const HELP: &str = r#"OptChat memory — this session branch, persisted with the session

```text
/optchat                 status: messages, lines, bytes
/optchat view [lines|all] the summary view, newest lines first (default 40)
/optchat zoom <id> [n]   open message id, or the block of n messages from id
/optchat search <text>   find logged messages by content
/optchat note <text>     append a durable note to memory
/optchat import <file>   append a text/markdown/JSONL file as imported notes
/optchat fit             merge the view as far as the tree allows
/optchat help            this list
```

Messages, replies and tool activity of this session branch are logged
automatically; `zoom` and `search` are also available to the model as tools.
Summary lines are extractive: a leaf keeps the head of its message, a merge
keeps the head of both halves, so `zoom(id, 1)` is the way to read anything
whole."#;

fn status() -> String {
    let Ok((stats, kinds)) = with_memory(|memory| {
        let mut counts = [0usize; 6];
        for entry in memory.entries() {
            counts[match entry.kind {
                Kind::User => 0,
                Kind::Talk => 1,
                Kind::Tool => 2,
                Kind::Echo => 3,
                Kind::Note => 4,
                Kind::Work => 5,
            }] += 1;
        }
        (memory.stats(), counts)
    }) else {
        return "⚠️ OptChat memory is unavailable in this host.".to_string();
    };
    if stats.messages == 0 {
        return "OptChat memory is empty for this session branch. Messages are logged as the \
                conversation continues; `/optchat note <text>` adds one now."
            .to_string();
    }
    format!(
        "OptChat memory · {} messages · {} view lines · {} of {} bytes\n\
         user {} · talk {} · tool {} · echo {} · note {} · work {}\n\n\
         Stored as `optchat-entry` custom entries on this session branch. \
         `/optchat view` shows the summary view; `/optchat help` lists commands.",
        stats.messages,
        stats.lines,
        stats.bytes,
        VIEW,
        kinds[0],
        kinds[1],
        kinds[2],
        kinds[3],
        kinds[4],
        kinds[5]
    )
}

fn parse_u64(text: &str, what: &str) -> Result<u64, String> {
    text.trim()
        .parse::<u64>()
        .map_err(|_| format!("`{}` is not a {what}", text.trim()))
}

fn zoom_command(rest: &str) -> Result<String, String> {
    let mut parts = rest.split_whitespace();
    let id = parts
        .next()
        .ok_or("Usage: /optchat zoom <id> [n]")?
        .to_string();
    let id = parse_u64(&id, "message id")?;
    let n = match parts.next() {
        Some(token) => u32::try_from(parse_u64(token, "block size")?)
            .map_err(|_| "`n` is too large".to_string())?,
        None => 1,
    };
    with_memory(|memory| memory.zoom(id, n, 0, PAGE))?
}

fn view_command(rest: &str) -> Result<String, String> {
    if rest.trim() == "all" {
        return with_memory(|memory| {
            let stats = memory.stats();
            format!(
                "{} messages · {} view lines · {} bytes\n{}",
                stats.messages,
                stats.lines,
                stats.bytes,
                clip(&memory.view_text(), memory::CAP)
            )
        });
    }
    let count = match rest.split_whitespace().next() {
        Some(token) => usize::try_from(parse_u64(token, "line count")?)
            .map_err(|_| "line count is too large".to_string())?
            .clamp(1, 500),
        None => 40,
    };
    with_memory(|memory| {
        let stats = memory.stats();
        format!(
            "{} messages · {} view lines · {} bytes\n{}",
            stats.messages,
            stats.lines,
            stats.bytes,
            memory.view_tail(count)
        )
    })
}

fn fit_command() -> Result<String, String> {
    with_memory(|memory| {
        let before = memory.stats();
        memory.collapse();
        let after = memory.stats();
        format!(
            "Merged the view from {} to {} lines ({} to {} bytes).",
            before.lines, after.lines, before.bytes, after.bytes
        )
    })
}

/// A line of an import file: JSON text when the line is a JSON object/string,
/// otherwise the raw line.
fn import_line(line: &str) -> String {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix('#') {
        return rest.trim_start_matches('#').trim().to_string();
    }
    if !line.starts_with(['{', '"']) {
        return line.to_string();
    }
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return line.to_string();
    };
    if let Some(text) = value.as_str() {
        return text.to_string();
    }
    for key in ["text", "message", "content", "body"] {
        match value.get(key) {
            Some(Value::String(text)) => return text.clone(),
            Some(other) if other.is_array() => {
                let text = text_of(other);
                if !text.trim().is_empty() {
                    return text;
                }
            }
            _ => {}
        }
    }
    line.to_string()
}

fn import_command(path: &str) -> Result<String, String> {
    let path = PathBuf::from(path.trim().trim_matches('"'));
    let metadata = fs::metadata(&path)
        .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("`{}` is not a file", path.display()));
    }
    if metadata.len() > MAX_IMPORT_BYTES {
        return Err(format!(
            "`{}` is larger than {} MiB; split it first",
            path.display(),
            MAX_IMPORT_BYTES / (1024 * 1024)
        ));
    }
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("`{}` is not readable UTF-8 text: {error}", path.display()))?;
    let conversation = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let mut added = 0usize;
    let mut skipped = 0usize;
    for (line_no, line) in text.lines().enumerate() {
        let body = import_line(line);
        if body.trim().is_empty() || added >= MAX_IMPORT_LINES {
            skipped += 1;
            continue;
        }
        let origin = Origin {
            source: "import".into(),
            conversation: conversation.clone(),
            message: (line_no + 1).to_string(),
            title: conversation.clone(),
            project: None,
        };
        match record_note(Kind::Note, body, Some(origin)) {
            Ok(_) => added += 1,
            Err(error) => return Err(format!("stopped after {added} messages: {error}")),
        }
    }
    let mut summary = format!("Imported {added} messages from `{conversation}`.");
    if skipped > 0 {
        summary.push_str(&format!(
            " {skipped} empty lines or lines over the {MAX_IMPORT_LINES}-message limit were skipped."
        ));
    }
    Ok(summary)
}

fn run_command(args: &str) -> String {
    let args = args.trim();
    let mut parts = args.splitn(2, char::is_whitespace);
    let verb = parts.next().unwrap_or_default();
    let rest = parts.next().unwrap_or_default().trim();
    let result = match verb {
        "" | "status" | "show" => Ok(status()),
        "view" => view_command(rest),
        "zoom" => zoom_command(rest),
        "search" if !rest.is_empty() => with_memory(|memory| memory.search(rest, None)),
        "note" if !rest.is_empty() => record_note(Kind::Note, rest.to_string(), None)
            .map(|i| format!("Noted as memory message {i}.")),
        "import" if !rest.is_empty() => import_command(rest),
        "fit" | "compact" => fit_command(),
        "help" | "-h" | "--help" => Ok(HELP.to_string()),
        other => Err(format!("unknown /optchat command `{other}`\n\n{HELP}")),
    };
    match result {
        Ok(text) => text,
        Err(error) => format!("⚠️ {error}"),
    }
}

fn command_output(out: &mut rpi_plugin_sdk::callbacks::Output<'_>, value: Value) -> i32 {
    if !out.is_available() {
        return 1;
    }
    out.write(value.to_string());
    0
}

rpi_plugin_sdk::string_handler! {
fn optchat_command(input, out, data) {
    let _scope = runtime::enter(runtime::callback(data));
    let envelope: Value = serde_json::from_str(input).unwrap_or(Value::Null);
    let args = envelope
        .get("args")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text = run_command(args);
    command_output(out, json!({"kind": "message", "text": text, "markdown": true}))
}
}

// ---------------------------------------------------------------------------
// Entrypoint
// ---------------------------------------------------------------------------

rpi_plugin_sdk::export_plugin!(|api| {
    capture_process_role();
    runtime::install(*api);
    let Some(register_event) = api.register_event_handler else {
        return 1;
    };
    for (tag, handler) in [
        (EventTag::MessageEnd, on_message_end as EventHandlerFn),
        (
            EventTag::ToolExecutionStart,
            on_tool_start as EventHandlerFn,
        ),
        (EventTag::ToolExecutionEnd, on_tool_end as EventHandlerFn),
    ] {
        let rc = register_event(tag, handler, api.user_data);
        if rc != 0 {
            return rc;
        }
    }
    if let Some(register_prompt) = api.register_before_agent_start {
        let rc = register_prompt(before_agent_start, free_string, api.user_data);
        if rc != 0 {
            return rc;
        }
    }
    let Some(register_tool) = api.register_tool else {
        return 1;
    };
    let zoom = Box::new(zoom_schema());
    let rc = register_tool(&*zoom, zoom_execute, poll, cancel, destroy, free_string);
    drop(zoom);
    if rc != 0 {
        return rc;
    }
    let search = Box::new(search_schema());
    let rc = register_tool(&*search, search_execute, poll, cancel, destroy, free_string);
    drop(search);
    if rc != 0 {
        return rc;
    }
    if let Some(register_command) = api.register_command {
        let _ = register_command(
            StbStringRef::from_str("optchat"),
            StbStringRef::from_str("Show or search this session's persistent OptChat memory"),
            optchat_command,
        );
    }
    0
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_text_keeps_only_text_blocks() {
        let content = json!([
            {"type": "text", "text": "first"},
            {"type": "toolCall", "name": "read", "arguments": {}},
            {"type": "thinking", "thinking": "hmm"},
            {"type": "text", "text": "  "},
            {"type": "text", "text": "second"}
        ]);
        assert_eq!(text_of(&content), "first\nsecond");
        assert_eq!(text_of(&json!("plain")), "plain");
        assert_eq!(text_of(&Value::Null), "");
    }

    #[test]
    fn capture_message_maps_roles_and_ignores_custom_and_tool_results() {
        let mut memory = Memory::new();
        let event = |raw: &str| {
            let value: Value = serde_json::from_str(raw).unwrap();
            let kind = match value["kind"].as_str() {
                Some("user") => Some(Kind::User),
                Some("assistant") => Some(Kind::Talk),
                _ => None,
            };
            kind.map(|kind| (kind, text_of(&value["content"])))
        };
        assert_eq!(
            event(r#"{"kind":"user","content":[{"type":"text","text":"hi"}],"timestamp":1}"#),
            Some((Kind::User, "hi".to_string()))
        );
        assert_eq!(
            event(r#"{"kind":"custom","role":"goal","content":[],"data":{}}"#),
            None
        );
        assert_eq!(event(r#"{"kind":"toolResult","content":[]}"#), None);
        memory.record(Kind::User, "hi".into(), iso8601(0), None, None);
        assert_eq!(memory.len(), 1);
    }

    #[test]
    fn import_lines_unwrap_json_and_markdown_headings() {
        assert_eq!(import_line(r#"{"text":"from json"}"#), "from json");
        assert_eq!(
            import_line(r#"{"message":{"content":[{"type":"text","text":"nested"}]}}"#),
            r#"{"message":{"content":[{"type":"text","text":"nested"}]}}"#
        );
        assert_eq!(
            import_line(r#"{"content":[{"type":"text","text":"blocks"}]}"#),
            "blocks"
        );
        assert_eq!(import_line(r#""quoted""#), "quoted");
        assert_eq!(import_line("## Heading"), "Heading");
        assert_eq!(import_line("  plain text  "), "plain text");
        assert_eq!(import_line("   "), "");
    }

    #[test]
    fn tool_schemas_advertise_canonical_arguments_without_host_context() {
        let zoom = zoom_schema();
        assert_eq!(zoom.name.to_string_lossy(), "zoom");
        assert!(zoom.description.to_string_lossy().contains("zoom(id, n)"));
        let params: Value = serde_json::from_str(&zoom.parameters.to_string_lossy())
            .expect("zoom parameters are JSON");
        assert_eq!(params["type"], "object");
        assert_eq!(params["additionalProperties"], Value::Bool(true));
        assert_eq!(params["required"], json!(["id"]));
        assert_eq!(params["properties"]["id"]["minimum"], 0);
        assert_eq!(params["properties"]["n"]["default"], 1);
        assert_eq!(params["properties"]["limit"]["maximum"], PAGE);
        assert_eq!(params["properties"]["offset"]["minimum"], 0);
        assert!(!params.to_string().contains("__rpi"));

        let search = search_schema();
        assert_eq!(search.name.to_string_lossy(), "search");
        let params: Value = serde_json::from_str(&search.parameters.to_string_lossy())
            .expect("search parameters are JSON");
        assert_eq!(params["required"], json!(["text"]));
        assert_eq!(params["properties"]["text"]["minLength"], 1);
        assert_eq!(params["properties"]["before"]["minimum"], 0);
        assert!(!params.to_string().contains("__rpi"));
    }

    #[test]
    fn help_and_unknown_commands_do_not_touch_the_host() {
        assert!(run_command("help").contains("/optchat view"));
        assert!(run_command("nope").contains("unknown /optchat command"));
        assert!(run_command("note").contains("unknown /optchat command"));
        assert!(run_command("import").contains("unknown /optchat command"));
    }

    #[test]
    fn zoom_command_requires_a_numeric_id() {
        let error = zoom_command("abc").unwrap_err();
        assert!(error.contains("not a message id"), "{error}");
        assert!(zoom_command("").is_err());
    }

    #[test]
    fn identical_messages_are_deduplicated_only_inside_the_window() {
        let now = now_ms();
        let duplicate = |last: Option<&(Kind, String, i64)>, kind, text: &str, millis: i64| {
            last.is_some_and(|(last_kind, last_text, last_ms)| {
                *last_kind == kind
                    && last_text == text
                    && (millis - last_ms).abs() <= DUPLICATE_WINDOW_MS
            })
        };
        let last = (Kind::User, "continue".to_string(), now);
        assert!(duplicate(Some(&last), Kind::User, "continue", now + 1));
        assert!(duplicate(
            Some(&last),
            Kind::User,
            "continue",
            now - DUPLICATE_WINDOW_MS
        ));
        assert!(!duplicate(
            Some(&last),
            Kind::User,
            "continue",
            now + DUPLICATE_WINDOW_MS + 1
        ));
        assert!(!duplicate(Some(&last), Kind::Talk, "continue", now));
        assert!(!duplicate(Some(&last), Kind::User, "go on", now));
        assert!(!duplicate(None, Kind::User, "continue", now));
    }
}
