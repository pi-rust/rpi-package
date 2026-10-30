//! Validated MCP 2025-06-18 sampling seam, not a wire dispatcher.
//!
//! Remote prompts apply only to an isolated sample. Implementations MUST NOT use
//! foreground conversations, implicit credentials/accounts, transcript or environment
//! context. Preferences are advisory, never authorization. Executors must report
//! the actual model used and own cancellation and drop-safe resource cleanup.
use crate::Control;
use base64::Engine;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use std::{
    fmt,
    future::Future,
    pin::Pin,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

pub const MAX_SAMPLING_BYTES: usize = 256 * 1024;
pub const MAX_SAMPLING_MESSAGES: usize = 128;
pub const MAX_SAMPLING_TOKENS: u64 = 16_384;
pub const MAX_SYSTEM_PROMPT_BYTES: usize = 64 * 1024;

/// Fixed messages only: never carry prompts, credentials or executor diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SamplingError {
    InvalidParams,
    Unsupported,
    Denied,
    Unavailable,
    Cancelled,
    TimedOut,
    InvalidResult,
    ExecutionFailed,
}
impl SamplingError {
    pub fn code(self) -> i32 {
        match self {
            Self::InvalidParams | Self::Unsupported => -32602,
            _ => -32603,
        }
    }
    pub fn message(self) -> &'static str {
        match self {
            Self::InvalidParams => "Invalid sampling parameters",
            Self::Unsupported => "Unsupported sampling semantics",
            Self::Denied => "Sampling denied",
            Self::Unavailable => "Sampling executor unavailable",
            Self::Cancelled => "Sampling cancelled",
            Self::TimedOut => "Sampling deadline exceeded",
            Self::InvalidResult => "Invalid sampling result",
            Self::ExecutionFailed => "Sampling execution failed",
        }
    }
}
impl fmt::Display for SamplingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}
impl std::error::Error for SamplingError {}

/// Trusted caller identity. No default, native/ACP correlation or ambient lookup.
/// The caller must retire/cancel old generations; this seam has no registry.
#[derive(Clone, Eq, PartialEq)]
pub struct SamplingContext {
    pub cwd: PathBuf,
    pub target: String,
    pub namespace: String,
    pub scope: String,
    pub server: String,
    pub generation: u64,
    pub operation: String,
    pub deadline: Instant,
}
impl SamplingContext {
    fn validate(&self) -> Result<(), SamplingError> {
        for id in [&self.namespace, &self.scope, &self.server, &self.operation, &self.target] {
            if id.trim().is_empty() || id.len() > 4096 || id.chars().any(char::is_control) {
                return Err(SamplingError::InvalidParams);
            }
        }
        Ok(())
    }
}

/// Per-operation caller consent/budgets, not a global enable switch. All identity
/// fields must match exactly; authorization may only SHORTEN the original absolute
/// deadline, never extend or reset it. Never deserialize policy from remote request
/// fields. Consent and execution share this bounded deadline. Callers implement
/// rate/concurrency budgets.
#[derive(Clone)]
pub struct SamplingPolicy {
    pub context: SamplingContext,
    pub max_messages: usize,
    pub max_tokens: u64,
    pub max_duration: Duration,
}
impl SamplingPolicy {
    fn admit(
        &self,
        context: &SamplingContext,
        request: &SamplingRequest,
    ) -> Result<(), SamplingError> {
        let mut bounded_identity = context.clone();
        bounded_identity.deadline = self.context.deadline;
        if self.context != bounded_identity
            || self.context.deadline > context.deadline
            || !(1..=MAX_SAMPLING_MESSAGES).contains(&self.max_messages)
            || !(1..=MAX_SAMPLING_TOKENS).contains(&self.max_tokens)
            || self.max_duration.is_zero()
            || self.max_duration > Duration::from_secs(300)
            || request.messages().len() > self.max_messages
            || request.max_tokens() > self.max_tokens
            || self.context.deadline.saturating_duration_since(Instant::now()) > self.max_duration
        {
            return Err(SamplingError::Denied);
        }
        Ok(())
    }
}

/// Immutable validated wire object. Unknown bounded fields remain opaque; they
/// MUST NOT grant execution, account selection or context access. Value-based
/// parsing bounds canonical serialized bytes, not the already-allocated input.
#[derive(Clone, Serialize)]
#[serde(transparent)]
pub struct SamplingRequest(Value);
impl fmt::Debug for SamplingRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SamplingRequest([redacted])")
    }
}
impl<'de> Deserialize<'de> for SamplingRequest {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::parse(Value::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}
impl SamplingRequest {
    pub fn parse(value: Value) -> Result<Self, SamplingError> {
        validate_request(&value)?;
        Ok(Self(value))
    }
    pub fn as_value(&self) -> &Value {
        &self.0
    }
    pub fn messages(&self) -> &[Value] {
        self.0["messages"].as_array().expect("validated messages")
    }
    pub fn max_tokens(&self) -> u64 {
        self.0["maxTokens"].as_u64().expect("validated tokens")
    }
    pub fn system_prompt(&self) -> Option<&str> {
        self.0.get("systemPrompt").and_then(Value::as_str)
    }
    /// Advisory only. Does not select any provider, account or credentials.
    pub fn model_preferences(&self) -> Option<&Value> {
        self.0.get("modelPreferences")
    }
    fn text_only(&self) -> Result<(), SamplingError> {
        if self
            .messages()
            .iter()
            .any(|m| m["content"]["type"] != "text")
        {
            return Err(SamplingError::Unsupported);
        }
        Ok(())
    }
}

/// Validated assistant text response. An executor must supply its actual model;
/// validation cannot attest model provenance. No fallback model is fabricated.
#[derive(Clone, Serialize)]
#[serde(transparent)]
pub struct SamplingResult(Value);
impl fmt::Debug for SamplingResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SamplingResult([redacted])")
    }
}
impl<'de> Deserialize<'de> for SamplingResult {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::parse(Value::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}
impl SamplingResult {
    pub fn parse(value: Value) -> Result<Self, SamplingError> {
        validate_result(&value).map_err(|_| SamplingError::InvalidResult)?;
        Ok(Self(value))
    }
    pub fn as_value(&self) -> &Value {
        &self.0
    }
    pub fn model(&self) -> &str {
        self.0["model"].as_str().expect("validated model")
    }
}

pub type SamplingFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SamplingResult, SamplingError>> + Send + 'a>>;

/// Caller-owned, object-safe async execution. Future drop MUST release owned
/// leases/children/guards; no detached work may survive it. Implementations must
/// yield cooperatively: this wrapper cannot forcibly interrupt blocking code.
pub trait SamplingExecutor: Send + Sync {
    fn execute<'a>(
        &'a self,
        context: &'a SamplingContext,
        request: &'a SamplingRequest,
        control: &'a Control,
    ) -> SamplingFuture<'a>;
}

fn check(context: &SamplingContext, control: &Control) -> Result<(), SamplingError> {
    control.check().map_err(|_| SamplingError::Cancelled)?;
    if Instant::now() >= context.deadline {
        return Err(SamplingError::TimedOut);
    }
    Ok(())
}

/// No executor or explicit matching policy means no execution. This API does not
/// advertise anything. Select drops the owned future on cancellation/deadline;
/// post-checks refuse late success even if an executor blocks across the deadline.
/// Dropping this wrapper also drops that future; no tasks are spawned.
pub async fn execute_validated(
    executor: Option<&dyn SamplingExecutor>,
    policy: Option<&SamplingPolicy>,
    context: &SamplingContext,
    request: &SamplingRequest,
    control: &Control,
) -> Result<SamplingResult, SamplingError> {
    context.validate()?;
    check(context, control)?;
    validate_request(request.as_value())?;
    request.text_only()?;
    let policy = policy.ok_or(SamplingError::Denied)?;
    policy.admit(context, request)?;
    let bounded = &policy.context;
    check(context, control)?;
    check(bounded, control)?;
    let executor = executor.ok_or(SamplingError::Unavailable)?;
    let result = tokio::select! {
        biased;
        _ = control.cancelled() => return Err(SamplingError::Cancelled),
        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(bounded.deadline)) => return Err(SamplingError::TimedOut),
        result = executor.execute(bounded, request, control) => result,
    };
    check(context, control)?;
    check(bounded, control)?;
    let result = result?;
    validate_result(result.as_value()).map_err(|_| SamplingError::InvalidResult)?;
    check(context, control)?;
    check(bounded, control)?;
    Ok(result)
}

fn invalid<T>() -> Result<T, SamplingError> {
    Err(SamplingError::InvalidParams)
}
fn object(v: &Value) -> Result<&Map<String, Value>, SamplingError> {
    v.as_object().ok_or(SamplingError::InvalidParams)
}
// Count serialized bytes without allocating another potentially 16 MiB frame.
fn within_wire_budget(v: &Value) -> bool {
    struct Budget(usize);
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.0 { return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "sampling byte limit")); }
            self.0 -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    serde_json::to_writer(Budget(MAX_SAMPLING_BYTES), v).is_ok()
}
fn bounded(v: &Value) -> Result<(), SamplingError> {
    // Bound nesting even for a Value assembled without serde's recursion limit.
    fn depth(v: &Value, n: usize) -> bool {
        n <= 64
            && match v {
                Value::Array(a) => a.iter().all(|v| depth(v, n + 1)),
                Value::Object(o) => o.values().all(|v| depth(v, n + 1)),
                _ => true,
            }
    }
    if !depth(v, 0)
        || !within_wire_budget(v)
    {
        return invalid();
    }
    Ok(())
}
fn extensions(o: &Map<String, Value>) -> Result<(), SamplingError> {
    if o.get("_meta").is_some_and(|v| !v.is_object()) {
        return invalid();
    }
    // These are executable semantics, not forward-compatible opaque metadata.
    if [
        "tools",
        "toolChoice",
        "toolUse",
        "toolResult",
        "tool_calls",
        "function_call",
    ]
    .iter()
    .any(|k| o.contains_key(*k))
    {
        return Err(SamplingError::Unsupported);
    }
    Ok(())
}
fn string(v: &Value, max: usize) -> Result<&str, SamplingError> {
    v.as_str()
        .filter(|s| s.len() <= max)
        .ok_or(SamplingError::InvalidParams)
}
fn content(v: &Value) -> Result<(), SamplingError> {
    let o = object(v)?;
    extensions(o)?;
    match o.get("type").and_then(Value::as_str) {
        Some("text") => {
            string(
                o.get("text").ok_or(SamplingError::InvalidParams)?,
                MAX_SAMPLING_BYTES,
            )?;
        }
        Some(kind @ ("image" | "audio")) => {
            let data = string(
                o.get("data").ok_or(SamplingError::InvalidParams)?,
                MAX_SAMPLING_BYTES,
            )?;
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|_| SamplingError::InvalidParams)?;
            let mime = string(o.get("mimeType").ok_or(SamplingError::InvalidParams)?, 256)?;
            let parsed: mime::Mime = mime.parse().map_err(|_| SamplingError::InvalidParams)?;
            if parsed.type_().as_str() != kind {
                return invalid();
            }
        }
        _ => return Err(SamplingError::Unsupported),
    }
    if let Some(a) = o.get("annotations") {
        let a = object(a)?;
        if let Some(roles) = a.get("audience") {
            for r in roles.as_array().ok_or(SamplingError::InvalidParams)? {
                if r != "user" && r != "assistant" {
                    return invalid();
                }
            }
        }
        if let Some(p) = a.get("priority") {
            priority(p)?;
        }
        if let Some(t) = a.get("lastModified") {
            string(t, 256)?;
        }
    }
    Ok(())
}
fn priority(v: &Value) -> Result<(), SamplingError> {
    if !v
        .as_f64()
        .is_some_and(|n| n.is_finite() && (0.0..=1.0).contains(&n))
    {
        return invalid();
    }
    Ok(())
}
pub(crate) fn validate_request(v: &Value) -> Result<(), SamplingError> {
    bounded(v)?;
    let o = object(v)?;
    extensions(o)?;
    let messages = o
        .get("messages")
        .and_then(Value::as_array)
        .ok_or(SamplingError::InvalidParams)?;
    if messages.is_empty() || messages.len() > MAX_SAMPLING_MESSAGES {
        return invalid();
    }
    for m in messages {
        let m = object(m)?;
        extensions(m)?;
        if !m
            .get("role")
            .is_some_and(|r| r == "user" || r == "assistant")
        {
            return invalid();
        }
        content(m.get("content").ok_or(SamplingError::InvalidParams)?)?;
    }
    if !o
        .get("maxTokens")
        .and_then(Value::as_u64)
        .is_some_and(|n| (1..=MAX_SAMPLING_TOKENS).contains(&n))
    {
        return invalid();
    }
    if let Some(s) = o.get("systemPrompt") {
        string(s, MAX_SYSTEM_PROMPT_BYTES)?;
    }
    if o.get("includeContext").is_some_and(|c| c != "none") {
        return Err(SamplingError::Unsupported);
    }
    if let Some(t) = o.get("temperature") {
        if !t
            .as_f64()
            .is_some_and(|n| n.is_finite() && (0.0..=2.0).contains(&n))
        {
            return invalid();
        }
    }
    if let Some(s) = o.get("stopSequences") {
        let a = s.as_array().ok_or(SamplingError::InvalidParams)?;
        if a.len() > 128 {
            return invalid();
        }
        for s in a {
            string(s, 4096)?;
        }
    }
    if let Some(m) = o.get("metadata") {
        object(m)?;
    }
    if let Some(p) = o.get("modelPreferences") {
        let p = object(p)?;
        extensions(p)?;
        for k in ["costPriority", "speedPriority", "intelligencePriority"] {
            if let Some(v) = p.get(k) {
                priority(v)?;
            }
        }
        if let Some(h) = p.get("hints") {
            let hints = h.as_array().ok_or(SamplingError::InvalidParams)?;
            if hints.len() > 32 {
                return invalid();
            }
            for hint in hints {
                let hint = object(hint)?;
                extensions(hint)?;
                if let Some(name) = hint.get("name") {
                    string(name, 256)?;
                }
            }
        }
    }
    Ok(())
}
fn validate_result(v: &Value) -> Result<(), SamplingError> {
    bounded(v)?;
    let o = object(v)?;
    extensions(o)?;
    if o.get("role") != Some(&Value::String("assistant".into())) {
        return invalid();
    }
    let model = string(o.get("model").ok_or(SamplingError::InvalidParams)?, 256)?;
    if model.trim().is_empty() || model.chars().any(char::is_control) {
        return invalid();
    }
    let c = o.get("content").ok_or(SamplingError::InvalidParams)?;
    content(c)?;
    if c["type"] != "text" {
        return invalid();
    }
    if !o
        .get("stopReason")
        .is_some_and(|s| s == "endTurn" || s == "maxTokens" || s == "stopSequence")
    {
        return invalid();
    }
    Ok(())
}

/// Explicit trusted providers. No ambient executor or default consent.
#[derive(Clone)]
pub struct SamplingOptions {
    pub executor: Arc<dyn SamplingExecutor>,
    pub authorizer: Arc<dyn SamplingAuthorizer>,
}
impl fmt::Debug for SamplingOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SamplingOptions([redacted])")
    }
}
pub type AuthorizationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SamplingPolicy, SamplingError>> + Send + 'a>>;
/// Authorization is bound to this immutable host operation, not remote IDs.
/// A returned policy may only SHORTEN its original absolute deadline, never
/// extend/reset it; all other context fields must remain exact. Compute that
/// deadline before consent so UI and execution consume the same time budget.
/// Futures must yield cooperatively and release resources on drop.
pub trait SamplingAuthorizer: Send + Sync {
    fn authorize<'a>(
        &'a self,
        context: &'a SamplingContext,
        request: &'a SamplingRequest,
        control: &'a Control,
    ) -> AuthorizationFuture<'a>;
}

impl fmt::Debug for SamplingContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("SamplingContext([redacted])") }
}
impl fmt::Debug for SamplingPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("SamplingPolicy([redacted])") }
}
