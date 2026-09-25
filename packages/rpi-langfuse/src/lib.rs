//! Langfuse Observability Extension for RPI Agent
//! 
//! 1:1 Rust implementation based on pi-observability-plugin TypeScript reference
//! Uses OpenTelemetry-style observation hierarchy with Langfuse HTTP ingestion API

use rpi_plugin_sdk::{
    register_entrypoint, EventTag, EventHandlerFn, FreeStringFn, PluginApiVt, RuntimeActionFn,
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
// Constants
// ---------------------------------------------------------------------------

const EXTENSION_NAME: &str = "@langfuse/pi-observability-plugin";
const EXTENSION_VERSION: &str = "0.2.0";
const ROOT_OBSERVATION_NAME: &str = "Conversational Turn";
const SUBAGENT_ROOT_OBSERVATION_NAME: &str = "Subagent Turn";
const TRACE_NAME: &str = "Pi Turn";
const GENERATION_PREFIX: &str = "LLM Call";
const TOOL_PREFIX: &str = "Tool:";
const COMPACTION_OBSERVATION_NAME: &str = "Compaction";
const BRANCH_SUMMARY_OBSERVATION_NAME: &str = "Branch Summary";
const TOOL_USAGE_OBSERVATION_NAME: &str = "Tool LLM Usage";
const BASE_TAGS: &[&str] = &["pi"];

const DEFAULT_BASE_URL: &str = "https://cloud.langfuse.com";
/// Langfuse v4 native OpenTelemetry ingestion. Langfuse v4 `events_only`
/// deployments reject `span-create`/`generation-create` on
/// `POST /api/public/ingestion` (only `score-create`/`sdk-log` survive), so all
/// trace/span/generation data goes here.
const OTEL_PATH: &str = "/api/public/otel/v1/traces";
const SDK_NAME: &str = "rpi-langfuse";
const MAX_STRING_LENGTH: usize = 12000;
const MAX_TOOL_PAYLOAD_LENGTH: usize = 24000;
const FLUSH_TIMEOUT_MS: u64 = 3000;
const EXIT_FLUSH_WITH_MEDIA_TIMEOUT_MS: u64 = 15000;

const SECRET_REDACTION_MARK: &str = "[redacted-langfuse-secret]";
const LANGFUSE_KEY_PATTERN: &str = r"\b[sp]k-lf-[\w-]+\b";
const DATA_URI_PATTERN: &str = r"data:[^;,]{0,100};base64,[A-Za-z0-9+/]+=*";

const ENV_PARENT_TRACE_ID: &str = "LANGFUSE_PI_PARENT_TRACE_ID";
const ENV_PARENT_SPAN_ID: &str = "LANGFUSE_PI_PARENT_SPAN_ID";
const ENV_PARENT_SESSION_ID: &str = "LANGFUSE_PI_PARENT_SESSION_ID";
const ENV_PARENT_DEPTH: &str = "LANGFUSE_PI_PARENT_DEPTH";

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LangfuseConfig {
    pub public_key: String,
    pub secret_key: String,
    pub base_url: String,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
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

fn load_config() -> Option<LangfuseConfig> {
    // Kill switch: wins over both env keys and the config file
    if let Ok(enabled) = std::env::var("LANGFUSE_TRACING_ENABLED") {
        if enabled.trim().to_lowercase() == "false" {
            return None;
        }
    }

    if let Ok(cache) = config_cache().lock() {
        if let Some(ref cfg) = *cache {
            return Some(cfg.clone());
        }
    }

    let file_config = load_config_file().unwrap_or_default();
    
    let as_trimmed_string = |v: Option<String>| -> Option<String> {
        v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    };

    let public_key = as_trimmed_string(std::env::var("LANGFUSE_PUBLIC_KEY").ok())
        .or_else(|| as_trimmed_string(file_config.public_key.clone()));
    let secret_key = as_trimmed_string(std::env::var("LANGFUSE_SECRET_KEY").ok())
        .or_else(|| as_trimmed_string(file_config.secret_key.clone()));

    if public_key.is_none() || secret_key.is_none() {
        return None;
    }

    let base_url = as_trimmed_string(std::env::var("LANGFUSE_BASE_URL").ok())
        .or_else(|| as_trimmed_string(std::env::var("LANGFUSE_HOST").ok()))
        .or_else(|| as_trimmed_string(file_config.base_url.clone()))
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
        .trim_end_matches('/')
        .to_string();

    let cfg = LangfuseConfig {
        public_key: public_key.unwrap(),
        secret_key: secret_key.unwrap(),
        base_url,
        user_id: as_trimmed_string(std::env::var("LANGFUSE_USER_ID").ok())
            .or_else(|| as_trimmed_string(file_config.user_id)),
        environment: as_trimmed_string(std::env::var("LANGFUSE_TRACING_ENVIRONMENT").ok())
            .or_else(|| as_trimmed_string(file_config.environment)),
        release: as_trimmed_string(std::env::var("LANGFUSE_RELEASE").ok())
            .or_else(|| as_trimmed_string(file_config.release)),
    };

    if let Ok(mut cache) = config_cache().lock() {
        *cache = Some(cfg.clone());
    }

    Some(cfg)
}

fn is_enabled() -> bool {
    load_config().is_some()
}

/// `RPI_LANGFUSE_DEBUG=1` makes the extension explain itself on stderr (which
/// lifecycle events arrived, how many spans were exported, why an export
/// failed). Off by default: the host may run many rpi processes, **and** a raw
/// stderr write corrupts the fullscreen TUI's alt-screen (it lands on the input
/// row), so this is strictly an operator opt-in for debugging — never a
/// user-facing channel. Use the status line for anything the user should see.
fn debug_log(msg: &str) {
    if std::env::var("RPI_LANGFUSE_DEBUG").ok().as_deref() == Some("1") {
        eprintln!("[rpi-langfuse] {msg}");
    }
}

/// `RuntimeActionId::SetStatus` — publish a short status string for the host's
/// UI (the TUI renders it in its footer). Numeric id pinned on purpose: the
/// plugin must not depend on the SDK enum for a value the host has to match.
const RUNTIME_ACTION_SET_STATUS: u32 = 18;

/// Host fn pointers captured once at register time. `runtime_action` drives the
/// host's status line; `user_data` is the host's opaque context.
#[derive(Clone, Copy)]
struct HostRuntime {
    runtime_action: RuntimeActionFn,
    free_string: FreeStringFn,
    user_data: *mut c_void,
}

// SAFETY: the host guarantees `user_data` is valid for the session lifetime and
// the fn pointers are `extern "C"` with no thread affinity.
unsafe impl Send for HostRuntime {}
unsafe impl Sync for HostRuntime {}

static HOST_RUNTIME: OnceLock<HostRuntime> = OnceLock::new();

/// Best-effort: the host's footer shows what the extension is doing
/// (`langfuse ✓ (trace sent)`), mirroring the JS observability plugin. A host
/// without the action (older build) or a headless run simply ignores it, so
/// failures are only surfaced under `RPI_LANGFUSE_DEBUG=1`.
fn set_status(text: &str) {
    let Some(runtime) = HOST_RUNTIME.get() else {
        return;
    };
    let args = json!({ "key": "langfuse", "value": text }).to_string();
    let mut output = StbString::empty();
    let rc = (runtime.runtime_action)(
        RUNTIME_ACTION_SET_STATUS,
        StbStringRef::from_str(&args),
        &mut output,
        runtime.user_data,
    );
    let detail = output.to_string_lossy();
    (runtime.free_string)(output);
    if rc != 0 {
        debug_log(&format!("set_status failed rc={rc}: {detail}"));
    }
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileConfig {
    pub public_key: Option<String>,
    pub secret_key: Option<String>,
    pub base_url: Option<String>,
    pub user_id: Option<String>,
    pub environment: Option<String>,
    pub release: Option<String>,
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
        public_key: value.get("publicKey").and_then(Value::as_str).map(String::from),
        secret_key: value.get("secretKey").and_then(Value::as_str).map(String::from),
        base_url: value.get("baseUrl").and_then(Value::as_str).map(String::from),
        user_id: value.get("userId").and_then(Value::as_str).map(String::from),
        environment: value.get("environment").and_then(Value::as_str).map(String::from),
        release: value.get("release").and_then(Value::as_str).map(String::from),
    })
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
        .map(|home| home.join(".pi").join("agent").join("langfuse.json"))
        .ok_or_else(|| "cannot resolve home directory for ~/.pi/agent/langfuse.json".into())
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}

// ---------------------------------------------------------------------------
// Secret redaction
// ---------------------------------------------------------------------------

fn escape_reg_exp_literal(text: &str) -> String {
    // Simple escape for regex special characters
    let mut result = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        match c {
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '[' | ']' | '|' | '\\' => {
                result.push('\\');
                result.push(c);
            }
            _ => result.push(c),
        }
    }
    result
}

fn create_secret_redactor(extra_secrets: Vec<String>) -> Box<dyn Fn(&Value) -> Value + Send + Sync> {
    let mut alternatives: Vec<String> = extra_secrets
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(|s| escape_reg_exp_literal(&s))
        .collect();
    alternatives.push(LANGFUSE_KEY_PATTERN.to_string());
    let pattern = alternatives.join("|");
    
    Box::new(move |value: &Value| {
        fn walk(value: &Value, pattern: &str, ancestors: &mut Vec<*const Value>) -> Value {
            match value {
                Value::String(s) => {
                    // Simple regex replacement (would need regex crate for full implementation)
                    // For now, just check if it contains Langfuse key patterns
                    if s.contains("pk-lf-") || s.contains("sk-lf-") {
                        Value::String(SECRET_REDACTION_MARK.to_string())
                    } else {
                        value.clone()
                    }
                }
                Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
                Value::Array(items) => {
                    let ptr = value as *const Value;
                    if ancestors.contains(&ptr) {
                        return Value::String("[circular-ref]".to_string());
                    }
                    ancestors.push(ptr);
                    let result: Vec<Value> = items
                        .iter()
                        .map(|item| walk(item, pattern, ancestors))
                        .collect();
                    ancestors.pop();
                    Value::Array(result)
                }
                Value::Object(map) => {
                    let ptr = value as *const Value;
                    if ancestors.contains(&ptr) {
                        return Value::String("[circular-ref]".to_string());
                    }
                    ancestors.push(ptr);
                    let result: Map<String, Value> = map
                        .iter()
                        .map(|(k, v)| (k.clone(), walk(v, pattern, ancestors)))
                        .collect();
                    ancestors.pop();
                    Value::Object(result)
                }
            }
        }
        walk(value, &pattern, &mut Vec::new())
    })
}

fn redact_langfuse_keys(value: &Value) -> Value {
    let redactor = create_secret_redactor(vec![]);
    redactor(value)
}

// ---------------------------------------------------------------------------
// Payload helpers
// ---------------------------------------------------------------------------

fn extract_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            items
                .iter()
                .filter_map(|p| {
                    if p.get("type").and_then(Value::as_str) == Some("text") {
                        p.get("text").and_then(Value::as_str).map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("")
        }
        _ => String::new(),
    }
}

fn extract_tool_calls(content: &Value) -> Vec<Value> {
    let Value::Array(items) = content else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|p| {
            if p.get("type").and_then(Value::as_str) != Some("toolCall") {
                return None;
            }
            let id = p.get("id").and_then(Value::as_str)?;
            let name = p.get("name").and_then(Value::as_str)?;
            Some(json!({
                "id": id,
                "type": "function",
                "function": { "name": name }
            }))
        })
        .collect()
}

fn extract_images(content: &Value) -> Vec<Value> {
    let Value::Array(items) = content else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|p| {
            p.get("type").and_then(Value::as_str) == Some("image")
                && p.get("data").and_then(Value::as_str).is_some()
                && p.get("mimeType").and_then(Value::as_str).is_some()
        })
        .cloned()
        .collect()
}

fn describe_image(image: &Value) -> String {
    let mime = image
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("unknown type");
    let data = image.get("data").and_then(Value::as_str).unwrap_or("");
    if data.is_empty() {
        return format!("[image {}]", mime);
    }
    let kb = (data.len() * 3) / 4 / 1024;
    format!("[image {} ~{}KB]", mime, kb)
}

fn render_content_with_image_markers(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|p| {
                let part_type = p.get("type").and_then(Value::as_str)?;
                match part_type {
                    "text" => p.get("text").and_then(Value::as_str).map(|s| s.to_string()),
                    "image" => Some(describe_image(p)),
                    _ => None,
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn to_data_uri(image: &Value) -> Option<String> {
    let data = image.get("data").and_then(Value::as_str)?;
    let data = data.replace(char::is_whitespace, "");
    if data.is_empty() || !is_valid_base64(&data) {
        return None;
    }
    let mime = image.get("mimeType").and_then(Value::as_str)?;
    if mime.contains(';') {
        return None;
    }
    Some(format!("data:{};base64,{}", mime, data))
}

fn is_valid_base64(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let len = s.len();
    let valid_chars = s.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=');
    if !valid_chars {
        return false;
    }
    // Check padding
    let padding_count = s.chars().rev().take_while(|&c| c == '=').count();
    if padding_count > 2 {
        return false;
    }
    // Length should be multiple of 4 after padding
    (len % 4 == 0) || (padding_count > 0 && (len + 4 - padding_count) % 4 == 0)
}

fn to_multimodal_content(text: &str, images: &[Value]) -> Value {
    let urls: Vec<String> = images.iter().filter_map(|img| to_data_uri(img)).collect();
    if urls.is_empty() {
        return Value::String(text.to_string());
    }
    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(json!({ "type": "text", "text": text }));
    }
    for url in urls {
        content.push(json!({ "type": "image_url", "image_url": { "url": url } }));
    }
    Value::Array(content)
}

fn mark_data_uris(text: &str) -> String {
    // Simple implementation: replace data URIs with size markers
    let mut result = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == 'd' && text[result.len()..].starts_with("data:") {
            // Found potential data URI
            let start = result.len();
            let mut uri = String::from("data:");
            for _ in 0..5 {
                chars.next();
            }
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || c == ',' || c == ';' {
                    break;
                }
                uri.push(c);
                chars.next();
            }
            // Skip to end of base64 data
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || (!c.is_ascii_alphanumeric() && c != '+' && c != '/' && c != '=') {
                    break;
                }
                uri.push(c);
                chars.next();
            }
            let kb = (uri.len() * 3) / 4 / 1024;
            result.push_str(&format!("[data uri ~{}KB]", kb));
        } else {
            result.push(c);
        }
    }
    result
}

fn safe_stringify(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| format!("{:?}", value)),
    }
}

fn find_number(bag: &Value, keys: &[&str], depth: usize) -> Option<u64> {
    if depth == 0 || !bag.is_object() {
        return None;
    }
    let obj = bag.as_object()?;
    for key in keys {
        if let Some(Value::Number(n)) = obj.get(*key) {
            if let Some(v) = n.as_u64() {
                if v > 0 {
                    return Some(v);
                }
            }
        }
    }
    for value in obj.values() {
        if let Some(found) = find_number(value, keys, depth - 1) {
            return Some(found);
        }
    }
    None
}

const MAX_TOKENS_KEYS: &[&str] = &[
    "max_tokens",
    "max_completion_tokens",
    "max_output_tokens",
    "maxOutputTokens",
    "maxTokens",
];

const THINKING_BUDGET_KEYS: &[&str] = &[
    "thinking_token_budget",
    "thinking_budget",
    "thinking_budget_tokens",
    "budget_tokens",
    "thinkingBudget",
];

fn pick_cache_retention(payload: &Value) -> Option<String> {
    let obj = payload.as_object()?;
    if let Some(Value::String(s)) = obj.get("prompt_cache_retention") {
        return Some(s.clone());
    }
    if let Some(Value::Array(system)) = obj.get("system") {
        for block in system {
            if let Some(ttl) = block
                .get("cache_control")
                .and_then(|cc| cc.get("ttl"))
                .and_then(Value::as_str)
            {
                return Some(ttl.to_string());
            }
        }
    }
    None
}

fn pick_tool_choice(payload: &Value) -> Option<String> {
    let obj = payload.as_object()?;
    let value = obj.get("tool_choice")?;
    if let Some(s) = value.as_str() {
        return Some(s.to_string());
    }
    if let Some(t) = value.get("type").and_then(Value::as_str) {
        return Some(t.to_string());
    }
    None
}

fn extract_model_parameters(
    payload: &Value,
    model: Option<&Value>,
    thinking_level: Option<&str>,
) -> Option<Map<String, Value>> {
    let mut out = Map::new();
    
    if let Some(max_tokens) = find_number(payload, MAX_TOKENS_KEYS, 2) {
        out.insert("max_tokens".to_string(), json!(max_tokens));
    }
    
    if let (Some(model), Some(level)) = (model, thinking_level) {
        if model.get("reasoning").and_then(Value::as_bool).unwrap_or(false)
            && level != "off"
        {
            out.insert("thinking_level".to_string(), json!(level));
        }
    }
    
    if let Some(budget) = find_number(payload, THINKING_BUDGET_KEYS, 2) {
        out.insert("thinking_budget_tokens".to_string(), json!(budget));
    }
    
    if let Some(retention) = pick_cache_retention(payload) {
        out.insert("prompt_cache_retention".to_string(), json!(retention));
    }
    
    if let Some(tier) = payload.get("service_tier").and_then(Value::as_str) {
        out.insert("service_tier".to_string(), json!(tier));
    }
    
    if let Some(choice) = pick_tool_choice(payload) {
        out.insert("tool_choice".to_string(), json!(choice));
    }
    
    if let Some(model) = model {
        if let Some(Value::Object(params)) = model.get("samplingParams") {
            for (key, value) in params {
                if let Some(v) = payload.get(key) {
                    match v {
                        Value::Number(n) => {
                            out.insert(key.clone(), json!(n));
                        }
                        Value::String(s) => {
                            if s.len() <= 200 {
                                out.insert(key.clone(), json!(s));
                            }
                        }
                        _ => {
                            let text = serde_json::to_string(v).unwrap_or_default();
                            if text.len() <= 200 {
                                out.insert(key.clone(), json!(text));
                            }
                        }
                    }
                }
            }
        }
    }
    
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

// ---------------------------------------------------------------------------
// Usage/cost details
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PiUsage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    reasoning: Option<u64>,
    cache_write_1h: Option<u64>,
    cost: Option<CostDetails>,
}

#[derive(Debug, Clone)]
struct CostDetails {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    total: u64,
}

fn parse_usage(value: &Value) -> Option<PiUsage> {
    let input = value.get("input")?.as_u64()?;
    let output = value.get("output")?.as_u64()?;
    let cache_read = value.get("cacheRead")?.as_u64()?;
    let cache_write = value.get("cacheWrite")?.as_u64()?;
    let reasoning = value.get("reasoning").and_then(Value::as_u64);
    let cache_write_1h = value.get("cacheWrite1h").and_then(Value::as_u64);
    
    let cost = value.get("cost").and_then(|c| {
        Some(CostDetails {
            input: c.get("input")?.as_u64()?,
            output: c.get("output")?.as_u64()?,
            cache_read: c.get("cacheRead")?.as_u64()?,
            cache_write: c.get("cacheWrite")?.as_u64()?,
            total: c.get("total")?.as_u64()?,
        })
    });
    
    Some(PiUsage {
        input,
        output,
        cache_read,
        cache_write,
        reasoning,
        cache_write_1h,
        cost,
    })
}

fn resolve_reasoning_split(usage: &PiUsage) -> (u64, bool) {
    let reasoning = usage.reasoning.unwrap_or(0);
    let can_split = reasoning > 0 && reasoning <= usage.output;
    (reasoning, can_split)
}

fn build_usage_details(usage: &PiUsage) -> Option<Map<String, Value>> {
    let mut details = Map::new();
    
    if usage.input > 0 {
        details.insert("input".to_string(), json!(usage.input));
    }
    
    let (reasoning, can_split) = resolve_reasoning_split(usage);
    let output = if can_split {
        usage.output - reasoning
    } else {
        usage.output
    };
    
    if output > 0 {
        details.insert("output".to_string(), json!(output));
    }
    
    if can_split {
        details.insert("output_reasoning_tokens".to_string(), json!(reasoning));
    }
    
    if usage.cache_read > 0 {
        details.insert("cache_read_input_tokens".to_string(), json!(usage.cache_read));
    }
    
    if usage.cache_write > 0 {
        details.insert("cache_creation_input_tokens".to_string(), json!(usage.cache_write));
    }
    
    if details.is_empty() {
        None
    } else {
        Some(details)
    }
}

fn build_cost_details(usage: &PiUsage) -> Option<Map<String, Value>> {
    let cost = usage.cost.as_ref()?;
    if cost.total == 0 {
        return None;
    }
    
    let mut details = Map::new();
    details.insert("total".to_string(), json!(cost.total));
    
    if cost.input > 0 {
        details.insert("input".to_string(), json!(cost.input));
    }
    
    if cost.output > 0 {
        let (reasoning, can_split) = resolve_reasoning_split(usage);
        if can_split {
            let reasoning_cost = (cost.output as f64 * (reasoning as f64 / usage.output as f64)) as u64;
            let non_reasoning_cost = cost.output - reasoning_cost;
            if non_reasoning_cost > 0 {
                details.insert("output".to_string(), json!(non_reasoning_cost));
            }
            if reasoning_cost > 0 {
                details.insert("output_reasoning_tokens".to_string(), json!(reasoning_cost));
            }
        } else {
            details.insert("output".to_string(), json!(cost.output));
        }
    }
    
    if cost.cache_read > 0 {
        details.insert("cache_read_input_tokens".to_string(), json!(cost.cache_read));
    }
    
    if cost.cache_write > 0 {
        details.insert("cache_creation_input_tokens".to_string(), json!(cost.cache_write));
    }
    
    Some(details)
}

// ---------------------------------------------------------------------------
// ChatML conversion
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ChatMlToolCall {
    id: String,
    name: String,
    arguments: Option<String>,
}

#[derive(Debug, Clone)]
struct ChatMlThinkingPart {
    content: String,
    redacted: bool,
}

#[derive(Debug, Clone)]
enum ChatMlMessage {
    User { content: String },
    Assistant {
        content: Option<String>,
        thinking: Vec<ChatMlThinkingPart>,
        tool_calls: Vec<ChatMlToolCall>,
    },
    Tool {
        tool_call_id: String,
        name: String,
        content: String,
        is_error: bool,
    },
}

fn extract_thinking(content: &Value) -> Vec<ChatMlThinkingPart> {
    let Value::Array(items) = content else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|p| {
            if p.get("type").and_then(Value::as_str) != Some("thinking") {
                return None;
            }
            let thinking = p.get("thinking").and_then(Value::as_str)?;
            if thinking.trim().is_empty() {
                return None;
            }
            Some(ChatMlThinkingPart {
                content: mark_data_uris(thinking),
                redacted: p.get("redacted").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

fn history_tool_calls(content: &Value) -> Vec<ChatMlToolCall> {
    let Value::Array(items) = content else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|p| {
            if p.get("type").and_then(Value::as_str) != Some("toolCall") {
                return None;
            }
            let id = p.get("id").and_then(Value::as_str)?.to_string();
            let name = p.get("name").and_then(Value::as_str)?.to_string();
            let arguments = p.get("arguments").map(|args| {
                let redacted = redact_langfuse_keys(args);
                mark_data_uris(&safe_stringify(&redacted))
            });
            Some(ChatMlToolCall {
                id,
                name,
                arguments,
            })
        })
        .collect()
}

fn to_chat_ml_message(message: &Value) -> Option<ChatMlMessage> {
    let role = message.get("role").and_then(Value::as_str)?;
    
    match role {
        "user" => {
            let content = message.get("content")?;
            let text = if content.is_string() {
                content.as_str()?.to_string()
            } else {
                render_content_with_image_markers(content)
            };
            Some(ChatMlMessage::User {
                content: mark_data_uris(&text),
            })
        }
        "assistant" => {
            let content = message.get("content")?;
            let text = extract_text(content);
            let thinking = extract_thinking(content);
            let tool_calls = history_tool_calls(content);
            
            if text.is_empty() && thinking.is_empty() && tool_calls.is_empty() {
                return None;
            }
            
            Some(ChatMlMessage::Assistant {
                content: if text.is_empty() { None } else { Some(mark_data_uris(&text)) },
                thinking,
                tool_calls,
            })
        }
        "toolResult" => {
            let tool_call_id = message.get("toolCallId").and_then(Value::as_str)?.to_string();
            let name = message.get("toolName").and_then(Value::as_str)?.to_string();
            let content = message.get("content")?;
            let text = if content.is_string() {
                content.as_str()?.to_string()
            } else {
                render_content_with_image_markers(content)
            };
            let is_error = message.get("isError").and_then(Value::as_bool).unwrap_or(false);
            
            Some(ChatMlMessage::Tool {
                tool_call_id,
                name,
                content: mark_data_uris(&text),
                is_error,
            })
        }
        _ => None,
    }
}

fn build_history_input(messages: &Value) -> Option<Vec<ChatMlMessage>> {
    let Value::Array(items) = messages else {
        return None;
    };
    let history: Vec<ChatMlMessage> = items
        .iter()
        .filter_map(|msg| to_chat_ml_message(msg))
        .collect();
    if history.is_empty() {
        None
    } else {
        Some(history)
    }
}

fn chat_ml_message_to_value(msg: &ChatMlMessage) -> Value {
    match msg {
        ChatMlMessage::User { content } => {
            json!({ "role": "user", "content": content })
        }
        ChatMlMessage::Assistant { content, thinking, tool_calls } => {
            let mut result = json!({ "role": "assistant" });
            if let Some(content) = content {
                result["content"] = json!(content);
            }
            if !thinking.is_empty() {
                result["thinking"] = json!(thinking.iter().map(|t| {
                    let mut part = json!({ "type": "thinking", "content": t.content });
                    if t.redacted {
                        part["redacted"] = json!(true);
                    }
                    part
                }).collect::<Vec<_>>());
            }
            if !tool_calls.is_empty() {
                result["tool_calls"] = json!(tool_calls.iter().map(|tc| {
                    let mut call = json!({
                        "id": tc.id,
                        "type": "function",
                        "function": { "name": tc.name }
                    });
                    if let Some(args) = &tc.arguments {
                        call["function"]["arguments"] = json!(args);
                    }
                    call
                }).collect::<Vec<_>>());
            }
            result
        }
        ChatMlMessage::Tool { tool_call_id, name, content, is_error } => {
            let mut result = json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
                "name": name,
                "content": content
            });
            if *is_error {
                result["is_error"] = json!(true);
            }
            result
        }
    }
}

// ---------------------------------------------------------------------------
// State management
// ---------------------------------------------------------------------------

struct OpenGeneration {
    obs_id: String,
    index: u32,
    saw_first_token: bool,
    finished: bool,
}

struct OpenTool {
    obs_id: String,
    name: String,
    started_at: Instant,
}

struct PromptState {
    root_obs_id: String,
    trace_id: String,
    turn_number: u32,
    generation_count: u32,
    open_generation: Option<OpenGeneration>,
    open_tools: HashMap<String, OpenTool>,
    pending_tool_results: Vec<Value>,
    last_assistant_text: Option<String>,
    saw_error: bool,
    user_text: String,
    turn_images: Vec<Value>,
    system_prompt: Option<String>,
    session_id: String,
}

static PROMPT_STATE: OnceLock<Mutex<Option<PromptState>>> = OnceLock::new();

fn prompt_state() -> &'static Mutex<Option<PromptState>> {
    PROMPT_STATE.get_or_init(|| Mutex::new(None))
}

// ---------------------------------------------------------------------------
// Span store
// ---------------------------------------------------------------------------

/// One in-flight observation.
///
/// Langfuse's legacy ingestion API had `span-create` / `span-update` events, but
/// the OTLP endpoint has no "update" verb: a span is exported **once**, complete
/// (start + end + attributes). Observations are therefore buffered here and
/// serialized at flush time; only records that have been ended are exported, so
/// a later flush never rewrites an observation Langfuse already finalized.
#[derive(Clone)]
struct SpanRecord {
    /// OTLP span id (16 lowercase hex).
    id: String,
    /// OTLP trace id (32 lowercase hex).
    trace_id: String,
    parent_id: Option<String>,
    name: String,
    /// "span" | "generation"
    obs_type: String,
    start_ms: u128,
    end_ms: Option<u128>,
    input: Option<Value>,
    output: Option<Value>,
    metadata: Map<String, Value>,
    model: Option<String>,
    model_parameters: Option<Map<String, Value>>,
    usage_details: Option<Map<String, Value>>,
    cost_details: Option<Map<String, Value>>,
    level: Option<String>,
    status_message: Option<String>,
    completion_start_ms: Option<u128>,
}

impl SpanRecord {
    fn new(
        id: String,
        trace_id: String,
        parent_id: Option<String>,
        name: String,
        obs_type: String,
        input: Option<Value>,
        metadata: Option<Map<String, Value>>,
    ) -> Self {
        Self {
            id,
            trace_id,
            parent_id,
            name,
            obs_type,
            start_ms: now_epoch_ms(),
            end_ms: None,
            input,
            output: None,
            metadata: metadata.unwrap_or_default(),
            model: None,
            model_parameters: None,
            usage_details: None,
            cost_details: None,
            level: None,
            status_message: None,
            completion_start_ms: None,
        }
    }
}

struct TracerState {
    spans: Mutex<Vec<SpanRecord>>,
}

static TRACER: OnceLock<Arc<TracerState>> = OnceLock::new();

fn tracer() -> Arc<TracerState> {
    TRACER
        .get_or_init(|| {
            Arc::new(TracerState {
                spans: Mutex::new(Vec::new()),
            })
        })
        .clone()
}

/// Merge an observation update (the shape `update_observation` receives) into
/// the buffered record. Unknown keys land in metadata, mirroring the legacy
/// `span-update` body which forwarded arbitrary fields.
fn apply_span_update(span: &mut SpanRecord, updates: &Value) {
    let Some(map) = updates.as_object() else {
        return;
    };
    for (key, value) in map {
        match key.as_str() {
            "input" => span.input = Some(value.clone()),
            "output" => span.output = Some(value.clone()),
            "model" => span.model = value.as_str().map(String::from),
            "modelParameters" => {
                span.model_parameters = value.as_object().cloned();
            }
            "usageDetails" => {
                span.usage_details = value.as_object().cloned();
            }
            "costDetails" => {
                span.cost_details = value.as_object().cloned();
            }
            "level" => span.level = value.as_str().map(String::from),
            "statusMessage" => span.status_message = value.as_str().map(String::from),
            "completionStartTime" => {
                span.completion_start_ms = value.as_str().and_then(iso_to_epoch_ms);
            }
            "metadata" => {
                if let Some(obj) = value.as_object() {
                    for (k, v) in obj {
                        span.metadata.insert(k.clone(), v.clone());
                    }
                }
            }
            "endTime" => span.end_ms = value.as_str().and_then(iso_to_epoch_ms),
            _ => {
                span.metadata.insert(key.clone(), value.clone());
            }
        }
    }
}
// ---------------------------------------------------------------------------
// OTLP export (Langfuse v4)
// ---------------------------------------------------------------------------

fn attr_str(key: &str, value: &str) -> Value {
    json!({ "key": key, "value": { "stringValue": value } })
}

/// OTLP/JSON maps int64 to a string (proto3 JSON mapping); the Langfuse SDK
/// transformers decode that back to a number.
fn attr_int(key: &str, value: i64) -> Value {
    json!({ "key": key, "value": { "intValue": value.to_string() } })
}

fn attr_double(key: &str, value: f64) -> Value {
    json!({ "key": key, "value": { "doubleValue": value } })
}

/// Langfuse reads `input`/`output`/metadata as JSON strings, so objects and
/// arrays are serialized instead of being sent as structured values.
fn attr_json(key: &str, value: &Value) -> Value {
    let text = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    attr_str(key, &text)
}

/// Langfuse expects a JSON array of strings for tags.
fn attr_tags(key: &str, tags: &[&str]) -> Value {
    let text = Value::Array(tags.iter().map(|t| json!(t)).collect()).to_string();
    attr_str(key, &text)
}

fn put_unix_nanos(span: &mut Map<String, Value>, key: &str, ms: u128) {
    span.insert(key.into(), json!((ms * 1_000_000).to_string()));
}

fn span_to_otlp(rec: &SpanRecord, is_root: bool, inherited_parent: Option<&str>) -> Value {
    let mut attrs = vec![attr_str("langfuse.observation.type", &rec.obs_type)];
    if let Some(ref input) = rec.input {
        attrs.push(attr_json("langfuse.observation.input", input));
    }
    if let Some(ref output) = rec.output {
        attrs.push(attr_json("langfuse.observation.output", output));
    }
    if let Some(ref model) = rec.model {
        attrs.push(attr_str("langfuse.observation.model.name", model));
    }
    if let Some(ref params) = rec.model_parameters {
        attrs.push(attr_str(
            "langfuse.observation.model.parameters",
            &Value::Object(params.clone()).to_string(),
        ));
    }
    if let Some(ref usage) = rec.usage_details {
        for (k, v) in usage {
            let key = format!("langfuse.observation.usage_details.{k}");
            attrs.push(match v {
                Value::Number(n) if n.is_i64() => attr_int(&key, n.as_i64().unwrap_or(0)),
                Value::Number(n) => attr_double(&key, n.as_f64().unwrap_or(0.0)),
                other => attr_str(&key, &other.to_string()),
            });
        }
    }
    if let Some(ref cost) = rec.cost_details {
        for (k, v) in cost {
            if let Some(n) = v.as_f64() {
                attrs.push(attr_double(&format!("langfuse.observation.cost_details.{k}"), n));
            }
        }
    }
    if let Some(ms) = rec.completion_start_ms {
        attrs.push(attr_str(
            "langfuse.observation.completion_start_time",
            &epoch_ms_to_iso(ms),
        ));
    }
    if let Some(ref level) = rec.level {
        attrs.push(attr_str("langfuse.observation.level", level));
    }
    if let Some(ref status) = rec.status_message {
        attrs.push(attr_str("langfuse.observation.status_message", status));
    }
    for (k, v) in &rec.metadata {
        // The root span carries trace-level fields; children carry observation
        // metadata. Langfuse reads both per span.
        let key = if is_root {
            format!("langfuse.trace.metadata.{k}")
        } else {
            format!("langfuse.observation.metadata.{k}")
        };
        attrs.push(attr_json(&key, v));
    }
    if is_root {
        attrs.push(attr_str("langfuse.trace.name", TRACE_NAME));
        attrs.push(attr_tags("langfuse.trace.tags", BASE_TAGS));
        if let Some(Value::String(session)) = rec.metadata.get("session_id") {
            attrs.push(attr_str("langfuse.session.id", session));
        }
    }

    let mut span = Map::new();
    span.insert("traceId".into(), json!(rec.trace_id));
    span.insert("spanId".into(), json!(rec.id));
    match rec.parent_id.as_deref().or(inherited_parent) {
        Some(parent) => {
            span.insert("parentSpanId".into(), json!(parent));
        }
        None => {}
    }
    span.insert("name".into(), json!(rec.name));
    span.insert("kind".into(), json!(1)); // SPAN_KIND_INTERNAL
    put_unix_nanos(&mut span, "startTimeUnixNano", rec.start_ms);
    // An un-ended record is only exported when the caller forces it (session
    // shutdown): collapse it to a zero-length span rather than dropping it.
    put_unix_nanos(&mut span, "endTimeUnixNano", rec.end_ms.unwrap_or(rec.start_ms));
    span.insert("attributes".into(), Value::Array(attrs));
    Value::Object(span)
}

/// POST spans to `{base_url}/api/public/otel/v1/traces` (OTLP/HTTP JSON).
fn flush_otlp(spans: Vec<Value>) -> Result<(), String> {
    if spans.is_empty() {
        return Ok(());
    }
    let cfg = load_config().ok_or("langfuse config not valid")?;
    let payload = json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [
                    attr_str("service.name", "rpi"),
                    attr_str("service.version", EXTENSION_VERSION),
                    attr_str("telemetry.sdk.name", SDK_NAME),
                    attr_str("telemetry.sdk.language", "rust"),
                    attr_str("telemetry.sdk.version", EXTENSION_VERSION),
                ]
            },
            "scopeSpans": [{
                "scope": { "name": SDK_NAME, "version": EXTENSION_VERSION },
                "spans": spans
            }]
        }]
    });
    let url = format!("{}{}", cfg.base_url, OTEL_PATH);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(&url)
        .basic_auth(&cfg.public_key, Some(&cfg.secret_key))
        .header("x-langfuse-sdk-name", SDK_NAME)
        .header("x-langfuse-sdk-version", EXTENSION_VERSION)
        // Without this, a native OTel producer is treated as a legacy SDK on v4
        // and shows up with up to 15 minutes of delay (dual-write pipeline).
        .header("x-langfuse-ingestion-version", "4")
        .json(&payload)
        .send()
        .map_err(|e| format!("langfuse otlp flush failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().unwrap_or_default();
        return Err(format!("langfuse otlp error {status}: {text}"));
    }
    Ok(())
}

/// Export every buffered observation that has been ended, and drop it from the
/// store once the export succeeded (so a retry cannot duplicate work, and a
/// failed export keeps the data for the next flush).
fn flush_ended_spans(include_open: bool) -> Result<usize, String> {
    let inherited_parent = std::env::var(ENV_PARENT_SPAN_ID).ok();
    let mut spans = Vec::new();
    {
        let state = tracer();
        let store = state.spans.lock().unwrap();
        for rec in store.iter() {
            if rec.end_ms.is_some() || include_open {
                spans.push(span_to_otlp(rec, rec.parent_id.is_none(), inherited_parent.as_deref()));
            }
        }
    }
    if spans.is_empty() {
        return Ok(0);
    }
    let sent = spans.len();
    debug_log(&format!("exporting {sent} span(s) via OTLP"));
    match flush_otlp(spans) {
        Ok(()) => set_status("langfuse ✓ (trace sent)"),
        Err(e) => {
            set_status("langfuse ✗ (flush failed)");
            return Err(e);
        }
    }
    let state = tracer();
    let mut store = state.spans.lock().unwrap();
    store.retain(|rec| rec.end_ms.is_none() && !include_open);
    // Keep the store from growing without bound when exports keep failing.
    if store.len() > 2000 {
        let drop_to = store.len() - 1000;
        store.drain(0..drop_to);
    }
    Ok(sent)
}

/// Report a failed export.
///
/// **Never** `eprintln!` here: in fullscreen TUI mode a raw stderr write lands
/// at the cursor — i.e. on the input editor row — and corrupts the alt-screen
/// (same reason pi-rust's anthropic provider stopped writing stderr). The status
/// line is the intended channel; the detailed message stays behind
/// `RPI_LANGFUSE_DEBUG=1`, where the operator explicitly asked for stderr output.
fn report_flush_error(err: &str) {
    set_status("langfuse ✗ (flush failed)");
    debug_log(&format!("flush error: {err}"));
}

fn maybe_flush(_state: &TracerState) {
    if let Err(e) = flush_ended_spans(false) {
        report_flush_error(&e);
    }
}

fn force_flush(_state: &TracerState) {
    if let Err(e) = flush_ended_spans(true) {
        report_flush_error(&e);
    }
}
// ---------------------------------------------------------------------------
// Observation management
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

fn now_epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn epoch_ms_to_iso(ms: u128) -> String {
    let secs = (ms / 1000) as u64;
    let millis = (ms % 1000) as u32;
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

/// Parse `YYYY-MM-DDTHH:MM:SS.mmmZ` into epoch millis. Returns `None` for
/// anything that does not match (the value is then dropped, never guessed).
fn iso_to_epoch_ms(ts: &str) -> Option<u128> {
    let bytes = ts.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let num = |range: std::ops::Range<usize>| ts.get(range)?.parse::<i128>().ok();
    let y = num(0..4)?;
    let m = num(5..7)?;
    let d = num(8..10)?;
    let hh = num(11..13)?;
    let mm = num(14..16)?;
    let ss = num(17..19)?;
    let ms = num(20..23).unwrap_or(0);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // days_from_civil (Howard Hinnant), proleptic Gregorian calendar
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = y_adj - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hh * 3600 + mm * 60 + ss;
    if secs < 0 {
        return None;
    }
    Some(secs as u128 * 1000 + ms as u128)
}

static ID_SEED: OnceLock<u64> = OnceLock::new();

fn id_seed() -> u64 {
    *ID_SEED.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    })
}

/// 64-bit entropy stream (process id + startup time + call counter) rendered as
/// lowercase hex. OTLP ids must be 16 (span) / 32 (trace) hex chars, so they can
/// no longer be human-readable strings like `obs-....`.
fn next_hex(bytes: usize) -> String {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut x = id_seed()
        ^ (u64::from(std::process::id()) << 32)
        ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut out = String::with_capacity(bytes * 2);
    while out.len() < bytes * 2 {
        // splitmix64: cheap, well mixed, no extra dependency
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.push_str(&format!("{z:016x}"));
    }
    out.truncate(bytes * 2);
    if out.bytes().all(|b| b == b'0') {
        // an all-zero id is rejected by Langfuse
        out.pop();
        out.push('1');
    }
    out
}

/// OTLP trace id: 16 bytes as 32 lowercase hex chars.
fn trace_id() -> String {
    next_hex(16)
}

/// OTLP span id: 8 bytes as 16 lowercase hex chars.
fn obs_id() -> String {
    next_hex(8)
}

fn trace_metadata() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("source".into(), json!("pi"));
    m.insert("extension".into(), json!(EXTENSION_NAME));
    m.insert("extension_version".into(), json!(EXTENSION_VERSION));
    if let Ok(cwd) = std::env::current_dir() {
        m.insert("cwd".into(), json!(cwd.to_string_lossy()));
    }
    m
}

fn start_observation(
    name: &str,
    obs_type: &str,
    trace_id: &str,
    parent_obs_id: Option<&str>,
    input: Option<Value>,
    metadata: Option<Map<String, Value>>,
) -> String {
    let id = obs_id();
    let record = SpanRecord::new(
        id.clone(),
        trace_id.to_string(),
        parent_obs_id.map(String::from),
        name.to_string(),
        obs_type.to_string(),
        input,
        metadata,
    );
    tracer().spans.lock().unwrap().push(record);
    id
}

fn update_observation(obs_id_param: &str, updates: Value) {
    let state = tracer();
    let mut store = state.spans.lock().unwrap();
    match store.iter_mut().find(|rec| rec.id == obs_id_param) {
        Some(rec) => apply_span_update(rec, &updates),
        None => debug_log(&format!("update for unknown observation {obs_id_param}")),
    }
}

fn end_observation(obs_id_param: &str) {
    let state = tracer();
    let mut store = state.spans.lock().unwrap();
    match store.iter_mut().find(|rec| rec.id == obs_id_param) {
        Some(rec) => {
            if rec.end_ms.is_none() {
                rec.end_ms = Some(now_epoch_ms());
            }
        }
        None => debug_log(&format!("end for unknown observation {obs_id_param}")),
    }
}

// ---------------------------------------------------------------------------
// Event handlers
// ---------------------------------------------------------------------------

extern "C" fn on_session_start(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    debug_log("event: SessionStart");
    if !is_enabled() {
        return 0;
    }
    // Reset state
    *prompt_state().lock().unwrap() = None;
    0
}

extern "C" fn on_before_agent_start(event: StablePluginEvent, _: *mut c_void) -> i32 {
    debug_log("event: BeforeAgentStart");
    if !is_enabled() {
        return 0;
    }
    
    let data_str = unsafe { event.payload.data.data.to_string_lossy() };
    let data: Value = serde_json::from_str(&data_str).unwrap_or(Value::Null);
    
    // Finalize previous state if exists
    if let Some(state) = prompt_state().lock().unwrap().take() {
        finalize_root(&state, true);
    }
    
    let prompt = data.get("prompt").and_then(Value::as_str).unwrap_or("");
    let images = data.get("images").and_then(|i| i.as_array()).cloned().unwrap_or_default();
    
    let user_text = if images.is_empty() {
        prompt.to_string()
    } else {
        let image_descs: Vec<String> = images.iter().map(|img| describe_image(img)).collect();
        format!("{}\n{}", prompt, image_descs.join("\n"))
    };
    
    let session_id = std::env::var("RPI_SESSION_ID").unwrap_or_else(|_| "default".to_string());
    let turn_number = resolve_turn_number(&session_id, prompt);
    
    let is_subagent = std::env::var(ENV_PARENT_TRACE_ID).is_ok();
    
    let root_name = if is_subagent {
        SUBAGENT_ROOT_OBSERVATION_NAME
    } else {
        ROOT_OBSERVATION_NAME
    };

    // A subagent continues the parent's trace and hangs off the parent's root
    // span, so Langfuse nests the whole subagent run under the turn that
    // spawned it (this is what ENV_PARENT_TRACE_ID/SPAN_ID carry).
    let trace_id_val = if is_subagent {
        std::env::var(ENV_PARENT_TRACE_ID).unwrap_or_else(|_| trace_id())
    } else {
        trace_id()
    };
    
    let mut metadata = trace_metadata();
    metadata.insert("session_id".into(), json!(session_id));
    metadata.insert("turn_number".into(), json!(turn_number));
    
    if is_subagent {
        metadata.insert("pi_subagent".into(), json!(true));
        if let Ok(depth) = std::env::var(ENV_PARENT_DEPTH) {
            metadata.insert("subagent_depth".into(), json!(depth));
        }
        if let Ok(parent_session) = std::env::var(ENV_PARENT_SESSION_ID) {
            metadata.insert("parent_session_id".into(), json!(parent_session));
        }
    }
    
    let root_obs_id = start_observation(
        root_name,
        "span",
        &trace_id_val,
        None,
        Some(json!({ "role": "user", "content": user_text })),
        Some(metadata),
    );
    
    // Publish parent context for subagents
    if !is_subagent {
        std::env::set_var(ENV_PARENT_TRACE_ID, &trace_id_val);
        std::env::set_var(ENV_PARENT_SPAN_ID, &root_obs_id);
        std::env::set_var(ENV_PARENT_SESSION_ID, &session_id);
        std::env::set_var(ENV_PARENT_DEPTH, "0");
    }
    
    let state = PromptState {
        root_obs_id,
        trace_id: trace_id_val,
        turn_number,
        generation_count: 0,
        open_generation: None,
        open_tools: HashMap::new(),
        pending_tool_results: Vec::new(),
        last_assistant_text: None,
        saw_error: false,
        user_text,
        turn_images: images,
        system_prompt: None,
        session_id,
    };
    
    *prompt_state().lock().unwrap() = Some(state);

    // Armed: the trace exists from here on. The footer flips to
    // `langfuse ✓ (trace sent)` once an export actually succeeded.
    set_status("langfuse ✓");

    0
}

fn resolve_turn_number(session_id: &str, prompt: &str) -> u32 {
    // Simple implementation: use turn number from state or default to 1
    static TURN_COUNTER: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    let counter = TURN_COUNTER.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = counter.lock().unwrap();
    let entry = map.entry(session_id.to_string()).or_insert(0);
    *entry += 1;
    *entry
}

extern "C" fn on_context(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    // Context event carries conversation history - we'll use it for generation input
    // For now, just acknowledge it
    0
}

extern "C" fn on_agent_start(event: StablePluginEvent, _: *mut c_void) -> i32 {
    debug_log("event: AgentStart");
    if !is_enabled() {
        return 0;
    }
    // AgentStart is dispatched with empty payload - don't read data
    let _ = event;
    
    // Capture system prompt if available
    // In the new protocol, system prompt is retrieved via ctx.getSystemPrompt()
    // but we don't have direct access to that in the Rust SDK
    // For now, we'll skip this
    
    0
}

extern "C" fn on_before_provider_request(event: StablePluginEvent, _: *mut c_void) -> i32 {
    debug_log("event: BeforeProviderRequest");
    if !is_enabled() {
        return 0;
    }
    
    let mut state_guard = prompt_state().lock().unwrap();
    let Some(state) = state_guard.as_mut() else {
        return 0;
    };
    
    // Close previous generation if still open
    if let Some(gen) = state.open_generation.take() {
        if !gen.finished {
            update_observation(&gen.obs_id, json!({
                "level": "WARNING",
                "statusMessage": "Superseded by provider retry",
                "metadata": { "superseded": true }
            }));
            end_observation(&gen.obs_id);
        }
    }
    
    let data_str = unsafe { event.payload.data.data.to_string_lossy() };
    let data: Value = serde_json::from_str(&data_str).unwrap_or(Value::Null);
    
    let index = state.generation_count + 1;
    state.generation_count = index;
    
    // Build generation input
    let mut generation_input = Vec::new();
    
    // Add system prompt if available
    if let Some(system_prompt) = &state.system_prompt {
        generation_input.push(json!({ "role": "system", "content": system_prompt }));
    }
    
    // Add user prompt for first generation
    if index == 1 {
        generation_input.push(json!({ "role": "user", "content": state.user_text }));
    } else if !state.pending_tool_results.is_empty() {
        // Add pending tool results
        for result in &state.pending_tool_results {
            generation_input.push(result.clone());
        }
    }
    
    let input = if generation_input.is_empty() {
        None
    } else {
        Some(Value::Array(generation_input))
    };
    
    let model = data.get("model").and_then(Value::as_str);
    let model_params = extract_model_parameters(&data, None, None);
    
    let mut metadata = Map::new();
    metadata.insert("assistant_index".into(), json!(index - 1));
    metadata.insert("input_source".into(), json!("delta"));
    
    if let Some(model) = model {
        metadata.insert("model".into(), json!(model));
    }
    
    let obs_id = start_observation(
        GENERATION_PREFIX,
        "generation",
        &state.trace_id,
        Some(&state.root_obs_id),
        input,
        Some(metadata),
    );
    
    if let Some(params) = model_params {
        update_observation(&obs_id, json!({ "modelParameters": params }));
    }
    
    state.open_generation = Some(OpenGeneration {
        obs_id,
        index,
        saw_first_token: false,
        finished: false,
    });
    
    0
}

extern "C" fn on_message_update(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    
    let mut state_guard = prompt_state().lock().unwrap();
    let Some(state) = state_guard.as_mut() else {
        return 0;
    };
    
    let Some(gen) = &mut state.open_generation else {
        return 0;
    };
    
    if gen.finished || gen.saw_first_token {
        return 0;
    }
    
    let data_str = unsafe { event.payload.message.message.to_string_lossy() };
    let data: Value = serde_json::from_str(&data_str).unwrap_or(Value::Null);
    
    let content = data.get("content").unwrap_or(&Value::Null);
    let text = extract_text(content);
    
    if !text.is_empty() {
        gen.saw_first_token = true;
        update_observation(&gen.obs_id, json!({
            "completionStartTime": now_iso()
        }));
    }
    
    0
}

extern "C" fn on_message_end(event: StablePluginEvent, _: *mut c_void) -> i32 {
    debug_log("event: MessageEnd");
    if !is_enabled() {
        return 0;
    }
    
    let mut state_guard = prompt_state().lock().unwrap();
    let Some(state) = state_guard.as_mut() else {
        return 0;
    };
    
    let data_str = unsafe { event.payload.message.message.to_string_lossy() };
    let data: Value = serde_json::from_str(&data_str).unwrap_or(Value::Null);
    
    // Check if this is an assistant message (new protocol uses "role", old uses "kind")
    let role = data.get("role").or_else(|| data.get("kind")).and_then(Value::as_str);
    if role != Some("assistant") {
        // Handle user message for backwards compatibility
        if role == Some("user") {
            if let Some(content) = data.get("content") {
                let text = extract_text(content);
                state.user_text = text;
            }
        }
        return 0;
    }
    
    let Some(gen) = state.open_generation.take() else {
        return 0;
    };
    
    if gen.finished {
        return 0;
    }
    
    let content = data.get("content").unwrap_or(&Value::Null);
    let text = extract_text(content);
    let tool_calls = extract_tool_calls(content);
    let thinking = extract_thinking(content);
    
    let stop_reason = data.get("stopReason").and_then(Value::as_str);
    let is_error = stop_reason == Some("error") || stop_reason == Some("aborted");
    
    if stop_reason == Some("error") {
        state.saw_error = true;
    }
    
    // Build output
    let mut output = json!({ "role": "assistant" });
    if !text.is_empty() {
        output["content"] = json!(text);
    }
    if !thinking.is_empty() {
        output["thinking"] = json!(thinking.iter().map(|t| {
            let mut part = json!({ "type": "thinking", "content": t.content });
            if t.redacted {
                part["redacted"] = json!(true);
            }
            part
        }).collect::<Vec<_>>());
    }
    if !tool_calls.is_empty() {
        output["tool_calls"] = json!(tool_calls);
    }
    
    let mut updates = json!({
        "output": output,
    });
    
    if let Some(model) = data.get("responseModel").or_else(|| data.get("model")).and_then(Value::as_str) {
        updates["model"] = json!(model);
    }
    
    if let Some(usage) = data.get("usage").and_then(parse_usage) {
        if let Some(usage_details) = build_usage_details(&usage) {
            updates["usageDetails"] = json!(usage_details);
        }
        if let Some(cost_details) = build_cost_details(&usage) {
            updates["costDetails"] = json!(cost_details);
        }
    }
    
    if is_error {
        updates["level"] = json!("ERROR");
        let error_msg = data.get("errorMessage").and_then(Value::as_str).unwrap_or("error");
        updates["statusMessage"] = json!(error_msg);
    }
    
    let mut metadata = Map::new();
    metadata.insert("tool_count".into(), json!(tool_calls.len()));
    if let Some(reason) = stop_reason {
        metadata.insert("stop_reason".into(), json!(reason));
    }
    updates["metadata"] = json!(metadata);
    
    update_observation(&gen.obs_id, updates);
    end_observation(&gen.obs_id);
    
    if !text.is_empty() {
        state.last_assistant_text = Some(text);
    }
    state.pending_tool_results.clear();

    // `rpi -p` (print), `--mode json` and `--mode rpc` never deliver
    // AgentEnd / AgentSettled / SessionShutdown — only the four events above —
    // so a one-shot run would otherwise buffer spans and exit without ever
    // exporting them. When the assistant stops without pending tool calls the
    // turn is over: close the root observation and export right away.
    // Interactive sessions are unaffected (AgentSettled still finalizes), and a
    // tool-call reply keeps the turn open for the follow-up request.
    let turn_finished = tool_calls.is_empty()
        && state.open_generation.is_none()
        && state.open_tools.is_empty();
    drop(state_guard);

    if turn_finished {
        if let Some(finished) = prompt_state().lock().unwrap().take() {
            finalize_root(&finished, false);
        }
        let state = tracer();
        force_flush(&state);
    } else {
        // Mid-turn: export whatever already ended (generations, finished tools)
        // so long tool-calling runs stay observable, and keep the open root.
        let state = tracer();
        maybe_flush(&state);
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
    
    let mut state_guard = prompt_state().lock().unwrap();
    let Some(state) = state_guard.as_mut() else {
        return 0;
    };
    
    let input: Value = serde_json::from_str(&params).unwrap_or(Value::Null);
    let redacted_input = redact_langfuse_keys(&input);
    
    let mut metadata = Map::new();
    metadata.insert("tool_name".into(), json!(tool_name));
    metadata.insert("tool_id".into(), json!(tool_call_id));
    
    let obs_id = start_observation(
        &format!("{} {}", TOOL_PREFIX, tool_name),
        "span",
        &state.trace_id,
        Some(&state.root_obs_id),
        Some(redacted_input),
        Some(metadata),
    );
    
    state.open_tools.insert(tool_call_id.clone(), OpenTool {
        obs_id: obs_id.clone(),
        name: tool_name.clone(),
        started_at: Instant::now(),
    });
    
    0
}

extern "C" fn on_tool_execution_end(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    
    let tool_call_id = unsafe { event.payload.tool_result.tool_call_id.to_string_lossy() };
    let result = unsafe { event.payload.tool_result.result.to_string_lossy() };
    let is_error = unsafe { event.payload.tool_result.is_error } != 0;
    
    let mut state_guard = prompt_state().lock().unwrap();
    let Some(state) = state_guard.as_mut() else {
        return 0;
    };
    
    let Some(tool) = state.open_tools.remove(&tool_call_id) else {
        return 0;
    };
    
    let result_value: Value = serde_json::from_str(&result).unwrap_or(Value::Null);
    
    let output = if let Some(content) = result_value.get("content") {
        render_content_with_image_markers(content)
    } else {
        safe_stringify(&result_value)
    };
    
    if is_error {
        state.saw_error = true;
    }
    
    // Check for tool result usage
    if let Some(usage_value) = result_value.get("usage") {
        if let Some(usage) = parse_usage(usage_value) {
            let mut metadata = Map::new();
            metadata.insert("tool_call_id".into(), json!(tool_call_id));
            metadata.insert("tool_name".into(), json!(tool.name));
            metadata.insert("source".into(), json!("tool_result_usage"));
            metadata.insert("model_is_session_model".into(), json!(true));
            
            let usage_obs_id = start_observation(
                TOOL_USAGE_OBSERVATION_NAME,
                "generation",
                &state.trace_id,
                Some(&tool.obs_id),
                None,
                Some(metadata),
            );
            
            let mut updates = Map::new();
            if let Some(usage_details) = build_usage_details(&usage) {
                updates.insert("usageDetails".to_string(), json!(usage_details));
            }
            if let Some(cost_details) = build_cost_details(&usage) {
                updates.insert("costDetails".to_string(), json!(cost_details));
            }
            
            if !updates.is_empty() {
                update_observation(&usage_obs_id, Value::Object(updates));
            }
            end_observation(&usage_obs_id);
        }
    }
    
    let mut updates = json!({
        "output": output,
    });
    
    if is_error {
        updates["level"] = json!("ERROR");
        updates["statusMessage"] = json!("Tool execution failed");
    }
    
    let mut metadata = Map::new();
    metadata.insert("is_error".into(), json!(is_error));
    updates["metadata"] = json!(metadata);
    
    update_observation(&tool.obs_id, updates);
    end_observation(&tool.obs_id);
    
    // Add to pending tool results
    state.pending_tool_results.push(json!({
        "role": "tool",
        "tool_call_id": tool_call_id,
        "name": tool.name,
        "content": output,
    }));
    
    0
}

extern "C" fn on_session_before_compact(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    // Mark compaction start time
    // This will be used in on_session_compact
    0
}

extern "C" fn on_session_compact(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    
    let data_str = unsafe { event.payload.data.data.to_string_lossy() };
    let data: Value = serde_json::from_str(&data_str).unwrap_or(Value::Null);
    
    let entry = data.get("compactionEntry");
    let reason = data.get("reason").and_then(Value::as_str).unwrap_or("unknown");
    let will_retry = data.get("willRetry").and_then(Value::as_bool).unwrap_or(false);
    
    if let Some(entry) = entry {
        let summary = entry.get("summary").and_then(Value::as_str).unwrap_or("");
        let usage = entry.get("usage").and_then(parse_usage);
        
        let mut metadata = Map::new();
        metadata.insert("compaction_reason".into(), json!(reason));
        metadata.insert("will_retry".into(), json!(will_retry));
        
        if let Some(tokens_before) = entry.get("tokensBefore").and_then(Value::as_u64) {
            metadata.insert("tokens_before".into(), json!(tokens_before));
        }
        
        let trace_id_val = trace_id();
        let obs_id = start_observation(
            COMPACTION_OBSERVATION_NAME,
            "generation",
            &trace_id_val,
            None,
            None,
            Some(metadata),
        );
        
        let mut updates = Map::new();
        if !summary.is_empty() {
            updates.insert("output".to_string(), json!({ "role": "assistant", "content": summary }));
        }
        
        if let Some(usage) = usage {
            if let Some(usage_details) = build_usage_details(&usage) {
                updates.insert("usageDetails".to_string(), json!(usage_details));
            }
            if let Some(cost_details) = build_cost_details(&usage) {
                updates.insert("costDetails".to_string(), json!(cost_details));
            }
        }
        
        if !updates.is_empty() {
            update_observation(&obs_id, Value::Object(updates));
        }
        end_observation(&obs_id);
    }
    
    0
}

extern "C" fn on_session_tree(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if !is_enabled() {
        return 0;
    }
    
    let data_str = unsafe { event.payload.data.data.to_string_lossy() };
    let data: Value = serde_json::from_str(&data_str).unwrap_or(Value::Null);
    
    let entry = data.get("summaryEntry");
    if entry.is_none() {
        return 0;
    }
    let entry = entry.unwrap();
    
    let summary = entry.get("summary").and_then(Value::as_str).unwrap_or("");
    let usage = entry.get("usage").and_then(parse_usage);
    
    let mut metadata = Map::new();
    if data.get("fromExtension").and_then(Value::as_bool).unwrap_or(false) {
        metadata.insert("from_extension".into(), json!(true));
    }
    
    let trace_id_val = trace_id();
    let obs_id = start_observation(
        BRANCH_SUMMARY_OBSERVATION_NAME,
        "generation",
        &trace_id_val,
        None,
        None,
        Some(metadata),
    );
    
    let mut updates = Map::new();
    if !summary.is_empty() {
        updates.insert("output".to_string(), json!({ "role": "assistant", "content": summary }));
    }
    
    if let Some(usage) = usage {
        if let Some(usage_details) = build_usage_details(&usage) {
            updates.insert("usageDetails".to_string(), json!(usage_details));
        }
        if let Some(cost_details) = build_cost_details(&usage) {
            updates.insert("costDetails".to_string(), json!(cost_details));
        }
    }
    
    if !updates.is_empty() {
        update_observation(&obs_id, Value::Object(updates));
    }
    end_observation(&obs_id);
    
    0
}

extern "C" fn on_agent_settled(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    debug_log("event: AgentSettled");
    if !is_enabled() {
        return 0;
    }
    
    if let Some(state) = prompt_state().lock().unwrap().take() {
        finalize_root(&state, false);
    }
    
    let state = tracer();
    force_flush(&state);
    
    0
}

fn finalize_root(state: &PromptState, cancelled: bool) {
    // Close any open generation
    if let Some(gen) = &state.open_generation {
        if !gen.finished {
            update_observation(&gen.obs_id, json!({
                "level": "WARNING",
                "statusMessage": "Generation interrupted",
                "metadata": { "interrupted": true }
            }));
            end_observation(&gen.obs_id);
        }
    }
    
    // Close any open tools
    for (_, tool) in &state.open_tools {
        update_observation(&tool.obs_id, json!({
            "level": "WARNING",
            "statusMessage": "Tool run interrupted",
            "metadata": { "interrupted": true }
        }));
        end_observation(&tool.obs_id);
    }
    
    // Update root with final input/output
    let mut updates = Map::new();
    
    if !state.turn_images.is_empty() {
        updates.insert("input".to_string(), to_multimodal_content(&state.user_text, &state.turn_images));
    }
    
    if let Some(text) = &state.last_assistant_text {
        updates.insert("output".to_string(), json!({ "role": "assistant", "content": text }));
    }
    
    if state.saw_error {
        updates.insert("level".to_string(), json!("ERROR"));
    }
    
    let mut metadata = Map::new();
    if !state.turn_images.is_empty() {
        metadata.insert("image_count".into(), json!(state.turn_images.len()));
    }
    if cancelled {
        metadata.insert("cancelled".into(), json!(true));
    }
    
    if !metadata.is_empty() {
        updates.insert("metadata".to_string(), json!(metadata));
    }
    
    if !updates.is_empty() {
        update_observation(&state.root_obs_id, Value::Object(updates));
    }
    
    end_observation(&state.root_obs_id);
    
    // Withdraw parent context
    std::env::remove_var(ENV_PARENT_TRACE_ID);
    std::env::remove_var(ENV_PARENT_SPAN_ID);
    std::env::remove_var(ENV_PARENT_SESSION_ID);
    std::env::remove_var(ENV_PARENT_DEPTH);
}

extern "C" fn on_session_shutdown(_event: StablePluginEvent, _: *mut c_void) -> i32 {
    debug_log("event: SessionShutdown");
    if !is_enabled() {
        return 0;
    }
    
    if let Some(state) = prompt_state().lock().unwrap().take() {
        finalize_root(&state, true);
    }
    
    let state = tracer();
    force_flush(&state);
    
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
        .user_agent("rpi-langfuse/0.2")
        .build()
        .map_err(|e| e.to_string())
}

fn api_request(
    client: &reqwest::blocking::Client,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, String> {
    let cfg = load_config().ok_or("LANGFUSE_PUBLIC_KEY and LANGFUSE_SECRET_KEY are required")?;
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
            // Langfuse v4: the score *read* endpoints moved to Scores API v3
            // (`GET /api/public/scores` and `/v2/scores` return 404).
            let v = api_request(&client(30)?, "GET", &format!("/api/public/v3/scores{}", qs), None)?;
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
            let mut query = vec![format!("name={}", name)];
            if let Some(version) = p.get("version").and_then(Value::as_u64) {
                query.push(format!("version={}", version));
            }
            let qs = format!("?{}", query.join("&"));
            let v = api_request(&client(30)?, "GET", &format!("/api/public/prompts{}", qs), None)?;
            Ok(json!({"action":"get","prompt":v}).to_string())
        }
        "list" => {
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
            // Langfuse v4 removed GET /api/public/traces/{id} (404). A trace is
            // now represented by its observations, so read them through
            // Observations API v2 filtered by traceId.
            let v = api_request(
                &client(30)?,
                "GET",
                &format!("/api/public/v2/observations?traceId={}&limit=200", id),
                None,
            )?;
            Ok(json!({"action":"get","traceId":id,"observations":v}).to_string())
        }
        "list" => {
            let mut query = Vec::new();
            if let Some(limit) = p.get("limit").and_then(Value::as_u64) {
                // Observations API v2 allows up to 1000 (v1 traces capped at 100)
                query.push(format!("limit={}", limit.clamp(1, 1000)));
            }
            if let Some(user_id) = p.get("userId").and_then(Value::as_str) {
                query.push(format!("userId={}", user_id));
            }
            // One row per trace, which is what the removed traces list returned.
            query.push("isRootObservation=true".into());
            let qs = format!("?{}", query.join("&"));
            let v = api_request(
                &client(30)?,
                "GET",
                &format!("/api/public/v2/observations{}", qs),
                None,
            )?;
            Ok(json!({"action":"list","observations":v}).to_string())
        }
        "update" => Err(
            "action 'update' is not supported on Langfuse v4: the legacy \
             trace-create ingestion event is rejected in events_only mode. \
             Trace name/session/metadata are set when the turn starts (they are \
             exported as OTLP root-span attributes); edit them in the Langfuse UI."
                .into(),
        ),
        _ => Err("action must be one of: get, list".into()),
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
        // Capture the host's action trampoline before anything else can run:
        // event handlers and the flush path both publish status through it.
        let _ = HOST_RUNTIME.set(HostRuntime {
            runtime_action: api.runtime_action,
            free_string: api.free_string,
            user_data: api.user_data,
        });

        if let Some(register_event) = api.register_event_handler {
            let handlers: &[(EventTag, EventHandlerFn)] = &[
                (EventTag::SessionStart, on_session_start),
                (EventTag::BeforeAgentStart, on_before_agent_start),
                (EventTag::Context, on_context),
                (EventTag::AgentStart, on_agent_start),
                (EventTag::BeforeProviderRequest, on_before_provider_request),
                (EventTag::MessageUpdate, on_message_update),
                (EventTag::MessageEnd, on_message_end),
                (EventTag::ToolExecutionStart, on_tool_execution_start),
                (EventTag::ToolExecutionEnd, on_tool_execution_end),
                (EventTag::SessionBeforeCompact, on_session_before_compact),
                (EventTag::SessionCompact, on_session_compact),
                (EventTag::SessionTree, on_session_tree),
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
                "Query Langfuse traces (Langfuse v4 Observations API v2). 'get' returns every observation of one trace; 'list' returns one root observation per trace. 'update' is gone in v4.",
                r#"{"type":"object","properties":{"action":{"type":"string","enum":["get","list"]},"id":{"type":"string","description":"trace id (action=get)"},"userId":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":1000}}}"#,
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

    /// The status footer is driven through the host's `SetStatus` runtime
    /// action. Capture what the plugin actually sends.
    static CAPTURED_STATUS: Mutex<Option<String>> = Mutex::new(None);

    extern "C" fn mock_runtime_action(
        action: u32,
        args: StbStringRef,
        out: *mut StbString,
        _user_data: *mut c_void,
    ) -> i32 {
        if action == RUNTIME_ACTION_SET_STATUS {
            // SAFETY: the host's trampoline guarantees the borrow is valid for
            // the duration of this call, and we copy out of it immediately.
            let text = unsafe { args.as_str() }.to_string();
            *CAPTURED_STATUS.lock().unwrap() = Some(text);
        }
        if !out.is_null() {
            unsafe { *out = StbString::empty() };
        }
        0
    }

    extern "C" fn mock_free_string(_s: StbString) {}

    #[test]
    fn set_status_sends_the_host_action() {
        // Only this test installs the host runtime, so `set` is uncontended.
        let _ = HOST_RUNTIME.set(HostRuntime {
            runtime_action: mock_runtime_action,
            free_string: mock_free_string,
            user_data: std::ptr::null_mut(),
        });
        set_status("langfuse ✓ (trace sent)");
        let captured = CAPTURED_STATUS
            .lock()
            .unwrap()
            .clone()
            .expect("the action must have been invoked");
        let payload: Value = serde_json::from_str(&captured).expect("args are JSON");
        assert_eq!(payload["key"], json!("langfuse"));
        assert_eq!(payload["value"], json!("langfuse ✓ (trace sent)"));
    }

    #[test]
    fn test_config_loading() {
        // This test requires environment variables to be set
        // Just verify the function doesn't panic
        let _ = load_config();
    }

    #[test]
    fn test_timestamp_format() {
        let ts = now_iso();
        assert!(ts.contains('T'));
        assert!(ts.ends_with('Z'));
    }

    #[test]
    fn obs_ids_are_unique_and_are_valid_otlp_span_ids() {
        let a = obs_id();
        let b = obs_id();
        assert_ne!(a, b, "observation ids must not repeat");
        assert_eq!(a.len(), 16, "OTLP span ids are 8 bytes = 16 hex chars");
        assert!(
            a.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "id {a} must be lowercase hex"
        );
        assert_ne!(a, "0000000000000000");
    }

    #[test]
    fn trace_ids_are_valid_otlp_trace_ids() {
        let t = trace_id();
        assert_eq!(t.len(), 32, "OTLP trace ids are 16 bytes = 32 hex chars");
        assert!(t
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // ids are per-process random-ish, so two calls must differ
        assert_ne!(trace_id(), t);
    }

    #[test]
    fn iso_and_epoch_millis_round_trip() {
        assert_eq!(iso_to_epoch_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            iso_to_epoch_ms("2024-02-29T12:34:56.789Z"),
            Some(1_709_210_096_789)
        );
        assert_eq!(epoch_ms_to_iso(1_709_210_096_789), "2024-02-29T12:34:56.789Z");
        assert_eq!(iso_to_epoch_ms("not-a-timestamp"), None);
        assert_eq!(iso_to_epoch_ms("2024-13-01T00:00:00.000Z"), None);
        let now = now_epoch_ms();
        assert!(now > 1_700_000_000_000, "now_epoch_ms must be epoch millis");
    }

    #[test]
    fn otlp_spans_carry_langfuse_attributes() {
        let trace = trace_id();
        let root_id = obs_id();
        let mut metadata = Map::new();
        metadata.insert("session_id".into(), json!("sess-1"));
        let mut root = SpanRecord::new(
            root_id.clone(),
            trace.clone(),
            None,
            ROOT_OBSERVATION_NAME.into(),
            "span".into(),
            Some(json!({ "role": "user", "content": "hi" })),
            Some(metadata),
        );
        root.end_ms = Some(root.start_ms + 5);
        let span = span_to_otlp(&root, true, None);
        assert_eq!(span["traceId"], json!(trace));
        assert_eq!(span["spanId"], json!(root_id));
        assert!(span.get("parentSpanId").is_none(), "a root span has no parent");
        assert_eq!(
            span["startTimeUnixNano"],
            json!((root.start_ms * 1_000_000).to_string())
        );
        let attrs = span["attributes"].as_array().unwrap();
        let attr = |key: &str| {
            attrs
                .iter()
                .find(|a| a["key"] == json!(key))
                .unwrap_or_else(|| panic!("missing attribute {key}"))
        };
        assert_eq!(
            attr("langfuse.trace.name")["value"]["stringValue"],
            json!(TRACE_NAME)
        );
        assert_eq!(
            attr("langfuse.session.id")["value"]["stringValue"],
            json!("sess-1")
        );

        // A child generation: ints must be OTLP/JSON strings, and a missing
        // parent falls back to the inherited (subagent) span when provided.
        let child_id = obs_id();
        let mut child = SpanRecord::new(
            child_id.clone(),
            trace.clone(),
            Some(root_id.clone()),
            format!("{GENERATION_PREFIX} 1"),
            "generation".into(),
            None,
            None,
        );
        child.model = Some("m".into());
        let mut usage = Map::new();
        usage.insert("input".into(), json!(11));
        child.usage_details = Some(usage);
        child.end_ms = Some(child.start_ms + 3);
        let span = span_to_otlp(&child, false, Some("ffffffffffffffff"));
        assert_eq!(span["parentSpanId"], json!(root_id));
        let attrs = span["attributes"].as_array().unwrap();
        let attr = |key: &str| {
            attrs
                .iter()
                .find(|a| a["key"] == json!(key))
                .unwrap_or_else(|| panic!("missing attribute {key}"))
        };
        assert_eq!(
            attr("langfuse.observation.type")["value"]["stringValue"],
            json!("generation")
        );
        assert_eq!(
            attr("langfuse.observation.usage_details.input")["value"]["intValue"],
            json!("11")
        );

        // No parent recorded and no inherited parent: span is exported parentless.
        let mut orphan = child.clone();
        orphan.parent_id = None;
        let span = span_to_otlp(&orphan, false, None);
        assert!(span.get("parentSpanId").is_none());
    }

    /// Live round-trip against the configured Langfuse instance. Ignored by
    /// default because it needs network + real credentials:
    ///
    /// ```text
    /// LANGFUSE_BASE_URL=https://langfuse.laofu.online \
    /// LANGFUSE_PUBLIC_KEY=pk-lf-... LANGFUSE_SECRET_KEY=sk-lf-... \
    /// cargo test -p rpi-langfuse -- --ignored live_otlp_round_trip --nocapture
    /// ```
    /// End-to-end test of the *plugin* itself: it drives the real event
    /// handlers with the payload shapes the host produces, then reads the
    /// result back from Langfuse. This is what verifies the OTLP rewrite
    /// without depending on the host dispatching events (print/json modes do
    /// not dispatch them today).
    ///
    /// ```text
    /// LANGFUSE_BASE_URL=https://langfuse.laofu.online \
    /// LANGFUSE_PUBLIC_KEY=pk-lf-... LANGFUSE_SECRET_KEY=sk-lf-... \
    /// cargo test -p rpi-langfuse -- --ignored live_plugin_event_flow --nocapture
    /// ```
    #[test]
    #[ignore = "live network test"]
    fn live_plugin_event_flow() {
        assert!(
            load_config().is_some(),
            "set LANGFUSE_BASE_URL/PUBLIC_KEY/SECRET_KEY first"
        );
        let tag = |s: &str| StablePluginEvent::data(EventTag::BeforeAgentStart, StbString::from_string(s.into()));
        let _ = tag; // keep the closure type explicit above

        // Fresh state, and make sure we are treated as a top-level run.
        *prompt_state().lock().unwrap() = None;
        tracer().spans.lock().unwrap().clear();
        std::env::remove_var(ENV_PARENT_TRACE_ID);
        std::env::remove_var(ENV_PARENT_SPAN_ID);

        // 1) A turn starts: the plugin opens the root observation.
        let start = json!({ "prompt": "rpi-langfuse integration test", "images": [] });
        let rc = on_before_agent_start(
            StablePluginEvent::data(
                EventTag::BeforeAgentStart,
                StbString::from_string(start.to_string()),
            ),
            std::ptr::null_mut(),
        );
        assert_eq!(rc, 0);
        // the plugin publishes its trace id for subagents through this env var
        let trace = std::env::var(ENV_PARENT_TRACE_ID).expect("trace id published");
        assert_eq!(trace.len(), 32);

        // 2) A model request opens a generation observation.
        let req = json!({
            "model": "rpi-integration-test",
            "provider": "test",
            "maxTokens": 64,
            "temperature": 0.0,
        });
        on_before_provider_request(
            StablePluginEvent::data(
                EventTag::BeforeProviderRequest,
                StbString::from_string(req.to_string()),
            ),
            std::ptr::null_mut(),
        );

        // 3) The assistant message closes it (usage + output).
        let msg = json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "integration ok" }],
            "stopReason": "endTurn",
            "usage": { "input": 7, "output": 3, "totalTokens": 10 },
        });
        on_message_end(
            StablePluginEvent::message(EventTag::MessageEnd, StbString::from_string(msg.to_string())),
            std::ptr::null_mut(),
        );

        // 4) The turn settles: finalize + export over OTLP.
        on_agent_settled(StablePluginEvent::empty(EventTag::AgentSettled), std::ptr::null_mut());
        assert!(
            tracer().spans.lock().unwrap().is_empty(),
            "a successful export must drain the span store"
        );
        eprintln!("exported trace {trace}");

        // 5) Read it back through Observations API v2.
        let client = client(30).expect("client");
        let mut text = String::new();
        let mut seen = false;
        for attempt in 0..10 {
            let v = api_request(
                &client,
                "GET",
                &format!("/api/public/v2/observations?traceId={trace}&limit=50"),
                None,
            )
            .expect("read back");
            text = v.to_string();
            if text.contains(&trace) && text.contains(GENERATION_PREFIX) {
                eprintln!("visible after {attempt} poll(s)");
                seen = true;
                break;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        assert!(
            seen,
            "plugin output never showed up on v2/observations: {}",
            &text[..text.len().min(400)]
        );
        assert!(
            text.contains(ROOT_OBSERVATION_NAME),
            "root observation name missing: {}",
            &text[..text.len().min(400)]
        );
        assert!(
            text.contains(GENERATION_PREFIX),
            "generation observation name missing: {}",
            &text[..text.len().min(400)]
        );
    }

    #[test]
    #[ignore = "live network test"]
    fn live_otlp_round_trip() {
        assert!(
            load_config().is_some(),
            "set LANGFUSE_BASE_URL/PUBLIC_KEY/SECRET_KEY first"
        );
        let trace = trace_id();
        let root_id = obs_id();
        let mut metadata = Map::new();
        metadata.insert("session_id".into(), json!("rpi-langfuse-live-test"));
        let mut root = SpanRecord::new(
            root_id.clone(),
            trace.clone(),
            None,
            ROOT_OBSERVATION_NAME.into(),
            "span".into(),
            Some(json!({ "role": "user", "content": "round trip input" })),
            Some(metadata),
        );
        root.end_ms = Some(now_epoch_ms());
        let mut gen = SpanRecord::new(
            obs_id(),
            trace.clone(),
            Some(root_id.clone()),
            format!("{GENERATION_PREFIX} 1"),
            "generation".into(),
            Some(json!({ "role": "user", "content": "hi" })),
            None,
        );
        gen.model = Some("rpi-live-test".into());
        gen.end_ms = Some(now_epoch_ms());
        flush_otlp(vec![
            span_to_otlp(&root, true, None),
            span_to_otlp(&gen, false, None),
        ])
        .expect("otlp flush must succeed");
        eprintln!("flushed trace {trace}");

        // Read back through Observations API v2 (v4 has no trace getter).
        let client = client(30).expect("client");
        let mut seen = false;
        for attempt in 0..10 {
            let v = api_request(
                &client,
                "GET",
                &format!("/api/public/v2/observations?traceId={trace}"),
                None,
            )
            .expect("read back");
            let text = v.to_string();
            if text.contains(&trace) && text.contains(&root_id) {
                eprintln!("visible after {attempt} poll(s)");
                seen = true;
                break;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        assert!(seen, "trace {trace} never showed up on v2/observations");
    }

    #[test]
    fn test_extract_text() {
        assert_eq!(extract_text(&json!("hello")), "hello");
        assert_eq!(
            extract_text(&json!([
                { "type": "text", "text": "a" },
                { "type": "text", "text": "b" }
            ])),
            "ab"
        );
    }

    #[test]
    fn test_extract_tool_calls() {
        let content = json!([
            { "type": "text", "text": "hello" },
            { "type": "toolCall", "id": "call_1", "name": "bash" }
        ]);
        let calls = extract_tool_calls(&content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "call_1");
        assert_eq!(calls[0]["function"]["name"], "bash");
    }

    #[test]
    fn test_usage_details() {
        let usage = PiUsage {
            input: 100,
            output: 50,
            cache_read: 20,
            cache_write: 10,
            reasoning: Some(10),
            cache_write_1h: None,
            cost: None,
        };
        let details = build_usage_details(&usage).unwrap();
        assert_eq!(details.get("input").unwrap(), &json!(100));
        assert_eq!(details.get("output").unwrap(), &json!(40)); // 50 - 10 reasoning
        assert_eq!(details.get("output_reasoning_tokens").unwrap(), &json!(10));
        assert_eq!(details.get("cache_read_input_tokens").unwrap(), &json!(20));
        assert_eq!(details.get("cache_creation_input_tokens").unwrap(), &json!(10));
    }

    #[test]
    fn test_chat_ml_conversion() {
        let msg = json!({
            "role": "user",
            "content": "hello"
        });
        let chat_ml = to_chat_ml_message(&msg).unwrap();
        match chat_ml {
            ChatMlMessage::User { content } => assert_eq!(content, "hello"),
            _ => panic!("Expected User message"),
        }
    }
}
