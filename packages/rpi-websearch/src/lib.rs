use rpi_plugin_sdk::{
    register_entrypoint, FreeStringFn, PluginApiVt, StableToolSchema, StbString, StbStringRef,
    StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use url::Url;
struct Drive {
    params: Value,
    cancelled: AtomicBool,
    done: bool,
}
fn search(p: &Value) -> Result<String, String> {
    let query = p
        .get("query")
        .and_then(Value::as_str)
        .ok_or("query is required")?
        .trim();
    if query.is_empty() {
        return Err("query must not be empty".into());
    }
    let limit = p
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 20) as usize;
    let url = Url::parse_with_params(
        "https://api.duckduckgo.com/",
        &[
            ("q", query),
            ("format", "json"),
            ("no_html", "1"),
            ("skip_disambig", "1"),
        ],
    )
    .map_err(|e| e.to_string())?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("rpi-websearch/0.1")
        .build()
        .map_err(|e| e.to_string())?;
    let value: Value = client
        .get(url)
        .send()
        .map_err(|e| format!("web search failed: {e}"))?
        .json()
        .map_err(|e| format!("invalid search response: {e}"))?;
    let mut hits = Vec::new();
    if let Some(text) = value
        .get("AbstractText")
        .and_then(Value::as_str)
        .filter(|x| !x.is_empty())
    {
        hits.push(json!({"title":value.get("Heading").and_then(Value::as_str).unwrap_or(query),"url":value.get("AbstractURL").and_then(Value::as_str).unwrap_or(""),"snippet":text,"source":"duckduckgo"}));
    }
    fn collect(v: &Value, hits: &mut Vec<Value>, limit: usize) {
        if hits.len() >= limit {
            return;
        }
        if let Some(arr) = v.as_array() {
            for x in arr {
                if hits.len() >= limit {
                    return;
                }
                if let Some(t) = x.get("Text").and_then(Value::as_str) {
                    hits.push(json!({"title":t,"url":x.get("FirstURL").and_then(Value::as_str).unwrap_or(""),"snippet":t,"source":"duckduckgo"}));
                }
                collect(x.get("Topics").unwrap_or(&Value::Null), hits, limit);
            }
        }
    }
    collect(
        value.get("RelatedTopics").unwrap_or(&Value::Null),
        &mut hits,
        limit,
    );
    Ok(json!({"query":query,"count":hits.len(),"results":hits}).to_string())
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
        return StepResult::err(StbString::from_string("null websearch handle".into()));
    }
    let d = unsafe { &mut *(h as *mut Drive) };
    if d.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("websearch cancelled".into()));
    }
    if d.done {
        return StepResult::err(StbString::from_string(
            "websearch polled after completion".into(),
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
        let schema=Box::new(StableToolSchema{name:StbString::from_string("websearch".into()),description:StbString::from_string("Search public web references with DuckDuckGo.".into()),parameters:StbString::from_string(r#"{"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":20}},"required":["query"]}"#.into())});
        let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
        drop(schema);
        rc
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_empty() {
        assert!(search(&json!({"query":""})).is_err());
    }
}
