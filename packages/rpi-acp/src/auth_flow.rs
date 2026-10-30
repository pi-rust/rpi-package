//! ACP-only authentication projection; native credentials and child lifecycle stay in extcore.
use crate::protocol::Writer;
use rpi_extcore::auth::{
    self, AuthAction, AuthDescriptor, AuthEvent, AuthInteraction, AuthKind, AuthMode, AuthOptions,
    AuthProcess, AuthResponse,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

pub const AGENT_METHOD_ID: &str = "rpi-agent-credentials";
/// Connection-local advertisement, never session state or a credential cache.
pub type Negotiated = Arc<Mutex<Option<AuthDescriptor>>>;

pub fn options() -> Result<AuthOptions, String> {
    Ok(AuthOptions::new(
        crate::session::resolve_rpi_executable(),
        crate::auth::configured_provider()?,
    ))
}

pub async fn probe(writer: &Writer) -> Option<AuthDescriptor> {
    let descriptor = auth::describe(&options().ok()?).await.ok()?;
    if writer.supports_elicitation_form()
        && (descriptor.kind == AuthKind::ApiKey || writer.supports_elicitation_url())
    {
        Some(descriptor)
    } else {
        None
    }
}

pub fn advertise(result: &mut Value, descriptor: Option<&AuthDescriptor>, v2: bool, status: bool) {
    if descriptor.is_some() {
        let mut method = json!({"type":"agent","name":"Sign in to rpi","description":"Authenticate using the configured native provider"});
        method[if v2 { "methodId" } else { "id" }] = AGENT_METHOD_ID.into();
        result["authMethods"]
            .as_array_mut()
            .expect("initialize auth methods")
            .insert(0, method);
    }
    if status {
        result[if v2 {
            "capabilities"
        } else {
            "agentCapabilities"
        }]["auth"]["status"] = true.into();
    }
}

fn elicitation(interaction: &AuthInteraction, parent: &Value) -> Result<Value, FlowError> {
    let mut params = json!({"requestId":parent,"message":interaction.message});
    match interaction.mode {
        AuthMode::Form => {
            let schema = interaction
                .requested_schema
                .as_ref()
                .filter(|s| s.is_object())
                .ok_or(FlowError::Invalid)?;
            params["mode"] = "form".into();
            params["requestedSchema"] = schema.clone();
        }
        AuthMode::Url => {
            let url = interaction.url.as_deref().ok_or(FlowError::Invalid)?;
            let parsed = url::Url::parse(url).map_err(|_| FlowError::Invalid)?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
            {
                return Err(FlowError::Invalid);
            }
            params["mode"] = "url".into();
            params["url"] = url.into();
            // Native IDs are process-local and may collide across concurrent auth requests.
            params["elicitationId"] = uuid::Uuid::new_v4().to_string().into();
        }
    }
    Ok(params)
}

/// Validate only ACP response types; native owns provider-specific field validation.
fn response(value: Value, interaction: &AuthInteraction) -> Result<AuthResponse, FlowError> {
    let object = value.as_object().ok_or(FlowError::Invalid)?;
    let action = match object.get("action").and_then(Value::as_str) {
        Some("accept") => AuthAction::Accept,
        Some("decline") => AuthAction::Decline,
        Some("cancel") => AuthAction::Cancel,
        _ => return Err(FlowError::Invalid),
    };
    let content = match object.get("content").filter(|v| !v.is_null()) {
        None => None,
        Some(Value::Object(content))
            if content.values().all(|v| {
                v.is_string()
                    || v.is_number()
                    || v.is_boolean()
                    || v.as_array().is_some_and(|a| a.iter().all(Value::is_string))
            }) =>
        {
            Some(content.clone())
        }
        _ => return Err(FlowError::Invalid),
    };
    if matches!(action, AuthAction::Accept)
        && interaction.mode == AuthMode::Form
        && content.is_none()
    {
        return Err(FlowError::Invalid);
    }
    Ok(AuthResponse {
        request_id: interaction.request_id.clone(),
        action,
        content,
    })
}

#[derive(Debug)]
pub enum FlowError {
    Cancelled,
    Invalid,
    Failed,
}
impl From<auth::AuthError> for FlowError {
    fn from(_: auth::AuthError) -> Self {
        Self::Failed
    }
}

/// The request future owns AuthProcess. Abort/disconnect drops it and kills its tree.
pub async fn authenticate(
    writer: Arc<Writer>,
    parent: Value,
    descriptor: AuthDescriptor,
    mut cancelled: watch::Receiver<bool>,
) -> Result<Value, FlowError> {
    let mut process = AuthProcess::spawn(options().map_err(|_| FlowError::Failed)?)?;
    let result = drive(&mut process, &writer, &parent, &descriptor, &mut cancelled).await;
    let _ = process.shutdown().await;
    result
}

async fn wait_cancelled(cancelled: &mut watch::Receiver<bool>) {
    while !*cancelled.borrow_and_update() {
        if cancelled.changed().await.is_err() {
            break;
        }
    }
}

// Keep the client request alive until its cancellation notification has been written.
// The scoped token also lets the deadline cancel the request without detached work.
async fn request_elicitation(
    writer: &Writer,
    params: Value,
    cancelled: &mut watch::Receiver<bool>,
) -> Result<Value, FlowError> {
    let (owner, mut receiver) = watch::channel(false);
    let request = writer.request_cancellable("elicitation/create", params, &mut receiver);
    tokio::pin!(request);
    let error = tokio::select! {
        biased;
        _ = wait_cancelled(cancelled) => FlowError::Cancelled,
        _ = tokio::time::sleep(std::time::Duration::from_secs(120)) => FlowError::Failed,
        value = &mut request => {
            return if *cancelled.borrow() || writer.is_closed() {
                Err(FlowError::Cancelled)
            } else {
                Ok(value)
            };
        }
    };
    owner.send_replace(true);
    // Do not race cancellation again: request_cancellable owns the wire cleanup.
    request.await;
    Err(error)
}

async fn drive(
    process: &mut AuthProcess,
    writer: &Writer,
    parent: &Value,
    descriptor: &AuthDescriptor,
    cancelled: &mut watch::Receiver<bool>,
) -> Result<Value, FlowError> {
    let mut ready = false;
    let mut urls = Vec::new();
    loop {
        let event = tokio::select! {
            biased;
            _ = wait_cancelled(cancelled) => return Err(FlowError::Cancelled),
            event = process.next_event() => event?,
        };
        match event.ok_or(FlowError::Failed)? {
            AuthEvent::Ready(actual) if !ready && actual == *descriptor => ready = true,
            AuthEvent::Ready(_) => return Err(FlowError::Failed),
            AuthEvent::Interaction(interaction) if ready => {
                if (interaction.mode == AuthMode::Form && !writer.supports_elicitation_form())
                    || (interaction.mode == AuthMode::Url && !writer.supports_elicitation_url())
                {
                    return Err(FlowError::Failed);
                }
                let params = elicitation(&interaction, parent)?;
                let completion = (interaction.mode == AuthMode::Url)
                    .then(|| json!({"requestId":parent,"elicitationId":params["elicitationId"]}));
                // No response data is logged or reflected into error messages.
                let value = request_elicitation(writer, params, cancelled).await?;
                let answer = response(value, &interaction)?;
                if matches!(answer.action, AuthAction::Accept) {
                    if let Some(completion) = completion {
                        urls.push(completion);
                    }
                }
                tokio::select! {
                    biased;
                    _ = wait_cancelled(cancelled) => return Err(FlowError::Cancelled),
                    result = process.respond(answer) => result?,
                }

            }
            AuthEvent::Result(result) if ready => {
                if !result.authenticated {
                    return Err(FlowError::Failed);
                }
                // extcore emits Result only after authoritative auth_result + checked exit.
                for completion in urls {
                    tokio::select! {
                        biased;
                        _ = wait_cancelled(cancelled) => return Err(FlowError::Cancelled),
                        _ = writer.notify("elicitation/complete", completion) => {},
                    }
                }
                return Ok(json!({}));
            }
            _ => return Err(FlowError::Failed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn interaction(mode: AuthMode) -> AuthInteraction {
        AuthInteraction {
            request_id: "native-private".into(),
            mode,
            message: "Login".into(),
            requested_schema: Some(json!({"type":"object","properties":{"key":{"type":"string"}}})),
            url: Some("https://example.com/auth".into()),
            elicitation_id: Some("oauth".into()),
        }
    }
    #[test]
    fn request_scope_is_parent_not_native_or_session() {
        for parent in [json!(7), json!("parent")] {
            for mode in [AuthMode::Form, AuthMode::Url] {
                let params = elicitation(&interaction(mode), &parent).unwrap();
                assert_eq!(params["requestId"], parent);
                assert!(params.get("sessionId").is_none());
                assert!(params.get("toolCallId").is_none());
            }
        }
    }
    #[test]
    fn typed_responses_keep_native_identity_and_never_coerce() {
        let i = interaction(AuthMode::Form);
        let r = response(
            json!({"action":"accept","content":{"key":"secret","number":2,"bool":true}}),
            &i,
        )
        .unwrap();
        assert_eq!(r.request_id, "native-private");
        assert_eq!(r.content.unwrap()["number"], 2);
        for value in [
            json!({}),
            json!({"action":"other"}),
            json!({"action":"accept","content":"secret"}),
            json!({"action":"accept","content":{"nested":{}}}),
            json!({"action":"accept"}),
        ] {
            assert!(response(value, &i).is_err());
        }
        for action in ["decline", "cancel"] {
            assert!(response(json!({"action":action}), &i).is_ok());
        }
    }
}
