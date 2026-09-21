use rpi_plugin_sdk::{
    register_entrypoint, FreeStringFn, PluginApiVt, StableToolSchema, StbString, StbStringRef,
    StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
struct Drive {
    params: Value,
    cancelled: AtomicBool,
    done: bool,
}
fn files(root: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 8 || out.len() >= 100 {
        return;
    }
    let Ok(rd) = fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let n = p.file_name().and_then(|x| x.to_str()).unwrap_or("");
        if n == ".git" || n == "target" || n == "node_modules" || n == ".rpi" {
            continue;
        }
        if p.is_dir() {
            files(&p, out, depth + 1)
        } else if matches!(
            p.extension().and_then(|x| x.to_str()),
            Some(
                "rs" | "ts"
                    | "tsx"
                    | "js"
                    | "jsx"
                    | "py"
                    | "go"
                    | "java"
                    | "c"
                    | "h"
                    | "cpp"
                    | "hpp"
            )
        ) {
            out.push(p);
            if out.len() >= 100 {
                return;
            }
        }
    }
}
fn simplify(p: &Value) -> Result<String, String> {
    let root = PathBuf::from(p.get("root").and_then(Value::as_str).unwrap_or("."));
    if !root.exists() {
        return Err(format!("root does not exist: {}", root.display()));
    }
    let mut paths = Vec::new();
    if root.is_file() {
        paths.push(root.clone())
    } else {
        files(&root, &mut paths, 0)
    }
    let files_reviewed = paths.len();
    let mut reports = Vec::new();
    for path in paths {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        let long = lines.iter().filter(|l| l.chars().count() > 120).count();
        let todos = lines
            .iter()
            .filter(|l| l.contains("TODO") || l.contains("FIXME"))
            .count();
        let functions = lines
            .iter()
            .filter(|l| {
                l.contains("fn ")
                    || l.contains("function ")
                    || l.contains("def ")
                    || l.contains("func ")
            })
            .count();
        let mut dup = 0;
        for i in 1..lines.len() {
            if !lines[i].trim().is_empty() && lines[i].trim() == lines[i - 1].trim() {
                dup += 1;
            }
        }
        if long + todos + dup > 0 {
            reports.push(json!({"file":path,"lines":lines.len(),"longLines":long,"todoMarkers":todos,"adjacentDuplicates":dup,"functions":functions}));
        }
    }
    Ok(json!({"root":root,"filesReviewed":files_reviewed,"filesWithFindings":reports.len(),"findings":reports}).to_string())
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
        return StepResult::err(StbString::from_string("null simplify handle".into()));
    }
    let d = unsafe { &mut *(h as *mut Drive) };
    if d.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("simplify cancelled".into()));
    }
    if d.done {
        return StepResult::err(StbString::from_string(
            "simplify polled after completion".into(),
        ));
    }
    d.done = true;
    match simplify(&d.params) {
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
    unsafe { register_entrypoint(api, abi, |api| {
        let Some(register) = api.register_tool else {
            return 1;
        };
        let schema = Box::new(StableToolSchema {
            name: StbString::from_string("simplify".into()),
            description: StbString::from_string(
                "Review local source for simple complexity and duplication findings.".into(),
            ),
            parameters: StbString::from_string(
                r#"{"type":"object","properties":{"root":{"type":"string"}},"required":[]}"#.into(),
            ),
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
    fn missing_root_is_error() {
        assert!(simplify(&json!({"root":"x-does-not-exist"})).is_err());
    }
}
