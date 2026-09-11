use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rpi_plugin_sdk::{
    register_entrypoint, FreeStringFn, PluginApiVt, StbString, StbStringRef, StepHandle,
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
    fs::write(path, text).map_err(|e| format!("write todo store: {e}"))
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
            let text = params
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if text.is_empty() || text.len() > 2000 {
                return Err("text must contain 1-2000 characters".into());
            }
            let id = items
                .iter()
                .filter_map(|v| v.get("id").and_then(Value::as_u64))
                .max()
                .unwrap_or(0)
                + 1;
            let tags = params
                .get("tags")
                .cloned()
                .filter(|v| v.is_array())
                .unwrap_or_else(|| json!([]));
            items.push(json!({"id": id, "text": text, "done": false, "tags": tags, "createdAt": now(), "updatedAt": now()}));
            save(&path, &items)?;
            Ok(json!({"action":"add","item":items.last().unwrap()}).to_string())
        }
        "done" | "complete" => {
            let id = params
                .get("id")
                .and_then(Value::as_u64)
                .ok_or("id is required")?;
            let updated = {
                let item = items
                    .iter_mut()
                    .find(|v| v.get("id").and_then(Value::as_u64) == Some(id))
                    .ok_or("todo item not found")?;
                item["done"] = Value::Bool(true);
                item["updatedAt"] = json!(now());
                item.clone()
            };
            save(&path, &items)?;
            Ok(json!({"action":"done","item":updated}).to_string())
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
            Ok(json!({"action":"remove","id":id}).to_string())
        }
        "clear" => {
            items.clear();
            save(&path, &items)?;
            Ok(json!({"action":"clear","count":0}).to_string())
        }
        "list" => {
            let include_done = params
                .get("includeDone")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if include_done {
                Ok(json!({"action":"list","items":items}).to_string())
            } else {
                Ok(json!({"action":"list","items":items.into_iter().filter(|v| v.get("done") != Some(&Value::Bool(true))).collect::<Vec<_>>()} ).to_string())
            }
        }
        _ => Err("action must be one of: add, list, done, remove, clear".into()),
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
        Ok(text) => StepResult::done(StbString::from_string(
            json!({"content":[{"type":"text","text":text}]}).to_string(),
        )),
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
pub extern "C" fn rpi_plugin_register(api: *const PluginApiVt, abi: u32) -> i32 {
    register_entrypoint(api, abi, |api| {
        let Some(register) = api.register_tool else {
            return 1;
        };
        let schema = Box::new(rpi_plugin_sdk::StableToolSchema {
            name: StbString::from_string("todo".into()),
            description: StbString::from_string("Manage persistent project todos.".into()),
            parameters: StbString::from_string(r#"{"type":"object","properties":{"action":{"type":"string","enum":["add","list","done","remove","clear"]},"text":{"type":"string"},"id":{"type":"integer"},"tags":{"type":"array"},"includeDone":{"type":"boolean"},"cwd":{"type":"string"}}}"#.into()),
        });
        let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
        drop(schema);
        rc
    })
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
}
