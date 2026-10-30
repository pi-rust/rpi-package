//! Operation-scoped external MCP backends, independent of any adapter transport.
//!
//! Unlike the initialized stdio/HTTP runtime, every request supplies MCP2026
//! context. A backend is owned by one binding: it must supervise its work, honor
//! cancellation, and join owned resources in `shutdown`. Sharing application
//! pools is fine; sharing an operation registry across bindings is not.
use crate::{
    host::HostSession,
    mcp::{self, McpBinding, McpConfig, ToolDescriptor},
};
use rpi_mcp_core::{Control, RequestContext};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
};
pub use tokio_util::sync::CancellationToken;
mod input;
pub use input::*;

pub const PROTOCOL_VERSION: &str = "2026-07-28";
pub type BackendFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Implementations return an inner outcome, not a transport response envelope.
/// Cancellation must settle only after operation cleanup. Dropping the future
/// must not leave detached execution; shutdown joins any backend-owned workers.
/// Shutdown must be idempotent, including after failed attachment. A backend
/// that ignores cancellation can delay shutdown; this API cannot kill arbitrary
/// application work and does not claim otherwise.
pub trait Backend: Send + Sync {
    /// Optional neutral input handler; ordinary backends advertise no inputs.
    fn input_policy(&self) -> Option<Arc<dyn InputPolicy>> {
        None
    }
    fn request(
        &self,
        request: Operation,
        cancellation: CancellationToken,
    ) -> BackendFuture<'_, Result<Outcome, String>>;
    fn shutdown(&self) -> BackendFuture<'_, Result<(), String>>;
}

#[derive(Clone, Debug)]
pub struct Operation {
    pub scope: String,
    pub cwd: PathBuf,
    pub server_id: String,
    pub request_id: String,
    /// Native execution correlation, absent for descriptor discovery. This is
    /// not the logical MCP ID and must not be used as a transport-hop ID.
    pub native_request_id: Option<String>,
    pub method: String,
    /// Includes required version/capabilities/clientInfo metadata on EVERY call.
    pub params: Value,
}

/// Error provenance is retained; `data: None` differs from explicit JSON null.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProtocolError {
    pub code: i64,
    pub message: String,
    #[serde(
        default,
        deserialize_with = "present_data",
        skip_serializing_if = "Option::is_none"
    )]
    pub data: Option<Value>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}
fn present_data<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Result(Value),
    InputRequired(InputRequired),
    Error(ProtocolError),
}

/// MCP2026 MRTR state and requests remain opaque. No automatic approval/retry.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "resultType")]
pub enum ToolResult {
    #[serde(rename = "complete")]
    Complete {
        content: Vec<Value>,
        #[serde(flatten)]
        fields: BTreeMap<String, Value>,
    },
    #[serde(rename = "input_required")]
    InputRequired {
        #[serde(flatten)]
        fields: BTreeMap<String, Value>,
    },
}

pub struct ExternalConfig {
    /// Local tool namespace -> provider registration identity. No hidden session.
    pub servers: BTreeMap<String, String>,
    /// Required MCP metadata; additional context/progress metadata is preserved.
    pub metadata: BTreeMap<String, Value>,
    pub backend: Arc<dyn Backend>,
}

/// One binding configures native tools once for all transports. Native server
/// names and external names must be disjoint. The binding owns backend shutdown.
pub async fn attach(
    host: Arc<HostSession>,
    config: McpConfig,
    external: ExternalConfig,
) -> Result<McpBinding, String> {
    mcp::attach_external(host, config, external).await
}

pub(crate) struct Driver {
    config: ExternalConfig,
    capabilities: InputCapabilities,
    policy: Option<Arc<dyn InputPolicy>>,
    next: Mutex<u64>,
    prefix: String,
}
impl Driver {
    pub(crate) fn new(mut config: ExternalConfig, native: &McpConfig) -> Result<Self, String> {
        if config.servers.iter().any(|(name, id)| {
            name.trim().is_empty() || id.trim().is_empty() || native.servers.contains_key(name)
        }) {
            return Err(
                "external MCP names must be nonempty and disjoint from native servers".into(),
            );
        }
        if config
            .metadata
            .get("io.modelcontextprotocol/protocolVersion")
            != Some(&json!(PROTOCOL_VERSION))
            || !config
                .metadata
                .get("io.modelcontextprotocol/clientCapabilities")
                .is_some_and(Value::is_object)
            || !config
                .metadata
                .get("io.modelcontextprotocol/clientInfo")
                .is_some_and(|v| v["name"].is_string() && v["version"].is_string())
        {
            return Err(
                "external MCP requires explicit MCP2026 version, capabilities and clientInfo"
                    .into(),
            );
        }
        // Never trust caller metadata to advertise callbacks that do not exist.
        let policy = config.backend.input_policy();
        let capabilities = policy
            .as_ref()
            .map(|p| p.capabilities())
            .unwrap_or_default();
        config.metadata.insert(
            "io.modelcontextprotocol/clientCapabilities".into(),
            capabilities.metadata(),
        );
        Ok(Self {
            config,
            capabilities,
            policy,
            next: Mutex::new(0),
            prefix: uuid::Uuid::new_v4().to_string(),
        })
    }
    pub(crate) fn contains(&self, server: &str) -> bool {
        self.config.servers.contains_key(server)
    }
    async fn request(
        &self,
        context: &RequestContext,
        server: &str,
        method: &str,
        mut params: Value,
        native_request_id: Option<String>,
        control: &Control,
    ) -> Result<(String, Outcome), String> {
        control.check()?;
        let sequence = {
            let mut next = self
                .next
                .lock()
                .map_err(|_| "external MCP request ID lock poisoned")?;
            let sequence = *next;
            *next = next
                .checked_add(1)
                .ok_or("external MCP request IDs exhausted")?;
            sequence
        };
        params["_meta"] = serde_json::to_value(&self.config.metadata).map_err(|e| e.to_string())?;
        let operation = Operation {
            scope: context.scope.clone(),
            cwd: context.cwd.clone(),
            server_id: self.config.servers[server].clone(),
            request_id: format!("{}:{sequence}", self.prefix),
            native_request_id,
            method: method.into(),
            params,
        };
        let request_id = operation.request_id.clone();
        control.check()?;
        let cancellation = CancellationToken::new();
        let request = self.config.backend.request(operation, cancellation.clone());
        tokio::pin!(request);
        let result = tokio::select! {
            result = &mut request => result,
            _ = control.cancelled() => { cancellation.cancel(); request.await },
            _ = tokio::time::sleep(std::time::Duration::from_secs(300)) => { cancellation.cancel(); let _ = request.await; return Err("external MCP operation timed out".into()); },
        }.map_err(|_| "external MCP backend failed (remote diagnostics withheld)")?;
        control.check()?;
        let result = match result {
            Outcome::Result(value) => Outcome::from_result(method, value)?,
            Outcome::InputRequired(input) => {
                Outcome::InputRequired(InputRequired::from_value(method, input.as_value().clone())?)
            }
            other => other,
        };
        match &result {
            Outcome::Result(value) => {
                if serde_json::to_vec(value).map_err(|e| e.to_string())?.len() > 1024 * 1024 - 4096
                {
                    return Err("external MCP outcome exceeds native frame limit".into());
                }
            }
            Outcome::InputRequired(_) => {}
            Outcome::Error(error) => {
                return Err(format!(
                    "external MCP protocol error {} (remote diagnostics withheld)",
                    error.code
                ))
            }
        }
        Ok((request_id, result))
    }
    pub(crate) async fn discover(
        &self,
        context: &RequestContext,
        control: &Control,
    ) -> Result<Vec<ToolDescriptor>, String> {
        let mut descriptors = Vec::new();
        for server in self.config.servers.keys() {
            let mut cursor = None;
            let mut seen = HashSet::new();
            loop {
                let params = cursor
                    .as_ref()
                    .map(|v| json!({"cursor":v}))
                    .unwrap_or_else(|| json!({}));
                let (_, result) = self
                    .request(context, server, "tools/list", params, None, control)
                    .await?;
                let Outcome::Result(result) = result else {
                    return Err("invalid discovery outcome".into());
                };
                for tool in rpi_mcp_core::tool_list(&result)
                    .map_err(|_| "invalid external MCP tools/list")?
                {
                    if descriptors.len() >= 128 {
                        return Err("MCP discovery exceeds 128 native tools".into());
                    }
                    descriptors.push(ToolDescriptor {
                        native_name: mcp::tool_name(server, &tool.name),
                        server: server.clone(),
                        remote_name: tool.name,
                        description: tool.description,
                        input_schema: tool.input_schema,
                    });
                }
                match result.get("nextCursor") {
                    None => break,
                    Some(Value::String(next)) if seen.len() < 100 && seen.insert(next.clone()) => {
                        cursor = Some(next.clone())
                    }
                    _ => {
                        return Err(
                            "invalid external MCP discovery cursor or page limit exceeded".into(),
                        )
                    }
                }
            }
        }
        Ok(descriptors)
    }
    pub(crate) async fn call(
        &self,
        context: &RequestContext,
        tool: ToolDescriptor,
        arguments: Value,
        native_request_id: String,
        control: &Control,
    ) -> Result<Value, String> {
        // One native owner for the whole continuation, including callback cleanup.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
        let continuation = self.continue_call(
            context,
            tool,
            arguments,
            native_request_id,
            control,
            deadline,
        );
        tokio::pin!(continuation);
        tokio::select! {
            result = &mut continuation => result,
            _ = tokio::time::sleep_until(deadline) => {
                control.cancel();
                let _ = continuation.await;
                Err("external MCP continuation timed out".into())
            }
        }
    }
    async fn continue_call(
        &self,
        context: &RequestContext,
        tool: ToolDescriptor,
        arguments: Value,
        native_request_id: String,
        control: &Control,
        deadline: tokio::time::Instant,
    ) -> Result<Value, String> {
        let original = json!({"name":tool.remote_name,"arguments":arguments});
        let mut params = original.clone();
        let continuation_id = uuid::Uuid::new_v4().to_string();
        let policy = &self.policy;
        for round in 0..=MAX_INPUT_ROUNDS {
            control.check()?;
            if tokio::time::Instant::now() >= deadline {
                return Err("external MCP continuation timed out".into());
            }
            let (previous_request_id, outcome) = self
                .request(
                    context,
                    &tool.server,
                    "tools/call",
                    params,
                    Some(native_request_id.clone()),
                    control,
                )
                .await?;
            let input = match outcome {
                Outcome::Result(value) => {
                    match serde_json::from_value::<ToolResult>(value.clone())
                        .map_err(|_| "invalid MCP2026 tool result")?
                    {
                        ToolResult::Complete { .. } => return Ok(value),
                        _ => return Err("unvalidated input_required".into()),
                    }
                }
                Outcome::InputRequired(input) => input,
                Outcome::Error(_) => unreachable!("request maps protocol errors"),
            };
            if round == MAX_INPUT_ROUNDS {
                return Err("MCP input round limit exceeded".into());
            }
            let policy = policy
                .as_ref()
                .ok_or("MCP2026 input_required needs caller policy")?;
            if !self.capabilities.supports(&input) {
                return Err("unsupported MCP input_required capability".into());
            }
            control.check()?;
            if tokio::time::Instant::now() >= deadline {
                return Err("external MCP continuation timed out".into());
            }
            let cancellation = CancellationToken::new();
            let callback = policy.resolve(
                RetryContext {
                    continuation_id: continuation_id.clone(),
                    scope: context.scope.clone(),
                    server_id: self.config.servers[&tool.server].clone(),
                    native_request_id: native_request_id.clone(),
                    previous_request_id,
                    round: round + 1,
                    deadline: deadline.into_std(),
                },
                input.clone(),
                cancellation.clone(),
            );
            tokio::pin!(callback);
            let decision = tokio::select! {
                result = &mut callback => result,
                _ = control.cancelled() => { cancellation.cancel(); let _ = callback.await; return Err("MCP input cancelled".into()); }
            }.map_err(|_| "MCP input policy failed (diagnostics withheld)")?;
            control.check()?;
            let InputDecision::Retry { input_responses } = decision else {
                return Err("MCP input continuation abandoned".into());
            };
            if serde_json::to_vec(&input_responses)
                .map_err(|e| e.to_string())?
                .len()
                > MAX_INPUT_BYTES
            {
                return Err("MCP input responses exceed byte limit".into());
            }
            let requests = input.input_requests();
            match (&requests, &input_responses) {
                (Some(requests), Some(responses))
                    if requests.len() == responses.len()
                        && requests.keys().all(|k| responses.contains_key(k)) => {}
                (None, None) => {}
                _ => return Err("MCP input response keys must match requested inputs".into()),
            }
            // Always rebuild from the immutable original: older state/responses cannot leak.
            params = original.clone();
            if let Some(responses) = input_responses {
                params["inputResponses"] =
                    serde_json::to_value(responses).map_err(|e| e.to_string())?;
            }
            if let Some(state) = input.request_state() {
                params["requestState"] = json!(state);
            }
        }
        unreachable!("bounded loop terminates")
    }
    pub(crate) async fn shutdown(&self) -> Result<(), String> {
        self.config
            .backend
            .shutdown()
            .await
            .map_err(|_| "external MCP backend shutdown failed".into())
    }
}
