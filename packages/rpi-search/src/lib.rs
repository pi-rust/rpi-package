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
fn walk(root: &Path, query: &str, results: &mut Vec<Value>, depth: usize, max: usize) {
    if depth > 10 || results.len() >= max {
        return;
    }
    let Ok(rd) = fs::read_dir(root) else {
        return;
    };
    // Sort directory entries so repeated calls produce stable model context.
    let mut entries = rd.flatten().map(|e| e.path()).collect::<Vec<_>>();
    entries.sort();
    for p in entries {
        if results.len() >= max {
            return;
        }
        let n = p.file_name().and_then(|x| x.to_str()).unwrap_or("");
        if n == ".git"
            || n == "target"
            || n == "node_modules"
            || n == ".rpi"
            || n == ".pi"
            || is_sensitive_name(n)
            || p.is_symlink()
        {
            continue;
        }
        if p.is_dir() {
            walk(&p, query, results, depth + 1, max);
            continue;
        }
        let name = n.to_ascii_lowercase();
        if name.contains(query) {
            results.push(json!({"path":p,"kind":"filename"}));
            continue;
        }
        if let Ok(text) = fs::read_to_string(&p) {
            for (line_no, line) in text.lines().enumerate() {
                if line.to_ascii_lowercase().contains(query) {
                    results.push(json!({"path":p,"line":line_no+1,"text":line.trim()}));
                    if results.len() >= max {
                        return;
                    }
                }
            }
        }
    }
}

fn is_sensitive_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == ".env"
        || lower.starts_with(".env.")
        || lower.contains("secret")
        || lower.contains("credential")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
}
fn search(p: &Value) -> Result<String, String> {
    let query = p
        .get("query")
        .and_then(Value::as_str)
        .ok_or("query is required")?
        .trim()
        .to_ascii_lowercase();
    if query.is_empty() {
        return Err("query must not be empty".into());
    }
    let root = PathBuf::from(p.get("root").and_then(Value::as_str).unwrap_or("."));
    if !root.exists() {
        return Err(format!("root does not exist: {}", root.display()));
    }
    let max = p
        .get("maxResults")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let mut results = Vec::new();
    if root.is_file() {
        if root.is_symlink()
            || root
                .file_name()
                .and_then(|x| x.to_str())
                .map(is_sensitive_name)
                .unwrap_or(false)
        {
            return Ok(
                json!({"query":query,"root":root,"count":0,"results":[],"truncated":false})
                    .to_string(),
            );
        }
        if let Some(n) = root.file_name().and_then(|x| x.to_str()) {
            if n.to_ascii_lowercase().contains(&query) {
                results.push(json!({"path":root,"kind":"filename"}));
            }
        }
        if results.len() < max {
            if let Ok(text) = fs::read_to_string(&root) {
                for (line_no, line) in text.lines().enumerate() {
                    if line.to_ascii_lowercase().contains(&query) {
                        results.push(json!({"path":root,"line":line_no+1,"text":line.trim()}));
                        if results.len() >= max {
                            break;
                        }
                    }
                }
            }
        }
    } else {
        walk(&root, &query, &mut results, 0, max);
    }
    Ok(json!({"query":query,"root":root,"count":results.len(),"results":results,"truncated":results.len()>=max}).to_string())
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
        return StepResult::err(StbString::from_string("null search handle".into()));
    }
    let d = unsafe { &mut *(h as *mut Drive) };
    if d.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("search cancelled".into()));
    }
    if d.done {
        return StepResult::err(StbString::from_string(
            "search polled after completion".into(),
        ));
    }
    d.done = true;
    match search(&d.params) {
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
        let schema=Box::new(StableToolSchema{name:StbString::from_string("search".into()),description:StbString::from_string("Search local filenames and text with bounded traversal.".into()),parameters:StbString::from_string(r#"{"type":"object","properties":{"query":{"type":"string"},"root":{"type":"string"},"maxResults":{"type":"integer","minimum":1,"maximum":200}},"required":["query"]}"#.into())});
        let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
        drop(schema);
        rc
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn rejects_empty_query() {
        assert!(search(&json!({"query":"  "})).is_err());
    }

    #[test]
    fn searches_content_in_single_file() {
        let path = std::env::temp_dir().join(format!(
            "rpi-search-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, "first line\nneedle here\n").unwrap();
        let result = search(&json!({"query":"needle","root":path})).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(result.contains("\"line\":2"));
    }
}
