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
fn path(params: &Value) -> PathBuf {
    let root = params
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    root.join(".rpi").join("memory.json")
}
fn load(file: &PathBuf) -> Result<Vec<Value>, String> {
    if !file.exists() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&fs::read_to_string(file).map_err(|e| format!("read memory store: {e}"))?)
        .map_err(|e| format!("parse memory store: {e}"))
}
fn save(file: &PathBuf, items: &[Value]) -> Result<(), String> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create memory directory: {e}"))?;
    }
    fs::write(
        file,
        serde_json::to_string_pretty(items).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write memory store: {e}"))
}
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|v| v.len() > 1)
        .map(str::to_string)
        .collect()
}
fn memory(params: &Value) -> Result<String, String> {
    let file = path(params);
    let mut items = load(&file)?;
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("list");
    match action {
        "remember" => {
            let text = params
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if text.is_empty() || text.len() > 10000 {
                return Err("text must contain 1-10000 characters".into());
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
            items
                .push(json!({"id":id,"text":text,"tags":tags,"createdAt":now(),"updatedAt":now()}));
            save(&file, &items)?;
            Ok(json!({"action":"remember","id":id}).to_string())
        }
        "recall" => {
            let query = params.get("query").and_then(Value::as_str).unwrap_or("");
            if query.trim().is_empty() {
                return Err("query is required".into());
            }
            let q = words(query);
            let mut scored: Vec<(usize, Value)> = items
                .into_iter()
                .filter_map(|item| {
                    let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                    let hay = words(text);
                    let score = q.iter().filter(|word| hay.contains(word)).count();
                    (score > 0).then_some((score, item))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            let limit = params
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(10)
                .clamp(1, 50) as usize;
            let results: Vec<Value> = scored
                .into_iter()
                .take(limit)
                .map(|(score, mut item)| {
                    item["score"] = json!(score);
                    item
                })
                .collect();
            Ok(json!({"action":"recall","query":query,"results":results}).to_string())
        }
        "forget" => {
            let id = params
                .get("id")
                .and_then(Value::as_u64)
                .ok_or("id is required")?;
            let before = items.len();
            items.retain(|v| v.get("id").and_then(Value::as_u64) != Some(id));
            if before == items.len() {
                return Err("memory item not found".into());
            }
            save(&file, &items)?;
            Ok(json!({"action":"forget","id":id}).to_string())
        }
        "clear" => {
            items.clear();
            save(&file, &items)?;
            Ok(json!({"action":"clear"}).to_string())
        }
        "list" => Ok(json!({"action":"list","items":items}).to_string()),
        _ => Err("action must be one of: remember, recall, list, forget, clear".into()),
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
        return StepResult::err(StbString::from_string("null memory handle".into()));
    }
    let drive = unsafe { &mut *(handle as *mut Drive) };
    if drive.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("memory cancelled".into()));
    }
    if drive.completed {
        return StepResult::err(StbString::from_string(
            "memory polled after completion".into(),
        ));
    }
    drive.completed = true;
    match memory(&drive.params) {
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
pub extern "C" fn rpi_plugin_register_v2(api: *const PluginApiVt, abi: u32) -> i32 {
    unsafe { register_entrypoint(api, abi, |api| {
        let Some(register) = api.register_tool else {
            return 1;
        };
        let schema=Box::new(rpi_plugin_sdk::StableToolSchema{name:StbString::from_string("memory".into()),description:StbString::from_string("Remember and recall project-local facts.".into()),parameters:StbString::from_string(r#"{"type":"object","properties":{"action":{"type":"string","enum":["remember","recall","list","forget","clear"]},"text":{"type":"string"},"query":{"type":"string"},"id":{"type":"integer"},"tags":{"type":"array"},"limit":{"type":"integer"},"cwd":{"type":"string"}}}"#.into())});
        let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
        drop(schema);
        rc
    }) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scores_overlap() {
        assert_eq!(words("Rust memory"), vec!["rust", "memory"]);
        assert!(memory(&json!({"action":"wat","cwd":"."})).is_err());
    }
}
