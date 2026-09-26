use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rpi_plugin_sdk::{
    register_entrypoint_unified, FreeStringFn, PluginApi, StbString, StbStringRef, StepHandle,
    StepResult, ToolPartialCb, HOST_CONTEXT_KEY,
};
use serde_json::{json, Value};

struct Drive {
    params: Value,
    cancelled: AtomicBool,
    completed: bool,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Serializes the read-modify-write performed by [`todo`].
///
/// rpi can run several tool calls from one turn concurrently. Without this,
/// two `add`s both `load()` the same items, compute the same `next_id`, and
/// the second `save` overwrites the first — three parallel `todo` calls were
/// observed losing two entries. The guard is held for the whole load/apply/
/// save so this process only ever mutates the store one call at a time.
///
/// Cross-process writers (two rpi sessions sharing one project) still rely on
/// the atomic rename in [`save`] for integrity; they can overwrite each
/// other's updates, which is a pre-existing limitation.
static STORE_LOCK: Mutex<()> = Mutex::new(());

/// Makes each [`save`] temp-file name unique. With the pid this removes the
/// collision that used to make concurrent saves contend for the same
/// `todo.json.tmp-<pid>` path: one rename consumed it and the others failed
/// with "cannot find the file".
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Reserved key the host uses to hand a plugin its session context.
///
/// The host defines it (`rpi_plugin_sdk::HOST_CONTEXT_KEY`) and injects it into
/// every plugin tool call; see `docs/extension-authoring.md` in pi-rust for the
/// contract, including the one failure mode worth knowing — a
/// `#[serde(deny_unknown_fields)]` parameter struct rejects the key.

/// Which list a call acts on.
///
/// A todo list is a plan, and a plan belongs to the conversation that made it.
/// One list per project means a new session opens on last week's abandoned
/// items — and the agent then plans against work that is not its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    /// This conversation (the default).
    Session,
    /// The project's durable backlog: `<root>/.rpi/todos/project.json`.
    Project,
}

impl Scope {
    fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw.map(str::trim) {
            None | Some("") | Some("session") => Ok(Scope::Session),
            Some("project") => Ok(Scope::Project),
            Some(other) => Err(format!(
                "scope must be \"session\" or \"project\", got {other:?}"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Scope::Session => "session",
            Scope::Project => "project",
        }
    }
}

/// The project a call works in, and the session it belongs to.
///
/// Both come from the host-injected `__rpi` object. A model-supplied `cwd` is
/// still honored as a fallback — hosts older than the injection, and the tests —
/// but the injected value wins when present: a path the model guessed is a
/// silent way to split one store into two.
struct Store {
    root: PathBuf,
    session_id: Option<String>,
}

impl Store {
    fn from_params(params: &Value) -> Self {
        let context = params.get(HOST_CONTEXT_KEY);
        let injected = |key: &str| {
            context
                .and_then(|c| c.get(key))
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
        };
        let root = injected("cwd")
            .or_else(|| {
                params
                    .get("cwd")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
            })
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        Self {
            root,
            session_id: injected("sessionId").map(str::to_string),
        }
    }

    /// `Session` with no session id (an ephemeral session, or a host that does
    /// not inject one) resolves to the project list — a shared pseudo-session
    /// would reintroduce exactly the mixing this scoping exists to stop.
    fn resolve(&self, requested: Scope) -> Scope {
        match requested {
            Scope::Session if self.session_id.is_none() => Scope::Project,
            other => other,
        }
    }

    fn path(&self, scope: Scope) -> PathBuf {
        let dir = self.root.join(".rpi").join("todos");
        match scope {
            Scope::Project => dir.join("project.json"),
            Scope::Session => match &self.session_id {
                Some(id) => dir.join(format!("{}.json", sanitize_id(id))),
                None => dir.join("project.json"),
            },
        }
    }

    /// The pre-0.2 layout: one list per project, at `<root>/.rpi/todo.json`.
    fn legacy_project_path(&self) -> PathBuf {
        self.root.join(".rpi").join("todo.json")
    }

    /// Read a scope's list, falling back to the legacy single-file layout the
    /// first time the project list is touched — an existing backlog is adopted
    /// rather than silently starting empty. The next mutation writes the new
    /// path; the old file is left on disk untouched.
    fn load_scope(&self, scope: Scope) -> Result<Vec<Value>, String> {
        let path = self.path(scope);
        if path.exists() {
            return load(&path);
        }
        if scope == Scope::Project {
            let legacy = self.legacy_project_path();
            if legacy.exists() {
                return load(&legacy);
            }
        }
        Ok(Vec::new())
    }
}

/// Session ids are UUIDs, but this value arrives as JSON across a plugin
/// boundary: dropping path separators means a malformed or hostile id cannot
/// make the store write outside `.rpi/todos/`.
fn sanitize_id(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "session".to_string()
    } else {
        cleaned
    }
}

fn load(path: &PathBuf) -> Result<Vec<Value>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).map_err(|e| format!("read todo store: {e}"))?;
    // Accept the current array format plus older JSONL/concatenated JSON
    // documents. This prevents a previously appended record from making the
    // whole todo store unusable with "trailing characters" errors.
    let mut stream = serde_json::Deserializer::from_str(&text).into_iter::<Value>();
    let mut items = Vec::new();
    while let Some(value) = stream.next() {
        let value = value.map_err(|e| format!("parse todo store: {e}"))?;
        match value {
            Value::Array(values) => items.extend(values),
            Value::Object(mut object) => {
                if let Some(Value::Array(values)) = object.remove("items") {
                    items.extend(values);
                } else {
                    items.push(Value::Object(object));
                }
            }
            other => {
                return Err(format!(
                    "parse todo store: expected an item object or array, got {}",
                    other
                ));
            }
        }
    }
    Ok(items)
}

fn save(path: &PathBuf, items: &[Value]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create todo directory: {e}"))?;
    }
    let text =
        serde_json::to_string_pretty(items).map_err(|e| format!("encode todo store: {e}"))?;
    // Write to a sibling temp file and rename it into place.
    //
    // A plain `fs::write` opens with `CREATE_ALWAYS`/`O_TRUNC` and then writes
    // in place. Two rpi sessions sharing one project (or one session caught
    // mid-write) can therefore leave the *tail of the previous, longer
    // document* after the new one — the `[...]\n  {"id":3,\n…\n}` shape that
    // `load` has to defend against with a concatenated-JSON reader. Writing a
    // complete document elsewhere and renaming it in makes readers observe
    // either the old document or the new one, never a mixture: `rename`
    // replaces atomically on Unix and on Windows (`MoveFileEx` with
    // `MOVEFILE_REPLACE_EXISTING`).
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp = path.with_extension(format!("json.tmp-{}-{}-{}", std::process::id(), nanos, seq));
    fs::write(&tmp, &text).map_err(|e| format!("write todo store: {e}"))?;
    if let Err(error) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("commit todo store: {error}"));
    }
    Ok(())
}

/// Collapse whitespace and drop a leading list marker from an incoming todo
/// text.
///
/// Agents routinely pass whole plan lines — `"1. Tool names should be
/// lowercase"`, `"- Fix the bash border"` — and that used to create a second
/// todo for a task that was already tracked, so the plan looked unfinished and
/// the agent re-planned it. The stored text should be the task, not the list
/// position it happened to be read from.
fn normalize_text(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut text = collapsed.as_str();

    // `1.` / `12)` — a plan-list number. Only a marker when a separator follows
    // it (`1. Foo`, `2)Foo`): `1.5x` is a value, not a list position. Leading
    // digits are ASCII, so the byte offset equals the char offset and
    // `digits + 1` is always a char boundary.
    let digits = text.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        if matches!(text.as_bytes().get(digits), Some(b'.') | Some(b')')) {
            let after = &text[digits + 1..];
            if after.is_empty() || after.starts_with(char::is_whitespace) {
                text = after.trim_start();
            }
        }
    }

    // `-` / `*` / `•` / `·` bullets. A bare `-` or `*` needs a following space
    // so a hyphenated word or emphasised text is left alone.
    if let Some(first) = text.chars().next() {
        let rest = &text[first.len_utf8()..];
        let is_bullet = match first {
            '•' | '·' => true,
            '-' | '*' => rest.is_empty() || rest.starts_with(char::is_whitespace),
            _ => false,
        };
        if is_bullet {
            text = rest.trim_start();
        }
    }

    text.trim().to_string()
}

fn next_id(items: &[Value]) -> u64 {
    items
        .iter()
        .filter_map(|v| v.get("id").and_then(Value::as_u64))
        .max()
        .unwrap_or(0)
        + 1
}

/// Render a list for the transcript.
///
/// A task list (`- [ ]` / `- [x]`) rather than the table this used to emit: the
/// TUI's markdown renderer turns those markers into `☐` / `☒`, so it reads as a
/// checklist, and each item costs one line instead of a bordered row whose widest
/// column is always the task text. Completed items are folded into a count line
/// unless asked for — a plan's history is not what the next action needs, and
/// unbounded history is what buried the pending items in the first place.
fn display_list(items: &[Value], scope: Scope, include_done: bool) -> String {
    let total = items.len();
    let completed = items
        .iter()
        .filter(|item| item.get("done") == Some(&Value::Bool(true)))
        .count();
    let pending = total - completed;
    if total == 0 {
        return match scope {
            Scope::Session => "📋 No todos in this session".to_string(),
            Scope::Project => "📋 No todos in the project list".to_string(),
        };
    }

    let mut output = String::new();
    let marker = match scope {
        Scope::Session => String::new(),
        Scope::Project => "**project** · ".to_string(),
    };
    output.push_str(&format!(
        "📋 {}{} pending · {} done · {} {}%\n\n",
        marker,
        pending,
        completed,
        progress_bar(completed, total),
        (completed * 100) / total,
    ));

    // Pending first, then completed; each group by id so the order is stable
    // across calls (ids are per-list and monotonic, so it is also creation
    // order).
    let mut sorted: Vec<&Value> = items.iter().collect();
    sorted.sort_by_key(|item| {
        (
            item.get("done") == Some(&Value::Bool(true)),
            item.get("id").and_then(Value::as_u64).unwrap_or(0),
        )
    });

    for item in sorted
        .iter()
        .filter(|i| i.get("done") != Some(&Value::Bool(true)))
    {
        output.push_str(&format!("- [ ] {}\n", task_line(item)));
    }
    let done_items: Vec<&&Value> = sorted
        .iter()
        .filter(|i| i.get("done") == Some(&Value::Bool(true)))
        .collect();
    if include_done {
        for item in done_items {
            output.push_str(&format!("- [x] {}\n", task_line(item)));
        }
    } else if completed > 0 {
        output.push_str(&format!(
            "\n✓ {} completed — `includeDone=true` lists them\n",
            completed
        ));
    }

    output.trim_end().to_string()
}

/// `#id text` plus backticked tags. Backticks rather than an emoji: the renderer
/// colours inline code, so tags stay scannable without a column that is usually
/// empty.
fn task_line(item: &Value) -> String {
    let id = item.get("id").and_then(Value::as_u64).unwrap_or(0);
    let text = item.get("text").and_then(Value::as_str).unwrap_or("");
    let tags = item
        .get("tags")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(|tag| format!("`{tag}`"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    if tags.is_empty() {
        format!("#{id} {text}")
    } else {
        format!("#{id} {text} · {tags}")
    }
}

/// A 10-cell bar. Ten, not twenty: twenty cells plus a percentage plus the
/// counts is wider than the tool panel on a split terminal, and the bar is a
/// glance, not a measurement.
fn progress_bar(done: usize, total: usize) -> String {
    const WIDTH: usize = 10;
    let filled = if total == 0 {
        0
    } else {
        (done * WIDTH) / total
    };
    format!("{}{}", "█".repeat(filled), "░".repeat(WIDTH - filled))
}

/// What a response says about itself, so the six action arms do not each repeat
/// the same three arguments.
struct Reply<'a> {
    action: &'a str,
    /// The scope actually acted on, after `Session`→`Project` fallback.
    scope: Scope,
    session_id: Option<&'a str>,
    /// The caller asked for the session list but there was no session id.
    fell_back: bool,
}

impl Reply<'_> {
    /// The list body for this reply's scope.
    fn list(&self, items: &[Value], include_done: bool) -> String {
        let mut rendered = display_list(items, self.scope, include_done);
        if self.fell_back {
            rendered.push_str("\n\n_(no session id from the host — this is the project list)_");
        }
        rendered
    }
}

fn result(
    reply: &Reply<'_>,
    items: &[Value],
    all_items: &[Value],
    content: impl Into<String>,
) -> String {
    json!({
        "content": [{"type": "text", "text": content.into()}],
        "details": {
            "kind": "todo",
            "mode": "check",
            "action": reply.action,
            // Which list was acted on. "The todo list" is no longer one thing:
            // a consumer that assumes a single list is how sessions got mixed.
            "scope": reply.scope.as_str(),
            "sessionId": reply.session_id,
            // Opt into markdown rendering in the rpi TUI: the content is a task
            // list + progress bar, which reads as literal text without this
            // flag. See pi-rust `tool_result_requests_markdown`.
            "markdown": true,
            "todos": items,
            // Always the complete list for this scope, never a filtered view:
            // a hidden completed item must not let the next add reuse its id.
            "nextId": next_id(all_items),
        }
    })
    .to_string()
}

fn todo(params: &Value) -> Result<String, String> {
    // Serialize the whole read-modify-write; see [`STORE_LOCK`].
    let _guard = STORE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("list");
    let requested = Scope::parse(params.get("scope").and_then(Value::as_str))?;
    let store = Store::from_params(params);
    let scope = store.resolve(requested);
    let reply = Reply {
        action,
        scope,
        session_id: store.session_id.as_deref(),
        fell_back: scope != requested,
    };
    let path = store.path(scope);
    let mut items = store.load_scope(scope)?;
    // Confirmations render the list folded: the pending items are the point, and
    // an `add` is usually followed by another `add`.
    match action {
        "add" => {
            let text = normalize_text(params.get("text").and_then(Value::as_str).unwrap_or(""));
            if text.is_empty() || text.chars().count() > 2000 {
                return Err("text must contain 1-2000 characters".into());
            }
            // Re-adding a task that is already pending returns the existing
            // entry instead of tracking it twice. Plan lines are often restated
            // verbatim across turns; a duplicate made the todo list look like
            // it still had the work outstanding. Scoped to this list: the same
            // task may legitimately exist in both the session and the backlog.
            if let Some(existing) = items.iter().find(|item| {
                item.get("done") != Some(&Value::Bool(true))
                    && item
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|stored| normalize_text(stored) == text)
            }) {
                let id = existing.get("id").and_then(Value::as_u64).unwrap_or(0);
                return Ok(result(
                    &reply,
                    &items,
                    &items,
                    format!(
                        "# {id} already tracks: {text}\n\n{}",
                        reply.list(&items, false)
                    ),
                ));
            }
            let id = next_id(&items);
            let tags = params
                .get("tags")
                .cloned()
                .filter(|v| v.is_array())
                .unwrap_or_else(|| json!([]));
            items.push(json!({"id": id, "text": text, "done": false, "tags": tags, "createdAt": now(), "updatedAt": now()}));
            save(&path, &items)?;
            Ok(result(
                &reply,
                &items,
                &items,
                format!("✓ Added #{id} {}\n\n{}", text, reply.list(&items, false)),
            ))
        }
        "done" | "complete" | "check" | "toggle" => {
            let id = params
                .get("id")
                .and_then(Value::as_u64)
                .ok_or("id is required")?;
            let updated = {
                let item = items
                    .iter_mut()
                    .find(|v| v.get("id").and_then(Value::as_u64) == Some(id))
                    .ok_or("todo item not found")?;
                item["done"] = if action == "toggle" {
                    Value::Bool(item.get("done") != Some(&Value::Bool(true)))
                } else {
                    Value::Bool(true)
                };
                item["updatedAt"] = json!(now());
                item.clone()
            };
            save(&path, &items)?;
            let id = updated.get("id").and_then(Value::as_u64).unwrap_or(0);
            let text = updated.get("text").and_then(Value::as_str).unwrap_or("");
            let done = updated.get("done") == Some(&Value::Bool(true));
            let verb = if done { "checked" } else { "unchecked" };
            Ok(result(
                &reply,
                &items,
                &items,
                format!("✓ #{id} {verb} {text}\n\n{}", reply.list(&items, false)),
            ))
        }
        "remove" => {
            let id = params
                .get("id")
                .and_then(Value::as_u64)
                .ok_or("id is required")?;
            let before = items.len();
            items.retain(|v| v.get("id").and_then(Value::as_u64) != Some(id));
            if items.len() == before {
                return Err("todo item not found".into());
            }
            save(&path, &items)?;
            Ok(result(
                &reply,
                &items,
                &items,
                format!("✓ Removed todo #{id}\n\n{}", reply.list(&items, false)),
            ))
        }
        "clear" => {
            items.clear();
            save(&path, &items)?;
            let scope_label = match scope {
                Scope::Session => "this session",
                Scope::Project => "the project list",
            };
            Ok(result(
                &reply,
                &items,
                &items,
                format!(
                    "✓ Cleared all todos in {scope_label}\n\n{}",
                    reply.list(&items, false)
                ),
            ))
        }
        "list" => {
            // Completed items are folded unless asked for; see `display_list`.
            let include_done = params
                .get("includeDone")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(result(
                &reply,
                &items,
                &items,
                reply.list(&items, include_done),
            ))
        }
        _ => Err(
            "action must be one of: add, list, done, complete, check, toggle, remove, clear".into(),
        ),
    }
}

extern "C" fn execute(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let text = params.to_string_lossy();
    params.free_with(free);
    Box::into_raw(Box::new(Drive {
        params: serde_json::from_str(&text).unwrap_or(Value::Null),
        cancelled: AtomicBool::new(false),
        completed: false,
    })) as StepHandle
}
extern "C" fn poll(handle: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null todo handle".into()));
    }
    let drive = unsafe { &mut *(handle as *mut Drive) };
    if drive.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("todo cancelled".into()));
    }
    if drive.completed {
        return StepResult::err(StbString::from_string(
            "todo polled after completion".into(),
        ));
    }
    drive.completed = true;
    match todo(&drive.params) {
        Ok(text) => {
            let output = serde_json::from_str::<Value>(&text)
                .unwrap_or_else(|_| json!({"content":[{"type":"text","text":text}]}));
            StepResult::done(StbString::from_string(output.to_string()))
        }
        Err(e) => StepResult::err(StbString::from_string(e)),
    }
}
extern "C" fn cancel(handle: StepHandle) {
    if !handle.is_null() {
        unsafe {
            (&*(handle as *mut Drive))
                .cancelled
                .store(true, Ordering::SeqCst);
        }
    }
}
extern "C" fn destroy(handle: StepHandle) {
    if !handle.is_null() {
        unsafe {
            drop(Box::from_raw(handle as *mut Drive));
        }
    }
}
extern "C" fn free_string(s: StbString) {
    if !s.is_empty() && !s.ptr.is_null() {
        unsafe {
            let slice = std::slice::from_raw_parts(s.ptr as *const u8, s.len);
            let _ = Box::from_raw(slice as *const [u8] as *mut [u8]);
        }
    }
}

#[no_mangle]
pub extern "C" fn rpi_plugin_register(api: *const PluginApi) -> i32 {
    unsafe {
        register_entrypoint_unified(api, |api| {
            let Some(register) = api.register_tool else {
                return 1;
            };
            let schema = Box::new(rpi_plugin_sdk::StableToolSchema {
            name: StbString::from_string("todo".into()),
            description: StbString::from_string(
                "Manage todos. Two separate lists: this session's plan (the default) and the project backlog \
                 (scope=\"project\"). Completed items are folded into a count unless includeDone=true."
                    .into(),
            ),
            // `cwd` is deliberately absent: the host injects the working
            // directory (and session id) into every call, and a path the model
            // supplied instead is a silent way to split one list into two. It is
            // still *read* as a fallback for hosts that do not inject yet.
            parameters: StbString::from_string(r#"{"type":"object","properties":{"action":{"type":"string","enum":["add","list","done","complete","check","toggle","remove","clear"]},"text":{"type":"string"},"id":{"type":"integer"},"tags":{"type":"array"},"scope":{"type":"string","enum":["session","project"],"description":"Which list: this session's plan (default) or the project backlog."},"includeDone":{"type":"boolean","description":"List completed items instead of folding them into a count."}}}"#.into()),
        });
            let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
            drop(schema);
            rc
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The project-scope path under a bare `cwd`. Tests that do not set up host
    /// context take this path — which is also the fallback an injected-context
    /// host takes when it has no session id to give.
    pub(crate) fn project_path(cwd: &str) -> PathBuf {
        Store::from_params(&json!({"cwd": cwd})).path(Scope::Project)
    }

    /// Parameters shaped the way a real host sends them: the working directory
    /// and session id injected under `__rpi`, not supplied by the model.
    pub(crate) fn hosted(cwd: &str, session: &str, body: Value) -> Value {
        let mut object = body.as_object().cloned().unwrap_or_default();
        object.insert(
            HOST_CONTEXT_KEY.to_string(),
            json!({"cwd": cwd, "sessionId": session}),
        );
        Value::Object(object)
    }

    #[test]
    fn loads_concatenated_json_documents() {
        let path =
            std::env::temp_dir().join(format!("rpi-todo-{}-{}.json", std::process::id(), now()));
        fs::write(
            &path,
            r#"[{"id":1,"text":"one","done":false}] {"id":2,"text":"two","done":false}"#,
        )
        .unwrap();
        let items = load(&path).unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn rejects_unknown_action() {
        assert!(todo(&json!({"action":"wat","cwd":"."})).is_err());
    }

    #[test]
    fn renders_check_mode_content_and_details() {
        let path = std::env::temp_dir().join(format!("rpi-todo-display-{}", now()));
        let cwd = path.to_string_lossy().to_string();
        let added = todo(&json!({"action":"add","text":"ship it","cwd":cwd})).unwrap();
        let value: Value = serde_json::from_str(&added).unwrap();
        assert_eq!(value["details"]["mode"], "check");
        assert_eq!(
            value["details"]["markdown"], true,
            "the TUI renders the list only when details.markdown opts in"
        );
        assert_eq!(
            value["details"]["scope"], "project",
            "a host that injects no session id lands in the project list"
        );
        let text = value["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Added #1"), "confirmation missing: {text}");
        assert!(text.contains("1 pending"), "list header missing: {text}");
        assert!(
            text.contains("- [ ] #1 ship it"),
            "task row missing: {text}"
        );
        fs::remove_dir_all(path).ok();
    }

    #[test]
    fn filtered_list_keeps_next_id_from_full_store() {
        let path = std::env::temp_dir().join(format!("rpi-todo-next-id-{}", now()));
        let cwd = path.to_string_lossy().to_string();
        todo(&json!({"action":"add","text":"first","cwd":cwd})).unwrap();
        todo(&json!({"action":"add","text":"second","cwd":cwd})).unwrap();
        todo(&json!({"action":"check","id":2,"cwd":cwd})).unwrap();
        let listed: Value = serde_json::from_str(
            &todo(&json!({"action":"list","includeDone":false,"cwd":cwd})).unwrap(),
        )
        .unwrap();
        assert_eq!(listed["details"]["nextId"], 3);
        fs::remove_dir_all(path).ok();
    }
}

#[cfg(test)]
mod display_tests {
    use super::tests::{hosted, project_path};
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_cwd() -> String {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "rpi-todo-display-{}-{}-{}",
            std::process::id(),
            now(),
            id
        ));
        std::fs::create_dir_all(&p).unwrap();
        p.to_string_lossy().to_string()
    }

    fn extract_text(json_str: &str) -> String {
        let v: Value = serde_json::from_str(json_str).unwrap();
        v["content"][0]["text"].as_str().unwrap().to_string()
    }

    #[test]
    fn add_strips_plan_list_markers() {
        // Agents pass whole plan lines. "1. Foo" and "- Foo" are the same task
        // as "Foo", and used to be tracked separately.
        assert_eq!(
            normalize_text("1. Tool names lowercase"),
            "Tool names lowercase"
        );
        assert_eq!(
            normalize_text("12) Tool names lowercase"),
            "Tool names lowercase"
        );
        assert_eq!(normalize_text("-   Bash border color"), "Bash border color");
        assert_eq!(normalize_text("* Bash border color"), "Bash border color");
        assert_eq!(normalize_text("• Bash border color"), "Bash border color");
        // Whitespace/newlines collapse so a re-wrapped plan line still matches.
        assert_eq!(normalize_text("a\n  b\tc"), "a b c");
        // A genuine decimal is NOT a list marker when no separator follows the
        // digits.
        assert_eq!(normalize_text("1.5x latency"), "1.5x latency");
        assert_eq!(normalize_text("plain task"), "plain task");
    }

    #[test]
    fn add_dedupes_against_a_pending_task() {
        let cwd = temp_cwd();
        todo(&json!({"action":"add","text":"Tool names lowercase","cwd":cwd})).unwrap();
        // The agent restates the plan line with its numbering.
        let text = extract_text(
            &todo(&json!({"action":"add","text":"1. Tool names lowercase","cwd":cwd})).unwrap(),
        );
        assert!(text.contains("already tracks"), "dedupe message: {text}");
        let items = load(&project_path(&cwd)).unwrap();
        assert_eq!(items.len(), 1, "duplicate todo was stored: {items:?}");
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn add_allows_a_task_again_once_it_is_done() {
        let cwd = temp_cwd();
        todo(&json!({"action":"add","text":"repeatable","cwd":cwd})).unwrap();
        todo(&json!({"action":"done","id":1,"cwd":cwd})).unwrap();
        todo(&json!({"action":"add","text":"repeatable","cwd":cwd})).unwrap();
        let items = load(&project_path(&cwd)).unwrap();
        assert_eq!(
            items.len(),
            2,
            "a completed task may be re-added: {items:?}"
        );
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn save_leaves_no_stale_tail_when_the_document_shrinks() {
        // The corruption this guards: a shorter document written over a longer
        // one used to leave the old tail behind (`[...]\n  {"id":3…}\n]`),
        // which is invalid JSON and only parsed thanks to `load`'s tolerant
        // concatenated-JSON reader.
        let cwd = temp_cwd();
        let path = project_path(&cwd);
        for label in ["one", "two", "three"] {
            let mut items = load(&path).unwrap();
            let id = next_id(&items);
            items.push(json!({
                "id": id,
                "text": label,
                "done": false,
                "tags": [],
                "createdAt": 0,
                "updatedAt": 0
            }));
            save(&path, &items).unwrap();
        }
        // Now shrink it back down.
        save(&path, &[]).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&raw).unwrap(),
            json!([]),
            "store must be exactly the new document, got: {raw:?}"
        );
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn add_returns_confirmation_plus_list() {
        let cwd = temp_cwd();
        let text = extract_text(&todo(&json!({"action":"add","text":"first","cwd":cwd})).unwrap());
        assert!(text.contains("Added #1 first"), "confirmation: {}", text);
        assert!(text.contains("- [ ] #1 first"), "task list row: {}", text);
        assert!(text.contains("1 pending"), "counts: {}", text);
        assert!(
            text.contains("█") || text.contains("░"),
            "progress bar: {}",
            text
        );
        // The table this used to emit is exactly what had to go.
        assert!(!text.contains("| Status |"), "no table: {}", text);
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn done_returns_confirmation_plus_list() {
        let cwd = temp_cwd();
        todo(&json!({"action":"add","text":"task a","cwd":cwd})).unwrap();
        todo(&json!({"action":"add","text":"task b","cwd":cwd})).unwrap();
        let text = extract_text(&todo(&json!({"action":"done","id":1,"cwd":cwd})).unwrap());
        assert!(text.contains("#1 checked task a"), "confirmation: {}", text);
        assert!(text.contains("50%"), "progress should be 50%: {}", text);
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn remove_returns_confirmation_plus_list() {
        let cwd = temp_cwd();
        todo(&json!({"action":"add","text":"task a","cwd":cwd})).unwrap();
        todo(&json!({"action":"add","text":"task b","cwd":cwd})).unwrap();
        let text = extract_text(&todo(&json!({"action":"remove","id":1,"cwd":cwd})).unwrap());
        assert!(text.contains("Removed todo #1"), "confirmation: {}", text);
        assert!(text.contains("task b"), "remaining task shown: {}", text);
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn clear_returns_confirmation_plus_empty() {
        let cwd = temp_cwd();
        todo(&json!({"action":"add","text":"task a","cwd":cwd})).unwrap();
        let text = extract_text(&todo(&json!({"action":"clear","cwd":cwd})).unwrap());
        assert!(text.contains("Cleared all todos"), "confirmation: {}", text);
        assert!(text.contains("No todos"), "empty list: {}", text);
        std::fs::remove_dir_all(&cwd).ok();
    }

    // ---- scoping: a plan belongs to the session that made it ----

    #[test]
    fn each_session_gets_its_own_list() {
        // The reported bug: every conversation in a project shared one list, so
        // a new session opened on another session's abandoned items.
        let cwd = temp_cwd();
        todo(&hosted(
            &cwd,
            "session-a",
            json!({"action":"add","text":"a's task"}),
        ))
        .unwrap();
        todo(&hosted(
            &cwd,
            "session-b",
            json!({"action":"add","text":"b's task"}),
        ))
        .unwrap();

        let a = extract_text(&todo(&hosted(&cwd, "session-a", json!({"action":"list"}))).unwrap());
        assert!(a.contains("a's task"), "{a}");
        assert!(
            !a.contains("b's task"),
            "session-a saw session-b's list: {a}"
        );
        assert!(a.contains("#1 "), "ids restart per list: {a}");

        let b = extract_text(&todo(&hosted(&cwd, "session-b", json!({"action":"list"}))).unwrap());
        assert!(b.contains("b's task"), "{b}");
        assert!(
            !b.contains("a's task"),
            "session-b saw session-a's list: {b}"
        );

        // One file per session — none of them the pre-0.2 single store.
        let dir = PathBuf::from(&cwd).join(".rpi").join("todos");
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        assert_eq!(names, vec!["session-a.json", "session-b.json"]);
        assert!(
            !PathBuf::from(&cwd).join(".rpi").join("todo.json").exists(),
            "the legacy single store is not what wrote these"
        );
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn the_project_backlog_is_a_second_list() {
        let cwd = temp_cwd();
        todo(&hosted(
            &cwd,
            "s1",
            json!({"action":"add","text":"session work"}),
        ))
        .unwrap();
        todo(&hosted(
            &cwd,
            "s1",
            json!({"action":"add","text":"durable work","scope":"project"}),
        ))
        .unwrap();

        let session = extract_text(&todo(&hosted(&cwd, "s1", json!({"action":"list"}))).unwrap());
        assert!(session.contains("session work"), "{session}");
        assert!(
            !session.contains("durable work"),
            "backlog leaked into the plan: {session}"
        );

        // A different session sees the backlog (it is the project's) but not
        // the first session's plan.
        let other = extract_text(
            &todo(&hosted(
                &cwd,
                "s2",
                json!({"action":"list","scope":"project"}),
            ))
            .unwrap(),
        );
        assert!(other.contains("durable work"), "{other}");
        assert!(!other.contains("session work"), "{other}");
        assert!(
            other.contains("**project**"),
            "project scope is labelled: {other}"
        );

        // The reply says which list it acted on — a consumer must not have to
        // infer it.
        let details: Value = serde_json::from_str(
            &todo(&hosted(
                &cwd,
                "s1",
                json!({"action":"list","scope":"project"}),
            ))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(details["details"]["scope"], "project");
        assert_eq!(details["details"]["sessionId"], "s1");
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn without_a_session_id_the_session_scope_uses_the_project_list() {
        // An ephemeral session (or a host that predates the context) has no id.
        // A shared pseudo-session would reintroduce the mixing, so fall back
        // and say so instead of silently inventing one.
        let cwd = temp_cwd();
        todo(&json!({"action":"add","text":"bare cwd task","cwd":cwd})).unwrap();
        let text = extract_text(&todo(&json!({"action":"list","cwd":cwd})).unwrap());
        assert!(text.contains("bare cwd task"), "{text}");
        assert!(text.contains("**project**"), "{text}");
        assert!(
            text.contains("no session id"),
            "fallback is disclosed: {text}"
        );
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn the_legacy_single_store_becomes_the_project_list() {
        // Existing installs keep their backlog: `.rpi/todo.json` is adopted as
        // the project list, then written to the new path on the next mutation.
        let cwd = temp_cwd();
        let legacy = PathBuf::from(&cwd).join(".rpi").join("todo.json");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(
            &legacy,
            r#"[{"id":1,"text":"from the old store","done":false,"tags":[]}]"#,
        )
        .unwrap();

        let text = extract_text(
            &todo(&hosted(
                &cwd,
                "s1",
                json!({"action":"list","scope":"project"}),
            ))
            .unwrap(),
        );
        assert!(
            text.contains("from the old store"),
            "legacy list adopted: {text}"
        );

        // A session list starts empty — that is the whole point of the change.
        let session = extract_text(&todo(&hosted(&cwd, "s1", json!({"action":"list"}))).unwrap());
        assert!(session.contains("No todos in this session"), "{session}");

        // First mutation migrates; the legacy file is left alone.
        todo(&hosted(
            &cwd,
            "s1",
            json!({"action":"add","text":"new","scope":"project"}),
        ))
        .unwrap();
        let migrated = load(&project_path(&cwd)).unwrap();
        assert_eq!(migrated.len(), 2, "adopted + new: {migrated:?}");
        assert!(legacy.exists(), "legacy file is not deleted");
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn unknown_scope_is_rejected_rather_than_silently_ignored() {
        let cwd = temp_cwd();
        let err = todo(&json!({"action":"list","scope":"everything","cwd":cwd})).unwrap_err();
        assert!(err.contains("scope must be"), "{err}");
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn project_scope_keeps_working_when_the_store_lives_elsewhere() {
        // A scope can never escape `.rpi/todos/`, whatever the host reports.
        let cwd = temp_cwd();
        let store = Store {
            root: PathBuf::from(&cwd),
            session_id: Some("../../etc/passwd".to_string()),
        };
        let path = store.path(Scope::Session);
        assert_eq!(
            path.parent().unwrap(),
            PathBuf::from(&cwd).join(".rpi").join("todos")
        );
        assert_eq!(path.file_name().unwrap(), "....etcpasswd.json");
        assert!(
            !path.to_string_lossy().contains("..\\") && !path.to_string_lossy().contains("../")
        );
        std::fs::remove_dir_all(&cwd).ok();
    }

    // ---- rendering ----

    #[test]
    fn completed_items_are_folded_until_asked_for() {
        let cwd = temp_cwd();
        todo(&json!({"action":"add","text":"done one","cwd":cwd})).unwrap();
        todo(&json!({"action":"add","text":"still open","cwd":cwd})).unwrap();
        todo(&json!({"action":"done","id":1,"cwd":cwd})).unwrap();

        let folded = extract_text(&todo(&json!({"action":"list","cwd":cwd})).unwrap());
        assert!(folded.contains("- [ ] #2 still open"), "{folded}");
        assert!(!folded.contains("- [x]"), "history folded: {folded}");
        assert!(folded.contains("✓ 1 completed"), "fold line: {folded}");
        assert!(folded.contains("50%"), "progress: {folded}");

        let expanded =
            extract_text(&todo(&json!({"action":"list","includeDone":true,"cwd":cwd})).unwrap());
        assert!(expanded.contains("- [x] #1 done one"), "{expanded}");
        assert!(
            !expanded.contains("completed —"),
            "no fold line: {expanded}"
        );
        // No strikethrough: the checklist marker already says it is done.
        assert!(!expanded.contains("~~"), "{expanded}");
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn tags_render_as_inline_code_without_an_empty_column() {
        let cwd = temp_cwd();
        todo(&json!({"action":"add","text":"tagged","tags":["p1","docs"],"cwd":cwd})).unwrap();
        todo(&json!({"action":"add","text":"untagged","cwd":cwd})).unwrap();
        let text = extract_text(&todo(&json!({"action":"list","cwd":cwd})).unwrap());
        assert!(text.contains("#1 tagged · `p1` `docs`"), "{text}");
        assert!(
            text.contains("- [ ] #2 untagged"),
            "no dangling separator: {text}"
        );
        assert!(!text.contains("🏷️"), "no emoji column: {text}");
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn concurrent_adds_do_not_lose_entries_or_reuse_ids() {
        // Regression: `save` used a pid-only temp name and the read-modify-write
        // was unsynchronized, so parallel tool calls clobbered each other —
        // three concurrent `add`s lost two entries and reused one id.
        let cwd = temp_cwd();
        std::thread::scope(|scope| {
            for i in 0..8 {
                let cwd = cwd.clone();
                scope.spawn(move || {
                    todo(&json!({"action":"add","text":format!("task {i}"),"cwd":cwd}))
                        .unwrap_or_else(|e| panic!("add {i} failed: {e}"));
                });
            }
        });
        let items = load(&project_path(&cwd)).unwrap();
        assert_eq!(items.len(), 8, "concurrent adds lost entries: {items:?}");
        let mut ids: Vec<u64> = items
            .iter()
            .filter_map(|v| v.get("id").and_then(Value::as_u64))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 8, "concurrent adds reused ids: {items:?}");
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn concurrent_saves_use_distinct_temp_files() {
        // Regression for the observed failure: every `save` used the same
        // `todo.json.tmp-<pid>` path, so concurrent writers raced — one rename
        // consumed the temp file, the rest failed with "cannot find the file"
        // (os error 2) and their updates were lost.
        let cwd = temp_cwd();
        let path = project_path(&cwd);
        let docs: Vec<Vec<Value>> = (0..16)
            .map(|i| vec![json!({"id": i, "text": format!("t{i}"), "done": false})])
            .collect();
        std::thread::scope(|scope| {
            for doc in &docs {
                let path = path.clone();
                scope.spawn(move || {
                    save(&path, doc).unwrap_or_else(|e| panic!("concurrent save failed: {e}"));
                });
            }
        });
        // Whichever write landed last, the store is one clean document — never
        // a mixture or a stale tail.
        let items = load(&path).unwrap();
        assert_eq!(items.len(), 1, "store is not one clean document: {items:?}");
        std::fs::remove_dir_all(&cwd).ok();
    }
}
