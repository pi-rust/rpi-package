#![deny(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod engine;
// The only unsafe module: validated delegation to the pinned QuickJS allocator.
#[allow(unsafe_code)]
mod heap;
use engine::{Action, Request};
use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

// Compatibility shim: published workspace SDK 0.3.20 predates ToolProgram.
const TOOL_PROGRAM: u32 = 30;
#[derive(Clone, Copy)]
struct RuntimeFns {
    action: extern "C" fn(u32, StbStringRef, *mut StbString, *mut std::ffi::c_void) -> i32,
    free: extern "C" fn(StbString),
}
// Process-lifetime functions only, never registration user_data or capabilities.
static HOST: Mutex<Option<RuntimeFns>> = Mutex::new(None);
static JOBS: ToolHandles<Job> = ToolHandles::new();
struct Job {
    receiver: Receiver<Result<Value, String>>,
    worker: Option<JoinHandle<()>>,
    completed: bool,
}

fn bind(host: Option<RuntimeFns>, params: &Value) -> Result<Arc<Action>, String> {
    let host = host
        .ok_or("ToolProgram host bridge unavailable; upgrade to an action-30 host (no fallback)")?;
    let metadata = &params["__rpi"];
    let handle = metadata["runtimeHandle"].as_u64().and_then(|n|usize::try_from(n).ok()).filter(|n|*n != 0)
        .ok_or("ToolProgram requires a host-provided invocation runtimeHandle; upgrade the host (no fallback)")?;
    let key = metadata["runtimeKey"].as_str().filter(|k|!k.is_empty())
        .ok_or("ToolProgram requires a host-provided invocation runtimeKey; upgrade the host (no fallback)")?.to_owned();
    Ok(Arc::new(move |mut args: Value| {
        args["__rpiRuntimeKey"] = json!(key);
        let request_id = args
            .get("requestId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let encoded = args.to_string();
        let mut out = StbString::empty();
        let rc = (host.action)(
            TOOL_PROGRAM,
            StbStringRef::from_str(&encoded),
            &mut out,
            handle as *mut std::ffi::c_void,
        );
        let text = out.to_string_lossy();
        (host.free)(out);
        // Never echo malformed host frames or authentication capabilities.
        if text.len() > engine::MAX_JSON {
            return Err("ToolProgram response exceeds 1 MiB".into());
        }
        let mut value: Value = serde_json::from_str(&text).map_err(|_| {
            format!(
                "invalid ToolProgram response (status {rc}); host action 30 / protocol v1 required"
            )
        })?;
        redact(&mut value, &key);
        if let Some(error) = value
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            let mut error = error.replace(&handle.to_string(), "[invocation]");
            if let Some(id) = &request_id {
                error = error.replace(id, "[request]");
            }
            value["error"] = json!(error);
        }
        if rc != 0 {
            return Err(format!(
                "ToolProgram action 30 failed: {}",
                engine::bounded(
                    value
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("unsupported host or expired invocation; upgrade host"),
                    4096
                )
            ));
        }
        Ok(value)
    }))
}
fn redact(value: &mut Value, key: &str) {
    engine::public_value(value);
    match value {
        Value::String(s) => *s = s.replace(key, "[redacted]"),
        Value::Object(m) => {
            for v in m.values_mut() {
                redact(v, key);
            }
        }
        Value::Array(a) => {
            for v in a {
                redact(v, key);
            }
        }
        _ => {}
    }
}
fn tool_result(value: Value) -> Result<String, String> {
    // The native adapter ignores isError in Done JSON. Fail via the ABI tag,
    // retaining the complete bounded summary as error text rather than details.
    if value.get("status").is_some()
        && !matches!(value["status"].as_str(), Some("completed" | "terminated"))
    {
        return Err(value.to_string());
    }
    let encoded = json!({"content":[{"type":"text","text":value.to_string()}],
        "terminate":value["terminate"] == true,"details":value})
    .to_string();
    if encoded.len() <= engine::MAX_JSON {
        return Ok(encoded);
    }
    Err(json!({"status":"error","result":null,
        "error":"encoded codemode output exceeds 1 MiB (including text/structured framing)",
        "logs":value.get("logs"),"calls":value.get("calls"),
        "durationMs":value.get("durationMs"),"terminate":value["terminate"] == true})
    .to_string())
}

rpi_plugin_sdk::execute_handler!(fn execute(_tool_call_id, params, free_params) {
    rpi_plugin_sdk::guard(|| {
        let text = params.to_string_lossy();
        params.free_with(free_params);
        let params: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let request = engine::parse_request(&params);
        let host = *HOST.lock().unwrap_or_else(|e|e.into_inner());
        let cancelled = Arc::new(AtomicBool::new(false));
        let token = cancelled.clone();
        let (sender,receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = rpi_plugin_sdk::guard(|| {
                let request = request?;
                let action = bind(host,&params)?;
                match request {
                    Request::List { offset,limit } => engine::list_page(&*action,offset,limit),
                    Request::Execute { code,timeout_ms } => Ok(engine::execute(&code,timeout_ms,action,token)),
                }
            }).unwrap_or_else(|_|Err("codemode worker panicked; no calls retried".into()));
            let _ = sender.send(result);
        });
        JOBS.insert_with_cancel(Job { receiver,worker:Some(worker),completed:false },cancelled)
    }).unwrap_or(std::ptr::null_mut())
});

extern "C" fn poll(
    handle: StepHandle,
    _: Option<ToolPartialCb>,
    _: *mut std::ffi::c_void,
) -> StepResult {
    rpi_plugin_sdk::guard(|| {
        let Some(entry) = JOBS.get(handle) else {
            return StepResult::err(StbString::from_string("invalid codemode handle".into()));
        };
        let mut job = entry.lock();
        if job.completed {
            return StepResult::err(StbString::from_string(
                "codemode polled after completion".into(),
            ));
        }
        match job.receiver.try_recv() {
            Ok(Ok(value)) => {
                job.completed = true;
                match tool_result(value) {
                    Ok(text) => StepResult::done(StbString::from_string(text)),
                    Err(text) => StepResult::err(StbString::from_string(text)),
                }
            }
            Ok(Err(e)) => {
                job.completed = true;
                StepResult::err(StbString::from_string(e))
            }
            Err(TryRecvError::Empty) => StepResult::pending(StbString::empty()),
            Err(TryRecvError::Disconnected) => {
                job.completed = true;
                StepResult::err(StbString::from_string(
                    "codemode worker disconnected".into(),
                ))
            }
        }
    })
    .unwrap_or_else(|_| StepResult::err(StbString::from_string("codemode poll panicked".into())))
}
extern "C" fn cancel(handle: StepHandle) {
    let _ = rpi_plugin_sdk::guard(|| JOBS.cancel(handle));
}
extern "C" fn destroy(handle: StepHandle) {
    let _ = rpi_plugin_sdk::guard(|| {
        if let Some(entry) = JOBS.remove(handle) {
            let worker = entry.lock().worker.take();
            if let Some(worker) = worker {
                let _ = worker.join();
            }
        }
    });
}

fn schema() -> rpi_plugin_sdk::StableToolSchema {
    rpi_plugin_sdk::StableToolSchema {
        name: StbString::from_string("codemode".into()),
        description: StbString::from_string("Execute a bounded JavaScript async body (with return) using the current invocation's tools, without another model turn or MCP. tools.name(args) / tools.call(name,args) return finalized tool results; Promise.all runs at most 8 active calls concurrently (128 total); batch larger sets yourself. print / console capture bounded logs. action=list pages the effective catalog. Fresh JS; RegExp literals/constructor and regex-backed string operations are unsupported (the pinned native regex engine cannot be interrupted). No direct filesystem/network/process. Reserved host metadata names are filtered recursively in results/catalog schemas and forbidden in arguments; not lossless for arbitrary property names. Calls retain host approvals and may have irreversible side effects; errors/cancellation never roll back or retry. Failures return a JSON summary as error text; success includes structured details. Requires ToolProgram action 30 / protocol v1.".into()),
        parameters: StbString::from_string(json!({
            "type":"object", "x-rpi-execution-mode":"sequential", "properties": {
                "action":{"type":"string","enum":["list","execute"],"default":"execute","description":"execute (default) runs code; list pages the effective invocation catalog without running code. Follow nextOffset until null."},
                "code":{"type":"string","minLength":1,"description":"JavaScript async function body, not TypeScript; required for execute. 1..65536 UTF-8 bytes. Return a JSON-compatible value (up to 1 MiB)."},
                "timeoutMs":{"type":"integer","minimum":1,"maximum":120000,"default":30000,"description":"Execute-only total deadline in milliseconds, including catalog discovery; default 30000, range 1..120000. Invalid values are rejected. Timeout does not undo tool effects."},
                "offset":{"type":"integer","minimum":0,"maximum":1024,"default":0,"description":"List-only entry offset, default 0, range 0..1024. Use the previous nextOffset; catalogs above the extension limit fail explicitly."},
                "limit":{"type":"integer","minimum":1,"maximum":128,"default":32,"description":"List-only maximum entries per page, default 32, range 1..128; not a total catalog limit. nextOffset indicates more entries."}
            }, "additionalProperties":true,
            "allOf":[{"if":{"properties":{"action":{"const":"execute"}}},"then":{"required":["code"]}}]
        }).to_string()),
    }
}
rpi_plugin_sdk::export_plugin!(|api| {
    let Some(register) = api.register_tool else {
        return 1;
    };
    *HOST.lock().unwrap_or_else(|e| e.into_inner()) = Some(RuntimeFns {
        action: api.runtime_action,
        free: api.free_string,
    });
    let schema = schema();
    register(
        &schema,
        execute,
        poll,
        cancel,
        destroy,
        rpi_plugin_sdk::callbacks::free_string,
    )
});

#[cfg(test)]
mod tests;
