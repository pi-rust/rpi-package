//! Langfuse Observability Extension for RPI Agent
//! 
//! 1:1 Rust implementation based on pi-langfuse TypeScript reference

use rpi_plugin_sdk::{
    register_entrypoint, EventTag, EventHandlerFn, FreeStringFn, PluginApiVt,
    StablePluginEvent, StableToolSchema, StbString, StbStringRef, StepHandle, StepResult,
    ToolPartialCb,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
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
const MAX_STRING_LENGTH: usize = 12000;
const MAX_TOOL_PAYLOAD_LENGTH: usize = 24000;
const MAX_DEPTH: usize = 6;
const MAX_ARRAY_ITEMS: usize = 50;
const MAX_OBJECT_KEYS: usize = 80;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LangfuseConfig {
    pub base_url: String,
    pub public_key: String,
    pub secret_key: String,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub privacy_preset: Option<String>,
    #[serde(default)]
    pub capture_inputs: Option<bool>,
    #[serde(default)]
    pub capture_outputs: Option<bool>,
    #[serde(default)]
    pub capture_tool_io: Option<bool>,
    #[serde(default)]
    pub capture_system_prompt: Option<bool>,
    #[serde(default)]
    pub capture_cwd: Option<bool>,
    #[serde(default)]
    pub capture_source_metadata: Option<bool>,
    #[serde(default)]
    pub capture_paths: Option<bool>,
}

impl LangfuseConfig {
    fn is_valid(&self) -> bool {
        !self.public_key.is_empty() && !self.secret_key.is_empty()
    }

    fn from_file_config(file: FileConfig) -> Self {
        Self {
            base_url: file.base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            public_key: file.public_key.unwrap_or_default(),
            secret_key: file.secret_key.unwrap_or_default(),
            user_id: file.user_id,
            privacy_preset: None,
            capture_inputs: None,
            capture_outputs: None,
            capture_tool_io: None,
            capture_system_prompt: None,
            capture_cwd: None,
            capture_source_metadata: None,
            capture_paths: None,
        }
    }
}

static CONFIG_CACHE: OnceLock<Mutex<Option<LangfuseConfig>>> = OnceLock::new();

fn config_cache() -> &'static Mutex<Option<LangfuseConfig>> {
    CONFIG_CACHE.get_or_init(|| Mutex::new(None))
}

fn load_config() -> LangfuseConfig {
    if let Ok(cache) = config_cache().lock() {
        if let Some(ref cfg) = *cache {
            return cfg.clone();
        }
    }

    let file_config = load_config_file().unwrap_or_default();
    
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
        user_id: std::env::var("LANGFUSE_USER_ID")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or(file_config.user_id),
        privacy_preset: file_config.privacy_preset,
        capture_inputs: file_config.capture_inputs,
        capture_outputs: file_config.capture_outputs,
        capture_tool_io: file_config.capture_tool_io,
        capture_system_prompt: file_config.capture_system_prompt,
        capture_cwd: file_config.capture_cwd,
        capture_source_metadata: file_config.capture_source_metadata,
        capture_paths: file_config.capture_paths,
    };

    if let Ok(mut cache) = config_cache().lock() {
        *cache = Some(cfg.clone());
    }

    cfg
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileConfig {
    base_url: Option<String>,
    public_key: Option<String>,
    secret_key: Option<String>,
    user_id: Option<String>,
    privacy_preset: Option<String>,
    capture_inputs: Option<bool>,
    capture_outputs: Option<bool>,
    capture_tool_io: Option<bool>,
    capture_system_prompt: Option<bool>,
    capture_cwd: Option<bool>,
    capture_source_metadata: Option<bool>,
    capture_paths: Option<bool>,
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
        user_id: value.get("userId").and_then(Value::as_str).map(String::from),
        privacy_preset: value.get("privacyPreset").and_then(Value::as_str).map(String::from),
        capture_inputs: value.get("captureInputs").and_then(Value::as_bool),
        capture_outputs: value.get("captureOutputs").and_then(Value::as_bool),
        capture_tool_io: value.get("captureToolIO").and_then(Value::as_bool),
        capture_system_prompt: value.get("captureSystemPrompt").and_then(Value::as_bool),
        capture_cwd: value.get("captureCwd").and_then(Value::as_bool),
        capture_source_metadata: value.get("captureSourceMetadata").and_then(Value::as_bool),
        capture_paths: value.get("capturePaths").and_then(Value::as_bool),
    })
}

fn resolve_value_or_env(
    value: &Value,
    value_key: &str,
    env_key: &str,
) -> Result<Option<String>, String> {
    if let Some(v) = value.get(value_key).and_then(Value::as_str) {
        if !v.trim().is_empty() {
            return Ok(Some(v.to_string()));
        }
    }
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
    if let Some(path) = std::env::var_os("RPI_LANGFUSE_CONFIG") {
        let path = std::path::PathBuf::from(path);
        if !path.is_absolute() {
            return Err("RPI_LANGFUSE_CONFIG must be an absolute path".into());
        }
        return Ok(path);
    }
    let project = std::env::current_dir()
        .map_err(|e| format!("resolve current directory: {e}"))?
        .join(".rpi")
        .join("langfuse.json");
    if project.is_file() {
        return Ok(project);
    }
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

// ---------------------------------------------------------------------------
// Timestamp helpers
// ---------------------------------------------------------------------------

fn now_iso() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    let millis = d.subsec_millis();
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

fn unix_secs_to_iso(secs: i64) -> String {
    if secs <= 0 {
        return now_iso();
    }
    let secs_u = secs as u64;
    let days = secs_u / 86400;
    let (y, m, d) = days_to_ymd(days);
    let h = (secs_u % 86400) / 3600;
    let min = (secs_u % 3600) / 60;
    let s = secs_u % 60;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000Z",
        y, m, d, h, min, s
    )
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
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

// ---------------------------------------------------------------------------
// ID generation
// ---------------------------------------------------------------------------

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
// Observation types (matching TypeScript)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Observation {
    id: String,
    trace_id: String,
    name: String,
    r#type: String, // "SPAN" | "GENERATION"
    start_time: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_observation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_parameters: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage_details: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_details: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    completion_start_time: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Trace {
    id: String,
    timestamp: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingScore {
    id: Option<String>,
    name: String,
    value: Value,
    data_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    observation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    comment: Option<String>,
}

// ---------------------------------------------------------------------------
// State management
// ---------------------------------------------------------------------------

struct AgentState {
    root: Option<Observation>,
    attempt: Option<Observation>,
    latest_assistant_output: Option<Value>,
}

struct RunState {
    trace: Option<Trace>,
    observations: Vec<Observation>,
    observation_by_id: HashMap<String, Observation>,
    pending_scores: Vec<PendingScore>,
    agent_state: Option<AgentState>,
    turn_count: u32,
    current_model: String,
    current_provider: String,
    current_session_id: Option<String>,
}

impl RunState {
    fn new() -> Self {
        Self {
            trace: None,
            observations: Vec::new(),
            observation_by_id: HashMap::new(),
            pending_scores: Vec::new(),
            agent_state: None,
            turn_count: 0,
            current_model: String::new(),
            current_provider: String::new(),
            current_session_id: None,
        }
    }

    fn reset(&mut self) {
        self.trace = None;
        self.observations.clear();
        self.observation_by_id.clear();
        self.pending_scores.clear();
        self.agent_state = None;
        self.turn_count = 0;
    }
}

static RUN_STATE: OnceLock<Mutex<RunState>> = OnceLock::new();

fn run_state() -> &'static Mutex<RunState> {
    RUN_STATE.get_or_init(|| Mutex::new(RunState::new()))
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
}

static TRACER: OnceLock<Arc<TracerState>> = OnceLock::new();

fn tracer() -> Arc<TracerState> {
    TRACER
        .get_or_init(|| {
            Arc::new(TracerState {
                batch: Mutex::new(IngestionBatch::new()),
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
        std::thread::spawn(move || {
            if let Err(e) = flush_batch(events) {
                eprintln!("[rpi-langfuse] flush error: {e}");
            }
        });
    }
}

fn force_flush(state: &TracerState) {
    // Convert observations to Langfuse ingestion batch format
    let mut events = Vec::new();
    
    {
        let run = run_state().lock().unwrap();
        
        // Add trace-create event if trace exists
        if let Some(ref trace) = run.trace {
            events.push(json!({
                "id": obs_id(),
                "type": "trace-create",
                "timestamp": trace.timestamp,
                "body": {
                    "id": trace.id,
                    "name": trace.name,
                    "timestamp": trace.timestamp,
                    "input": trace.input,
                    "output": trace.output,
                    "sessionId": trace.session_id,
                    "userId": trace.user_id,
                    "metadata": trace.metadata
                },
                "metadata": {}
            }));
        }
        
        // Add observation events
        for obs in &run.observations {
            let event_type = if obs.r#type == "GENERATION" {
                "generation-create"
            } else {
                "span-create"
            };
            
            let mut body = json!({
                "id": obs.id,
                "traceId": obs.trace_id,
                "name": obs.name,
                "startTime": obs.start_time,
                "endTime": obs.end_time,
                "parentObservationId": obs.parent_observation_id,
                "input": obs.input,
                "output": obs.output,
                "metadata": obs.metadata
            });
            
            if obs.r#type == "GENERATION" {
                if let Some(ref model) = obs.model {
                    body["model"] = json!(model);
                }
                if let Some(ref model_params) = obs.model_parameters {
                    body["modelParameters"] = json!(model_params);
                }
                if let Some(ref usage) = obs.usage_details {
                    body["usageDetails"] = json!(usage);
                }
                if let Some(ref cost) = obs.cost_details {
                    body["costDetails"] = json!(cost);
                }
                if let Some(ref completion) = obs.completion_start_time {
                    body["completionStartTime"] = json!(completion);
                }
            }
            
            if let Some(ref level) = obs.level {
                body["level"] = json!(level);
            }
            if let Some(ref status) = obs.status_message {
                body["statusMessage"] = json!(status);
            }
            
            events.push(json!({
                "id": obs_id(),
                "type": event_type,
                "timestamp": obs.start_time,
                "body": body,
                "metadata": {}
            }));
        }
    }
    
    // Also include any pending batch events
    let mut batch_events = state.batch.lock().unwrap().take();
    events.append(&mut batch_events);
    
    if !events.is_empty() {
        if let Err(e) = flush_batch(events) {
            eprintln!("[rpi-langfuse] flush error: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Observation management
// ---------------------------------------------------------------------------

fn start_observation(
    name: &str,
    body: Option<Value>,
    as_type: Option<&str>,
    parent_observation_id: Option<&str>,
) -> Observation {
    let id = obs_id();
    let trace_id = {
        let state = run_state().lock().unwrap();
        state.trace.as_ref().map(|t| t.id.clone()).unwrap_or_else(trace_id)
    };
    
    let metadata = body.as_ref().and_then(|b| {
        b.get("metadata").and_then(|m| m.as_object()).cloned()
    });

    let mut obs = Observation {
        id: id.clone(),
        trace_id: trace_id.clone(),
        name: name.to_string(),
        r#type: if as_type == Some("generation") { "GENERATION".to_string() } else { "SPAN".to_string() },
        start_time: now_iso(),
        end_time: None,
        parent_observation_id: parent_observation_id.map(String::from),
        input: body.as_ref().and_then(|b| b.get("input").cloned()),
        output: None,
        metadata,
        model: None,
        model_parameters: None,
        usage_details: None,
        cost_details: None,
        level: None,
        status_message: None,
        completion_start_time: None,
    };

    // Apply body updates
    if let Some(body) = body {
        if let Some(input) = body.get("input") {
            obs.input = Some(input.clone());
        }
        if let Some(model) = body.get("model").and_then(|m| m.as_str()) {
            obs.model = Some(model.to_string());
        }
        if let Some(model_params) = body.get("modelParameters").and_then(|m| m.as_object()) {
            obs.model_parameters = Some(model_params.clone());
        }
    }

    // Store observation
    {
        let mut state = run_state().lock().unwrap();
        state.observations.push(obs.clone());
        state.observation_by_id.insert(id.clone(), obs.clone());
    }

    // Create trace if this is root observation
    if parent_observation_id.is_none() {
        let mut state = run_state().lock().unwrap();
        if state.trace.is_none() {
            let cfg = load_config();
            state.trace = Some(Trace {
                id: trace_id,
                timestamp: obs.start_time.clone(),
                name: name.to_string(),
                input: obs.input.clone(),
                output: None,
                session_id: state.current_session_id.clone(),
                user_id: cfg.user_id.clone(),
                metadata: obs.metadata.clone(),
            });
        }
    }

    obs
}

fn update_observation(id: &str, body: Option<Value>) {
    let mut state = run_state().lock().unwrap();
    if let Some(obs) = state.observation_by_id.get_mut(id) {
        if let Some(body) = body {
            if let Some(input) = body.get("input") {
                obs.input = Some(input.clone());
            }
            if let Some(output) = body.get("output") {
                obs.output = Some(output.clone());
            }
            if let Some(metadata) = body.get("metadata").and_then(|m| m.as_object()) {
                obs.metadata = Some(metadata.clone());
            }
            if let Some(model) = body.get("model").and_then(|m| m.as_str()) {
                obs.model = Some(model.to_string());
            }
            if let Some(model_params) = body.get("modelParameters").and_then(|m| m.as_object()) {
                obs.model_parameters = Some(model_params.clone());
            }
            if let Some(usage) = body.get("usageDetails").and_then(|u| u.as_object()) {
                obs.usage_details = Some(usage.clone());
            }
            if let Some(cost) = body.get("costDetails").and_then(|c| c.as_object()) {
                obs.cost_details = Some(cost.clone());
            }
            if let Some(level) = body.get("level").and_then(|l| l.as_str()) {
                obs.level = Some(level.to_string());
            }
            if let Some(status) = body.get("statusMessage").and_then(|s| s.as_str()) {
                obs.status_message = Some(status.to_string());
            }
            if let Some(completion) = body.get("completionStartTime").and_then(|c| c.as_str()) {
                obs.completion_start_time = Some(completion.to_string());
            }
        }
    }
}

fn end_observation(id: &str, body: Option<Value>) {
    if let Some(body) = body {
        update_observation(id, Some(body));
    }
    let mut state = run_state().lock().unwrap();
    if let Some(obs) = state.observation_by_id.get_mut(id) {
        obs.end_time = Some(now_iso());
    }
}

// ---------------------------------------------------------------------------
// Event handlers
// ---------------------------------------------------------------------------

extern "C" fn on_session_start(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let mut state = run_state().lock().unwrap();
    state.reset();
    
    // Get session ID from environment
    if let Ok(session_id) = std::env::var("RPI_SESSION_ID") {
        if !session_id.is_empty() {
            state.current_session_id = Some(session_id);
        }
    }
    0
}

extern "C" fn on_model_select(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    // The rpi host does not dispatch `ModelSelect` today (it is not part of
    // the emitted event set), so there is no stable payload to read. Keep the
    // handler as a no-op rather than reading `event.payload.data` — the host
    // may dispatch this tag with an empty payload, and reading the `data`
    // union member on a non-data event reads uninitialized stack bytes as a
    // `StbString`, producing a garbage `len` that aborts the whole process.
    0
}

extern "C" fn on_agent_start(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    // `AgentStart` is dispatched by the host with an EMPTY payload
    // (`StablePluginEvent::empty`) — it carries no data JSON. Reading
    // `event.payload.data.data` here reads uninitialized stack bytes as a
    // `StbString`, producing a garbage `len` that makes `to_string_lossy`
    // allocate ~terabytes and abort the whole host (TUI crash). Never read
    // the `data` union member for a tag the host dispatches empty.
    let _ = event;

    // Check if we need to create root observation
    let need_root = {
        let state = run_state().lock().unwrap();
        state.agent_state.is_none() || state.agent_state.as_ref().unwrap().root.is_none()
    };
    
    if need_root {
        let obs = start_observation("agent-run", None, Some("span"), None);
        let mut state = run_state().lock().unwrap();
        state.agent_state = Some(AgentState {
            root: Some(obs),
            attempt: None,
            latest_assistant_output: None,
        });
    }
    
    // Get parent id for attempt
    let parent_id = {
        let state = run_state().lock().unwrap();
        state.agent_state.as_ref()
            .and_then(|a| a.root.as_ref())
            .map(|o| o.id.clone())
    };
    
    // Start attempt
    let attempt = start_observation("agent-attempt", None, Some("span"), 
        parent_id.as_deref());
    let mut state = run_state().lock().unwrap();
    if let Some(agent_state) = state.agent_state.as_mut() {
        agent_state.attempt = Some(attempt);
    }
    0
}

extern "C" fn on_before_provider_request(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let data_str = unsafe { event.payload.data.data.to_string_lossy() };
    let data: Value = serde_json::from_str(&data_str).unwrap_or(Value::Null);
    
    let state = run_state().lock().unwrap();
    let parent_id = state.agent_state.as_ref()
        .and_then(|a| a.attempt.as_ref())
        .map(|o| o.id.clone());
    drop(state);
    
    // `start_observation` already registers the observation in `observations`
    // and `observation_by_id`; do not push it again here (that would create
    // duplicate generation spans in Langfuse).
    let _obs = start_observation("llm-request", Some(data.clone()), Some("generation"), 
        parent_id.as_deref());
    0
}

extern "C" fn on_after_provider_response(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let data_str = unsafe { event.payload.data.data.to_string_lossy() };
    let data: Value = serde_json::from_str(&data_str).unwrap_or(Value::Null);
    
    let state = run_state().lock().unwrap();
    let last_obs_id = state.observations.iter()
        .rev()
        .find(|o| o.r#type == "GENERATION")
        .map(|o| o.id.clone());
    drop(state);
    
    if let Some(obs_id) = last_obs_id {
        let mut update_body = Map::new();
        
        // Extract usage
        if let Some(usage) = data.get("usage") {
            let mut usage_details = Map::new();
            if let Some(input) = usage.get("input").and_then(|v| v.as_u64()) {
                usage_details.insert("input".into(), json!(input));
            }
            if let Some(output) = usage.get("output").and_then(|v| v.as_u64()) {
                usage_details.insert("output".into(), json!(output));
            }
            if let Some(total) = usage.get("totalTokens").and_then(|v| v.as_u64()) {
                usage_details.insert("total".into(), json!(total));
            }
            update_body.insert("usageDetails".into(), Value::Object(usage_details));
        }
        
        // Extract model
        if let Some(model) = data.get("model").and_then(|m| m.as_str()) {
            update_body.insert("model".into(), json!(model));
        }
        
        // Extract timestamp for completionStartTime
        if let Some(ts) = data.get("timestamp").and_then(|t| t.as_i64()) {
            update_body.insert("completionStartTime".into(), json!(unix_secs_to_iso(ts)));
        }
        
        update_observation(&obs_id, Some(Value::Object(update_body)));
    }
    0
}

extern "C" fn on_tool_execution_start(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let tool_call_id = unsafe { event.payload.tool_call.tool_call_id.to_string_lossy() };
    let tool_name = unsafe { event.payload.tool_call.tool_name.to_string_lossy() };
    let params = unsafe { event.payload.tool_call.params.to_string_lossy() };
    
    let state = run_state().lock().unwrap();
    let parent_id = state.agent_state.as_ref()
        .and_then(|a| a.attempt.as_ref())
        .map(|o| o.id.clone());
    drop(state);
    
    let input = serde_json::from_str::<Value>(&params).unwrap_or(Value::Null);
    let body = json!({ "input": input });
    
    let obs = start_observation(&tool_name, Some(body), Some("span"), parent_id.as_deref());
    
    let mut state = run_state().lock().unwrap();
    state.observation_by_id.insert(tool_call_id, obs);
    0
}

extern "C" fn on_tool_execution_end(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let tool_call_id = unsafe { event.payload.tool_result.tool_call_id.to_string_lossy() };
    let result = unsafe { event.payload.tool_result.result.to_string_lossy() };
    let is_error = unsafe { event.payload.tool_result.is_error } != 0;
    
    let state = run_state().lock().unwrap();
    let obs = state.observation_by_id.get(&tool_call_id).cloned();
    drop(state);
    
    if let Some(obs) = obs {
        let mut update_body = Map::new();
        
        if is_error {
            update_body.insert("level".into(), json!("ERROR"));
            update_body.insert("statusMessage".into(), json!(result));
        } else {
            let output = serde_json::from_str::<Value>(&result).unwrap_or(Value::Null);
            update_body.insert("output".into(), output);
        }
        
        end_observation(&obs.id, Some(Value::Object(update_body)));
    }
    0
}

extern "C" fn on_turn_start(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let mut state = run_state().lock().unwrap();
    state.turn_count += 1;
    0
}

extern "C" fn on_turn_end(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    0
}

extern "C" fn on_agent_end(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let mut state = run_state().lock().unwrap();
    if let Some(agent_state) = state.agent_state.as_mut() {
        if let Some(attempt) = agent_state.attempt.take() {
            end_observation(&attempt.id, None);
        }
    }
    0
}

extern "C" fn on_agent_settled(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let state = tracer();
    force_flush(&state);
    
    let mut state = run_state().lock().unwrap();
    if let Some(agent_state) = state.agent_state.as_mut() {
        if let Some(root) = agent_state.root.take() {
            end_observation(&root.id, None);
        }
    }
    0
}

extern "C" fn on_session_shutdown(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    let state = tracer();
    force_flush(&state);
    
    let mut state = run_state().lock().unwrap();
    state.reset();
    0
}

// ---------------------------------------------------------------------------
// Manual tools
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
    let action = p.get("action").and_then(Value::as_str).unwrap_or("create");
    match action {
        "create" => {
            let name = p.get("name").and_then(Value::as_str).ok_or("name is required")?;
            let value = p.get("value").ok_or("value is required")?;
            let trace_id = p.get("traceId").and_then(Value::as_str).ok_or("traceId is required")?;
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
            let v = api_request(&client(30)?, "GET", &format!("/api/public/scores{}", qs), None)?;
            Ok(json!({"action":"list","scores":v}).to_string())
        }
        _ => Err("action must be one of: create, list".into()),
    }
}

pub fn prompt(p: &Value) -> Result<String, String> {
    let action = p.get("action").and_then(Value::as_str).unwrap_or("create");
    match action {
        "create" => {
            let name = p.get("name").and_then(Value::as_str).ok_or("name is required")?;
            let prompt_text = p.get("prompt").ok_or("prompt is required")?;
            let mut body = json!({"name": name, "prompt": prompt_text, "isActive": true});
            if let Some(config) = p.get("config") {
                body["config"] = config.clone();
            }
            let v = api_request(&client(30)?, "POST", "/api/public/prompts", Some(&body))?;
            Ok(json!({"action":"create","prompt":v}).to_string())
        }
        "get" => {
            let name = p.get("name").and_then(Value::as_str).ok_or("name is required")?;
            // Langfuse v2 public API has no path route for prompt-by-name;
            // the prompt is fetched via the query-string form.
            let mut query = vec![format!("name={}", name)];
            if let Some(version) = p.get("version").and_then(Value::as_u64) {
                query.push(format!("version={}", version));
            }
            let qs = format!("?{}", query.join("&"));
            let v = api_request(&client(30)?, "GET", &format!("/api/public/prompts{}", qs), None)?;
            Ok(json!({"action":"get","prompt":v}).to_string())
        }
        "list" => {
            // Langfuse v2.95+ requires `name` on GET /api/public/prompts and
            // returns a single prompt (latest version), so there is no
            // list-all-prompts endpoint to hit. Require `name` here and return
            // the fetched prompt wrapped in a `data` array for list semantics.
            let name = p.get("name").and_then(Value::as_str).ok_or(
                "list requires a prompt name: this Langfuse version only supports \
                 GET /api/public/prompts?name=... (no list-all endpoint). Use \"get\" \
                 with a name instead.",
            )?;
            let mut query = vec![format!("name={}", name)];
            if let Some(version) = p.get("version").and_then(Value::as_u64) {
                query.push(format!("version={}", version));
            }
            if let Some(limit) = p.get("limit").and_then(Value::as_u64) {
                query.push(format!("limit={}", limit.clamp(1, 100)));
            }
            if let Some(page) = p.get("page").and_then(Value::as_u64) {
                query.push(format!("page={}", page.max(1)));
            }
            let qs = format!("?{}", query.join("&"));
            let v = api_request(&client(30)?, "GET", &format!("/api/public/prompts{}", qs), None)?;
            let prompts = json!([v]);
            Ok(json!({"action":"list","prompts":prompts}).to_string())
        }
        _ => Err("action must be one of: create, get, list".into()),
    }
}

pub fn trace(p: &Value) -> Result<String, String> {
    let action = p.get("action").and_then(Value::as_str).unwrap_or("get");
    match action {
        "get" => {
            let id = p.get("id").and_then(Value::as_str).ok_or("id is required")?;
            let v = api_request(&client(30)?, "GET", &format!("/api/public/traces/{}", id), None)?;
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
            let v = api_request(&client(30)?, "GET", &format!("/api/public/traces{}", qs), None)?;
            Ok(json!({"action":"list","traces":v}).to_string())
        }
        "update" => {
            let id = p.get("id").and_then(Value::as_str).ok_or("id is required")?;
            // Langfuse v2 has no PUT/PATCH /api/public/traces/{id} route (405).
            // Traces are updated via the ingestion API: a `trace-create` event
            // with the same trace id upserts the trace and merges its fields.
            let mut body = Map::new();
            body.insert("id".into(), json!(id));
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
            let event = json!({
                "id": format!("trace-update-{}", id),
                "type": "trace-create",
                "timestamp": now_iso(),
                "body": Value::Object(body),
            });
            let batch = json!({ "batch": [event] });
            let cfg = load_config();
            if !cfg.is_valid() {
                return Err("LANGFUSE_PUBLIC_KEY and LANGFUSE_SECRET_KEY are required".into());
            }
            let url = format!("{}/api/public/ingestion", cfg.base_url);
            let client = client(30)?;
            let resp = client
                .post(&url)
                .basic_auth(&cfg.public_key, Some(&cfg.secret_key))
                .json(&batch)
                .send()
                .map_err(|e| format!("langfuse trace update failed: {e}"))?;
            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().unwrap_or_default();
                return Err(format!("langfuse api error {status}: {text}"));
            }
            Ok(json!({"action":"update","trace":{"id": id, "status": "updated"}}).to_string())
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

extern "C" fn execute_score(_: StbStringRef, params: StbString, free: Option<FreeStringFn>) -> StepHandle {
    start(params, free, score)
}

extern "C" fn execute_prompt(_: StbStringRef, params: StbString, free: Option<FreeStringFn>) -> StepHandle {
    start(params, free, prompt)
}

extern "C" fn execute_trace(_: StbStringRef, params: StbString, free: Option<FreeStringFn>) -> StepHandle {
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
        return StepResult::err(StbString::from_string("langfuse polled after completion".into()));
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
            (&*(h as *mut Drive)).cancelled.store(true, Ordering::SeqCst);
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
// Plugin registration
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn rpi_plugin_register_v2(api: *const PluginApiVt, abi: u32) -> i32 {
    unsafe { register_entrypoint(api, abi, |api| {
        if let Some(register_event) = api.register_event_handler {
            let handlers: &[(EventTag, EventHandlerFn)] = &[
                (EventTag::SessionStart, on_session_start),
                (EventTag::ModelSelect, on_model_select),
                (EventTag::AgentStart, on_agent_start),
                (EventTag::BeforeProviderRequest, on_before_provider_request),
                (EventTag::AfterProviderResponse, on_after_provider_response),
                (EventTag::ToolExecutionStart, on_tool_execution_start),
                (EventTag::ToolExecutionEnd, on_tool_execution_end),
                (EventTag::TurnStart, on_turn_start),
                (EventTag::TurnEnd, on_turn_end),
                (EventTag::AgentEnd, on_agent_end),
                (EventTag::AgentSettled, on_agent_settled),
                (EventTag::SessionShutdown, on_session_shutdown),
            ];
            for &(tag, handler) in handlers {
                let rc = register_event(tag, handler, std::ptr::null_mut());
                if rc != 0 {
                    return rc;
                }
            }
        }

        let Some(register) = api.register_tool else {
            return 1;
        };
        let schemas = [
            (
                "langfuse_score",
                "Create or list Langfuse scores",
                r#"{"type":"object","properties":{"action":{"type":"string","enum":["create","list"]},"name":{"type":"string"},"value":{"type":["number","string","object"]},"traceId":{"type":"string"},"observationId":{"type":"string"},"comment":{"type":"string"},"userId":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100},"page":{"type":"integer","minimum":1}}}"#,
            ),
            (
                "langfuse_prompt",
                "Create, get, or list Langfuse prompts",
                r#"{"type":"object","properties":{"action":{"type":"string","enum":["create","get","list"]},"name":{"type":"string"},"prompt":{"type":"string"},"config":{"type":"object"},"version":{"type":"integer"},"limit":{"type":"integer","minimum":1,"maximum":100},"page":{"type":"integer","minimum":1}}}"#,
            ),
            (
                "langfuse_trace",
                "Get, list, or update Langfuse traces",
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
    fn test_config_loading() {
        let cfg = load_config();
        assert!(cfg.base_url.contains("langfuse"));
    }

    #[test]
    fn test_timestamp_format() {
        let ts = now_iso();
        assert!(ts.contains('T'));
        assert!(ts.ends_with('Z'));
    }

    #[test]
    fn test_observation_creation() {
        let obs = start_observation("test", None, Some("span"), None);
        assert_eq!(obs.name, "test");
        assert_eq!(obs.r#type, "SPAN");
    }
}
