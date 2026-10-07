use rpi_plugin_sdk::{
    register_entrypoint, EventTag, FreeStringFn, PluginApi, RuntimeActionFn, RuntimeActionId,
    StablePluginEvent, StbString, StbStringRef,
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    ffi::c_void,
    sync::{Mutex, OnceLock},
    time::Instant,
};

const KEY: &str = "rpi.run-stats";
#[derive(Clone, Copy)]
struct Runtime {
    action: RuntimeActionFn,
    free: FreeStringFn,
    data: *mut c_void,
}
// The host callbacks and context remain valid for the loaded plugin lifetime.
unsafe impl Send for Runtime {}
unsafe impl Sync for Runtime {}
static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static STATE: OnceLock<Mutex<Stats>> = OnceLock::new();
static TIMER: Mutex<Option<(std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>)>> =
    Mutex::new(None);
fn state() -> &'static Mutex<Stats> {
    STATE.get_or_init(|| Mutex::new(Stats::default()))
}

struct Stats {
    enabled: bool,
    anchor: String,
    running: bool,
    rounds: u64,
    steps: u64,
    errors: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost: Option<f64>,
    request: Option<Instant>,
    first: Option<Instant>,
    ttft: Option<f64>,
    latency: Option<f64>,
    speed: Option<f64>,
    recent: VecDeque<(Instant, u64)>,
}
impl Default for Stats {
    fn default() -> Self {
        Self {
            enabled: true,
            anchor: "top-right".into(),
            running: false,
            rounds: 0,
            steps: 0,
            errors: 0,
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cost: None,
            request: None,
            first: None,
            ttft: None,
            latency: None,
            speed: None,
            recent: VecDeque::new(),
        }
    }
}
fn count(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}
fn assistant(v: &Value) -> bool {
    v.get("role")
        .or_else(|| v.get("kind"))
        .and_then(Value::as_str)
        == Some("assistant")
}
impl Stats {
    fn update(&mut self, tag: EventTag, v: &Value, now: Instant) {
        match tag {
            EventTag::SessionStart => {
                let enabled = self.enabled;
                let anchor = self.anchor.clone();
                *self = Self::default();
                self.enabled = enabled;
                self.anchor = anchor;
            }
            EventTag::AgentStart => {
                self.rounds += 1;
                self.running = true;
            }
            EventTag::AgentEnd => {
                self.running = false;
                self.request = None;
                self.first = None;
            }
            EventTag::BeforeProviderRequest => {
                self.steps += 1;
                self.request = Some(now);
                self.first = None;
                self.ttft = None;
                self.latency = None;
                self.speed = None;
            }
            EventTag::MessageUpdate => {
                let event = v.get("assistantMessageEvent").unwrap_or(v);
                let ty = event.get("type").and_then(Value::as_str).unwrap_or("");
                if self.first.is_none()
                    && matches!(ty, "text_delta" | "thinking_delta" | "toolcall_delta")
                    && event
                        .get("delta")
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty())
                {
                    if let Some(start) = self.request {
                        self.first = Some(now);
                        self.ttft = Some(now.duration_since(start).as_secs_f64());
                    }
                }
            }
            EventTag::MessageEnd if assistant(v) => {
                // Only finalize an open request; duplicate terminal events cannot double count.
                let Some(start) = self.request.take() else {
                    return;
                };
                self.latency = Some(now.duration_since(start).as_secs_f64());
                if matches!(
                    v.get("stopReason").and_then(Value::as_str),
                    Some("error" | "aborted")
                ) {
                    self.errors += 1;
                }
                if let Some(usage) = v.get("usage") {
                    let output = count(usage, "output");
                    self.input += count(usage, "input");
                    self.output += output;
                    self.cache_read += count(usage, "cacheRead");
                    self.cache_write += count(usage, "cacheWrite");
                    if let Some(cost) = usage
                        .pointer("/cost/total")
                        .and_then(Value::as_f64)
                        .filter(|v| v.is_finite() && *v >= 0.0)
                    {
                        self.cost = Some(self.cost.unwrap_or(0.0) + cost);
                    }
                    let total = usage
                        .get("totalTokens")
                        .and_then(Value::as_u64)
                        .unwrap_or_else(|| {
                            count(usage, "input")
                                + output
                                + count(usage, "cacheRead")
                                + count(usage, "cacheWrite")
                        });
                    self.recent.push_back((now, total));
                    // Include TTFT in throughput: true end-to-end output tokens / request second.
                    self.speed = self
                        .latency
                        .filter(|seconds| *seconds > 0.0)
                        .map(|seconds| output as f64 / seconds);
                }
                self.first = None;
            }
            _ => {}
        }
    }
    fn snapshot(&mut self, now: Instant) -> Value {
        while self
            .recent
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at).as_secs_f64() >= 60.0)
        {
            self.recent.pop_front();
        }
        json!({"version":1,"running":self.running,"rounds":self.rounds,"steps":self.steps,
            "errors":self.errors,"input":self.input,"output":self.output,"cacheRead":self.cache_read,
            "cacheWrite":self.cache_write,"cost":self.cost,"ttft":self.ttft,"latency":self.latency,
            "speed":self.speed,"tpm":self.recent.iter().map(|(_,tokens)|tokens).sum::<u64>(),
            "waiting":self.request.map(|start|now.duration_since(start).as_secs_f64())})
    }
}
fn metric(v: &Value, key: &str, precision: usize, suffix: &str) -> String {
    v[key]
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0)
        .map(|n| format!("{n:.precision$}{suffix}"))
        .unwrap_or_else(|| "—".into())
}
fn valid_anchor(anchor: &str) -> bool {
    matches!(
        anchor,
        "top-left"
            | "top-center"
            | "top-right"
            | "left-center"
            | "center"
            | "right-center"
            | "bottom-left"
            | "bottom-center"
            | "bottom-right"
    )
}
fn panel(snapshot: &Value, anchor: &str) -> Value {
    let status = if snapshot["running"] == true {
        "运行中"
    } else {
        "就绪"
    };
    json!({"version":1,"anchor":anchor,"width":44,"maxHeight":9,"minScreenWidth":50,
    "title":format!("监控 · {status}          /stats off"),"lines":[
        format!("{} 轮  {} 步  {} tok/s",count(snapshot,"rounds"),count(snapshot,"steps"),metric(snapshot,"speed",0,"")),
        format!("首 token {}  耗时 {}",metric(snapshot,"ttft",2,"s"),metric(snapshot,"latency",2,"s")),
        format!("TPM {}   等待 {}",count(snapshot,"tpm"),metric(snapshot,"waiting",2,"s")),
        format!("输入 {}  输出 {}",count(snapshot,"input"),count(snapshot,"output")),
        format!("缓存读 {}  写 {}",count(snapshot,"cacheRead"),count(snapshot,"cacheWrite")),
        format!("错误/中止 {}  USD {}",count(snapshot,"errors"),metric(snapshot,"cost",4,""))
    ]})
}
fn publish(stats: &mut Stats) {
    let Some(runtime) = RUNTIME.get() else {
        return;
    };
    let value = if stats.enabled {
        panel(&stats.snapshot(Instant::now()), &stats.anchor)
    } else {
        Value::Null
    };
    let args = json!({"key":KEY,"panel":value}).to_string();
    let mut out = StbString::empty();
    (runtime.action)(
        RuntimeActionId::SetStatus as u32,
        StbStringRef::from_str(&args),
        &mut out,
        runtime.data,
    );
    (runtime.free)(out);
}
extern "C" fn event(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if event.tag == EventTag::SessionShutdown {
        if let Some((stop, thread)) = TIMER.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = stop.send(());
            let _ = thread.join();
        }
        let mut stats = state().lock().unwrap_or_else(|p| p.into_inner());
        stats.enabled = false;
        publish(&mut stats);
        return 0;
    }
    let Some(runtime) = RUNTIME.get() else {
        return 0;
    };
    // Free every owning event payload, including ignored user/tool messages.
    let payload = match event.tag {
        EventTag::MessageUpdate | EventTag::MessageEnd => {
            Some(unsafe { event.payload.message.message })
        }
        EventTag::BeforeProviderRequest => Some(unsafe { event.payload.data.data }),
        _ => None,
    };
    let value = payload
        .map(|s| {
            let parsed = serde_json::from_str(&s.to_string_lossy()).unwrap_or(Value::Null);
            (runtime.free)(s);
            parsed
        })
        .unwrap_or(Value::Null);
    let mut stats = state().lock().unwrap_or_else(|p| p.into_inner());
    stats.update(event.tag, &value, Instant::now());
    publish(&mut stats);
    0
}
extern "C" fn command(args: StbStringRef, out: *mut StbString, _: *mut c_void) -> i32 {
    if out.is_null() {
        return 1;
    }
    let mut stats = state().lock().unwrap_or_else(|p| p.into_inner());
    let envelope: Value = serde_json::from_str(unsafe { args.as_str() }).unwrap_or(Value::Null);
    let text = match envelope
        .get("args")
        .and_then(Value::as_str)
        .unwrap_or("help")
        .trim()
    {
        "on" => {
            stats.enabled = true;
            "运行监控已打开"
        }
        "off" => {
            stats.enabled = false;
            "运行监控已关闭（继续统计）"
        }
        "" | "toggle" => {
            stats.enabled = !stats.enabled;
            if stats.enabled {
                "运行监控已打开"
            } else {
                "运行监控已关闭（继续统计）"
            }
        }
        position if position.starts_with("position ") => {
            let anchor = position.trim_start_matches("position ").trim();
            if valid_anchor(anchor) {
                stats.anchor = anchor.into();
                "监控位置已更新"
            } else {
                "位置：top-left|top-center|top-right|left-center|center|right-center|bottom-left|bottom-center|bottom-right"
            }
        }
        _ => "用法：/stats [on|off|toggle|position <位置>]",
    };
    publish(&mut stats);
    unsafe {
        *out = StbString::from_string(json!({"kind":"message","text":text}).to_string());
    }
    0
}
#[no_mangle]
/// # Safety
/// The host must provide a valid ABI-compatible API for the plugin lifetime.
pub unsafe extern "C" fn rpi_plugin_register(api: *const PluginApi) -> i32 {
    unsafe {
        register_entrypoint(api, |api| {
            let _ = RUNTIME.set(Runtime {
                action: api.runtime_action,
                free: api.free_string,
                data: api.user_data,
            });
            let Some(register) = api.register_event_handler else {
                return 1;
            };
            for tag in [
                EventTag::SessionStart,
                EventTag::SessionShutdown,
                EventTag::AgentStart,
                EventTag::AgentEnd,
                EventTag::BeforeProviderRequest,
                EventTag::MessageUpdate,
                EventTag::MessageEnd,
            ] {
                if register(tag, event, std::ptr::null_mut()) != 0 {
                    return 1;
                }
            }
            let Some(register) = api.register_command else {
                return 1;
            };
            if register(
                StbStringRef::from_str("stats"),
                StbStringRef::from_str("Runtime monitor: /stats on|off|toggle|position <anchor>"),
                command,
            ) != 0
            {
                return 1;
            }
            publish(&mut state().lock().unwrap_or_else(|p| p.into_inner()));
            // Keep waiting time and the rolling TPM window fresh even without streaming events.
            let mut timer = TIMER.lock().unwrap_or_else(|p| p.into_inner());
            if timer.is_none() {
                let (stop, receiver) = std::sync::mpsc::channel();
                let thread = std::thread::spawn(move || {
                    while matches!(
                        receiver.recv_timeout(std::time::Duration::from_secs(1)),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                    ) {
                        publish(&mut state().lock().unwrap_or_else(|p| p.into_inner()));
                    }
                });
                *timer = Some((stop, thread));
            }
            0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_accepts_host_envelope_and_rejects_unknown_arguments() {
        for (args, enabled) in [
            ("off", false),
            ("on", true),
            ("toggle", false),
            ("", true),
            ("unknown", true),
            ("position bottom-left", true),
            ("position invalid", true),
        ] {
            let input = json!({"args":args,"command":"stats"}).to_string();
            let mut out = StbString::empty();
            assert_eq!(
                command(
                    StbStringRef::from_str(&input),
                    &mut out,
                    std::ptr::null_mut()
                ),
                0
            );
            assert_eq!(state().lock().unwrap().enabled, enabled);
            if args.starts_with("position ") {
                assert_eq!(state().lock().unwrap().anchor, "bottom-left");
            }
            let response: Value = serde_json::from_str(&out.to_string_lossy()).unwrap();
            assert_eq!(response["kind"], "message");
            if args == "unknown" {
                assert!(response["text"].as_str().unwrap().contains("用法"));
            }
            // The command output uses the SDK boxed-slice allocation contract.
            unsafe {
                drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    out.ptr as *mut u8,
                    out.len,
                )));
            }
        }
    }
    #[test]
    fn counts_requests_and_real_usage_without_counting_tool_messages_or_duplicate_ends() {
        let mut s = Stats::default();
        let now = Instant::now();
        s.update(EventTag::AgentStart, &Value::Null, now);
        s.update(EventTag::BeforeProviderRequest, &Value::Null, now);
        s.update(
            EventTag::MessageUpdate,
            &json!({"assistantMessageEvent":{"type":"start"}}),
            now,
        );
        assert_eq!(s.ttft, None);
        s.update(
            EventTag::MessageUpdate,
            &json!({"assistantMessageEvent":{"type":"thinking_delta","delta":"x"}}),
            now + std::time::Duration::from_secs(1),
        );
        let message = json!({"role":"assistant","usage":{"input":100,"output":252,"cacheRead":20,"cacheWrite":0,"cost":{"total":0.01}}});
        s.update(EventTag::MessageEnd, &json!({"role":"toolResult"}), now);
        s.update(
            EventTag::MessageEnd,
            &message,
            now + std::time::Duration::from_secs(2),
        );
        s.update(
            EventTag::MessageEnd,
            &message,
            now + std::time::Duration::from_secs(2),
        );
        assert_eq!((s.rounds, s.steps, s.input, s.output), (1, 1, 100, 252));
        assert_eq!(s.ttft, Some(1.0));
        assert_eq!(s.speed, Some(126.0));
        assert_eq!(
            s.snapshot(now + std::time::Duration::from_secs(3))["tpm"],
            372
        );
        assert_eq!(
            s.snapshot(now + std::time::Duration::from_secs(62))["tpm"],
            0
        );
    }
    #[test]
    fn session_reset_preserves_off_and_nonstreaming_has_no_invented_ttft() {
        let mut s = Stats::default();
        let now = Instant::now();
        s.enabled = false;
        s.anchor = "bottom-left".into();
        s.update(EventTag::BeforeProviderRequest, &Value::Null, now);
        s.update(
            EventTag::MessageEnd,
            &json!({"role":"assistant","stopReason":"error"}),
            now,
        );
        assert_eq!(s.ttft, None);
        assert_eq!(s.errors, 1);
        s.update(EventTag::SessionStart, &Value::Null, now);
        assert!(!s.enabled);
        assert_eq!(s.anchor, "bottom-left");
        assert_eq!(s.steps, 0);
    }
}
