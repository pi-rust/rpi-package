use std::ffi::c_void;
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
fn estimate(params: &Value) -> Result<String, String> {
    let text = params.get("text").and_then(Value::as_str).unwrap_or("");
    if text.is_empty() {
        return Err("text is required".into());
    }
    let input = ((text.chars().count() as u64) + 3) / 4;
    let output = params
        .get("outputTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok(
        json!({"input":input,"output":output,"total":input+output,"method":"chars-div-4"})
            .to_string(),
    )
}
fn compact(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}m", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
fn number(v: &Value, keys: &[&str]) -> u64 {
    keys.iter()
        .find_map(|key| v.get(*key).and_then(Value::as_u64))
        .unwrap_or(0)
}
extern "C" fn render_usage(input: StbStringRef, out: *mut StbString, _: *mut c_void) -> i32 {
    let raw = unsafe { input.as_str() };
    let value: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
    let usage = value.get("usage").unwrap_or(&value);
    let input_tokens = number(usage, &["input", "inputTokens", "input_tokens"]);
    let output = number(usage, &["output", "outputTokens", "output_tokens"]);
    let cache_read = number(usage, &["cacheRead", "cache_read"]);
    let cache_write = number(usage, &["cacheWrite", "cache_write"]);
    let total = number(usage, &["total", "totalTokens", "total_tokens"])
        .max(input_tokens + output + cache_read + cache_write);
    if total == 0 {
        return 1;
    }
    let text = if cache_read + cache_write > 0 {
        format!(
            "tokens: in {} | out {} | cache {} | total {}",
            compact(input_tokens),
            compact(output),
            compact(cache_read + cache_write),
            compact(total)
        )
    } else {
        format!(
            "tokens: in {} | out {} | total {}",
            compact(input_tokens),
            compact(output),
            compact(total)
        )
    };
    unsafe {
        *out = StbString::from_string(json!({"text":text}).to_string());
    }
    0
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
        return StepResult::err(StbString::from_string("null token handle".into()));
    }
    let drive = unsafe { &mut *(handle as *mut Drive) };
    if drive.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("token count cancelled".into()));
    }
    if drive.completed {
        return StepResult::err(StbString::from_string(
            "token count polled after completion".into(),
        ));
    }
    drive.completed = true;
    match estimate(&drive.params) {
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
        let schema=Box::new(rpi_plugin_sdk::StableToolSchema{name:StbString::from_string("token_count".into()),description:StbString::from_string("Estimate token usage for text.".into()),parameters:StbString::from_string(r#"{"type":"object","properties":{"text":{"type":"string"},"outputTokens":{"type":"integer"}},"required":["text"]}"#.into())});
        let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
        drop(schema);
        if rc != 0 {
            return rc;
        }
        if let Some(register_renderer) = api.register_message_renderer {
            let name = StbStringRef::from_str("token-usage");
            let _ = register_renderer(name, render_usage, free_string, std::ptr::null_mut());
        }
        0
    }) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn estimates_and_formats() {
        let value: Value =
            serde_json::from_str(&estimate(&json!({"text":"12345678","outputTokens":2})).unwrap())
                .unwrap();
        assert_eq!(value["input"], 2);
        let mut out = StbString::empty();
        assert_eq!(
            render_usage(
                StbStringRef::from_str(r#"{"usage":{"input":1200,"output":300}}"#),
                &mut out,
                std::ptr::null_mut()
            ),
            0
        );
        assert!(out.to_string_lossy().contains("1.2k"));
        free_string(out);
    }
}
