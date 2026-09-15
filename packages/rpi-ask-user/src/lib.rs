use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use rpi_plugin_sdk::{
    register_entrypoint, FreeStringFn, PluginApiVt, StableToolSchema, StbString, StbStringRef,
    StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};

struct Drive {
    params: Value,
    cancelled: AtomicBool,
    done: bool,
}

static PENDING: OnceLock<Mutex<Option<Value>>> = OnceLock::new();

fn ask(params: &Value) -> Result<String, String> {
    // Native pi-ask-user uses one question with structured option objects.
    // Keep the earlier `questions[]` shape as a compatibility alias while
    // normalizing both forms to the native contract.
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
    let mut normalized = Vec::new();
    if let Some(question) = params.get("question").and_then(Value::as_str) {
        normalized.push(normalize_question(params, question)?);
    } else if let Some(questions) = params.get("questions").and_then(Value::as_array) {
        if questions.is_empty() || questions.len() > 3 {
            return Err("questions must contain 1-3 items".into());
        }
        for item in questions {
            let question = item.get("question").and_then(Value::as_str).unwrap_or("");
            normalized.push(normalize_question(item, question)?);
        }
    } else {
        return Err("question is required".into());
    }
    Ok(json!({
        "question": normalized.first().and_then(|v| v.get("question")),
        "context": params.get("context"),
        "options": normalized.first().and_then(|v| v.get("options")),
        "allowMultiple": params.get("allowMultiple").or_else(|| params.get("multiple")).and_then(Value::as_bool).unwrap_or(false),
        "allowFreeform": params.get("allowFreeform").or_else(|| params.get("allow_custom")).and_then(Value::as_bool).unwrap_or(true),
        "allowComment": params.get("allowComment").and_then(Value::as_bool).unwrap_or(false),
        "displayMode": params.get("displayMode"),
        "timeout": params.get("timeout"),
        "suggest": params.get("suggest"),
        "ui": {"kind":"selector", "questions":normalized}
    })
    .to_string())
}

fn normalize_question(item: &Value, question: &str) -> Result<Value, String> {
    let question = question.trim();
    if question.is_empty() || question.len() > 500 {
        return Err("each question must contain 1-500 characters".into());
    }
    let options = item
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if options.len() > 20 {
        return Err("a question may contain at most 20 options".into());
    }
    let options = options
        .into_iter()
        .map(|option| match option {
            Value::String(title) => json!({"title": title}),
            Value::Object(mut object) => {
                if !object.contains_key("title") {
                    for alias in ["label", "text", "value", "name"] {
                        if let Some(value) = object.remove(alias) {
                            object.insert("title".into(), value);
                            break;
                        }
                    }
                }
                Value::Object(object)
            }
            other => json!({"title": other.to_string()}),
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "question": question,
        "options": options,
        "allowMultiple": item.get("allowMultiple").or_else(|| item.get("multiple")).and_then(Value::as_bool).unwrap_or(false),
        "allowFreeform": item.get("allowFreeform").or_else(|| item.get("allow_custom")).and_then(Value::as_bool).unwrap_or(true),
        "allowComment": item.get("allowComment").and_then(Value::as_bool).unwrap_or(false),
    }))
}

fn command_output(out: *mut StbString, value: Value) -> i32 {
    if out.is_null() {
        return 1;
    }
    unsafe { *out = StbString::from_string(value.to_string()) };
    0
}

/// `/ask_user` command bridge for the native rpi selector. Tool calls still
/// return a selector-shaped result for headless hosts; interactive Pi sessions
/// can use this command path until tool-to-UI prompting is wired into the loop.
extern "C" fn ask_command(args_json: StbStringRef, out: *mut StbString, _: *mut c_void) -> i32 {
    let request_text = unsafe { args_json.as_str().to_owned() };
    let request: Value = serde_json::from_str(&request_text).unwrap_or(Value::Null);
    let raw = request.get("args").and_then(Value::as_str).unwrap_or("");
    let params: Value = serde_json::from_str(raw).unwrap_or_else(|_| json!({"question": raw}));
    if params.get("action").and_then(Value::as_str) == Some("select") {
        let value = params.get("value").and_then(Value::as_str).unwrap_or("");
        let _ = PENDING
            .get_or_init(|| Mutex::new(None))
            .lock()
            .map(|mut p| *p = None);
        return command_output(
            out,
            json!({"kind":"message","text":format!("Selected: {value}")}),
        );
    }
    let normalized = match ask(&params) {
        Ok(text) => serde_json::from_str::<Value>(&text).unwrap_or(Value::Null),
        Err(e) => {
            return command_output(
                out,
                json!({"kind":"message","text":format!("ask_user failed: {e}")}),
            )
        }
    };
    let options = normalized
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if options.is_empty() {
        return command_output(out, json!({"kind":"editor","initialText":""}));
    }
    let items = options
        .into_iter()
        .enumerate()
        .map(|(i, option)| {
            let label = option.get("title").and_then(Value::as_str).unwrap_or("");
            let description = option.get("description").and_then(Value::as_str);
            let mut item = json!({"value": label, "label": label});
            if let Some(description) = description {
                item["description"] = json!(description);
            }
            if label.is_empty() {
                item["value"] = json!(i.to_string());
                item["label"] = json!(i.to_string());
            }
            item
        })
        .collect::<Vec<_>>();
    let _ = PENDING
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map(|mut p| *p = Some(normalized));
    command_output(out, json!({"kind":"selector","items":items}))
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
pub extern "C" fn rpi_plugin_register_v2(api: *const PluginApiVt, abi: u32) -> i32 {
    register_entrypoint(api, abi, |api| {
        let Some(register) = api.register_tool else {
            return 1;
        };
        let schema = Box::new(StableToolSchema { name: StbString::from_string("ask_user".into()), description: StbString::from_string("Ask the user an interactive question with selectable options or freeform input.".into()), parameters: StbString::from_string(r#"{"type":"object","properties":{"question":{"type":"string"},"context":{"type":"string"},"options":{"type":"array","items":{"oneOf":[{"type":"string"},{"type":"object"}]}},"allowMultiple":{"type":"boolean"},"allowFreeform":{"type":"boolean"},"allowComment":{"type":"boolean"},"displayMode":{"type":"string","enum":["overlay","inline"]},"timeout":{"type":"integer","minimum":1},"type":{"type":"string","enum":["question","confirm"]},"questions":{"type":"array","minItems":1,"maxItems":3},"suggest":{"type":"string"},"summary":{"type":"string"}},"additionalProperties":false}"#.into()) });
        let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
        drop(schema);
        if let Some(register_command) = api.register_command {
            let name = StbStringRef::from_str("ask_user");
            let description =
                StbStringRef::from_str("Ask a question with the interactive Pi selector");
            let _ = register_command(name, description, ask_command);
        }
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

    #[test]
    fn accepts_native_single_question_contract() {
        let value: Value = serde_json::from_str(
            &ask(&json!({
                "question": "Which target?",
                "options": [{"title":"Linux", "description":"glibc"}, "Windows"],
                "allowMultiple": true,
                "allowFreeform": false,
                "context": "release"
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(value["question"], "Which target?");
        assert_eq!(value["options"][0]["title"], "Linux");
        assert_eq!(value["options"][1]["title"], "Windows");
        assert_eq!(value["allowMultiple"], true);
        assert_eq!(value["allowFreeform"], false);
    }
}
