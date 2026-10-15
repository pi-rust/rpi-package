#![forbid(unsafe_code)]

use rpi_plugin_sdk::{EventTag, PluginApi, RuntimeActionId, StbString, StbStringRef};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
    time::Instant,
};

const KEY: &str = "rpi.run-stats";
// Reuse the SDK's host-context lifetime and thread-safety contract.
type Runtime = PluginApi;
static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static STATE: OnceLock<Mutex<Stats>> = OnceLock::new();
static TIMER: Mutex<Option<(std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>)>> =
    Mutex::new(None);
fn state() -> &'static Mutex<Stats> {
    STATE.get_or_init(|| Mutex::new(Stats::default()))
}

struct Stats {
    last_publication: Option<String>,
    last_visibility: Option<bool>,
    enabled: bool,
    anchor: String,
    running: bool,
    rounds: u64,
    steps: u64,
    tools: u64,
    errors: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost: Option<f64>,
    pricing_available: Option<bool>,
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
            last_publication: None,
            last_visibility: None,
            enabled: false,
            anchor: "top-right".into(),
            running: false,
            rounds: 0,
            steps: 0,
            tools: 0,
            errors: 0,
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cost: None,
            pricing_available: None,
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
                self.pricing_available = v.get("pricingAvailable").and_then(Value::as_bool);
                if self.pricing_available == Some(true) {
                    self.cost.get_or_insert(0.0);
                }
                self.request = Some(now);
                self.first = None;
                self.ttft = None;
                self.latency = None;
                self.speed = None;
            }
            EventTag::ToolExecutionStart => self.tools += 1,
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
                        // Older hosts lack the pricing flag: only a positive
                        // reported cost proves that their model has pricing.
                        if self.pricing_available.unwrap_or(cost > 0.0) {
                            self.cost = Some(self.cost.unwrap_or(0.0) + cost);
                            self.pricing_available = Some(true);
                        }
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
        json!({"version":1,"running":self.running,"rounds":self.rounds,"steps":self.steps,"tools":self.tools,
            "errors":self.errors,"input":self.input,"output":self.output,"cacheRead":self.cache_read,
            "cacheWrite":self.cache_write,"cost":self.cost,"ttft":self.ttft,"latency":self.latency,
            "pricingAvailable":self.pricing_available.unwrap_or(false),
            "speed":self.speed,"tpm":self.recent.iter().map(|(_,tokens)|tokens).sum::<u64>(),
            "waiting":self.request.map(|start|now.duration_since(start).as_secs_f64())})
    }
}
fn metric(v: &Value, key: &str, precision: usize, suffix: &str, width: usize) -> String {
    let text = v[key]
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0)
        .map(|n| {
            let text = format!("{n:.precision$}{suffix}");
            if text.chars().count() <= width {
                text
            } else {
                // Keep unusually large values from shifting every later column.
                format!("{n:.0e}{suffix}")
            }
        })
        .unwrap_or_else(|| "—".into());
    format!("{text:>width$}")
}
fn panel(snapshot: &Value, _anchor: &str) -> Value {
    let cost = if snapshot["pricingAvailable"].as_bool() == Some(true) {
        format!(" · USD {}", metric(snapshot, "cost", 4, "", 9))
    } else {
        String::new()
    };
    json!({"version":1,"layout":"footer","width":240,"maxHeight":1,"border":false,
        // All values (including missing data) occupy stable terminal columns.
        "lines":[format!("Turns {:>6} · LLM Calls {:>6} · Tools {:>6}{} · TPS {} · TTFT {} · Latency {} · TPM {:>6} · Err {:>6}",
            compact(count(snapshot,"rounds")),compact(count(snapshot,"steps")),
            compact(count(snapshot,"tools")),cost,
            metric(snapshot,"speed",0,"",6),metric(snapshot,"ttft",2,"s",7),
            metric(snapshot,"latency",2,"s",7),compact(count(snapshot,"tpm")),
            compact(count(snapshot,"errors")))]})
}
fn compact(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    let mut value = n as f64 / 1_000.0;
    let units = ["k", "M", "G", "T", "P", "E"];
    let mut unit = 0;
    // Promote before rounding would produce a seven-column value like 1000.0k.
    while value >= 999.95 && unit + 1 < units.len() {
        value /= 1_000.0;
        unit += 1;
    }
    format!("{value:.1}{}", units[unit])
}
fn component_action(runtime: &rpi_plugin_sdk::PluginApi, args: Value) -> bool {
    let args = args.to_string();
    let mut out = StbString::empty();
    let rc = (runtime.runtime_action)(
        RuntimeActionId::UiComponent as u32,
        StbStringRef::from_str(&args),
        &mut out,
        runtime.user_data,
    );
    (runtime.free_string)(out);
    rc == 0
}
fn publish(stats: &mut Stats) {
    let Some(runtime) = RUNTIME.get() else {
        return;
    };
    if stats.last_publication.is_none() {
        if !component_action(
            runtime,
            json!({"op":"register","id":KEY,"type":"text","slot":"footer.after","width":240,"maxHeight":1,"border":false,"visible":false}),
        ) {
            return;
        }
        stats.last_visibility = Some(false);
    }
    let value = panel(&stats.snapshot(Instant::now()), &stats.anchor);
    let args = json!({"op":"update","id":KEY,"text":value["lines"][0]}).to_string();
    if stats.last_visibility != Some(stats.enabled)
        && component_action(
            runtime,
            json!({"op":if stats.enabled {"show"} else {"hide"},"id":KEY}),
        )
    {
        stats.last_visibility = Some(stats.enabled);
    }
    if stats.last_publication.as_deref() == Some(args.as_str()) {
        return;
    }
    let mut out = StbString::empty();
    let rc = (runtime.runtime_action)(
        RuntimeActionId::UiComponent as u32,
        StbStringRef::from_str(&args),
        &mut out,
        runtime.user_data,
    );
    (runtime.free_string)(out);
    if rc == 0 {
        stats.last_publication = Some(args);
    }
}
rpi_plugin_sdk::event_handler! {
fn event(event, _user_data) {
    if event.tag() == EventTag::SessionShutdown {
        if let Some((stop, thread)) = TIMER.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = stop.send(());
            let _ = thread.join();
        }
        let mut stats = state().lock().unwrap_or_else(|p| p.into_inner());
        stats.enabled = false;
        publish(&mut stats);
        return 0;
    }
    let Some(_runtime) = RUNTIME.get() else {
        return 0;
    };
    // Events are borrowed during host fan-out; the host frees their payloads
    // once all subscribers have returned (dispatch_to_handlers contract).
    let payload = match event.tag() {
        EventTag::MessageUpdate | EventTag::MessageEnd => {
            event.message_text()
        }
        EventTag::BeforeProviderRequest => event.data_text(),
        _ => None,
    };
    let value = payload
        .map(|s| {
            let parsed = serde_json::from_str(&s).unwrap_or(Value::Null);
            parsed
        })
        .unwrap_or(Value::Null);
    let mut stats = state().lock().unwrap_or_else(|p| p.into_inner());
    stats.update(event.tag(), &value, Instant::now());
    // Stream deltas only latch TTFT. The one-second timer publishes ongoing
    // timing; request/lifecycle boundaries still publish immediately.
    if event.tag() != EventTag::MessageUpdate {
        publish(&mut stats);
    }
    0
}
}
fn shortcut(stats: &mut Stats, value: &Value) -> i32 {
    use rpi_plugin_sdk::{EVENT_HANDLER_CLAIMED, EVENT_HANDLER_CONTINUE};
    if value["type"] != "key"
        || value["key"] != "f8"
        || ["ctrl", "alt", "shift"]
            .iter()
            .any(|key| value[*key].as_bool().unwrap_or(false))
    {
        return EVENT_HANDLER_CONTINUE;
    }
    match value["kind"].as_str() {
        Some("press") => {
            stats.enabled = !stats.enabled;
            EVENT_HANDLER_CLAIMED
        }
        Some("repeat" | "release") => EVENT_HANDLER_CLAIMED,
        _ => EVENT_HANDLER_CONTINUE,
    }
}
rpi_plugin_sdk::event_handler! {
fn input(event, _user_data) {
    let raw = event.data_text().unwrap_or_default();
    let value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let mut stats = state().lock().unwrap_or_else(|p| p.into_inner());
    let enabled = stats.enabled;
    let result = shortcut(&mut stats, &value);
    if stats.enabled != enabled {
        publish(&mut stats);
    }
    result
}
}
rpi_plugin_sdk::string_handler! {
fn command(args, out, _user_data) {
    if !out.is_available() {
        return 1;
    }
    let mut stats = state().lock().unwrap_or_else(|p| p.into_inner());
    let envelope: Value = serde_json::from_str(args).unwrap_or(Value::Null);
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
            "监控固定显示在用量统计下方，无需设置位置。"
        }
        _ => "用法：/stats [on|off|toggle]，也可使用 F8 切换",
    };
    publish(&mut stats);
    out.write(json!({"kind":"message","text":text}).to_string());
    0
}
}
rpi_plugin_sdk::export_plugin!(|api| {
    let _ = RUNTIME.set(*api);
    let Some(register) = api.register_event_handler else {
        return 1;
    };
    for tag in [
        EventTag::SessionStart,
        EventTag::SessionShutdown,
        EventTag::AgentStart,
        EventTag::AgentEnd,
        EventTag::BeforeProviderRequest,
        EventTag::ToolExecutionStart,
        EventTag::MessageUpdate,
        EventTag::MessageEnd,
    ] {
        if register(tag, event, std::ptr::null_mut()) != 0 {
            return 1;
        }
    }
    if register(EventTag::Input, input, std::ptr::null_mut()) != 0 {
        return 1;
    }
    let Some(register_shortcut) = api.register_shortcut else {
        return 1;
    };
    if register_shortcut(
        StbStringRef::from_str("f8"),
        StbStringRef::from_str("Toggle runtime monitor (F8)"),
    ) != 0
    {
        return 1;
    }
    let Some(register) = api.register_command else {
        return 1;
    };
    if register(
        StbStringRef::from_str("stats"),
        StbStringRef::from_str("Footer runtime monitor: /stats on|off|toggle (F8)"),
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
});

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn f8_toggles_once_and_modified_or_unrelated_keys_are_not_claimed() {
        use rpi_plugin_sdk::{EVENT_HANDLER_CLAIMED, EVENT_HANDLER_CONTINUE};
        let mut stats = Stats::default();
        for kind in ["press", "repeat", "release"] {
            assert_eq!(
                shortcut(&mut stats, &json!({"type":"key","key":"f8","kind":kind})),
                EVENT_HANDLER_CLAIMED
            );
            assert!(stats.enabled);
        }
        for value in [
            json!({"type":"key","key":"f8","kind":"press","ctrl":true}),
            json!({"type":"key","key":"f7","kind":"press"}),
            json!({"type":"key","key":"f8","kind":"invalid"}),
        ] {
            assert_eq!(shortcut(&mut stats, &value), EVENT_HANDLER_CONTINUE);
            assert!(stats.enabled);
        }
        assert_eq!(
            shortcut(&mut stats, &json!({"type":"key","key":"f8","kind":"press"})),
            EVENT_HANDLER_CLAIMED
        );
        assert!(!stats.enabled);
    }
    #[test]
    fn monitor_defaults_to_off_and_uses_compact_docked_metrics() {
        let mut stats = Stats::default();
        assert!(!stats.enabled);
        let view = panel(&stats.snapshot(Instant::now()), "top-right");
        assert_eq!(view["layout"], "footer");
        assert_eq!(view["maxHeight"], 1);
        assert_eq!(view["lines"].as_array().unwrap().len(), 1);
        assert!(view["lines"][0].as_str().unwrap().contains("TTFT"));
    }
    #[test]
    fn monitor_columns_stay_fixed_for_missing_values_digit_changes_and_large_usage() {
        let layout = |snapshot: &Value| {
            let view = panel(snapshot, "top-right");
            let line = view["lines"][0].as_str().unwrap();
            let positions = [
                "Turns",
                "LLM Calls",
                "Tools",
                "TPS",
                "TTFT",
                "Latency",
                "TPM",
                "Err",
            ]
            .map(|label| line[..line.find(label).unwrap()].chars().count());
            (positions, line.chars().count())
        };
        for priced in [false, true] {
            let expected = layout(&json!({"pricingAvailable":priced}));
            for n in [0, 9, 10, 999, 1_000, 999_949, 999_950, 1_000_000, u64::MAX] {
                assert_eq!(
                    layout(&json!({"rounds":n,"steps":n,"tools":n,"speed":n,"ttft":n,
                    "latency":n,"tpm":n,"errors":n,"cost":n,"pricingAvailable":priced})),
                    expected,
                    "column positions changed for {n}"
                );
            }
            assert_eq!(
                layout(&json!({"speed":f64::MAX,"ttft":f64::MAX,
                "latency":f64::MAX,"cost":f64::MAX,"pricingAvailable":priced})),
                expected
            );
        }
        assert_eq!(metric(&json!({"ttft":1.25}), "ttft", 2, "s", 7), "  1.25s");
        assert_eq!(metric(&json!({}), "ttft", 2, "s", 7), "      —");
        assert_eq!(compact(999_949), "999.9k");
        assert_eq!(compact(999_950), "1.0M");
        assert_eq!(compact(u64::MAX), "18.4E");
    }
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
                assert_eq!(state().lock().unwrap().anchor, "top-right");
            }
            let response: Value = serde_json::from_str(&out.to_string_lossy()).unwrap();
            assert_eq!(response["kind"], "message");
            if args == "unknown" {
                assert!(response["text"].as_str().unwrap().contains("用法"));
            }
            // The command output uses the SDK boxed-slice allocation contract.
            rpi_plugin_sdk::callbacks::free_string(out);
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
        assert_eq!(s.cost, Some(0.01));
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
    fn tool_executions_count_once_per_start_including_failures_and_reset_with_the_session() {
        let mut stats = Stats::default();
        let now = Instant::now();
        stats.update(EventTag::AgentStart, &Value::Null, now);
        stats.update(EventTag::BeforeProviderRequest, &Value::Null, now);
        for id in ["a", "b"] {
            stats.update(EventTag::ToolExecutionStart, &json!({"toolCallId":id}), now);
        }
        for id in ["a", "b"] {
            stats.update(
                EventTag::ToolExecutionEnd,
                &json!({"toolCallId":id,"isError":true}),
                now,
            );
        }
        let snapshot = stats.snapshot(now);
        assert_eq!((stats.rounds, stats.steps, stats.tools), (1, 1, 2));
        assert_eq!(snapshot["tools"], 2);
        let view = panel(&snapshot, "top-right");
        let line = view["lines"][0].as_str().unwrap();
        assert!(line.contains("LLM Calls      1 · Tools      2"));
        stats.update(EventTag::SessionStart, &Value::Null, now);
        assert_eq!(stats.snapshot(now)["tools"], 0);
    }
    #[test]
    fn reported_model_costs_accumulate_without_double_counting_terminal_events() {
        let mut stats = Stats::default();
        let now = Instant::now();
        assert!(stats.snapshot(now)["cost"].is_null());
        for cost in [Some(0.0123), None, Some(0.0), Some(-1.0), Some(0.01)] {
            stats.update(EventTag::BeforeProviderRequest, &Value::Null, now);
            let message = json!({"role":"assistant", "usage":{"output":5,"cost":{"total":cost}}});
            stats.update(EventTag::MessageEnd, &message, now);
            stats.update(EventTag::MessageEnd, &message, now);
        }
        assert!((stats.cost.unwrap() - 0.0223).abs() < 1e-12);
        let view = panel(&stats.snapshot(now), "top-right");
        let line = view["lines"][0].as_str().unwrap();
        assert!(line.contains("USD    0.0223"));
        // Keep costs visible even when a narrow terminal clips later metrics.
        assert!(line[..line.find("0.0223").unwrap()].chars().count() + 6 <= 80);
        stats.update(EventTag::SessionStart, &Value::Null, now);
        assert!(stats.snapshot(now)["cost"].is_null());
    }
    #[test]
    fn usd_is_hidden_without_pricing_and_tracks_model_switches_including_free_models() {
        let mut stats = Stats::default();
        let now = Instant::now();
        let shows_usd = |stats: &mut Stats| {
            let view = panel(&stats.snapshot(now), "top-right");
            view["lines"][0].as_str().unwrap().contains("USD")
        };
        assert!(!shows_usd(&mut stats));
        // Default zero usage from an older host is not evidence of pricing.
        stats.update(EventTag::BeforeProviderRequest, &Value::Null, now);
        stats.update(
            EventTag::MessageEnd,
            &json!({"role":"assistant","usage":{"cost":{"total":0}}}),
            now,
        );
        assert!(!shows_usd(&mut stats));
        assert_eq!(stats.cost, None);
        // Explicit zero rates are configured prices and remain visible.
        stats.update(
            EventTag::BeforeProviderRequest,
            &json!({"pricingAvailable":true}),
            now,
        );
        assert!(shows_usd(&mut stats));
        assert_eq!(stats.cost, Some(0.0));
        stats.update(
            EventTag::MessageEnd,
            &json!({"role":"assistant","usage":{"cost":{"total":0}}}),
            now,
        );
        assert!(shows_usd(&mut stats));
        stats.update(
            EventTag::BeforeProviderRequest,
            &json!({"pricingAvailable":true}),
            now,
        );
        stats.update(
            EventTag::MessageEnd,
            &json!({"role":"assistant","usage":{"cost":{"total":0.0123}}}),
            now,
        );
        // Previous spending must not leave USD visible on an unpriced model.
        stats.update(
            EventTag::BeforeProviderRequest,
            &json!({"pricingAvailable":false}),
            now,
        );
        assert!(!shows_usd(&mut stats));
        stats.update(
            EventTag::MessageEnd,
            &json!({"role":"assistant","usage":{"cost":{"total":0}}}),
            now,
        );
        assert!(!shows_usd(&mut stats));
        stats.update(
            EventTag::BeforeProviderRequest,
            &json!({"pricingAvailable":true}),
            now,
        );
        assert!(shows_usd(&mut stats));
        assert_eq!(stats.cost, Some(0.0123));
        stats.update(EventTag::SessionStart, &Value::Null, now);
        assert!(!shows_usd(&mut stats));
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
