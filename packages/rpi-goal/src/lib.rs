use rpi_plugin_sdk::{
    register_entrypoint, FreeStringFn, PluginApiVt, StableToolSchema, StbString, StbStringRef,
    StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
struct Drive {
    params: Value,
    cancelled: AtomicBool,
    done: bool,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn path(p: &Value) -> PathBuf {
    p.get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".rpi")
        .join("goals.json")
}
fn load(path: &PathBuf) -> Result<Vec<Value>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&fs::read_to_string(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn save(path: &PathBuf, v: &[Value]) -> Result<(), String> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    fs::write(
        path,
        serde_json::to_string_pretty(v).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
fn valid_title(value: &str) -> Result<&str, String> {
    let title = value.trim();
    if title.is_empty() || title.chars().count() > 500 {
        return Err("title must contain 1-500 characters".into());
    }
    Ok(title)
}
fn goal(p: &Value) -> Result<String, String> {
    let file = path(p);
    let mut goals = load(&file)?;
    let action = p.get("action").and_then(Value::as_str).unwrap_or("list");
    match action {
        "list" => Ok(json!({"action":"list","goals":goals}).to_string()),
        "add" => {
            let title = valid_title(p.get("title").and_then(Value::as_str).unwrap_or(""))?;
            let id = goals
                .iter()
                .filter_map(|v| v.get("id").and_then(Value::as_u64))
                .max()
                .unwrap_or(0)
                + 1;
            let item = json!({"id":id,"title":title,"status":"active","notes":p.get("notes"),"createdAt":now(),"updatedAt":now()});
            goals.push(item.clone());
            save(&file, &goals)?;
            Ok(json!({"action":"add","goal":item}).to_string())
        }
        "update" => {
            let id = p
                .get("id")
                .and_then(Value::as_u64)
                .ok_or("id is required")?;
            let item = goals
                .iter_mut()
                .find(|v| v.get("id").and_then(Value::as_u64) == Some(id))
                .ok_or("goal not found")?;
            if let Some(title) = p.get("title").and_then(Value::as_str) {
                item["title"] = json!(valid_title(title)?);
            }
            if let Some(notes) = p.get("notes") {
                item["notes"] = notes.clone();
            }
            if let Some(status) = p.get("status").and_then(Value::as_str) {
                if !matches!(status, "active" | "paused" | "complete") {
                    return Err("status must be active, paused, or complete".into());
                }
                item["status"] = json!(status);
            }
            item["updatedAt"] = json!(now());
            let out = item.clone();
            save(&file, &goals)?;
            Ok(json!({"action":"update","goal":out}).to_string())
        }
        "complete" => {
            let id = p
                .get("id")
                .and_then(Value::as_u64)
                .ok_or("id is required")?;
            let item = goals
                .iter_mut()
                .find(|v| v.get("id").and_then(Value::as_u64) == Some(id))
                .ok_or("goal not found")?;
            item["status"] = json!("complete");
            item["updatedAt"] = json!(now());
            let out = item.clone();
            save(&file, &goals)?;
            Ok(json!({"action":"complete","goal":out}).to_string())
        }
        "remove" => {
            let id = p
                .get("id")
                .and_then(Value::as_u64)
                .ok_or("id is required")?;
            let before = goals.len();
            goals.retain(|v| v.get("id").and_then(Value::as_u64) != Some(id));
            if before == goals.len() {
                return Err("goal not found".into());
            }
            save(&file, &goals)?;
            Ok(json!({"action":"remove","id":id}).to_string())
        }
        _ => Err("action must be one of: add, list, update, complete, remove".into()),
    }
}
extern "C" fn execute(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let t = params.to_string_lossy();
    params.free_with(free);
    Box::into_raw(Box::new(Drive {
        params: serde_json::from_str(&t).unwrap_or(Value::Null),
        cancelled: AtomicBool::new(false),
        done: false,
    })) as StepHandle
}
extern "C" fn poll(h: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if h.is_null() {
        return StepResult::err(StbString::from_string("null goal handle".into()));
    }
    let d = unsafe { &mut *(h as *mut Drive) };
    if d.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("goal cancelled".into()));
    }
    if d.done {
        return StepResult::err(StbString::from_string(
            "goal polled after completion".into(),
        ));
    }
    d.done = true;
    match goal(&d.params) {
        Ok(t) => StepResult::done(StbString::from_string(
            json!({"content":[{"type":"text","text":t}]}).to_string(),
        )),
        Err(e) => StepResult::err(StbString::from_string(e)),
    }
}
extern "C" fn cancel(h: StepHandle) {
    if !h.is_null() {
        unsafe {
            (&*(h as *mut Drive))
                .cancelled
                .store(true, Ordering::SeqCst);
        }
    }
}
extern "C" fn destroy(h: StepHandle) {
    if !h.is_null() {
        unsafe {
            drop(Box::from_raw(h as *mut Drive));
        }
    }
}
extern "C" fn free_string(s: StbString) {
    if !s.is_empty() && !s.ptr.is_null() {
        unsafe {
            let b = std::slice::from_raw_parts(s.ptr as *const u8, s.len);
            let _ = Box::from_raw(b as *const [u8] as *mut [u8]);
        }
    }
}
#[no_mangle]
pub extern "C" fn rpi_plugin_register_v2(api: *const PluginApiVt, abi: u32) -> i32 {
    register_entrypoint(api, abi, |api| {
        let Some(register) = api.register_tool else {
            return 1;
        };
        let schema=Box::new(StableToolSchema{name:StbString::from_string("goal".into()),description:StbString::from_string("Track persistent project goals and progress.".into()),parameters:StbString::from_string(r#"{"type":"object","properties":{"action":{"type":"string","enum":["add","list","update","complete","remove"]},"id":{"type":"integer"},"title":{"type":"string"},"notes":{"type":"string"},"status":{"type":"string","enum":["active","paused","complete"]},"cwd":{"type":"string"}},"required":["action"]}"#.into())});
        let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
        drop(schema);
        rc
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_action() {
        assert!(goal(&json!({"action":"wat","cwd":"."})).is_err());
    }

    #[test]
    fn validates_updated_title() {
        assert!(valid_title("  ").is_err());
        assert_eq!(valid_title("  ship it  ").unwrap(), "ship it");
    }
}
