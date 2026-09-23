use rpi_plugin_sdk::{
    register_entrypoint, EventTag, EventHandlerFn, FreeStringFn, PluginApiVt,
    StablePluginEvent, StableToolSchema, StbString, StbStringRef, StepHandle, StepResult,
    ToolPartialCb,
};
use serde_json::{json, Map, Value};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

const DEFAULT_BASE_URL: &str = "https://cloud.langfuse.com";
const FLUSH_INTERVAL_SECS: u64 = 10;
const BATCH_MAX_SIZE: usize = 50;

#[derive(Debug, Clone)]
struct LangfuseConfig {
    base_url: String,
    public_key: String,
    secret_key: String,
}

impl LangfuseConfig {
    fn is_valid(&self) -> bool {
        !self.public_key.is_empty() && !self.secret_key.is_empty()
    }
}

static CONFIG_CACHE: OnceLock<Mutex<Option<LangfuseConfig>>> = OnceLock::new();

fn config_cache() -> &'static Mutex<Option<LangfuseConfig>> {
    CONFIG_CACHE.get_or_init(|| Mutex::new(None))
}

fn load_config() -> LangfuseConfig {
    // Check cache first
    if let Ok(cache) = config_cache().lock() {
        if let Some(ref cfg) = *cache {
            return cfg.clone();
        }
    }

    // Try to load from config file
    let file_config = load_config_file().unwrap_or_default();

    // Merge: env vars override file config
    let cfg = LangfuseConfig {
        base_url: std::env::var("LANGFUSE_BASE_URL")
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .or(file_config.base_url)
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
        public_key: std::env::var("LANGFUSE_PUBLIC_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or(file_config.public_key)
            .unwrap_or_default(),
        secret_key: std::env::var("LANGFUSE_SECRET_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or(file_config.secret_key)
            .unwrap_or_default(),
    };

    // Cache the result
    if let Ok(mut cache) = config_cache().lock() {
        *cache = Some(cfg.clone());
    }

    cfg
}

#[derive(Default)]
struct FileConfig {
    base_url: Option<String>,
    public_key: Option<String>,
    secret_key: Option<String>,
}

fn load_config_file() -> Result<FileConfig, String> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(FileConfig::default());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("read config {}: {e}", path.display()))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| format!("parse config {}: {e}", path.display()))?;

    Ok(FileConfig {
        base_url: value.get("baseUrl").and_then(Value::as_str).map(String::from),
        public_key: resolve_value_or_env(&value, "publicKey", "publicKeyEnv")?,
        secret_key: resolve_value_or_env(&value, "secretKey", "secretKeyEnv")?,
    })
}

fn resolve_value_or_env(
    value: &Value,
    value_key: &str,
    env_key: &str,
) -> Result<Option<String>, String> {
    // Direct value takes precedence
    if let Some(v) = value.get(value_key).and_then(Value::as_str) {
        if !v.trim().is_empty() {
            return Ok(Some(v.to_string()));
        }
    }
    // Environment variable reference
    if let Some(env_name) = value.get(env_key).and_then(Value::as_str) {
        if !env_name.trim().is_empty() {
            return std::env::var(env_name)
                .map(Some)
                .map_err(|_| format!("environment variable {env_name} is not set"));
        }
    }
    Ok(None)
}

fn config_path() -> Result<std::path::PathBuf, String> {
    // Check env override
    if let Some(path) = std::env::var_os("RPI_LANGFUSE_CONFIG") {
        let path = std::path::PathBuf::from(path);
        if !path.is_absolute() {
            return Err("RPI_LANGFUSE_CONFIG must be an absolute path".into());
        }
        return Ok(path);
    }
    // Project-local config
    let project = std::env::current_dir()
        .map_err(|e| format!("resolve current directory: {e}"))?
        .join(".rpi")
        .join("langfuse.json");
    if project.is_file() {
        return Ok(project);
    }
    // Home directory fallback
    home_dir()
        .map(|home| home.join(".rpi").join("agent").join("langfuse.json"))
        .ok_or_else(|| "cannot resolve home directory for ~/.rpi/agent/langfuse.json".into())
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}

fn is_enabled() -> bool {
    load_config().is_valid()
}

fn now_iso() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // ISO 8601 with millis
    let secs = d.as_secs();
    let millis = d.subsec_millis();
    // Rough UTC formatting (good enough for Langfuse timestamps)
    let days = secs / 86400;
    let (y, m, d) = days_to_ymd(days);
    let h = (secs % 86400) / 3600;
    let min = (secs % 3600) / 60;
    let s = secs % 60;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, m, d, h, min, s, millis
    )
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Civil calendar from days since 1970-01-01 (Howard Hinnant algorithm)
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn trace_id() -> String {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let pid = std::process::id();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("rpi-{:08x}-{:04x}", pid, seq & 0xFFFF)
}

fn obs_id() -> String {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("obs-{:016x}", seq)
}

// ---------------------------------------------------------------------------
// Ingestion batch queue
// ---------------------------------------------------------------------------

struct IngestionBatch {
    events: Vec<Value>,
    last_flush: Instant,
}

impl IngestionBatch {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            last_flush: Instant::now(),
        }
    }

    fn push(&mut self, event: Value) {
        self.events.push(event);
    }

    fn should_flush(&self) -> bool {
        self.events.len() >= BATCH_MAX_SIZE
            || self.last_flush.elapsed() >= Duration::from_secs(FLUSH_INTERVAL_SECS)
    }

    fn take(&mut self) -> Vec<Value> {
        self.last_flush = Instant::now();
        std::mem::take(&mut self.events)
    }
}

struct TracerState {
    batch: Mutex<IngestionBatch>,
    current_trace_id: Mutex<Option<String>>,
    // Track active generation spans: key → obs_id
    active_generations: Mutex<Vec<(String, String)>>,
    // Track active tool spans: tool_call_id → obs_id
    active_tools: Mutex<Vec<(String, String)>>,
}

static TRACER: OnceLock<Arc<TracerState>> = OnceLock::new();

fn tracer() -> Arc<TracerState> {
    TRACER
        .get_or_init(|| {
            Arc::new(TracerState {
                batch: Mutex::new(IngestionBatch::new()),
                current_trace_id: Mutex::new(None),
                active_generations: Mutex::new(Vec::new()),
                active_tools: Mutex::new(Vec::new()),
            })
        })
        .clone()
}

// ---------------------------------------------------------------------------
// HTTP flush
// ---------------------------------------------------------------------------

fn flush_batch(events: Vec<Value>) -> Result<(), String> {
    if events.is_empty() {
        return Ok(());
    }
    let cfg = load_config();
    if !cfg.is_valid() {
        return Err("langfuse config not valid".into());
    }
    let body = json!({ "batch": events });
    let url = format!("{}/api/public/ingestion", cfg.base_url);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(&url)
        .basic_auth(&cfg.public_key, Some(&cfg.secret_key))
        .json(&body)
        .send()
        .map_err(|e| format!("langfuse flush failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().unwrap_or_default();
        return Err(format!("langfuse ingestion error {status}: {text}"));
    }
    Ok(())
}

fn maybe_flush(state: &TracerState) {
    let mut batch = state.batch.lock().unwrap();
    if batch.should_flush() {
        let events = batch.take();
        drop(batch);
        // Fire-and-forget flush on a thread so we don't block the agent
        std::thread::spawn(move || {
            if let Err(e) = flush_batch(events) {
                eprintln!("[rpi-langfuse] flush error: {e}");
            }
        });
    }
}

fn force_flush(state: &TracerState) {
    let events = state.batch.lock().unwrap().take();
    if !events.is_empty() {
        if let Err(e) = flush_batch(events) {
            eprintln!("[rpi-langfuse] flush error: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Event handlers — auto-tracing
// ---------------------------------------------------------------------------

extern "C" fn on_session_start(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let state = tracer();
    let tid = trace_id();
    let now = now_iso();
    
    // Build trace body with optional metadata
    let mut trace_body = json!({
        "id": &tid,
        "name": "rpi-session",
        "timestamp": &now,
    });
    
    // Add userId from environment if available
    if let Ok(user_id) = std::env::var("RPI_USER_ID") {
        if !user_id.is_empty() {
            trace_body["userId"] = json!(user_id);
        }
    }
    
    // Add sessionId from environment if available
    if let Ok(session_id) = std::env::var("RPI_SESSION_ID") {
        if !session_id.is_empty() {
            trace_body["sessionId"] = json!(session_id);
        }
    }
    
    // Add basic metadata
    trace_body["metadata"] = json!({
        "sdkVersion": "rpi-langfuse/0.1",
        "platform": std::env::consts::OS,
    });
    
    let trace_event = json!({
        "id": obs_id(),
        "type": "trace-create",
        "body": trace_body,
        "metadata": {}
    });
    state.batch.lock().unwrap().push(trace_event);
    *state.current_trace_id.lock().unwrap() = Some(tid);
    0
}

extern "C" fn on_before_provider_request(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let state = tracer();
    let trace_id = state
        .current_trace_id
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default();
    if trace_id.is_empty() {
        return 0;
    }
    // Parse the data payload to extract model info
    let data_str = unsafe { event.payload.data.data.to_string_lossy() };
    unsafe { host_free(event.payload.data.data); }
    let data: Value = serde_json::from_str(&data_str).unwrap_or(Value::Null);
    let model = data
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown");

    let obs = obs_id();
    let now = now_iso();
    
    // Build generation body with model parameters
    let mut gen_body = json!({
        "id": &obs,
        "traceId": &trace_id,
        "name": "llm-request",
        "model": model,
        "startTime": &now,
        "input": data.get("messages").cloned().unwrap_or(Value::Null),
    });
    
    // Extract and add model parameters (temperature, max_tokens, etc.)
    if let Some(params) = data.get("parameters") {
        let mut model_params = Map::new();
        if let Some(temp) = params.get("temperature") {
            model_params.insert("temperature".into(), temp.clone());
        }
        if let Some(max_tokens) = params.get("max_tokens").or_else(|| params.get("maxTokens")) {
            model_params.insert("max_tokens".into(), max_tokens.clone());
        }
        if let Some(top_p) = params.get("top_p").or_else(|| params.get("topP")) {
            model_params.insert("top_p".into(), top_p.clone());
        }
        if let Some(freq_penalty) = params.get("frequency_penalty").or_else(|| params.get("frequencyPenalty")) {
            model_params.insert("frequency_penalty".into(), freq_penalty.clone());
        }
        if let Some(pres_penalty) = params.get("presence_penalty").or_else(|| params.get("presencePenalty")) {
            model_params.insert("presence_penalty".into(), pres_penalty.clone());
        }
        if !model_params.is_empty() {
            gen_body["modelParameters"] = Value::Object(model_params);
        }
    }
    
    let gen_event = json!({
        "id": obs_id(),
        "type": "generation-create",
        "body": gen_body,
        "metadata": {}
    });
    state.batch.lock().unwrap().push(gen_event);
    // Track this generation so we can close it on AfterProviderResponse
    state
        .active_generations
        .lock()
        .unwrap()
        .push(("provider".into(), obs));
    maybe_flush(&state);
    0
}

extern "C" fn on_after_provider_response(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let state = tracer();
    let gen = state
        .active_generations
        .lock()
        .unwrap()
        .pop();
    let Some((_key, obs_id_val)) = gen else {
        return 0;
    };
    let data_str = unsafe { event.payload.data.data.to_string_lossy() };
    unsafe { host_free(event.payload.data.data); }
    let data: Value = serde_json::from_str(&data_str).unwrap_or(Value::Null);
    let now = now_iso();

    // Extract usage from response
    let usage = data.get("usage").cloned().unwrap_or(Value::Null);
    let input_tokens = usage
        .get("input_tokens")
        .or_else(|| usage.get("inputTokens"))
        .or_else(|| usage.get("promptTokens"))
        .and_then(Value::as_u64);
    let output_tokens = usage
        .get("output_tokens")
        .or_else(|| usage.get("outputTokens"))
        .or_else(|| usage.get("completionTokens"))
        .and_then(Value::as_u64);

    let mut end_body = json!({
        "id": &obs_id_val,
        "endTime": &now,
        "output": data.get("content").cloned().unwrap_or(Value::Null),
    });
    if let Some(inp) = input_tokens {
        end_body["usage"] = json!({
            "input": inp,
            "output": output_tokens.unwrap_or(0),
            "total": inp + output_tokens.unwrap_or(0),
            "unit": "TOKENS",
        });
    }
    if let Some(model) = data.get("model").and_then(Value::as_str) {
        end_body["model"] = json!(model);
    }
    // Extract completionStartTime for TTFT (time to first token)
    if let Some(start) = data.get("startTime").and_then(Value::as_str) {
        end_body["completionStartTime"] = json!(start);
    }

    let end_event = json!({
        "id": obs_id(),
        "type": "generation-update",
        "body": end_body,
        "metadata": {}
    });
    state.batch.lock().unwrap().push(end_event);
    maybe_flush(&state);
    0
}

extern "C" fn on_tool_call(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let state = tracer();
    let trace_id = state
        .current_trace_id
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default();
    if trace_id.is_empty() {
        return 0;
    }
    let tool_call_id = unsafe { event.payload.tool_call.tool_call_id.to_string_lossy() };
    let tool_name = unsafe { event.payload.tool_call.tool_name.to_string_lossy() };
    let params = unsafe { event.payload.tool_call.params.to_string_lossy() };
    unsafe {
        host_free(event.payload.tool_call.tool_call_id);
        host_free(event.payload.tool_call.tool_name);
        host_free(event.payload.tool_call.params);
    }

    let obs = obs_id();
    let now = now_iso();
    let span_event = json!({
        "id": obs_id(),
        "type": "span-create",
        "body": {
            "id": &obs,
            "traceId": &trace_id,
            "name": tool_name,
            "startTime": &now,
            "input": serde_json::from_str::<Value>(&params).unwrap_or(Value::Null),
        },
        "metadata": {}
    });
    state.batch.lock().unwrap().push(span_event);
    state
        .active_tools
        .lock()
        .unwrap()
        .push((tool_call_id.to_string(), obs));
    0
}

extern "C" fn on_tool_result(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let state = tracer();
    let tool_call_id = unsafe { event.payload.tool_result.tool_call_id.to_string_lossy() };
    let result = unsafe { event.payload.tool_result.result.to_string_lossy() };
    let is_error = unsafe { event.payload.tool_result.is_error } != 0;
    unsafe {
        host_free(event.payload.tool_result.tool_call_id);
        host_free(event.payload.tool_result.tool_name);
        host_free(event.payload.tool_result.result);
    }

    // Find matching span
    let obs_id_val = {
        let mut tools = state.active_tools.lock().unwrap();
        if let Some(pos) = tools.iter().position(|(id, _)| *id == tool_call_id) {
            let (_, obs) = tools.remove(pos);
            Some(obs)
        } else {
            None
        }
    };
    let Some(obs) = obs_id_val else {
        return 0;
    };

    let now = now_iso();
    let mut end_body = json!({
        "id": &obs,
        "endTime": &now,
    });
    if is_error {
        end_body["level"] = json!("ERROR");
        end_body["statusMessage"] = json!(result);
    } else {
        // Truncate long results for Langfuse
        let truncated: String = result.chars().take(4000).collect();
        end_body["output"] = json!(truncated);
    }

    let end_event = json!({
        "id": obs_id(),
        "type": "span-update",
        "body": end_body,
        "metadata": {}
    });
    state.batch.lock().unwrap().push(end_event);
    maybe_flush(&state);
    0
}

extern "C" fn on_turn_start(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let state = tracer();
    let trace_id = state
        .current_trace_id
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default();
    if trace_id.is_empty() {
        return 0;
    }
    let obs = obs_id();
    let now = now_iso();
    let span_event = json!({
        "id": obs_id(),
        "type": "span-create",
        "body": {
            "id": &obs,
            "traceId": &trace_id,
            "name": "agent-turn",
            "startTime": &now,
        },
        "metadata": {}
    });
    state.batch.lock().unwrap().push(span_event);
    state
        .active_generations
        .lock()
        .unwrap()
        .push(("turn".into(), obs));
    0
}

extern "C" fn on_turn_end(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let state = tracer();
    // Fix deadlock: hold lock for the entire operation
    let obs = {
        let mut gens = state.active_generations.lock().unwrap();
        if let Some(pos) = gens.iter().position(|(k, _)| k == "turn") {
            Some(gens.remove(pos).1)
        } else {
            None
        }
    };
    let Some(obs) = obs else {
        return 0;
    };
    let now = now_iso();
    let end_event = json!({
        "id": obs_id(),
        "type": "span-update",
        "body": {
            "id": &obs,
            "endTime": &now,
        },
        "metadata": {}
    });
    state.batch.lock().unwrap().push(end_event);
    maybe_flush(&state);
    0
}

extern "C" fn on_session_shutdown(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let state = tracer();
    // Force flush remaining events
    force_flush(&state);
    // Clear trace
    *state.current_trace_id.lock().unwrap() = None;
    0
}

extern "C" fn on_agent_start(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let state = tracer();
    let trace_id = state
        .current_trace_id
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default();
    if trace_id.is_empty() {
        return 0;
    }
    let obs = obs_id();
    let now = now_iso();
    let span_event = json!({
        "id": obs_id(),
        "type": "span-create",
        "body": {
            "id": &obs,
            "traceId": &trace_id,
            "name": "agent-start",
            "startTime": &now,
        },
        "metadata": {}
    });
    state.batch.lock().unwrap().push(span_event);
    state
        .active_generations
        .lock()
        .unwrap()
        .push(("agent".into(), obs));
    0
}

extern "C" fn on_agent_end(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let state = tracer();
    let obs = {
        let mut gens = state.active_generations.lock().unwrap();
        if let Some(pos) = gens.iter().position(|(k, _)| k == "agent") {
            Some(gens.remove(pos).1)
        } else {
            None
        }
    };
    let Some(obs) = obs else {
        return 0;
    };
    let now = now_iso();
    let end_event = json!({
        "id": obs_id(),
        "type": "span-update",
        "body": {
            "id": &obs,
            "endTime": &now,
        },
        "metadata": {}
    });
    state.batch.lock().unwrap().push(end_event);
    maybe_flush(&state);
    0
}

extern "C" fn on_session_before_compact(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let state = tracer();
    let trace_id = state
        .current_trace_id
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default();
    if trace_id.is_empty() {
        return 0;
    }
    let obs = obs_id();
    let now = now_iso();
    let span_event = json!({
        "id": obs_id(),
        "type": "span-create",
        "body": {
            "id": &obs,
            "traceId": &trace_id,
            "name": "session-compact",
            "startTime": &now,
        },
        "metadata": {}
    });
    state.batch.lock().unwrap().push(span_event);
    state
        .active_generations
        .lock()
        .unwrap()
        .push(("compact".into(), obs));
    0
}

extern "C" fn on_session_compact(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let state = tracer();
    let obs = {
        let mut gens = state.active_generations.lock().unwrap();
        if let Some(pos) = gens.iter().position(|(k, _)| k == "compact") {
            Some(gens.remove(pos).1)
        } else {
            None
        }
    };
    let Some(obs) = obs else {
        return 0;
    };
    let now = now_iso();
    let end_event = json!({
        "id": obs_id(),
        "type": "span-update",
        "body": {
            "id": &obs,
            "endTime": &now,
        },
        "metadata": {}
    });
    state.batch.lock().unwrap().push(end_event);
    maybe_flush(&state);
    0
}

// ---------------------------------------------------------------------------
// Manual tools (score + prompt management)
// ---------------------------------------------------------------------------

type Builder = fn(&Value) -> Result<String, String>;

struct Drive {
    params: Value,
    builder: Builder,
    cancelled: AtomicBool,
    done: bool,
}

fn client(timeout_secs: u64) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent("rpi-langfuse/0.1")
        .build()
        .map_err(|e| e.to_string())
}

fn api_request(
    client: &reqwest::blocking::Client,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, String> {
    let cfg = load_config();
    if !cfg.is_valid() {
        return Err("LANGFUSE_PUBLIC_KEY and LANGFUSE_SECRET_KEY are required".into());
    }
    let url = format!("{}{}", cfg.base_url, path);
    let mut req = match method {
        "GET" => client.get(&url),
        "POST" => client.post(&url),
        "PUT" => client.put(&url),
        "DELETE" => client.delete(&url),
        _ => return Err(format!("unsupported HTTP method: {}", method)),
    };
    req = req.basic_auth(&cfg.public_key, Some(&cfg.secret_key));
    if let Some(b) = body {
        req = req.json(b);
    }
    let resp = req
        .send()
        .map_err(|e| format!("langfuse request failed: {e}"))?;
    let status = resp.status();
    let value: Value = resp
        .json()
        .map_err(|e| format!("invalid langfuse response: {e}"))?;
    if !status.is_success() {
        let msg = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or(status.as_str());
        return Err(format!("langfuse api error {status}: {msg}"));
    }
    Ok(value)
}

pub fn score(p: &Value) -> Result<String, String> {
    let action = p
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("create");
    match action {
        "create" => {
            let name = p
                .get("name")
                .and_then(Value::as_str)
                .ok_or("name is required")?;
            let value = p.get("value").ok_or("value is required")?;
            let trace_id = p
                .get("traceId")
                .and_then(Value::as_str)
                .ok_or("traceId is required")?;
            let mut body = json!({"name": name, "value": value, "traceId": trace_id});
            if let Some(obs_id) = p.get("observationId").and_then(Value::as_str) {
                body["observationId"] = json!(obs_id);
            }
            if let Some(comment) = p.get("comment").and_then(Value::as_str) {
                body["comment"] = json!(comment);
            }
            let v = api_request(&client(30)?, "POST", "/api/public/scores", Some(&body))?;
            Ok(json!({"action":"create","score":v}).to_string())
        }
        "list" => {
            let mut query = Vec::new();
            if let Some(limit) = p.get("limit").and_then(Value::as_u64) {
                query.push(format!("limit={}", limit.clamp(1, 100)));
            }
            if let Some(page) = p.get("page").and_then(Value::as_u64) {
                query.push(format!("page={}", page.max(1)));
            }
            if let Some(user_id) = p.get("userId").and_then(Value::as_str) {
                query.push(format!("userId={}", user_id));
            }
            let qs = if query.is_empty() {
                String::new()
            } else {
                format!("?{}", query.join("&"))
            };
            let v = api_request(
                &client(30)?,
                "GET",
                &format!("/api/public/scores{}", qs),
                None,
            )?;
            Ok(json!({"action":"list","scores":v}).to_string())
        }
        _ => Err("action must be one of: create, list".into()),
    }
}

pub fn prompt(p: &Value) -> Result<String, String> {
    let action = p
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("create");
    match action {
        "create" => {
            let name = p
                .get("name")
                .and_then(Value::as_str)
                .ok_or("name is required")?;
            let prompt_text = p
                .get("prompt")
                .ok_or("prompt is required")?;
            let mut body = json!({"name": name, "prompt": prompt_text, "isActive": true});
            if let Some(config) = p.get("config") {
                body["config"] = config.clone();
            }
            let v = api_request(&client(30)?, "POST", "/api/public/prompts", Some(&body))?;
            Ok(json!({"action":"create","prompt":v}).to_string())
        }
        "get" => {
            let name = p
                .get("name")
                .and_then(Value::as_str)
                .ok_or("name is required for get action")?;
            let mut qs = String::new();
            if let Some(version) = p.get("version").and_then(Value::as_u64) {
                qs = format!("?version={}", version);
            }
            let v = api_request(
                &client(30)?,
                "GET",
                &format!("/api/public/prompts/{}{}", name, qs),
                None,
            )?;
            Ok(json!({"action":"get","prompt":v}).to_string())
        }
        "list" => {
            let mut query = Vec::new();
            if let Some(limit) = p.get("limit").and_then(Value::as_u64) {
                query.push(format!("limit={}", limit.clamp(1, 100)));
            }
            if let Some(page) = p.get("page").and_then(Value::as_u64) {
                query.push(format!("page={}", page.max(1)));
            }
            let qs = if query.is_empty() {
                String::new()
            } else {
                format!("?{}", query.join("&"))
            };
            let v = api_request(
                &client(30)?,
                "GET",
                &format!("/api/public/prompts{}", qs),
                None,
            )?;
            Ok(json!({"action":"list","prompts":v}).to_string())
        }
        _ => Err("action must be one of: create, get, list".into()),
    }
}

/// Manual trace control — create named traces, get trace details
pub fn trace(p: &Value) -> Result<String, String> {
    let action = p
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("get");
    match action {
        "get" => {
            let id = p
                .get("id")
                .and_then(Value::as_str)
                .ok_or("id is required for get action")?;
            let v = api_request(
                &client(30)?,
                "GET",
                &format!("/api/public/traces/{}", id),
                None,
            )?;
            Ok(json!({"action":"get","trace":v}).to_string())
        }
        "list" => {
            let mut query = Vec::new();
            if let Some(limit) = p.get("limit").and_then(Value::as_u64) {
                query.push(format!("limit={}", limit.clamp(1, 100)));
            }
            if let Some(page) = p.get("page").and_then(Value::as_u64) {
                query.push(format!("page={}", page.max(1)));
            }
            if let Some(user_id) = p.get("userId").and_then(Value::as_str) {
                query.push(format!("userId={}", user_id));
            }
            let qs = if query.is_empty() {
                String::new()
            } else {
                format!("?{}", query.join("&"))
            };
            let v = api_request(
                &client(30)?,
                "GET",
                &format!("/api/public/traces{}", qs),
                None,
            )?;
            Ok(json!({"action":"list","traces":v}).to_string())
        }
        "update" => {
            let id = p
                .get("id")
                .and_then(Value::as_str)
                .ok_or("id is required for update action")?;
            let mut body = Map::new();
            if let Some(name) = p.get("name").and_then(Value::as_str) {
                body.insert("name".into(), json!(name));
            }
            if let Some(user_id) = p.get("userId").and_then(Value::as_str) {
                body.insert("userId".into(), json!(user_id));
            }
            if let Some(session_id) = p.get("sessionId").and_then(Value::as_str) {
                body.insert("sessionId".into(), json!(session_id));
            }
            if let Some(metadata) = p.get("metadata") {
                body.insert("metadata".into(), metadata.clone());
            }
            if let Some(tags) = p.get("tags") {
                body.insert("tags".into(), tags.clone());
            }
            let v = api_request(
                &client(30)?,
                "PUT",
                &format!("/api/public/traces/{}", id),
                Some(&Value::Object(body)),
            )?;
            Ok(json!({"action":"update","trace":v}).to_string())
        }
        _ => Err("action must be one of: get, list, update".into()),
    }
}

fn start(params: StbString, free: Option<FreeStringFn>, builder: Builder) -> StepHandle {
    let t = params.to_string_lossy();
    params.free_with(free);
    let params = serde_json::from_str(&t).unwrap_or(Value::Null);
    Box::into_raw(Box::new(Drive {
        params,
        builder,
        cancelled: AtomicBool::new(false),
        done: false,
    })) as StepHandle
}

extern "C" fn execute_score(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    start(params, free, score)
}

extern "C" fn execute_prompt(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    start(params, free, prompt)
}

extern "C" fn execute_trace(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    start(params, free, trace)
}

extern "C" fn poll(h: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if h.is_null() {
        return StepResult::err(StbString::from_string("null langfuse handle".into()));
    }
    let d = unsafe { &mut *(h as *mut Drive) };
    if d.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("langfuse cancelled".into()));
    }
    if d.done {
        return StepResult::err(StbString::from_string(
            "langfuse polled after completion".into(),
        ));
    }
    d.done = true;
    let result = (d.builder)(&d.params);
    match result {
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

// ---------------------------------------------------------------------------
// Free StbStrings from event payloads (host-allocated)
// ---------------------------------------------------------------------------

static mut HOST_FREE: Option<FreeStringFn> = None;

unsafe fn host_free(s: StbString) {
    if let Some(f) = HOST_FREE {
        f(s);
    }
}

// ---------------------------------------------------------------------------
// Plugin registration
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn rpi_plugin_register_v2(api: *const PluginApiVt, abi: u32) -> i32 {
    unsafe { register_entrypoint(api, abi, |api| {
        // Store host free_string for event payload cleanup
        HOST_FREE = Some(api.free_string);

        // Register event handlers for auto-tracing
        if let Some(register_event) = api.register_event_handler {
            let handlers: &[(EventTag, EventHandlerFn)] = &[
                (EventTag::SessionStart, on_session_start),
                (EventTag::BeforeProviderRequest, on_before_provider_request),
                (EventTag::AfterProviderResponse, on_after_provider_response),
                (EventTag::ToolCall, on_tool_call),
                (EventTag::ToolResult, on_tool_result),
                (EventTag::TurnStart, on_turn_start),
                (EventTag::TurnEnd, on_turn_end),
                (EventTag::AgentStart, on_agent_start),
                (EventTag::AgentEnd, on_agent_end),
                (EventTag::SessionBeforeCompact, on_session_before_compact),
                (EventTag::SessionCompact, on_session_compact),
                (EventTag::SessionShutdown, on_session_shutdown),
            ];
            for &(tag, handler) in handlers {
                let rc = register_event(tag, handler, std::ptr::null_mut());
                if rc != 0 {
                    return rc;
                }
            }
        }

        // Register manual tools
        let Some(register) = api.register_tool else {
            return 1;
        };
        let schemas = [
            (
                "langfuse_score",
                "Create or list Langfuse scores. Scores evaluate traces/observations (e.g. accuracy, helpfulness, toxicity). Requires LANGFUSE_PUBLIC_KEY + LANGFUSE_SECRET_KEY env vars.",
                r#"{"type":"object","properties":{"action":{"type":"string","enum":["create","list"]},"name":{"type":"string"},"value":{"type":["number","string","object"]},"traceId":{"type":"string"},"observationId":{"type":"string"},"comment":{"type":"string"},"userId":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100},"page":{"type":"integer","minimum":1}}}"#,
            ),
            (
                "langfuse_prompt",
                "Create, get, or list Langfuse prompts for prompt version management. Requires LANGFUSE_PUBLIC_KEY + LANGFUSE_SECRET_KEY env vars.",
                r#"{"type":"object","properties":{"action":{"type":"string","enum":["create","get","list"]},"name":{"type":"string"},"prompt":{"type":"string"},"config":{"type":"object"},"version":{"type":"integer"},"limit":{"type":"integer","minimum":1,"maximum":100},"page":{"type":"integer","minimum":1}}}"#,
            ),
            (
                "langfuse_trace",
                "Get, list, or update Langfuse traces. Auto-tracing creates traces automatically; use this for manual inspection or updates. Requires LANGFUSE_PUBLIC_KEY + LANGFUSE_SECRET_KEY env vars.",
                r#"{"type":"object","properties":{"action":{"type":"string","enum":["get","list","update"]},"id":{"type":"string"},"name":{"type":"string"},"userId":{"type":"string"},"sessionId":{"type":"string"},"metadata":{"type":"object"},"tags":{"type":"array"},"limit":{"type":"integer","minimum":1,"maximum":100},"page":{"type":"integer","minimum":1}}}"#,
            ),
        ];
        for (name, desc, params) in schemas {
            let schema = Box::new(StableToolSchema {
                name: StbString::from_string(name.into()),
                description: StbString::from_string(desc.into()),
                parameters: StbString::from_string(params.into()),
            });
            let execute = match name {
                "langfuse_score" => execute_score,
                "langfuse_prompt" => execute_prompt,
                "langfuse_trace" => execute_trace,
                _ => unreachable!(),
            };
            let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
            drop(schema);
            if rc != 0 {
                return rc;
            }
        }
        0
    }) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_score_fields() {
        assert!(score(&json!({"action":"create","name":"test"})).is_err());
        assert!(score(&json!({"action":"create","value":1.0})).is_err());
        assert!(score(&json!({"action":"create","traceId":"t1"})).is_err());
    }

    #[test]
    fn rejects_missing_prompt_fields() {
        assert!(prompt(&json!({"action":"create","name":"test"})).is_err());
        assert!(prompt(&json!({"action":"create","prompt":"hello"})).is_err());
    }

    #[test]
    fn rejects_invalid_trace_action() {
        assert!(trace(&json!({"action":"invalid"})).is_err());
    }

    #[test]
    fn iso_timestamp_format() {
        let ts = now_iso();
        assert!(ts.contains('T'));
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.len(), 24); // YYYY-MM-DDTHH:MM:SS.mmmZ
    }

    #[test]
    fn days_to_ymd_known_dates() {
        // 1970-01-01 = day 0
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
        // 2024-01-01 = day 19723
        assert_eq!(days_to_ymd(19723), (2024, 1, 1));
    }

    #[test]
    fn trace_id_is_unique() {
        let a = trace_id();
        let b = trace_id();
        assert_ne!(a, b);
        assert!(a.starts_with("rpi-"));
    }

    #[test]
    fn batch_flush_threshold() {
        let mut batch = IngestionBatch::new();
        assert!(!batch.should_flush());
        for i in 0..BATCH_MAX_SIZE {
            batch.push(json!({"test": i}));
        }
        assert!(batch.should_flush());
        let events = batch.take();
        assert_eq!(events.len(), BATCH_MAX_SIZE);
        assert!(!batch.should_flush()); // reset after take
    }
}
