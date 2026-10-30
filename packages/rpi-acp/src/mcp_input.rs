//! ACP-only caller policy for neutral MCP continuations. Sampling execution and
//! consent are supplied explicitly; remote state never grants authorization.
use crate::protocol::Writer;
use rpi_extcore::mcp_external::{BackendFuture, CancellationToken, ExternalOptions, InputCapabilities, InputDecision, InputPolicy, InputRequired, RetryContext};
use rpi_extcore::sampling::{execute_validated, SamplingContext, SamplingOptions, SamplingRequest};
use rpi_extcore::mcp::McpControl;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf, sync::{Arc, Weak}};

#[derive(Clone, Default)]
pub(crate) struct Scope {
    pub session_id: String,
    pub directories: Vec<PathBuf>,
}
struct Policy {
    writer: Weak<Writer>,
    scope: Scope,
    roots: Vec<Value>,
    capabilities: InputCapabilities,
    sampling: Option<SamplingOptions>,
    configured: BTreeMap<String, String>,
    namespace: String,
    cwd: Option<PathBuf>,
}
pub(crate) fn options(writer: &Arc<Writer>, scope: &Scope, sampling: Option<SamplingOptions>, configured: BTreeMap<String, String>) -> Result<ExternalOptions, String> {
    let roots = scope.directories.iter().map(|path| {
        url::Url::from_directory_path(path).map(|uri| json!({"uri":uri.as_str()}))
            .map_err(|_| "invalid configured MCP root".to_owned())
    }).collect::<Result<Vec<_>, _>>()?;
    let sampling_enabled = sampling.is_some() && !configured.is_empty();
    let cwd = if sampling_enabled {
        let path = scope.directories.first().ok_or("missing MCP sampling directory")?;
        let path = std::fs::canonicalize(path).map_err(|_| "invalid MCP sampling directory")?;
        if !path.is_dir() { return Err("invalid MCP sampling directory".into()); }
        Some(path)
    } else { None };
    let capabilities = InputCapabilities {
        elicitation_form: writer.supports_elicitation_form(),
        elicitation_url: writer.supports_elicitation_url(),
        roots: !roots.is_empty(),
        sampling: sampling_enabled,
    };
    Ok(ExternalOptions { input_policy: Some(Arc::new(Policy {
        writer: Arc::downgrade(writer), scope: scope.clone(), roots, capabilities,
        sampling, configured, cwd, namespace: uuid::Uuid::new_v4().to_string(),
    })) })
}

// Fail closed on schemas we cannot validate. Keep this independent from auth:
// auth currently validates only response wire types, not requested field schemas.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Form {
    #[serde(rename = "type")]
    kind: String,
    properties: BTreeMap<String, Field>,
    #[serde(default)]
    required: Vec<String>,
    #[serde(default)]
    additional_properties: Option<bool>,
    #[serde(default, rename = "title")]
    _title: Option<String>,
    #[serde(default, rename = "description")]
    _description: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Field {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, rename = "enum")]
    choices: Option<Vec<Value>>,
    #[serde(default)]
    minimum: Option<f64>,
    #[serde(default)]
    maximum: Option<f64>,
    #[serde(default)]
    min_length: Option<usize>,
    #[serde(default)]
    max_length: Option<usize>,
    #[serde(default)]
    items: Option<Box<Field>>,
    #[serde(default)]
    min_items: Option<usize>,
    #[serde(default)]
    max_items: Option<usize>,
    #[serde(default)]
    unique_items: bool,
    #[serde(default, rename = "title")]
    _title: Option<String>,
    #[serde(default, rename = "description")]
    _description: Option<String>,
    #[serde(default, rename = "default")]
    _default: Option<Value>,
    #[serde(default, rename = "enumNames")]
    _enum_names: Option<Vec<String>>,
}
impl Field {
    fn supported(&self) -> bool {
        match self.kind.as_str() {
            "string" | "number" | "integer" | "boolean" => self.items.is_none(),
            "array" => self.items.as_ref().is_some_and(|f| f.kind == "string" && f.supported()),
            _ => false,
        }
    }
    fn accepts(&self, value: &Value) -> bool {
        if self.choices.as_ref().is_some_and(|choices| !choices.contains(value)) { return false; }
        match self.kind.as_str() {
            "string" => value.as_str().is_some_and(|s| {
                let n = s.chars().count();
                self.min_length.is_none_or(|min| n >= min) && self.max_length.is_none_or(|max| n <= max)
            }),
            "number" | "integer" => value.as_f64().is_some_and(|n| {
                (self.kind != "integer" || n.fract() == 0.0)
                    && self.minimum.is_none_or(|min| n >= min) && self.maximum.is_none_or(|max| n <= max)
            }),
            "boolean" => value.is_boolean(),
            "array" => value.as_array().is_some_and(|a| {
                self.min_items.is_none_or(|min| a.len() >= min)
                    && self.max_items.is_none_or(|max| a.len() <= max)
                    && a.iter().all(|v| self.items.as_ref().is_some_and(|f| f.accepts(v)))
                    && (!self.unique_items || a.iter().enumerate().all(|(i, v)| !a[..i].contains(v)))
            }),
            _ => false,
        }
    }
}
fn form(params: &Value) -> Result<Option<Form>, String> {
    if params.get("message").and_then(Value::as_str).is_none() { return Err("invalid elicitation message".into()); }
    match params.get("mode").and_then(Value::as_str).unwrap_or("form") {
        "form" => {
            let form: Form = serde_json::from_value(params["requestedSchema"].clone()).map_err(|_| "unsupported elicitation schema")?;
            if form.kind != "object" || form.required.iter().any(|k| !form.properties.contains_key(k))
                || !form.properties.values().all(Field::supported) {
                return Err("unsupported elicitation schema".into());
            }
            Ok(Some(form))
        }
        "url" => {
            let url = params.get("url").and_then(Value::as_str).ok_or("missing elicitation URL")?;
            let parsed = url::Url::parse(url).map_err(|_| "invalid elicitation URL")?;
            if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none()
                || !parsed.username().is_empty() || parsed.password().is_some()
                || params.get("elicitationId").and_then(Value::as_str).is_none_or(str::is_empty) {
                return Err("unsafe elicitation URL".into());
            }
            Ok(None)
        }
        _ => Err("unsupported elicitation mode".into()),
    }
}
fn response(value: Value, schema: Option<&Form>) -> Result<Value, String> {
    let action = value.get("action").and_then(Value::as_str).ok_or("invalid elicitation action")?;
    if !matches!(action, "accept" | "decline" | "cancel") { return Err("invalid elicitation action".into()); }
    if let Some(content) = value.get("content") {
        let fields = content.as_object().ok_or("invalid elicitation content")?;
        if !fields.values().all(|v| v.is_string() || v.is_number() || v.is_boolean()
            || v.as_array().is_some_and(|a| a.iter().all(Value::is_string))) {
            return Err("invalid elicitation content".into());
        }
    }
    if action == "accept" {
        if let Some(schema) = schema {
            let fields = value.get("content").and_then(Value::as_object).ok_or("missing elicitation content")?;
            if schema.required.iter().any(|k| !fields.contains_key(k)) || fields.iter().any(|(k, v)| {
                schema.properties.get(k).map_or(schema.additional_properties == Some(false), |field| !field.accepts(v))
            }) { return Err("elicitation content violates schema".into()); }
        }
    }
    Ok(value) // Preserve the actual action/content and extensions, including decline/cancel.
}
enum ValidatedInput {
    Roots,
    Form(Option<Form>),
    Sampling(SamplingRequest),
}
const SAMPLING_FAILED: &str = "MCP sampling failed (diagnostics withheld)";
impl Policy {
    async fn sample(&self, context: &RetryContext, key: &str, request: &SamplingRequest, cancellation: &CancellationToken) -> Result<Value, String> {
        if context.scope != self.scope.session_id { return Err(SAMPLING_FAILED.into()); }
        let options = self.sampling.as_ref().ok_or(SAMPLING_FAILED)?;
        let context = SamplingContext {
            cwd: self.cwd.clone().ok_or(SAMPLING_FAILED)?,
            target: self.configured.get(&context.server_id).ok_or(SAMPLING_FAILED)?.clone(),
            namespace: self.namespace.clone(),
            scope: self.scope.session_id.clone(),
            server: context.server_id.clone(),
            generation: 1,
            operation: format!("{}/{}/{}", context.native_request_id, context.round, key),
            deadline: context.deadline,
        };
        let control = McpControl::default();
        let work = async {
            let policy = options.authorizer.authorize(&context, request, &control).await?;
            execute_validated(Some(options.executor.as_ref()), Some(&policy), &context, request, &control).await
        };
        tokio::pin!(work);
        let result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                control.cancel();
                let _ = work.await;
                return Err("MCP input cancelled".into());
            }
            _ = tokio::time::sleep_until(context.deadline.into()) => {
                control.cancel();
                let _ = work.await;
                return Err(SAMPLING_FAILED.into());
            }
            result = &mut work => result,
        }.map_err(|_| SAMPLING_FAILED)?;
        if cancellation.is_cancelled() || std::time::Instant::now() >= context.deadline {
            return Err(SAMPLING_FAILED.into());
        }
        Ok(result.as_value().clone())
    }
}
impl InputPolicy for Policy {
    fn capabilities(&self) -> InputCapabilities { self.capabilities }
    fn resolve(&self, context: RetryContext, input: InputRequired, cancellation: CancellationToken) -> BackendFuture<'_, Result<InputDecision, String>> {
        Box::pin(async move {
            let requests = input.input_requests().unwrap_or_default();
            // State is opaque; no state-only or empty-input automatic retry.
            if requests.is_empty() { return Ok(InputDecision::Abandon); }
            let writer = self.writer.upgrade().ok_or("ACP connection closed")?;
            // Validate the entire batch before asking the client to perform anything.
            let schemas = requests.iter().map(|(key, request)| {
                let schema = match request.method.as_str() {
                    "roots/list" if self.capabilities.roots => ValidatedInput::Roots,
                    "sampling/createMessage" if self.capabilities.sampling => {
                        if !self.configured.contains_key(&context.server_id) { return Err(SAMPLING_FAILED.into()); }
                        ValidatedInput::Sampling(SamplingRequest::parse(request.params.clone().ok_or(SAMPLING_FAILED)?)
                            .map_err(|_| SAMPLING_FAILED)?)
                    }
                    "elicitation/create" => {
                        let params = request.params.as_ref().ok_or("missing elicitation params")?;
                        let schema = form(params)?;
                        if (schema.is_some() && !self.capabilities.elicitation_form)
                            || (schema.is_none() && !self.capabilities.elicitation_url) {
                            return Err("elicitation capability not negotiated".into());
                        }
                        ValidatedInput::Form(schema)
                    }
                    _ => return Err("unsupported MCP input".into()),
                };
                Ok((key.clone(), schema))
            }).collect::<Result<BTreeMap<_, _>, String>>()?;
            let mut responses = BTreeMap::new();
            for (key, request) in requests {
                if cancellation.is_cancelled() { return Err("MCP input cancelled".into()); }
                let answer = match &schemas[&key] {
                    ValidatedInput::Roots => json!({"roots":self.roots}),
                    ValidatedInput::Sampling(request) => self.sample(&context, &key, request, &cancellation).await?,
                    ValidatedInput::Form(schema) => {
                        let mut params = request.params.unwrap();
                        params["sessionId"] = json!(self.scope.session_id);
                        // request_checked owns cancellation notification and pending cleanup.
                        let answer = writer.request_checked("elicitation/create", params, cancellation.clone()).await?;
                        if cancellation.is_cancelled() { return Err("MCP input cancelled".into()); }
                        response(answer, schema.as_ref())?
                    }
                };
                responses.insert(key, answer);
            }
            Ok(InputDecision::Retry { input_responses: Some(responses) })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpi_extcore::sampling::{AuthorizationFuture, SamplingAuthorizer, SamplingExecutor, SamplingFuture, SamplingPolicy, SamplingResult};
    use std::sync::Mutex;

    struct Controlled(Mutex<Vec<SamplingContext>>);
    impl SamplingAuthorizer for Controlled {
        fn authorize<'a>(&'a self, context: &'a SamplingContext, _: &'a SamplingRequest, _: &'a McpControl) -> AuthorizationFuture<'a> {
            Box::pin(async move {
                self.0.lock().unwrap().push(context.clone());
                Ok(SamplingPolicy { context: context.clone(), max_messages: 1, max_tokens: 8, max_duration: std::time::Duration::from_secs(300) })
            })
        }
    }
    impl SamplingExecutor for Controlled {
        fn execute<'a>(&'a self, _: &'a SamplingContext, _: &'a SamplingRequest, _: &'a McpControl) -> SamplingFuture<'a> {
            Box::pin(async { SamplingResult::parse(json!({"role":"assistant","content":{"type":"text","text":"sampled"},"model":"controlled","stopReason":"endTurn"})) })
        }
    }
    #[tokio::test]
    async fn mrtr_sampling_validates_batch_and_binds_actual_context() {
        let writer = Arc::new(Writer::new());
        let cwd = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        let scope = Scope { session_id: "actual-root".into(), directories: vec![cwd.clone()] };
        let controlled = Arc::new(Controlled(Mutex::new(Vec::new())));
        let sampling = SamplingOptions { executor: controlled.clone(), authorizer: controlled.clone() };
        let configured = BTreeMap::from([("server".into(), "https://configured.example/mcp?exact=1".into())]);
        assert!(!options(&writer, &scope, None, configured.clone()).unwrap().input_policy.unwrap().capabilities().sampling);
        assert!(!options(&writer, &scope, Some(sampling.clone()), BTreeMap::new()).unwrap().input_policy.unwrap().capabilities().sampling);
        let policy = options(&writer, &scope, Some(sampling), configured).unwrap().input_policy.unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let context = RetryContext { continuation_id: "continuation".into(), scope: "actual-root".into(), server_id: "server".into(), native_request_id: "native".into(), previous_request_id: "previous".into(), round: 2, deadline };
        let request = json!({"method":"sampling/createMessage","params":{"messages":[{"role":"user","content":{"type":"text","text":"hello"}}],"maxTokens":8}});
        let invalid = InputRequired::from_value("tools/call", json!({"resultType":"input_required","inputRequests":{"a":request.clone(),"z":{"method":"sampling/createMessage","params":{}}}})).unwrap();
        assert!(policy.resolve(context.clone(), invalid, CancellationToken::new()).await.is_err());
        assert!(controlled.0.lock().unwrap().is_empty());
        let input = InputRequired::from_value("tools/call", json!({"resultType":"input_required","inputRequests":{"key":request}})).unwrap();
        let mut wrong_scope = context.clone(); wrong_scope.scope = "sibling".into();
        assert!(policy.resolve(wrong_scope, input.clone(), CancellationToken::new()).await.is_err());
        let InputDecision::Retry { input_responses: Some(responses) } = policy.resolve(context.clone(), input.clone(), CancellationToken::new()).await.unwrap() else { panic!("expected sampling response") };
        assert_eq!(responses["key"]["content"]["text"], "sampled");
        let seen = controlled.0.lock().unwrap()[0].clone();
        assert_eq!(seen.scope, scope.session_id);
        assert_eq!(seen.cwd, cwd);
        assert_eq!(seen.server, "server");
        assert_eq!(seen.target, "https://configured.example/mcp?exact=1");
        assert_eq!(seen.operation, "native/2/key");
        assert_eq!(seen.generation, 1);
        assert_eq!(seen.deadline, deadline);
        assert!(uuid::Uuid::parse_str(&seen.namespace).is_ok());
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(policy.resolve(context, input, cancelled).await.is_err());
        assert_eq!(controlled.0.lock().unwrap().len(), 1);
    }
    #[test]
    fn validates_typed_results_without_inventing_decisions() {
        let schema = form(&json!({"message":"Input", "requestedSchema":{"type":"object","properties":{"n":{"type":"integer","minimum":1}},"required":["n"],"additionalProperties":false}})).unwrap().unwrap();
        let value = json!({"action":"accept","content":{"n":2},"future":true});
        assert_eq!(response(value.clone(), Some(&schema)).unwrap(), value);
        assert!(response(json!({"action":"accept","content":{"n":0}}), Some(&schema)).is_err());
        assert!(response(json!({"action":"accept","content":{"n":"2"}}), Some(&schema)).is_err());
        for action in ["decline", "cancel"] {
            let value = json!({"action":action});
            assert_eq!(response(value.clone(), Some(&schema)).unwrap(), value);
        }
        for url in ["file:///tmp/key", "https://user:password@example.com", "javascript:alert(1)"] {
            assert!(form(&json!({"mode":"url","message":"Open","url":url,"elicitationId":"id"})).is_err());
        }
    }
}
