//! Invocation-local engine. Host identities exist only in Rust-owned guards.
use rquickjs::{Context, Ctx, Exception, Function, Persistent, Promise, Runtime};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) type Action = dyn Fn(Value) -> Result<Value, String> + Send + Sync;
pub(crate) const MAX_CODE: usize = 64 * 1024;
pub(crate) const MAX_JSON: usize = 1024 * 1024;
const MAX_LOG: usize = 64 * 1024;
const MAX_LOG_ENTRIES: usize = 256;
const MAX_CALLS: usize = 128;
const MAX_ACTIVE: usize = 8;
const MEMORY: usize = 32 * 1024 * 1024;
const STACK: usize = 256 * 1024;

#[derive(Debug)]
pub(crate) enum Request {
    List { offset: u64, limit: u64 },
    Execute { code: String, timeout_ms: u64 },
}

fn integer(v: &Value, name: &str, default: u64, min: u64, max: u64) -> Result<u64, String> {
    match v.get(name) {
        None => Ok(default),
        Some(x) => x
            .as_u64()
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| format!("{name} must be an integer in {min}..={max}")),
    }
}

pub(crate) fn parse_request(v: &Value) -> Result<Request, String> {
    if !v.is_object() {
        return Err("codemode arguments must be an object".into());
    }
    let timeout_ms = integer(v, "timeoutMs", 30000, 1, 120000)?;
    match v
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or(if v.get("action").is_none() {
            "execute"
        } else {
            ""
        }) {
        "list" => Ok(Request::List {
            offset: integer(v, "offset", 0, 0, 1024)?,
            limit: integer(v, "limit", 32, 1, 128)?,
        }),
        "execute" => {
            let code = v
                .get("code")
                .and_then(Value::as_str)
                .ok_or("execute requires string code")?;
            if code.trim().is_empty() || code.len() > MAX_CODE {
                return Err("code must contain 1..=65536 UTF-8 bytes".into());
            }
            Ok(Request::Execute {
                code: code.into(),
                timeout_ms,
            })
        }
        _ => Err("action must be list or execute".into()),
    }
}

/// Remove transport metadata even if a tool accidentally returns it in details.
pub(crate) fn public_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in ["__rpi", "__rpiRuntimeKey", "runtimeHandle", "runtimeKey"] {
                map.remove(key);
            }
            for v in map.values_mut() {
                public_value(v);
            }
        }
        Value::Array(array) => {
            for v in array {
                public_value(v);
            }
        }
        _ => {}
    }
}

pub(crate) fn list_page(action: &Action, offset: u64, limit: u64) -> Result<Value, String> {
    let mut page = action(json!({"op":"list", "offset":offset, "limit":limit}))
        .map_err(|e| format!("ToolProgram (action 30) unavailable: {e}. Upgrade to a host implementing protocol v1; no direct-call fallback."))?;
    if page.to_string().len() > MAX_JSON {
        return Err("catalog page exceeds 1 MiB".into());
    }
    if page.get("protocolVersion").and_then(Value::as_u64) != Some(1) {
        return Err("unsupported ToolProgram protocol; host must implement v1".into());
    }
    let tools = page
        .get("tools")
        .and_then(Value::as_array)
        .ok_or("invalid catalog tools")?;
    if tools.len() > limit as usize {
        return Err("catalog page exceeds requested limit".into());
    }
    if offset.saturating_add(tools.len() as u64) > 1024 {
        return Err("catalog exceeds extension limit: 1024 entries".into());
    }
    for t in tools {
        if t.get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty() && n.len() <= 200)
            .is_none()
            || !t.get("description").is_some_and(Value::is_string)
            || !t.get("parameters").is_some_and(Value::is_object)
        {
            return Err("invalid tool catalog entry".into());
        }
    }
    match page.get("nextOffset") {
        Some(Value::Null) => {}
        Some(n) if n.as_u64().is_some_and(|n| n > offset && n <= 1024) => {}
        _ => {
            return Err("invalid catalog nextOffset (extension catalog limit: 1024 entries)".into())
        }
    }
    for tool in page["tools"]
        .as_array_mut()
        .ok_or("invalid catalog tools")?
    {
        *tool = json!({"name":tool["name"],"description":tool["description"],"parameters":tool["parameters"]});
    }
    public_value(&mut page);
    // Return only the public contract, never arbitrary host envelope fields.
    Ok(json!({"protocolVersion":1,"tools":page["tools"],"nextOffset":page["nextOffset"]}))
}

struct Call {
    id: String,
    token: usize,
    name: String,
    started: Instant,
}
struct Bridge {
    action: Arc<Action>,
    names: HashSet<String>,
    pending: Vec<Call>,
    calls: Vec<Value>,
    logs: Vec<Value>,
    log_bytes: usize,
    started: usize,
    fatal: Option<String>,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
    terminate: Option<Value>,
}
impl Bridge {
    fn check(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err("codemode cancelled".into());
        }
        if Instant::now() >= self.deadline {
            return Err("codemode deadline exceeded".into());
        }
        if let Some(e) = &self.fatal {
            return Err(e.clone());
        }
        Ok(())
    }
    fn start(&mut self, name: String, args: String) -> Result<usize, String> {
        self.check()?;
        if !self.names.contains(&name) {
            return Err("tool is not in this invocation's effective catalog".into());
        }
        if args.len() > MAX_JSON {
            return self.fail("tool arguments exceed 1 MiB");
        }
        let args: Value = serde_json::from_str(&args).map_err(|_| "invalid tool argument JSON")?;
        if !args.is_object() {
            return Err("tool arguments must be an object".into());
        }
        // Reserved metadata is never supplied by the guest, even nested.
        if has_reserved(&args) {
            return Err("reserved host metadata is not a tool argument".into());
        }
        if self.started >= MAX_CALLS || self.pending.len() >= MAX_ACTIVE {
            return self.fail("tool call budget exceeded (128 total / 8 active)");
        }
        self.started += 1;
        let token = self.started;
        let started = Instant::now();
        let reply = (self.action)(json!({"op":"start","name":name,"arguments":args,
            "timeoutMs":self.deadline.saturating_duration_since(Instant::now()).as_millis().clamp(1,120000) as u64}));

        let reply = match reply {
            Ok(r) => r,
            Err(e) => {
                self.calls
                    .push(json!({"name":name,"status":"start_failed","error":bounded(&e,4096)}));
                return self.fail(&format!(
                    "host start failed (not retried): {}",
                    bounded(&e, 4096)
                ));
            }
        };
        let Some(id) = reply
            .get("requestId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return self
                .fail("host start returned no valid requestId; uncertain start not retried");
        };
        let id = id.to_owned();
        self.pending.push(Call {
            id,
            token,
            name,
            started,
        });
        if reply.get("status").and_then(Value::as_str) != Some("running") {
            return self.fail("invalid host start status");
        }
        Ok(token)
    }
    fn fail<T>(&mut self, error: &str) -> Result<T, String> {
        self.fatal = Some(error.into());
        Err(error.into())
    }
    fn log(&mut self, level: String, text: String) -> Result<(), String> {
        self.check()?;
        if self.logs.len() >= MAX_LOG_ENTRIES || self.log_bytes.saturating_add(text.len()) > MAX_LOG
        {
            return self.fail("log budget exceeded (256 entries / 64 KiB)");
        }
        self.log_bytes += text.len();
        self.logs.push(json!({"level":level,"text":text}));
        Ok(())
    }
    fn cleanup(&mut self) {
        for call in self.pending.drain(..) {
            let _ = (self.action)(json!({"op":"cancel","requestId":call.id}));
            let _ = (self.action)(json!({"op":"dispose","requestId":call.id}));
            self.calls.push(json!({"name":call.name,"status":"cancelled","durationMs":call.started.elapsed().as_millis() as u64}));
        }
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn has_reserved(value: &Value) -> bool {
    match value {
        Value::Object(m) => m.iter().any(|(k, v)| {
            ["__rpi", "__rpiRuntimeKey", "runtimeKey", "runtimeHandle"].contains(&k.as_str())
                || has_reserved(v)
        }),
        Value::Array(a) => a.iter().any(has_reserved),
        _ => false,
    }
}
pub(crate) fn bounded(s: &str, max: usize) -> String {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].into()
}
fn js_error(ctx: &Ctx<'_>, error: rquickjs::Error) -> String {
    if error.is_exception() {
        let value = ctx.catch();
        if let Some(object) = value.as_object() {
            if let Some(exception) = Exception::from_object(object.clone()) {
                return bounded(
                    &exception
                        .message()
                        .unwrap_or_else(|| "JavaScript exception (possibly out of memory)".into()),
                    4096,
                );
            }
        }
        "JavaScript rejected with a non-Error value".into()
    } else {
        bounded(&error.to_string(), 4096)
    }
}

pub(crate) fn execute(
    code: &str,
    timeout_ms: u64,
    action: Arc<Action>,
    cancelled: Arc<AtomicBool>,
) -> Value {
    let started = Instant::now();
    let deadline = started + Duration::from_millis(timeout_ms);
    let bridge = Rc::new(RefCell::new(Bridge {
        action,
        names: HashSet::new(),
        pending: Vec::new(),
        calls: Vec::new(),
        logs: Vec::new(),
        log_bytes: 0,
        started: 0,
        fatal: None,
        cancelled: cancelled.clone(),
        deadline,
        terminate: None,
    }));
    let result = run(code, &bridge, &cancelled, deadline);
    let mut state = bridge.borrow_mut();
    state.cleanup();
    let (status, result, error) = if let Some(result) = state.terminate.take() {
        ("terminated", result, None)
    } else if cancelled.load(Ordering::Acquire) {
        ("cancelled", Value::Null, Some("codemode cancelled".into()))
    } else if Instant::now() >= deadline {
        (
            "timed_out",
            Value::Null,
            Some("codemode deadline exceeded".into()),
        )
    } else if let Some(error) = state.fatal.take() {
        ("error", Value::Null, Some(error))
    } else {
        match result {
            Ok(v) => ("completed", v, None),
            Err(e) => ("error", Value::Null, Some(e)),
        }
    };
    json!({"status":status,"result":result,"error":error,"logs":state.logs,"calls":state.calls,
        "durationMs":started.elapsed().as_millis() as u64,"terminate":status == "terminated"})
}

fn run(
    code: &str,
    bridge: &Rc<RefCell<Bridge>>,
    cancelled: &Arc<AtomicBool>,
    deadline: Instant,
) -> Result<Value, String> {
    if code.len() > MAX_CODE {
        return Err("code budget exceeded".into());
    }
    let mut catalog = Vec::new();
    let mut offset = 0;
    loop {
        bridge.borrow().check()?;
        let page = list_page(&*bridge.borrow().action, offset, 32)?;
        catalog.extend(
            page["tools"]
                .as_array()
                .ok_or("invalid catalog")?
                .iter()
                .cloned(),
        );
        if catalog.len() > 1024
            || serde_json::to_vec(&catalog)
                .map_err(|e| e.to_string())?
                .len()
                > MAX_JSON
        {
            return Err("full catalog budget exceeded".into());
        }
        match page["nextOffset"].as_u64() {
            Some(n) => offset = n,
            None => break,
        }
    }
    bridge.borrow_mut().names = catalog
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_owned))
        .collect();
    let rt = Runtime::new().map_err(|e| e.to_string())?;
    rt.set_memory_limit(MEMORY);
    rt.set_max_stack_size(STACK);
    let token = cancelled.clone();
    rt.set_interrupt_handler(Some(Box::new(move || {
        token.load(Ordering::Acquire) || Instant::now() >= deadline
    })));
    let context = Context::full(&rt).map_err(|e| e.to_string())?;
    let (settle, promise) = context
        .with(|ctx| -> rquickjs::Result<_> {
            let start_state = bridge.clone();
            let start = Function::new(
                ctx.clone(),
                move |ctx: Ctx<'_>, name: String, args: String| {
                    start_state
                        .borrow_mut()
                        .start(name, args)
                        .map_err(|e| Exception::throw_message(&ctx, &e))
                },
            )?;
            let log_state = bridge.clone();
            let log = Function::new(
                ctx.clone(),
                move |ctx: Ctx<'_>, level: String, text: String| {
                    log_state
                        .borrow_mut()
                        .log(level, text)
                        .map_err(|e| Exception::throw_message(&ctx, &e))
                },
            )?;
            let factory: Function = ctx.eval(include_str!("guest.js"))?;
            let settle: Function = factory.call((
                start,
                log,
                serde_json::to_string(&catalog).unwrap_or_default(),
            ))?;
            let promise: Promise =
                ctx.eval(format!("(async function() {{ 'use strict';\n{code}\n}})()"))?;
            Ok((
                Persistent::save(&ctx, settle),
                Persistent::save(&ctx, promise),
            ))
        })
        .map_err(|e| context.with(|ctx| js_error(&ctx, e)))?;
    loop {
        bridge.borrow().check()?;
        // Use Runtime's fallible API outside Context::with's runtime lock.
        // Ctx::execute_pending_job only returns bool and hides job exceptions.
        for _ in 0..64 {
            bridge.borrow().check()?;
            match rt.execute_pending_job() {
                Ok(false) => break,
                Ok(true) => {}
                Err(e) => {
                    return Err(e.0.with(|ctx| {
                        // rquickjs-core 0.8.1 wraps JS_ExecutePendingJob's borrowed
                        // context in an owning JobException without JS_DupContext.
                        // Transfer one native-only Ctx reference to that wrapper so
                        // its destructor cannot over-release our invocation context.
                        // Ctx holds no Rust allocation/runtime Arc; this balances
                        // exactly the wrapper's JS_FreeContext, not a retained leak.
                        std::mem::forget(ctx.clone());
                        js_error(&ctx, rquickjs::Error::Exception)
                    }));
                }
            }
        }
        let outcome = context.with(|ctx| -> rquickjs::Result<Option<Value>> {
            let promise = promise.clone().restore(&ctx)?;
            let settle = settle.clone().restore(&ctx)?;
            if let Err(e) = bridge.borrow().check() { return Err(Exception::throw_message(&ctx,&e)); }
            if let Some(result) = promise.result::<rquickjs::Value>() {
                let result = result?;
                let json = ctx.json_stringify(result)?.map(|s|s.to_string()).transpose()?.unwrap_or_else(||"null".into());
                if json.len() > MAX_JSON { return Err(Exception::throw_message(&ctx,"result exceeds 1 MiB")); }
                let mut value: Value = serde_json::from_str(&json).map_err(|_|Exception::throw_message(&ctx,"result is not JSON"))?;
                public_value(&mut value);
                return Ok(Some(value));
            }
            let mut ready = Vec::new();
            {
                let mut state = bridge.borrow_mut();
                for i in (0..state.pending.len()).rev() {
                    let call = &state.pending[i];
                    let reply = (state.action)(json!({"op":"poll","requestId":call.id})).map_err(|e|Exception::throw_message(&ctx,&bounded(&e,4096)))?;
                    if reply.to_string().len() > MAX_JSON { return Err(Exception::throw_message(&ctx,"host result exceeds 1 MiB")); }
                    if reply.get("requestId").and_then(Value::as_str) != Some(&call.id) { return Err(Exception::throw_message(&ctx,"host poll identity mismatch")); }
                    match reply.get("status").and_then(Value::as_str) {
                        Some("running") => continue,
                        Some("done" | "failed") => {},
                        _ => return Err(Exception::throw_message(&ctx,"invalid host poll status")),
                    }
                    let failed = reply["status"] == "failed";
                    let is_error = failed || reply.get("isError").and_then(Value::as_bool) == Some(true)
                        || reply["result"]["isError"] == true;
                    let mut result = reply.get("result").cloned().unwrap_or(Value::Null);
                    if !failed && !result.is_object() { return Err(Exception::throw_message(&ctx,"host completed without finalized result")); }
                    public_value(&mut result);
                    let call = state.pending.remove(i);
                    let disposal = (state.action)(json!({"op":"dispose","requestId":call.id}));
                    state.calls.push(json!({"name":call.name,"status":if is_error {"error"} else {"done"},"isError":is_error,"durationMs":call.started.elapsed().as_millis() as u64}));
                    disposal.map_err(|e|Exception::throw_message(&ctx,&format!("host dispose failed: {}",bounded(&e,4096))))?;
                    if failed { return Err(Exception::throw_message(&ctx,&format!("host lifecycle failed: {}",bounded(reply.get("error").and_then(Value::as_str).unwrap_or("host tool failed"),4096)))); }
                    if result.get("terminate").and_then(Value::as_bool) == Some(true) { state.terminate = Some(result); return Ok(Some(Value::Null)); }
                    let payload = if is_error { bounded(&format!("tool returned isError: {}",result),4096) }
                        else { result.to_string() };
                    ready.push((call.token,!is_error,payload));
                }
            }
            for (token,ok,payload) in ready { settle.call::<_,()>((token,ok,payload))?; }
            Ok(None)
        }).map_err(|e| context.with(|ctx| js_error(&ctx,e)))?;
        if let Some(value) = outcome {
            return Ok(value);
        }
        if bridge.borrow().pending.is_empty() && !rt.is_job_pending() {
            // Settling the last call can synchronously complete the main promise;
            // inspect it once more before declaring a stalled program.
            let pending = context
                .with(|ctx| {
                    promise
                        .clone()
                        .restore(&ctx)
                        .map(|p| p.state() == rquickjs::promise::PromiseState::Pending)
                })
                .map_err(|e| context.with(|ctx| js_error(&ctx, e)))?;
            if pending {
                return Err("pending promise has no runnable jobs or host calls".into());
            }
        }
        if !bridge.borrow().pending.is_empty() {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[cfg(test)]
mod tests;
