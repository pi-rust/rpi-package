use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rpi_plugin_sdk::{
    register_entrypoint_unified, FreeStringFn, PluginApi, StbString, StbStringRef, StepHandle,
    StepResult, ToolPartialCb,
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

fn store_path(params: &Value) -> PathBuf {
    let root = params
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    root.join(".rpi").join("todo.json")
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
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
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

fn display_list(items: &[Value]) -> String {
    if items.is_empty() {
        return "No todos".into();
    }
    let total = items.len();
    let completed = items
        .iter()
        .filter(|item| item.get("done") == Some(&Value::Bool(true)))
        .count();
    let pending = total - completed;

    // Progress bar
    let progress = if total > 0 {
        let filled = (completed * 20) / total;
        let empty = 20 - filled;
        format!("[{}{}] {}%", "█".repeat(filled), "░".repeat(empty), (completed * 100) / total)
    } else {
        "[░░░░░░░░░░░░░░░░░░░░] 0%".to_string()
    };

    let mut output = String::new();
    output.push_str(&format!("📋 **Todos** | {} done, {} pending\n", completed, pending));
    output.push_str(&format!("{}\n\n", progress));

    // Table header
    output.push_str("| Status | ID | Task | Tags |\n");
    output.push_str("|--------|----|----- :|------|\n");

    // Sort: pending first, then completed
    let mut sorted_items = items.to_vec();
    sorted_items.sort_by(|a, b| {
        let a_done = a.get("done") == Some(&Value::Bool(true));
        let b_done = b.get("done") == Some(&Value::Bool(true));
        match (a_done, b_done) {
            (false, true) => std::cmp::Ordering::Less,
            (true, false) => std::cmp::Ordering::Greater,
            _ => {
                let a_id = a.get("id").and_then(Value::as_u64).unwrap_or(0);
                let b_id = b.get("id").and_then(Value::as_u64).unwrap_or(0);
                a_id.cmp(&b_id)
            }
        }
    });

    for item in &sorted_items {
        let id = item.get("id").and_then(Value::as_u64).unwrap_or(0);
        let text = item.get("text").and_then(Value::as_str).unwrap_or("");
        let done = item.get("done") == Some(&Value::Bool(true));
        let tags = item
            .get("tags")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();

        let status = if done { "✅" } else { "⬜" };
        let display_text = if done {
            format!("~~{}~~", text)
        } else {
            text.to_string()
        };
        let tags_display = if tags.is_empty() {
            "-".to_string()
        } else {
            format!("🏷️ {}", tags)
        };

        output.push_str(&format!("| {} | #{} | {} | {} |\n", status, id, display_text, tags_display));
    }

    output.trim_end().to_string()
}

fn result(
    action: &str,
    items: &[Value],
    all_items: &[Value],
    content: impl Into<String>,
) -> String {
    json!({
        "content": [{"type": "text", "text": content.into()}],
        "details": {
            "kind": "todo",
            "mode": "check",
            "action": action,
            "todos": items,
            // `items` may be a filtered view (includeDone=false). IDs must
            // always advance from the complete store or a hidden completed
            // item can cause the next add to reuse an existing ID.
            "nextId": next_id(all_items),
        }
    })
    .to_string()
}

fn todo(params: &Value) -> Result<String, String> {
    let path = store_path(params);
    let mut items = load(&path)?;
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("list");
    match action {
        "add" => {
            let text = normalize_text(
                params
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            );
            if text.is_empty() || text.chars().count() > 2000 {
                return Err("text must contain 1-2000 characters".into());
            }
            // Re-adding a task that is already pending returns the existing
            // entry instead of tracking it twice. Plan lines are often restated
            // verbatim across turns; a duplicate made the todo list look like
            // it still had the work outstanding.
            if let Some(existing) = items.iter().find(|item| {
                item.get("done") != Some(&Value::Bool(true))
                    && item
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|stored| normalize_text(stored) == text)
            }) {
                let id = existing.get("id").and_then(Value::as_u64).unwrap_or(0);
                return Ok(result(
                    "add",
                    &items,
                    &items,
                    format!("# {id} already tracks: {text}\n\n{}", display_list(&items)),
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
                "add",
                &items,
                &items,
                format!("✓ Added #{id} {}\n\n{}", text, display_list(&items)),
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
                action,
                &items,
                &items,
                format!("✓ #{id} {verb} {text}\n\n{}", display_list(&items)),
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
                "remove",
                &items,
                &items,
                format!("✓ Removed todo #{id}\n\n{}", display_list(&items)),
            ))
        }
        "clear" => {
            items.clear();
            save(&path, &items)?;
            Ok(result("clear", &items, &items, format!("✓ Cleared all todos\n\n{}", display_list(&items))))
        }
        "list" => {
            let include_done = params
                .get("includeDone")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if include_done {
                Ok(result("list", &items, &items, display_list(&items)))
            } else {
                let visible: Vec<Value> = items
                    .iter()
                    .filter(|v| v.get("done") != Some(&Value::Bool(true)))
                    .cloned()
                    .collect();
                Ok(result("list", &visible, &items, display_list(&visible)))
            }
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
    unsafe { register_entrypoint_unified(api, |api| {
        let Some(register) = api.register_tool else {
            return 1;
        };
        let schema = Box::new(rpi_plugin_sdk::StableToolSchema {
            name: StbString::from_string("todo".into()),
            description: StbString::from_string("Manage persistent project todos.".into()),
            parameters: StbString::from_string(r#"{"type":"object","properties":{"action":{"type":"string","enum":["add","list","done","complete","check","toggle","remove","clear"]},"text":{"type":"string"},"id":{"type":"integer"},"tags":{"type":"array"},"includeDone":{"type":"boolean"},"cwd":{"type":"string"}}}"#.into()),
        });
        let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
        drop(schema);
        rc
    }) }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let text = value["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Added #1"), "confirmation missing: {text}");
        assert!(text.contains("Todos"), "list header missing: {text}");
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
            &todo(&json!({"action":"add","text":"1. Tool names lowercase","cwd":cwd}))
                .unwrap(),
        );
        assert!(text.contains("already tracks"), "dedupe message: {text}");
        let items = load(&store_path(&json!({"cwd": cwd}))).unwrap();
        assert_eq!(items.len(), 1, "duplicate todo was stored: {items:?}");
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn add_allows_a_task_again_once_it_is_done() {
        let cwd = temp_cwd();
        todo(&json!({"action":"add","text":"repeatable","cwd":cwd})).unwrap();
        todo(&json!({"action":"done","id":1,"cwd":cwd})).unwrap();
        todo(&json!({"action":"add","text":"repeatable","cwd":cwd})).unwrap();
        let items = load(&store_path(&json!({"cwd": cwd}))).unwrap();
        assert_eq!(items.len(), 2, "a completed task may be re-added: {items:?}");
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn save_leaves_no_stale_tail_when_the_document_shrinks() {
        // The corruption this guards: a shorter document written over a longer
        // one used to leave the old tail behind (`[...]\n  {"id":3…}\n]`),
        // which is invalid JSON and only parsed thanks to `load`'s tolerant
        // concatenated-JSON reader.
        let cwd = temp_cwd();
        let path = store_path(&json!({"cwd": cwd}));
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
        assert!(text.contains("Todos"), "list header: {}", text);
        assert!(text.contains("█") || text.contains("░"), "progress bar: {}", text);
        assert!(text.contains("| Status |"), "table header: {}", text);
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
}
