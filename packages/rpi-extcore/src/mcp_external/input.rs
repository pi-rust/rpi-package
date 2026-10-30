//! Validated, lossless MCP2026 input-required results and explicit caller policy.
use super::*;

pub const MAX_INPUT_BYTES: usize = 64 * 1024;
pub const MAX_INPUT_ROUNDS: u32 = 8;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct InputRequest {
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

// Private representation forces all construction/deserialization through validation.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(transparent)]
pub struct InputRequired(Value);
impl InputRequired {
    pub fn from_value(method: &str, value: Value) -> Result<Self, String> {
        if !matches!(method, "tools/call" | "resources/read" | "prompts/get") {
            return Err("input_required is invalid for this MCP method".into());
        }
        if serde_json::to_vec(&value).map_err(|e| e.to_string())?.len() > MAX_INPUT_BYTES {
            return Err("MCP input_required exceeds input byte limit".into());
        }
        let obj = value
            .as_object()
            .ok_or("invalid MCP input_required object")?;
        if obj.get("resultType") != Some(&json!("input_required")) {
            return Err("invalid MCP input_required discriminator".into());
        }
        if !obj.contains_key("inputRequests") && !obj.contains_key("requestState") {
            return Err("MCP input_required needs inputs or state".into());
        }
        if let Some(state) = obj.get("requestState") {
            if !state.is_string() {
                return Err("MCP requestState must be an opaque string".into());
            }
        }
        if let Some(requests) = obj.get("inputRequests") {
            for request in requests
                .as_object()
                .ok_or("MCP inputRequests must be keyed object")?
                .values()
            {
                let request = request.as_object().ok_or("invalid MCP input request")?;
                if !matches!(
                    request.get("method").and_then(Value::as_str),
                    Some("elicitation/create" | "sampling/createMessage" | "roots/list")
                ) {
                    return Err("invalid MCP input request method".into());
                }
                if request.get("params").is_some_and(|p| !p.is_object()) {
                    return Err("MCP input request params must be object".into());
                }
            }
        }
        Ok(Self(value))
    }
    pub fn as_value(&self) -> &Value {
        &self.0
    }
    pub fn input_requests(&self) -> Option<BTreeMap<String, InputRequest>> {
        self.0
            .get("inputRequests")
            .map(|v| serde_json::from_value(v.clone()).expect("validated input requests"))
    }
    pub fn request_state(&self) -> Option<&str> {
        self.0.get("requestState").and_then(Value::as_str)
    }
}
impl Outcome {
    pub fn from_result(method: &str, value: Value) -> Result<Self, String> {
        if value.get("resultType") == Some(&json!("input_required")) {
            InputRequired::from_value(method, value).map(Self::InputRequired)
        } else {
            Ok(Self::Result(value))
        }
    }
}

/// Each enabled capability promises a handler in `resolve`. Defaults advertise nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct InputCapabilities {
    pub elicitation_form: bool,
    pub elicitation_url: bool,
    pub roots: bool,
    pub sampling: bool,
}
impl InputCapabilities {
    pub(super) fn metadata(self) -> Value {
        let mut value = json!({});
        if self.elicitation_form {
            value["elicitation"]["form"] = json!({});
        }
        if self.elicitation_url {
            value["elicitation"]["url"] = json!({});
        }
        if self.roots {
            value["roots"] = json!({});
        }
        if self.sampling {
            value["sampling"] = json!({});
        }
        value
    }
    pub(super) fn supports(self, input: &InputRequired) -> bool {
        input
            .input_requests()
            .unwrap_or_default()
            .values()
            .all(|r| match r.method.as_str() {
                "roots/list" => self.roots,
                "sampling/createMessage" => self.sampling,
                "elicitation/create" => match r
                    .params
                    .as_ref()
                    .and_then(|v| v.get("mode"))
                    .and_then(Value::as_str)
                {
                    None | Some("form") => self.elicitation_form,
                    Some("url") => self.elicitation_url,
                    _ => false,
                },
                _ => false,
            })
    }
}
#[derive(Clone, Debug)]
pub struct RetryContext {
    pub continuation_id: String,
    pub scope: String,
    pub server_id: String,
    pub native_request_id: String,
    pub previous_request_id: String,
    pub round: u32,
    /// Absolute deadline of the original continuation; retries must not renew it.
    pub deadline: std::time::Instant,
}
pub enum InputDecision {
    /// Values are input results, never JSON-RPC result/error envelopes.
    Retry {
        input_responses: Option<BTreeMap<String, Value>>,
    },
    Abandon,
}
/// Work must honor cancellation and finish cleanup before returning. No detached callbacks.
pub trait InputPolicy: Send + Sync {
    fn capabilities(&self) -> InputCapabilities;
    fn resolve(
        &self,
        context: RetryContext,
        input: InputRequired,
        cancellation: CancellationToken,
    ) -> BackendFuture<'_, Result<InputDecision, String>>;
}
#[derive(Default)]
pub struct ExternalOptions {
    pub input_policy: Option<Arc<dyn InputPolicy>>,
}

struct WithPolicy {
    backend: Arc<dyn Backend>,
    policy: Option<Arc<dyn InputPolicy>>,
}
impl Backend for WithPolicy {
    fn input_policy(&self) -> Option<Arc<dyn InputPolicy>> {
        self.policy.clone()
    }
    fn request(
        &self,
        operation: Operation,
        cancellation: CancellationToken,
    ) -> BackendFuture<'_, Result<Outcome, String>> {
        self.backend.request(operation, cancellation)
    }
    fn shutdown(&self) -> BackendFuture<'_, Result<(), String>> {
        self.backend.shutdown()
    }
}
pub async fn attach_with_options(
    host: Arc<HostSession>,
    config: McpConfig,
    external: ExternalConfig,
    options: ExternalOptions,
) -> Result<McpBinding, String> {
    attach_with_runtime_options(host, config, external, options, mcp::McpOptions::default()).await
}

/// Compose external input handling with optional trusted native HTTP credentials.
/// Existing callers keep their input-policy and header-only defaults unchanged.
pub async fn attach_with_runtime_options(
    host: Arc<HostSession>,
    config: McpConfig,
    mut external: ExternalConfig,
    options: ExternalOptions,
    runtime: mcp::McpOptions,
) -> Result<McpBinding, String> {
    external.backend = Arc::new(WithPolicy {
        backend: external.backend,
        policy: options.input_policy,
    });
    mcp::attach_external_with_options(host, config, external, runtime).await
}
