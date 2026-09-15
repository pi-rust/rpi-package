use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

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

fn language(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|v| v.to_str()).unwrap_or("") {
        "rs" => Some("rust"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" => Some("javascript"),
        "py" => Some("python"),
        "go" => Some("go"),
        "java" => Some("java"),
        "c" | "h" | "cpp" | "hpp" => Some("c-family"),
        _ => None,
    }
}

fn ignored(path: &Path) -> bool {
    path.file_name()
        .and_then(|v| v.to_str())
        .map(|v| {
            matches!(
                v,
                ".git" | ".rpi" | "target" | "node_modules" | "dist" | "build" | "vendor"
            )
        })
        .unwrap_or(false)
}

fn scan(
    root: &Path,
    current: &Path,
    files: &mut Vec<Value>,
    edges: &mut Vec<Value>,
    max_files: usize,
    max_edges: usize,
) {
    if files.len() >= max_files || edges.len() >= max_edges || ignored(current) {
        return;
    }
    let Ok(mut entries) = fs::read_dir(current).map(|v| v.flatten().collect::<Vec<_>>()) else {
        return;
    };
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        if files.len() >= max_files || edges.len() >= max_edges {
            break;
        }
        let path = entry.path();
        if ignored(&path) {
            continue;
        }
        if path.is_dir() {
            scan(root, &path, files, edges, max_files, max_edges);
            continue;
        }
        let Some(lang) = language(&path) else {
            continue;
        };
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        files.push(json!({"path":rel,"language":lang,"lines":text.lines().count()}));
        for line in text.lines() {
            let trimmed = line.trim();
            let target = if trimmed.starts_with("use ") {
                trimmed
                    .strip_prefix("use ")
                    .and_then(|v| v.split("::").next())
            } else if trimmed.starts_with("mod ") {
                trimmed
                    .strip_prefix("mod ")
                    .and_then(|v| v.split_whitespace().next())
            } else if trimmed.starts_with("from ") {
                trimmed
                    .strip_prefix("from ")
                    .and_then(|v| v.split_whitespace().next())
            } else if trimmed.starts_with("import ") {
                trimmed
                    .strip_prefix("import ")
                    .and_then(|v| v.split(|c: char| c == ',' || c.is_whitespace()).next())
            } else {
                None
            };
            if let Some(target) = target.filter(|v| !v.is_empty()) {
                edges.push(json!({"from":rel,"to":target.trim_matches(|c: char| c == '"' || c == '\'' || c == ';'),"kind":"import"}));
                if edges.len() >= max_edges {
                    break;
                }
            }
        }
    }
}

fn codegraph(params: &Value) -> Result<String, String> {
    let root = params
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    if !root.is_dir() {
        return Err(format!(
            "codegraph root is not a directory: {}",
            root.display()
        ));
    }
    let max_files = params
        .get("maxFiles")
        .and_then(Value::as_u64)
        .unwrap_or(500)
        .clamp(1, 2000) as usize;
    let max_edges = params
        .get("maxEdges")
        .and_then(Value::as_u64)
        .unwrap_or(2000)
        .clamp(1, 10000) as usize;
    let mut files = Vec::new();
    let mut edges = Vec::new();
    scan(&root, &root, &mut files, &mut edges, max_files, max_edges);
    Ok(json!({"root":root,"files":files,"edges":edges,"truncated":files.len() >= max_files || edges.len() >= max_edges}).to_string())
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
        return StepResult::err(StbString::from_string("null codegraph handle".into()));
    }
    let drive = unsafe { &mut *(handle as *mut Drive) };
    if drive.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("codegraph cancelled".into()));
    }
    if drive.completed {
        return StepResult::err(StbString::from_string(
            "codegraph polled after completion".into(),
        ));
    }
    drive.completed = true;
    match codegraph(&drive.params) {
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
    register_entrypoint(api, abi, |api| {
        let Some(register) = api.register_tool else {
            return 1;
        };
        let schema=Box::new(rpi_plugin_sdk::StableToolSchema{name:StbString::from_string("codegraph".into()),description:StbString::from_string("Inspect source files and import/module edges.".into()),parameters:StbString::from_string(r#"{"type":"object","properties":{"cwd":{"type":"string"},"maxFiles":{"type":"integer"},"maxEdges":{"type":"integer"}}}"#.into())});
        let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
        drop(schema);
        rc
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn skips_dependency_dirs() {
        assert!(ignored(Path::new("target")));
        assert_eq!(language(Path::new("main.rs")), Some("rust"));
    }
}
