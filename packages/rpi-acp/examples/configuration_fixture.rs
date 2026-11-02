#![forbid(unsafe_code)]
//! Test-only native runtime-action probe. Never install this example as a package.
use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{
    FreeStringFn, PluginApi, RuntimeActionId, StableToolSchema, StbString, StbStringRef,
    StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::{atomic::Ordering, OnceLock};

static RUNTIME: OnceLock<PluginApi> = OnceLock::new();
static HANDLES: ToolHandles<Drive> = ToolHandles::new();
struct Drive {
    params: Value,
    done: bool,
}

fn call_runtime(id: RuntimeActionId, args: Value) -> Result<Value, String> {
    let runtime = *RUNTIME
        .get()
        .ok_or("configuration fixture runtime unavailable")?;
    let input = args.to_string();
    let mut output = StbString::empty();
    let rc = (runtime.runtime_action)(
        id as u32,
        StbStringRef::from_str(&input),
        &mut output,
        runtime.user_data,
    );
    let text = output.to_string_lossy();
    (runtime.free_string)(output);
    if rc != 0 {
        return Err(format!("runtime action {id:?}: {text}"));
    }
    if text.trim().is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }
}

extern "C" fn execute(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let text = params.to_string_lossy();
    params.free_with(free);
    HANDLES.insert(Drive {
        params: serde_json::from_str(&text).unwrap_or(Value::Null),
        done: false,
    })
}

extern "C" fn poll(handle: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    let Some(entry) = HANDLES.get(handle) else {
        return StepResult::err(StbString::from_string("invalid fixture handle".into()));
    };
    let mut drive = entry.lock();
    if drive.done || entry.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string(
            "fixture cancelled or completed".into(),
        ));
    }
    drive.done = true;
    let result = (|| {
        let level = drive
            .params
            .get("level")
            .and_then(Value::as_str)
            .filter(|s| matches!(*s, "off" | "minimal" | "low" | "medium" | "high"))
            .ok_or("invalid fixture level")?;
        // This is native SDK RuntimeActionFn (action 8), NOT ACP set_config_option.
        call_runtime(RuntimeActionId::SetThinkingLevel, json!({"level": level}))?;
        let actual = call_runtime(RuntimeActionId::GetThinkingLevel, json!({}))?;
        if actual.get("level").and_then(Value::as_str) != Some(level) {
            return Err(format!("native readback mismatch: {actual}"));
        }
        Ok(json!({"content":[{"type":"text","text":format!("NATIVE_CONFIGURATION_FIXTURE:{level}")}],
            "details":{"action":8,"nativeReadback":actual}}).to_string())
    })();
    match result {
        Ok(value) => StepResult::done(StbString::from_string(value)),
        Err(error) => StepResult::err(StbString::from_string(error)),
    }
}
extern "C" fn cancel(handle: StepHandle) {
    HANDLES.cancel(handle);
}
extern "C" fn destroy(handle: StepHandle) {
    drop(HANDLES.remove(handle));
}

rpi_plugin_sdk::export_plugin!(|api| {
    // Explicit opt-in in an isolated test process, in addition to explicit DLL path.
    if std::env::var("RPI_CONFIGURATION_NATIVE_FIXTURE").as_deref() != Ok("1") {
        return 1;
    }
    let Some(register) = api.register_tool else {
        return 1;
    };
    let _ = RUNTIME.set(*api);
    let schema = StableToolSchema {
        name: StbString::from_string("configuration_native_fixture".into()),
        description: StbString::from_string(
            "Test-only native thinking-level runtime action probe".into(),
        ),
        parameters: StbString::from_string(
            json!({"type":"object","additionalProperties":false,
            "required":["level"],"properties":{"level":{"type":"string",
            "enum":["off","minimal","low","medium","high"]}}})
            .to_string(),
        ),
    };
    register(
        &schema,
        execute,
        poll,
        cancel,
        destroy,
        rpi_plugin_sdk::callbacks::free_string,
    )
});
