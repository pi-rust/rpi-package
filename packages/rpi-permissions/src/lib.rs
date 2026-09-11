use rpi_plugin_sdk::{
    register_entrypoint, FreeStringFn, PluginApiVt, StableToolSchema, StbString, StbStringRef,
    StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

struct Drive {
    params: Value,
    cancelled: AtomicBool,
    done: bool,
}
fn path(p: &Value) -> PathBuf {
    p.get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".rpi")
        .join("permissions.json")
}
fn load(path: &PathBuf) -> Result<Vec<Value>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&fs::read_to_string(path).map_err(|e| format!("read permissions: {e}"))?)
        .map_err(|e| format!("parse permissions: {e}"))
}
fn save(path: &PathBuf, rules: &[Value]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create permission directory: {e}"))?;
    }
    fs::write(
        path,
        serde_json::to_string_pretty(rules).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write permissions: {e}"))
}
fn permissions(p: &Value) -> Result<String, String> {
    let file = path(p);
    let mut rules = load(&file)?;
    let action = p.get("action").and_then(Value::as_str).unwrap_or("list");
    match action {
        "list" => Ok(json!({"action":"list","rules":rules}).to_string()),
        "check" => {
            let capability = p
                .get("capability")
                .and_then(Value::as_str)
                .ok_or("capability is required")?;
            let granted = rules.iter().any(|r| {
                r.get("capability").and_then(Value::as_str) == Some(capability)
                    && r.get("effect").and_then(Value::as_str) == Some("allow")
            });
            Ok(json!({"action":"check","capability":capability,"allowed":granted,"default":"deny"}).to_string())
        }
        "grant" | "revoke" => {
            let capability = p
                .get("capability")
                .and_then(Value::as_str)
                .ok_or("capability is required")?
                .trim();
            if capability.is_empty() || capability.len() > 200 {
                return Err("capability must contain 1-200 characters".into());
            }
            rules.retain(|r| r.get("capability").and_then(Value::as_str) != Some(capability));
            if action == "grant" {
                rules.push(json!({"capability":capability,"effect":"allow"}));
            }
            save(&file, &rules)?;
            Ok(
                json!({"action":action,"capability":capability,"allowed":action=="grant"})
                    .to_string(),
            )
        }
        _ => Err("action must be one of: check, grant, revoke, list".into()),
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
        done: false,
    })) as StepHandle
}
extern "C" fn poll(handle: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null permissions handle".into()));
    }
    let d = unsafe { &mut *(handle as *mut Drive) };
    if d.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("permissions cancelled".into()));
    }
    if d.done {
        return StepResult::err(StbString::from_string(
            "permissions polled after completion".into(),
        ));
    }
    d.done = true;
    match permissions(&d.params) {
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
pub extern "C" fn rpi_plugin_register(api: *const PluginApiVt, abi: u32) -> i32 {
    register_entrypoint(api, abi, |api| {
        let Some(register) = api.register_tool else {
            return 1;
        };
        let schema=Box::new(StableToolSchema{name:StbString::from_string("permissions".into()),description:StbString::from_string("Check and manage persistent allow/deny capabilities.".into()),parameters:StbString::from_string(r#"{"type":"object","properties":{"action":{"type":"string","enum":["check","grant","revoke","list"]},"capability":{"type":"string"},"cwd":{"type":"string"}},"required":["action"]}"#.into())});
        let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
        drop(schema);
        rc
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_denies() {
        let out = permissions(&json!({"action":"check","capability":"shell"})).unwrap();
        assert!(out.contains("false"));
    }
}
