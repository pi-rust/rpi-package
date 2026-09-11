use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use rpi_plugin_sdk::{
    register_entrypoint, FreeStringFn, PluginApiVt, StableToolSchema, StbString, StbStringRef,
    StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};

struct Drive {
    params: Value,
    cancelled: AtomicBool,
    done: bool,
}

fn ask(params: &Value) -> Result<String, String> {
    let kind = params
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("question");
    if kind != "question" && kind != "confirm" {
        return Err("type must be question or confirm".into());
    }
    if kind == "confirm" {
        let summary = params
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if summary.is_empty() {
            return Err("summary is required for confirm".into());
        }
        return Ok(
            json!({"type":"confirm","summary":summary,"ui":{"kind":"confirm","summary":summary}})
                .to_string(),
        );
    }
    let questions = params
        .get("questions")
        .and_then(Value::as_array)
        .ok_or("questions is required")?;
    if questions.is_empty() || questions.len() > 3 {
        return Err("questions must contain 1-3 items".into());
    }
    let mut normalized = Vec::new();
    for item in questions {
        let question = item
            .get("question")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if question.is_empty() || question.len() > 500 {
            return Err("each question must contain 1-500 characters".into());
        }
        let options = item
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if options.len() > 4 {
            return Err("each question may contain at most 4 options".into());
        }
        normalized.push(json!({"question":question,"options":options,"multiple":item.get("multiple").and_then(Value::as_bool).unwrap_or(false),"allowCustom":item.get("allowCustom").or_else(|| item.get("allow_custom")).and_then(Value::as_bool).unwrap_or(false)}));
    }
    Ok(json!({"type":"question","questions":normalized,"suggest":params.get("suggest"),"ui":{"kind":"selector","questions":normalized}}).to_string())
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
        return StepResult::err(StbString::from_string("null ask handle".into()));
    }
    let drive = unsafe { &mut *(handle as *mut Drive) };
    if drive.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("ask cancelled".into()));
    }
    if drive.done {
        return StepResult::err(StbString::from_string("ask polled after completion".into()));
    }
    drive.done = true;
    match ask(&drive.params) {
        Ok(text) => {
            let details = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|value| value.get("ui").cloned())
                .unwrap_or_else(|| json!({"kind":"selector"}));
            StepResult::done(StbString::from_string(
                json!({"content":[{"type":"text","text":text}],"details":{"ui":details}})
                    .to_string(),
            ))
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
pub extern "C" fn rpi_plugin_register(api: *const PluginApiVt, abi: u32) -> i32 {
    register_entrypoint(api, abi, |api| {
        let Some(register) = api.register_tool else {
            return 1;
        };
        let schema = Box::new(StableToolSchema { name: StbString::from_string("ask_user".into()), description: StbString::from_string("Ask the user validated single or multi-select questions.".into()), parameters: StbString::from_string(r#"{"type":"object","properties":{"type":{"type":"string","enum":["question","confirm"]},"questions":{"type":"array","minItems":1,"maxItems":3},"suggest":{"type":"string"},"summary":{"type":"string"}},"oneOf":[{"properties":{"type":{"const":"question"}},"required":["questions"]},{"properties":{"type":{"const":"confirm"}},"required":["summary"]}]}"#.into()) });
        let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
        drop(schema);
        rc
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_questions() {
        assert!(
            ask(&json!({"questions":[{"question":"Pick","options":["a","b"]}]}))
                .unwrap()
                .contains("selector")
        );
        assert!(ask(&json!({"questions":[]})).is_err());
    }
}
