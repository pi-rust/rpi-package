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

mod compact;
mod import;
mod memory;
mod runtime;
mod store;

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
/// Session custom-entry type remembering the view shape (its parts and whether a
/// merge batch is running), so a resumed session shows the same lines.
const VIEW_TYPE: &str = "optchat-view";
/// Session custom-entry type remembering which profile this session belongs to.
const PROFILE_TYPE: &str = "optchat-profile";
/// Marker that makes the injected pointer line idempotent.
const MEMORY_MARKER: &str = "[OptChat memory]";
/// The two tools this plugin owns; their own calls are not logged as memory,
/// because a search echo of memory inside memory is noise, not history.
const OWN_TOOLS: [&str; 2] = ["zoom", "search"];
/// Bounds for `/optchat import`.
const MAX_IMPORT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_IMPORT_LINES: usize = 2_000;
/// How many messages to release at once when the retained window overflows, so
/// the tree rebuild is amortized rather than paid per message.
const TRIM_SLACK: usize = 512;
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
    call_action(id as u32, args)
}

/// Call a runtime action by ABI id, for host actions the published SDK does not
/// declare yet (`GetExecutionContext`).
fn call_action(id: u32, args: Value) -> Result<Value, String> {
    let runtime = runtime::snapshot().ok_or("optchat runtime is unavailable")?;
    let input = args.to_string();
    let mut output = StbString::empty();
    let rc = (runtime.runtime_action)(
        id,
        StbStringRef::from_str(&input),
        &mut output,
        runtime.user_data,
    );
    let text = output.to_string_lossy();
    (runtime.free_string)(output);
    if rc != 0 {
        return Err(if text.trim().is_empty() {
            format!("runtime action {id} failed")
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
    /// Model-written summaries are enabled for this session.
    compact: bool,
    /// The cross-session profile this session mirrors into.
    profile: String,
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

/// What one branch read yields: the log plus the lines and settings stored with it.
struct Hydrated {
    memory: Memory,
    compact: bool,
    profile: String,
}

/// Reload the log from the current session branch. Only `optchat-entry` entries
/// are requested, and a host that cannot filter them is refused rather than
/// reading the whole branch.
fn hydrate() -> Result<Hydrated, String> {
    let mut entries: Vec<memory::Entry> = Vec::new();
    let mut nodes: Vec<(u32, u64, String)> = Vec::new();
    let mut compact: Option<bool> = None;
    let mut profile: Option<String> = None;
    let mut view: Option<(Vec<(u32, u64)>, bool)> = None;
    let mut after: Option<u64> = None;
    for _ in 0..(MAX_ENTRIES / HYDRATE_PAGE + 4) {
        let mut args = json!({
            "customTypes": [
                ENTRY_TYPE,
                compact::NODE_TYPE,
                compact::CONFIG_TYPE,
                PROFILE_TYPE,
                VIEW_TYPE,
            ],
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
            let Some(custom_type) = record.get("customType").and_then(Value::as_str) else {
                continue;
            };
            if custom_type != ENTRY_TYPE
                && custom_type != compact::NODE_TYPE
                && custom_type != compact::CONFIG_TYPE
                && custom_type != PROFILE_TYPE
                && custom_type != VIEW_TYPE
            {
                continue;
            }
            let Some(data) = record.get("data") else {
                continue;
            };
            match custom_type {
                ENTRY_TYPE => {
                    if let Ok(entry) = serde_json::from_value::<memory::Entry>(data.clone()) {
                        entries.push(entry);
                    }
                }
                compact::NODE_TYPE => {
                    let l = data.get("l").and_then(Value::as_u64).unwrap_or(0) as u32;
                    let i = data.get("i").and_then(Value::as_u64).unwrap_or(0);
                    if let Some(text) = data.get("text").and_then(Value::as_str) {
                        nodes.push((l, i, text.to_string()));
                    }
                }
                compact::CONFIG_TYPE => {
                    if let Some(enabled) = data.get("compact").and_then(Value::as_bool) {
                        compact = Some(enabled);
                    }
                }
                PROFILE_TYPE => {
                    if let Some(name) = data.get("name").and_then(Value::as_str) {
                        profile = Some(name.to_string());
                    }
                }
                _ => {
                    let parts: Vec<(u32, u64)> = data
                        .get("parts")
                        .and_then(Value::as_array)
                        .map(|parts| {
                            parts
                                .iter()
                                .filter_map(|part| {
                                    let part = part.as_array()?;
                                    Some((
                                        u32::try_from(part.first()?.as_u64()?).ok()?,
                                        part.get(1)?.as_u64()?,
                                    ))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let merging = data
                        .get("merging")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    view = Some((parts, merging));
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
    let mut memory = Memory::load(entries);
    memory.trim_to(MAX_ENTRIES);
    for (l, i, text) in nodes {
        memory.install_node(l, i, text);
    }
    if let Some((parts, merging)) = view {
        // A shape saved before a trim may no longer tile the window; then the
        // log is folded afresh instead of refusing the session.
        memory.restore_view(&parts, merging);
    }
    Ok(Hydrated {
        memory,
        compact: compact.unwrap_or_else(compact::env_enabled),
        profile: profile
            .and_then(|name| store::normalize(&name))
            .unwrap_or_else(default_profile),
    })
}

/// The profile this session serves: the launch environment first, then the
/// remembered binding, then the neutral default.
fn default_profile() -> String {
    std::env::var("RPI_OPTCHAT_PROFILE")
        .ok()
        .and_then(|name| store::normalize(&name))
        .unwrap_or_else(|| "default".to_string())
}

/// The id of the newest message the branch holds, read in one cheap query.
/// `None` means the branch holds no memory entry at all.
fn branch_mark() -> Result<Option<u64>, String> {
    let value = call_runtime(
        RuntimeActionId::GetSessionBranch,
        json!({
            "customTypes": [ENTRY_TYPE],
            "limit": 1,
            "order": "newestFirst",
        }),
    )?;
    if value.get("filtered").and_then(Value::as_bool) != Some(true) {
        return Err("This rpi host answered GetSessionBranch without `filtered: true`".into());
    }
    let Some(record) = value
        .get("entries")
        .and_then(Value::as_array)
        .and_then(|entries| entries.first())
    else {
        return Ok(None);
    };
    Ok(record
        .get("data")
        .and_then(|data| data.get("i"))
        .and_then(Value::as_u64))
}

/// Whether a cached state still describes the branch it was loaded from: the
/// newest message in the branch must be the newest one this state holds. A
/// branch switch inside one session keeps `RPI_SESSION_ID`, so identity — not a
/// timestamp or a session name — decides whether the cache is still valid.
fn state_is_current(session: &str, state: &State) -> bool {
    if state.session != session {
        return false;
    }
    let expected = if state.memory.len() == 0 {
        None
    } else {
        Some(state.memory.end() - 1)
    };
    match branch_mark() {
        Ok(actual) => actual == expected,
        // A failed identity probe must not destroy a working cache: the next
        // capture still appends, and a later probe repairs the view.
        Err(_) => true,
    }
}

/// Run `f` against the state of the session this process is serving, reloading
/// the log when the host switched sessions or moved to another branch. Both the
/// hydration and the identity probe call the host, which must never happen while
/// `STATE` is held: a host call can re-enter a callback.
fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> Result<T, String> {
    let session = current_session();
    let stale = {
        let guard = STATE.lock().unwrap_or_else(|error| error.into_inner());
        match guard.as_ref() {
            Some(state) => !state_is_current(&session, state),
            None => true,
        }
    };
    if stale {
        let Hydrated {
            memory,
            compact,
            profile,
        } = hydrate()?;
        if compact {
            compact::bind_session(&call_action);
        }
        let mut guard = STATE.lock().unwrap_or_else(|error| error.into_inner());
        let still_stale = match guard.as_ref() {
            Some(state) => !state_is_current(&session, state),
            None => true,
        };
        if still_stale {
            *guard = Some(State {
                session,
                memory,
                compact,
                profile,
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

/// Drop the cached log: the session or its branch changed.
fn reset_state() {
    let mut guard = STATE.lock().unwrap_or_else(|error| error.into_inner());
    *guard = None;
}

fn with_memory<T>(f: impl FnOnce(&mut Memory) -> T) -> Result<T, String> {
    with_state(|state| f(&mut state.memory))
}

/// Persist the current view shape so a resumed session shows the same lines.
fn persist_view() {
    let Ok(shape) = with_state(|state| state.memory.view_shape()) else {
        return;
    };
    let parts: Vec<Value> = shape.0.iter().map(|(l, i)| json!([l, i])).collect();
    if let Err(error) = call_action(
        RuntimeActionId::AppendEntry as u32,
        json!({
            "customType": VIEW_TYPE,
            "data": {"parts": parts, "merging": shape.1},
        }),
    ) {
        debug(&format!("could not persist the view shape: {error}"));
    }
}

/// Install finished lines, persist them, and queue merges the view still shows
/// as extractive cuts. Runs on paths that already talk to the host, so it never
/// adds a host call of its own to a hot path.
fn service_compaction() {
    let mut installed = false;
    for node in compact::drain() {
        if node.text.is_empty() {
            continue;
        }
        let applied =
            with_state(|state| state.memory.install_node(node.l, node.i, node.text.clone()))
                .unwrap_or(false);
        if !applied {
            continue;
        }
        installed = true;
        if let Err(error) = call_action(
            RuntimeActionId::AppendEntry as u32,
            json!({
                "customType": compact::NODE_TYPE,
                "data": {"l": node.l, "i": node.i, "text": node.text},
            }),
        ) {
            debug(&format!("could not persist a summary line: {error}"));
        }
    }
    if installed {
        persist_view();
    }
    let Ok((enabled, parts)) = with_state(|state| {
        let parts = if state.compact {
            state.memory.extractive_parts(2)
        } else {
            Vec::new()
        };
        (state.compact, parts)
    }) else {
        return;
    };
    if !enabled {
        return;
    }
    for (part, source) in &parts {
        compact::enqueue(part.l, part.i, source.clone());
    }
}

/// Publish a one-line footer (`SetStatus`) so the host UI can show the memory
/// without the user asking. Best effort: a headless host accepts and ignores it.
fn publish_status() {
    let Ok(text) = with_state(|state| {
        let stats = state.memory.stats();
        let window = format!("{}..{}", state.memory.base(), state.memory.end());
        let size = match stats.bytes {
            0 => String::new(),
            bytes => format!(" · {} KB", bytes / 1024),
        };
        format!(
            "optchat · {} messages ({window}) · {} lines · compact {} · profile {}{size}",
            stats.messages,
            stats.lines,
            if state.compact { "on" } else { "off" },
            state.profile,
        )
    }) else {
        return;
    };
    let _ = call_action(
        RuntimeActionId::SetStatus as u32,
        json!({"key": "optchat", "value": text}),
    );
}

/// Drop the footer when this session stops owning it, so a closed or switched
/// session does not leave a count behind. The host clears on an empty value.
fn clear_status() {
    if let Err(error) = call_runtime(
        RuntimeActionId::SetStatus,
        json!({"key": "optchat", "value": ""}),
    ) {
        debug(&format!("could not clear the footer: {error}"));
    }
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
) -> Result<u64, String> {
    persist(&entry)?;
    // Commit into the state this entry was prepared from, without the branch
    // identity probe `with_state` runs: the probe would see the entry this call
    // just appended as a foreign change and reload the log underneath it.
    let mut guard = STATE.lock().unwrap_or_else(|error| error.into_inner());
    (|| {
        let state = guard
            .as_mut()
            .ok_or("OptChat memory state is unavailable")?;
        if state.session != session {
            return Err("OptChat session changed while persisting an entry".to_string());
        }
        let committed = state.memory.append_prepared(entry)?;
        // Retaining a window, not a prefix: past the cap the oldest messages
        // fall out instead of new ones being dropped. Trimming in batches keeps
        // the tree rebuild off the per-message path.
        if state.memory.len() > MAX_ENTRIES {
            state.memory.trim_to(MAX_ENTRIES - TRIM_SLACK);
        }
        if let Some(last) = last {
            state.last = Some(last);
        }
        Ok(committed.i)
    })()
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
        if duplicate {
            return None;
        }
        Some(state.memory.prepare(kind, text, date, receipt, origin))
    })?;
    let Some(entry) = candidate else {
        return Ok(());
    };
    match persist_then_commit(entry, &session, Some((kind, dedupe_text, millis))) {
        Ok(id) => mirror(id),
        Err(error) => debug(&format!("could not persist message: {error}")),
    }
    Ok(())
}

/// Copy one committed entry into the project profile store, where another
/// session can attach it. Best effort: the branch is authoritative.
fn mirror(id: u64) {
    let Ok((profile, entry)) =
        with_state(|state| (state.profile.clone(), state.memory.get(id).cloned()))
    else {
        return;
    };
    let Some(entry) = entry else { return };
    if let Err(error) = store::append(&profile, &entry) {
        debug(&format!(
            "could not mirror message {id} into `{profile}`: {error}"
        ));
    }
}

/// Normalize an imported date into the ISO-8601 form stored in `Entry.date`.
/// Import parsers keep only the source day; the entry keeps midnight UTC so
/// `[IMP <day>]` and `short_date` still render the original day.
fn normalize_entry_date(date: Option<String>) -> String {
    match date
        .map(|date| date.trim().to_string())
        .filter(|date| !date.is_empty())
    {
        Some(date) if date.len() == 10 => format!("{date}T00:00:00Z"),
        Some(date) => date,
        None => iso8601(now_ms()),
    }
}

/// Log an entry the user asked for (`/optchat note`, import): not deduplicated,
/// because two identical notes are two notes. It also uses the same append gate
/// as automatic capture, so its index cannot race a message event.
fn record_note(kind: Kind, text: String, origin: Option<Origin>) -> Result<u64, String> {
    record_note_at(kind, text, origin, None)
}

/// Like `record_note`, but preserves the source date for imported history.
fn record_note_at(
    kind: Kind,
    text: String,
    origin: Option<Origin>,
    date: Option<String>,
) -> Result<u64, String> {
    let date = normalize_entry_date(date);
    let _gate = APPEND_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let session = current_session();
    let entry = with_state(|state| state.memory.prepare(kind, text, date, None, origin))?;
    let id = persist_then_commit(entry, &session, None)?;
    mirror(id);
    Ok(id)
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

rpi_plugin_sdk::event_handler! {
/// Session transitions invalidate the branch-local cache. The host dispatches
/// these events around tree navigation, fork, session switching and compaction;
/// the reset is deliberately idempotent because both before/after tree events
/// may be observed by a plugin.
fn on_session_lifecycle(event, data) {
    let _scope = runtime::enter(runtime::callback(data));
    debug(&format!("{:?}", event.tag()));
    match event.tag() {
        EventTag::SessionStart | EventTag::SessionShutdown => {
            // A session that ends without an explicit `/optchat fit` still keeps
            // the shape it folded to, so resuming shows the same lines.
            if event.tag() == EventTag::SessionShutdown {
                persist_view();
                clear_status();
            }
            reset_state();
        }
        EventTag::SessionBeforeSwitch
        | EventTag::SessionBeforeFork
        | EventTag::SessionBeforeTree
        | EventTag::SessionTree => {
            // Persist the old branch before a transition, then discard the
            // cache. The next callback hydrates the branch selected by the host.
            if matches!(
                event.tag(),
                EventTag::SessionBeforeSwitch
                    | EventTag::SessionBeforeFork
                    | EventTag::SessionBeforeTree
            ) {
                persist_view();
                clear_status();
            }
            reset_state();
        }
        EventTag::SessionBeforeCompact | EventTag::SessionCompact => {
            // Compaction changes native transcript state, not the branch id;
            // the next boundary probe still provides a safe fallback for older
            // hosts that do not dispatch these lifecycle events.
        }
        _ => {}
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
    service_compaction();
    publish_status();
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
    service_compaction();
    publish_status();
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
/optchat profile [name]  show or switch the cross-session profile
/optchat recall <text>   search the profile store, beyond the retained window
/optchat attach [name]   copy a profile store into this session
/optchat compact on|off  rewrite summary lines with a child model
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
    let window = if stats.messages > 0 {
        with_memory(|memory| (memory.base(), memory.end())).unwrap_or((0, 0))
    } else {
        (0, 0)
    };
    format!(
        "OptChat memory · {} messages (ids {}..{}) · {} view lines · {} of {} bytes\n\
         user {} · talk {} · tool {} · echo {} · note {} · work {}\n\n\
         Stored as `optchat-entry` custom entries on this session branch; model \
         compaction is {}; profile {} . `/optchat view` shows the summary view; \
         `/optchat help` lists commands.",
        stats.messages,
        window.0,
        window.1,
        stats.lines,
        stats.bytes,
        VIEW,
        kinds[0],
        kinds[1],
        kinds[2],
        kinds[3],
        kinds[4],
        kinds[5],
        with_state(|state| if state.compact { "on" } else { "off" }).unwrap_or("off"),
        with_state(|state| {
            let profile = state.profile.clone();
            (profile, store::count(&state.profile))
        })
        .map(|(profile, count)| format!("`{profile}` ({count} stored)"))
        .unwrap_or_else(|_| "unavailable".to_string())
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

/// `/optchat profile`: show or switch the cross-session profile.
fn profile_command(rest: &str) -> Result<String, String> {
    if rest.is_empty() {
        let (profile, mirrored, in_store) = with_state(|state| {
            let profile = state.profile.clone();
            let mirrored = state
                .memory
                .entries()
                .iter()
                .filter(|entry| {
                    entry
                        .origin
                        .as_ref()
                        .map(|origin| origin.source == "profile")
                        .unwrap_or(false)
                })
                .count();
            let stored = store::count(&profile);
            (profile, mirrored, stored)
        })?;
        let location = store::path(&profile)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "(unusable name)".to_string());
        return Ok(format!(
            "OptChat profile `{profile}` · {in_store} messages stored · {mirrored} attached here\n\
             Store: {location}\n\
             `/optchat profile <name>` switches profile; `/optchat attach` copies the \
             store's messages into this session as imported notes."
        ));
    }
    let name = store::normalize(rest).ok_or_else(|| {
        format!("`{rest}` is not a profile name: use letters, digits, `-`, `_`, `.`")
    })?;
    with_state(|state| state.profile = name.clone())?;
    call_action(
        RuntimeActionId::AppendEntry as u32,
        json!({"customType": PROFILE_TYPE, "data": {"name": name}}),
    )
    .map_err(|error| format!("could not persist the profile binding: {error}"))?;
    Ok(format!(
        "OptChat profile is now `{name}`; new messages mirror into it. \
         `/optchat attach` imports what it already holds."
    ))
}

/// Search a profile store: the messages that may already have left the retained
/// window, so `recall` is what makes the memory effectively unbounded.
fn recall_from(base: &std::path::Path, profile: &str, needle: &str) -> Result<String, String> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Err("Usage: /optchat recall <text>".to_string());
    }
    let entries = store::read_in(base, profile)?;
    if entries.is_empty() {
        return Ok(format!(
            "Profile `{profile}` holds nothing yet; messages mirror into it as they are logged."
        ));
    }
    let memory = Memory::load(entries);
    let hits = memory.search(needle, None);
    Ok(format!(
        "Profile `{profile}` store · {} messages\n{hits}",
        memory.len()
    ))
}

/// `/optchat recall <text>`: search the profile store.
fn recall_command(rest: &str) -> Result<String, String> {
    if rest.trim().is_empty() {
        return Err("Usage: /optchat recall <text>".to_string());
    }
    let profile = with_state(|state| state.profile.clone())?;
    recall_from(&store::directory(), &profile, rest)
}

/// `/optchat attach`: copy the profile store into this session as imported notes,/// `/optchat attach`: copy the profile store into this session as imported notes,
/// skipping anything this session already logged.
fn attach_command(rest: &str) -> Result<String, String> {
    let profile = with_state(|state| state.profile.clone())?;
    let name = if rest.is_empty() {
        profile
    } else {
        store::normalize(rest).ok_or_else(|| format!("`{rest}` is not a profile name"))?
    };
    let stored = store::read(&name)?;
    if stored.is_empty() {
        return Ok(format!("Profile `{name}` holds nothing yet."));
    }
    let known: std::collections::HashSet<(String, String)> = with_state(|state| {
        state
            .memory
            .entries()
            .iter()
            .map(|entry| (entry.text.clone(), entry.date.clone()))
            .collect()
    })?;
    let mut added = 0usize;
    let mut skipped = 0usize;
    for entry in stored {
        if known.contains(&(entry.text.clone(), entry.date.clone())) {
            skipped += 1;
            continue;
        }
        let origin = Origin {
            source: "profile".into(),
            conversation: name.clone(),
            message: entry.i.to_string(),
            title: name.clone(),
            project: None,
        };
        let date = entry.date.clone();
        record_note_at(entry.kind, entry.text, Some(origin), Some(date))?;
        added += 1;
    }
    Ok(format!(
        "Attached {added} messages from `{name}`; {skipped} were already here."
    ))
}

fn compact_status() -> String {
    let enabled = with_state(|state| state.compact).unwrap_or(false);
    let pending = with_state(|state| state.memory.extractive_parts(usize::MAX).len()).unwrap_or(0);
    format!(
        "Model compaction is {}. {pending} merge(s) are still extractive.\n\
         `/optchat compact on` enables it for this session (each line costs one \
         model call through a child `rpi -p`); `/optchat compact off` falls back to \
         extractive cuts. `RPI_OPTCHAT_COMPACT=auto` sets the launch default.",
        if enabled { "on" } else { "off" }
    )
}

fn set_compaction(enabled: bool) -> Result<String, String> {
    with_state(|state| state.compact = enabled)?;
    call_action(
        RuntimeActionId::AppendEntry as u32,
        json!({
            "customType": compact::CONFIG_TYPE,
            "data": {"compact": enabled},
        }),
    )
    .map_err(|error| format!("could not persist the compaction setting: {error}"))?;
    if enabled {
        compact::bind_session(&call_action);
        service_compaction();
    }
    Ok(if enabled {
        "Model compaction is on for this session; queued merges will be rewritten as the engine answers.".to_string()
    } else {
        "Model compaction is off; the view keeps extractive cuts.".to_string()
    })
}

fn fit_command() -> Result<String, String> {
    let text = with_memory(|memory| {
        let before = memory.stats();
        memory.collapse();
        let after = memory.stats();
        format!(
            "Merged the view from {} to {} lines ({} to {} bytes).",
            before.lines, after.lines, before.bytes, after.bytes
        )
    })?;
    persist_view();
    Ok(text)
}

/// A line of an import file: JSON text when the line is a JSON object/string,
/// otherwise the raw line.
pub(crate) fn import_line(line: &str) -> String {
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

/// `/optchat import <file> [--format claude|chatgpt|jsonl|text]`.
fn import_command(rest: &str) -> Result<String, String> {
    let mut format: Option<String> = None;
    let mut target: Option<String> = None;
    let mut tokens = rest.split_whitespace();
    while let Some(token) = tokens.next() {
        match token {
            "--format" | "-f" => {
                format = Some(
                    tokens
                        .next()
                        .ok_or("`--format` needs a name: claude, chatgpt, jsonl, text")?
                        .to_string(),
                );
            }
            other if target.is_none() => target = Some(other.trim_matches('"').to_string()),
            _ => {}
        }
    }
    let target =
        target.ok_or("Usage: /optchat import <file> [--format claude|chatgpt|jsonl|text]")?;
    let path = PathBuf::from(&target);
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
    let format = import::Format::resolve(format.as_deref(), &target, &text)
        .ok_or_else(|| "unknown `--format`: use claude, chatgpt, jsonl, or text".to_string())?;
    let items = import::parse(format, &text, MAX_IMPORT_LINES);
    if items.is_empty() {
        return Err(format!(
            "`{conversation}` holds no messages this parser recognises ({} format).",
            format.as_str()
        ));
    }
    let mut added = 0usize;
    for item in items {
        let origin = import::origin(format, &conversation, added, item.date.as_deref());
        let date = item.date;
        match record_note_at(item.kind, item.text, Some(origin), date) {
            Ok(_) => added += 1,
            Err(error) => return Err(format!("stopped after {added} messages: {error}")),
        }
    }
    Ok(format!(
        "Imported {added} messages from `{conversation}` as {} history.",
        format.as_str()
    ))
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
        "profile" => profile_command(rest),
        "recall" => recall_command(rest),
        "attach" => attach_command(rest),
        "compact" | "compress" => match rest {
            "" | "status" => Ok(compact_status()),
            "on" | "off" => set_compaction(rest == "on"),
            other => Err(format!(
                "`{other}` is not a compaction setting: use `/optchat compact on|off`"
            )),
        },
        "fit" => fit_command(),
        "help" | "-h" | "--help" => Ok(HELP.to_string()),
        other => Err(format!("unknown /optchat command `{other}`\n\n{HELP}")),
    };
    match result {
        Ok(text) => text,
        Err(error) => zoom_hint(error),
    }
}

/// A `zoom` past the retained window is answered by the store, not by luck.
fn zoom_hint(error: String) -> String {
    if error.contains("memory holds messages") {
        return format!(
            "⚠️ {error}\nOlder messages are kept in the profile store: `/optchat recall <text>` \
             searches it, and `/optchat attach` copies it back in."
        );
    }
    format!("⚠️ {error}")
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
    service_compaction();
    publish_status();
    let text = run_command(args);
    command_output(out, json!({"kind": "message", "text": text, "markdown": true}))
}
}

// ---------------------------------------------------------------------------
// Inline transcript rendering
// ---------------------------------------------------------------------------

// Render OptChat's custom entries when a host supplies the entry-renderer
// capability. Raw entries and summary nodes stay recognizable in the TUI while
// `optchat-view`/`optchat-profile` metadata remains silent.
rpi_plugin_sdk::string_handler! {
fn optchat_entry_renderer(input, out, _data) {
    let envelope: Value = serde_json::from_str(input).unwrap_or(Value::Null);
    let Some(custom_type) = envelope.get("customType").and_then(Value::as_str) else {
        return 1;
    };
    let Some(data) = envelope.get("data") else {
        return 1;
    };
    let text = match custom_type {
        ENTRY_TYPE => {
            let Ok(entry) = serde_json::from_value::<memory::Entry>(data.clone()) else {
                return 1;
            };
            format!("OptChat {} · {}", entry.kind.as_str(), clip(&entry.text, 320))
        }
        compact::NODE_TYPE => {
            let Some(text) = data.get("text").and_then(Value::as_str) else {
                return 1;
            };
            let level = data.get("l").and_then(Value::as_u64).unwrap_or_default();
            let index = data.get("i").and_then(Value::as_u64).unwrap_or_default();
            format!("OptChat summary {level}+{index} · {}", clip(text, 320))
        }
        _ => return 1,
    };
    out.write(json!({"text": text}).to_string());
    0
}
}

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
    let lifecycle_tags = [
        EventTag::SessionStart,
        EventTag::SessionShutdown,
        EventTag::SessionBeforeSwitch,
        EventTag::SessionBeforeFork,
        EventTag::SessionBeforeCompact,
        EventTag::SessionCompact,
        EventTag::SessionBeforeTree,
        EventTag::SessionTree,
    ];
    for tag in lifecycle_tags {
        let rc = register_event(tag, on_session_lifecycle as EventHandlerFn, api.user_data);
        if rc != 0 {
            return rc;
        }
    }

    if let Some(register_entry_renderer) = api.register_entry_renderer {
        let rc = register_entry_renderer(
            StbStringRef::from_str("optchat"),
            optchat_entry_renderer,
            free_string,
            api.user_data,
        );
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
    fn imported_dates_are_normalized_to_iso_midnight() {
        assert_eq!(
            normalize_entry_date(Some("2026-02-03".into())),
            "2026-02-03T00:00:00Z"
        );
        assert_eq!(
            normalize_entry_date(Some(" 2026-02-03T04:05:06Z ".into())),
            "2026-02-03T04:05:06Z"
        );
        assert!(normalize_entry_date(None).ends_with('Z'));
    }

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
    fn entry_renderer_formats_optchat_entries_and_ignores_other_types() {
        let render = |value: Value| {
            let mut out = StbString::empty();
            let rc = optchat_entry_renderer(
                StbStringRef::from_str(&value.to_string()),
                &mut out,
                std::ptr::null_mut(),
            );
            let text = out.to_string_lossy();
            free_string(out);
            (rc, text)
        };
        let (rc, text) = render(json!({
            "customType": compact::NODE_TYPE,
            "data": {"l": 2, "i": 3, "text": "bounded summary"}
        }));
        assert_eq!(rc, 0);
        assert!(text.contains("OptChat summary 2+3"), "{text}");
        assert!(text.contains("bounded summary"), "{text}");

        let (rc, text) = render(json!({
            "customType": "unrelated-entry",
            "data": {"text": "leave this to another renderer"}
        }));
        assert_eq!(rc, 1);
        assert!(text.is_empty());
    }

    #[test]
    fn entry_renderer_formats_a_memory_entry() {
        let entry = memory::Entry {
            i: 4,
            kind: Kind::Note,
            text: "a durable note".into(),
            date: iso8601(0),
            receipt: None,
            origin: None,
        };
        let mut out = StbString::empty();
        let input = json!({
            "customType": ENTRY_TYPE,
            "data": serde_json::to_value(entry).expect("entry json"),
        });
        assert_eq!(
            optchat_entry_renderer(
                StbStringRef::from_str(&input.to_string()),
                &mut out,
                std::ptr::null_mut(),
            ),
            0
        );
        let rendered = out.to_string_lossy();
        free_string(out);
        assert!(rendered.contains("OptChat note"), "{rendered}");
        assert!(rendered.contains("a durable note"), "{rendered}");
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
    fn a_zoom_past_the_window_points_at_recall() {
        let hinted = zoom_hint("No message 3: memory holds messages 7..9".to_string());
        assert!(hinted.contains("recall <text>"), "{hinted}");
        assert!(hinted.contains("memory holds messages 7..9"), "{hinted}");
        let plain = zoom_hint("zoom needs an integer `id`".to_string());
        assert!(!plain.contains("recall"), "{plain}");
        assert_eq!(plain, "⚠️ zoom needs an integer `id`");
    }

    #[test]
    fn recall_reads_the_store_without_a_session() {
        let dir = std::env::temp_dir().join(format!("optchat-recall-{}", std::process::id()));
        let base = dir.join(".rpi").join("optchat");
        let entry = |i: u64, text: &str| memory::Entry {
            i,
            kind: Kind::Note,
            text: text.to_string(),
            date: iso8601(1_700_000_000_000 + i as i64 * 1000),
            receipt: None,
            origin: None,
        };
        store::append_in_for_tests(&base, "work", &entry(0, "the parser bug was fixed"))
            .expect("append");
        store::append_in_for_tests(&base, "work", &entry(1, "unrelated note")).expect("append");

        let hits = recall_from(&base, "work", "PARSER").expect("recall");
        assert!(hits.contains("Profile `work` store"), "{hits}");
        assert!(hits.contains("1 message contains"), "{hits}");
        assert!(hits.contains("parser bug"), "{hits}");
        assert!(!hits.contains("unrelated note"), "{hits}");

        assert!(recall_from(&base, "work", "   ").is_err());
        let empty = recall_from(&base, "absent", "anything").expect("empty store");
        assert!(empty.contains("holds nothing yet"), "{empty}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn profile_commands_stay_inside_the_store_directory() {
        // Without a host the commands report unavailability instead of panicking.
        assert!(run_command("profile").contains("unavailable"));
        assert!(run_command("attach").contains("unavailable"));
        // A traversal attempt is refused before any file is touched.
        let refused = run_command("profile ../escape");
        assert!(refused.contains("not a profile name"), "{refused}");
        let refused = run_command("attach ../escape");
        assert!(
            refused.contains("not a profile name") || refused.contains("unavailable"),
            "{refused}"
        );
    }

    #[test]
    fn compact_command_reports_state_without_a_host() {
        let text = run_command("compact");
        assert!(text.contains("Model compaction is off"), "{text}");
        assert!(text.contains("extractive"), "{text}");
        assert!(run_command("compact nope").contains("not a compaction setting"));
        assert!(run_command("help").contains("/optchat compact on|off"));
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
