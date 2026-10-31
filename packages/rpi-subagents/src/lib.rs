#![forbid(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod kit;

use crate::kit::{plugin_free_string, schema};
use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

// ABI-compatible with RuntimeActionId::ChildAgent in the host SDK. Like
// rpi-ask-user, retain the published SDK until that enum is released.
const GET_EXECUTION_CONTEXT: u32 = 23;
const CHILD_AGENT: u32 = 24;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PARTIAL_EVENTS: usize = 8192;
static JOBS: ToolHandles<Job> = ToolHandles::new();
// Only process-lifetime host function pointers are global. Never retain a
// registration's user_data: a candidate reload may be rolled back and freed.
#[derive(Clone, Copy)]
struct RuntimeFns {
    action: extern "C" fn(u32, StbStringRef, *mut StbString, *mut std::ffi::c_void) -> i32,
    free: extern "C" fn(StbString),
}
static HOST: Mutex<Option<RuntimeFns>> = Mutex::new(None);

enum JobMessage {
    Partial(String),
    Done(String),
    Error(String),
}
struct Job {
    receiver: Receiver<JobMessage>,
    completed: bool,
    worker: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Request {
    task: String,
    role: String,
    max_turns: u64,
    timeout_seconds: u64,
}

fn parse_request(params: &Value) -> Result<Request, String> {
    let task = params
        .get("task")
        .and_then(Value::as_str)
        .ok_or("missing string parameter `task`")?;
    let role = params
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("worker")
        .trim();
    if task.trim().is_empty() || task.len() > 20_000 {
        return Err("task must contain 1-20000 bytes".into());
    }
    if role.is_empty() || role.len() > 200 {
        return Err("role must contain 1-200 bytes".into());
    }
    Ok(Request {
        task: task.into(),
        role: role.into(),
        max_turns: params
            .get("maxTurns")
            .and_then(Value::as_u64)
            .unwrap_or(8)
            .clamp(1, 32),
        timeout_seconds: params
            .get("timeoutSeconds")
            .and_then(Value::as_u64)
            .unwrap_or(300)
            .clamp(10, 1800),
    })
}

type Action = dyn Fn(Value) -> Result<Value, String> + Send + Sync;

fn host_action(
    host: &RuntimeFns,
    runtime_handle: usize,
    action: u32,
    args: Value,
) -> Result<Value, String> {
    let encoded = args.to_string();
    let mut out = StbString::empty();
    let rc = (host.action)(
        action,
        StbStringRef::from_str(&encoded),
        &mut out,
        runtime_handle as *mut std::ffi::c_void,
    );
    let text = out.to_string_lossy();
    (host.free)(out);
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| format!("invalid child response (status {rc}): {e}"))?;
    if rc != 0 {
        return Err(value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("child action failed")
            .into());
    }
    Ok(value)
}

/// Fail closed if another registration of this DLL replaced its host context.
fn validate_context(expected: &Value, actual: &Value) -> Result<(), String> {
    if let Some(id) = expected.get("sessionId").and_then(Value::as_str) {
        if actual.get("sessionId").and_then(Value::as_str) == Some(id) {
            return Ok(());
        }
    } else if let Some(cwd) = expected.get("cwd").and_then(Value::as_str) {
        if actual.get("cwd").and_then(Value::as_str) == Some(cwd) {
            return Ok(());
        }
    }
    Err("subagent host context does not match the calling project/session; reload the extension in this session".into())
}

/// Always release the host handle, including malformed replies and local cancellation.
struct Child<'a> {
    action: &'a Action,
    id: String,
}
impl Drop for Child<'_> {
    fn drop(&mut self) {
        let _ = (self.action)(json!({"op":"dispose","childId":self.id}));
    }
}

fn run_request(
    request: Request,
    tool_call_id: String,
    action: &Action,
    output: Sender<JobMessage>,
    cancelled: Arc<AtomicBool>,
) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("subagent cancelled".into());
    }
    let start = action(json!({"op":"start","task":request.task,"role":request.role,
        "maxTurns":request.max_turns,"timeoutSeconds":request.timeout_seconds,"toolCallId":tool_call_id}))
        .map_err(|error| format!("Cannot start a host-managed subagent: {error}. This plugin requires a host supporting ChildAgent; no default-config subprocess fallback is used."))?;
    let id = start
        .get("childId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or("child start returned no childId")?
        .to_owned();
    let child = Child { action, id };
    let deadline = Instant::now() + Duration::from_secs(request.timeout_seconds + 3);
    let mut bytes = 0usize;
    let mut events = 0usize;
    loop {
        if cancelled.load(Ordering::Acquire) {
            let _ = action(json!({"op":"cancel","childId":child.id}));
            return Err("subagent cancelled".into());
        }
        if Instant::now() >= deadline {
            return Err("host-managed subagent timed out".into());
        }
        let value = action(json!({"op":"poll","childId":child.id}))?;
        if let Some(deltas) = value.get("deltas").and_then(Value::as_array) {
            for delta in deltas {
                let text = delta.as_str().ok_or("invalid child text delta")?;
                bytes = bytes.saturating_add(text.len());
                events += 1;
                if bytes > MAX_OUTPUT_BYTES || events > MAX_PARTIAL_EVENTS {
                    return Err("subagent output limit exceeded".into());
                }
                output
                    .send(JobMessage::Partial(text.into()))
                    .map_err(|_| "subagent receiver closed")?;
            }
        }
        match value.get("status").and_then(Value::as_str) {
            Some("running") => thread::sleep(Duration::from_millis(25)),
            Some("failed") => {
                return Err(value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("child failed")
                    .into())
            }
            Some("done") => {
                let result = value
                    .get("result")
                    .ok_or("child completed without a result")?;
                let report = result.get("text").and_then(Value::as_str).unwrap_or("");
                if bytes.saturating_add(report.len()) > MAX_OUTPUT_BYTES {
                    return Err("subagent output limit exceeded".into());
                }
                if result.get("status").and_then(Value::as_str) != Some("completed") {
                    return Err(format!(
                        "subagent {}: {}",
                        result
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("failed"),
                        result
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or(report)
                    ));
                }
                output.send(JobMessage::Done(json!({"kind":"rpi-delegation", "task":request.task,
                    "role":request.role, "maxTurns":request.max_turns, "timeoutSeconds":request.timeout_seconds,
                    "childId":child.id, "sessionId":start.get("sessionId"), "status":"completed",
                    "turns":result.get("turns"), "turnLimited":result.get("turnLimited"), "report":report}).to_string()))
                    .map_err(|_| "subagent receiver closed")?;
                return Ok(());
            }
            _ => return Err("invalid child lifecycle status".into()),
        }
    }
}

fn tool_result(text: &str) -> String {
    json!({"content":[{"type":"text","text":text}]}).to_string()
}

rpi_plugin_sdk::execute_handler!(fn execute(tool_call_id, params, free_params) {
    rpi_plugin_sdk::guard(|| {
        let text = params.to_string_lossy();
        params.free_with(free_params);
        let id = tool_call_id.to_owned();
        let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        let expected_context = value.get("__rpi").cloned().unwrap_or(Value::Null);
        // Capture one registration for the entire worker, including disposal.
        let host = *HOST.lock().unwrap_or_else(|error| error.into_inner());
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker = match parse_request(&value) {
            Ok(request) => {
                let token = cancelled.clone();
                Some(thread::spawn(move || {
                    let result = rpi_plugin_sdk::guard(|| {
                        let host = host.ok_or("subagent host runtime is unavailable")?;
                        let runtime_handle = expected_context.get("runtimeHandle").and_then(Value::as_u64)
                            .and_then(|id| usize::try_from(id).ok()).filter(|id| *id != 0)
                            .ok_or("ChildAgent requires a host-provided tool invocation runtimeHandle; upgrade the host")?;
                        let actual = host_action(&host, runtime_handle, GET_EXECUTION_CONTEXT, json!({})).map_err(|error|
                            format!("Cannot read execution context: {error}; upgrade to a host supporting GetExecutionContext/ChildAgent"))?;
                        validate_context(&expected_context, &actual)?;
                        let runtime_key = expected_context.get("runtimeKey").and_then(Value::as_str)
                            .ok_or("ChildAgent requires a host-provided invocation runtimeKey; upgrade the host")?.to_owned();
                        let action = move |mut args: Value| {
                            args["__rpiRuntimeKey"] = json!(runtime_key);
                            host_action(&host, runtime_handle, CHILD_AGENT, args)
                        };
                        run_request(request, id, &action, sender.clone(), token)
                    });
                    match result {
                        Ok(Ok(())) => {},
                        Ok(Err(error)) => { let _ = sender.send(JobMessage::Error(error)); },
                        Err(_) => { let _ = sender.send(JobMessage::Error("subagent worker panicked".into())); },
                    }
                }))
            }
            Err(error) => { let _ = sender.send(JobMessage::Error(error)); None }
        };
        JOBS.insert_with_cancel(Job { receiver, completed: false, worker }, cancelled)
    }).unwrap_or(std::ptr::null_mut())
});

extern "C" fn poll(
    handle: StepHandle,
    partial_cb: Option<ToolPartialCb>,
    user_data: *mut std::ffi::c_void,
) -> StepResult {
    rpi_plugin_sdk::guard(|| {
        let Some(entry) = JOBS.get(handle) else {
            return StepResult::err(StbString::from_string("invalid subagent handle".into()));
        };
        if entry.cancelled.load(Ordering::Acquire) {
            return StepResult::err(StbString::from_string("subagent cancelled".into()));
        }
        let mut partials = Vec::new();
        let terminal = {
            let mut job = entry.lock();
            if job.completed {
                return StepResult::err(StbString::from_string(
                    "subagent polled after completion".into(),
                ));
            }
            let mut terminal = None;
            while let Ok(message) = job.receiver.try_recv() {
                match message {
                    JobMessage::Partial(text) => partials.push(text),
                    JobMessage::Done(text) => {
                        job.completed = true;
                        terminal = Some(Ok(text));
                        break;
                    }
                    JobMessage::Error(error) => {
                        job.completed = true;
                        terminal = Some(Err(error));
                        break;
                    }
                }
            }
            terminal
        };
        if let Some(callback) = partial_cb {
            for text in partials {
                callback(StbString::from_string(tool_result(&text)), user_data);
            }
        }
        match terminal {
            Some(Ok(text)) => StepResult::done(StbString::from_string(tool_result(&text))),
            Some(Err(error)) => StepResult::err(StbString::from_string(error)),
            None => StepResult::pending(StbString::empty()),
        }
    })
    .unwrap_or_else(|_| StepResult::err(StbString::from_string("subagent poll panicked".into())))
}
extern "C" fn cancel(handle: StepHandle) {
    JOBS.cancel(handle);
}
extern "C" fn destroy(handle: StepHandle) {
    // Never let a detached plugin worker outlive the DLL/host bridge keepalive.
    if let Some(entry) = JOBS.remove(handle) {
        let worker = entry.lock().worker.take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

/// Keep registration and schema tests on the same metadata path.
fn delegate_schema() -> rpi_plugin_sdk::StableToolSchema {
    schema(
        "delegate_task",
        "Delegate one bounded, non-recursive task to a host-managed in-memory child session, not a subagent subprocess or isolated worktree. The child inherits the current model/provider, tools, cwd and approval hooks; it shares the workspace and extension state. Supply necessary context, exact scope/file ownership, constraints, acceptance checks and expected report in task; do not assume the child knows the parent conversation. Coordinate overlapping writes. role is a label, not a tool/permission profile. The child cannot delegate further. Streams progress and returns a JSON report; review the report and verify acceptance, including turnLimited. Requires host ChildAgent support.",
        r#"{
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Required self-contained task: objective, relevant context/paths, allowed scope and write ownership, constraints, acceptance checks and expected report. Must contain non-whitespace text and be at most 20000 UTF-8 bytes (not characters); original text is forwarded unchanged. Shared workspace, not a sandbox or isolated checkout."
                },
                "role": {
                    "type": "string",
                    "default": "worker",
                    "description": "Optional descriptive role label, not a permission/tool selector. Trimmed value must contain 1-200 UTF-8 bytes; default worker."
                },
                "maxTurns": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 32,
                    "default": 8,
                    "description": "Per-child turn budget, 1-32; default 8. A completed report may be turnLimited; verify task acceptance."
                },
                "timeoutSeconds": {
                    "type": "integer",
                    "minimum": 10,
                    "maximum": 1800,
                    "default": 300,
                    "description": "Child timeout in seconds, 10-1800; default 300. Timeout/cancellation triggers host-managed cleanup."
                }
            },
            "required": ["task"],
            "additionalProperties": true
        }"#,
    )
}

rpi_plugin_sdk::export_plugin!(|api| {
    if std::env::var("RPI_SUBAGENT_CHILD").ok().as_deref() == Some("1") {
        return 0;
    }
    let Some(register) = api.register_tool else {
        return 1;
    };
    *HOST.lock().unwrap_or_else(|error| error.into_inner()) = Some(RuntimeFns {
        action: api.runtime_action,
        free: api.free_string,
    });
    let schema = Box::new(delegate_schema());
    let result = register(&*schema, execute, poll, cancel, destroy, plugin_free_string);
    drop(schema);
    result
});

#[cfg(test)]
mod tests;
