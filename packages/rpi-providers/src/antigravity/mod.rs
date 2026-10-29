//! Google Antigravity (Cloud Code Assist) provider, ported from cortexkit/antigravity-auth
//! (MIT, see `LICENSE` in this directory) to the rpi-providers SDK 0.8.3 surface.
//!
//! Catalog, Gemini conversion, SSE framing, project discovery and OAuth live here. Host-only
//! features (native transport, vault custody, fork-only wire keys) are not ported.
mod auth;
mod catalog;
mod convert;
mod driver;
mod events;
mod json;
mod metadata;
mod project;
mod redaction;
mod request;
mod schema;
mod sse;
mod stream;
#[cfg(test)]
mod tests;
mod transport;

use auth::{ClientSource, Endpoints};
use metadata::SessionStore;
use project::ProjectStore;
use rpi_ai::{
    event_stream::create_assistant_message_event_stream, AssistantMessageEventStream, Context,
    Model, Provider, SimpleStreamOptions, ThinkingLevel,
};
use rpi_plugin_sdk::{
    callbacks::free_string, provider::ProviderContext, PluginApi, StbString, StbStringRef,
};
use serde_json::{json, Value};
use std::{
    ffi::c_void,
    future::Future,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};
use transport::{Live, Net};

pub const PROVIDER_ID: &str = "antigravity";
/// The endpoint Pi's stream sends to (`ANTIGRAVITY_ENDPOINT`) when the model has no override.
const ENDPOINT: &str = "https://daily-cloudcode-pa.googleapis.com";

/// Process state shared by every request. It holds access-token bridges and sessions,
/// never request headers or API keys.
struct Shared {
    sessions: Mutex<SessionStore>,
    credentials: Mutex<auth::CredentialCache>,
    projects: Arc<ProjectStore>,
    /// Issued login attempts, keyed by the state their login URL carries.
    logins: Mutex<auth::PendingLogins>,
}
fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| Shared {
        sessions: Mutex::new(SessionStore::new(String::new())),
        credentials: Mutex::new(auth::CredentialCache::default()),
        projects: Arc::new(ProjectStore::default()),
        logins: Mutex::new(auth::PendingLogins::default()),
    })
}

/// The six catalog models, in Pi's order.
pub fn models() -> Vec<Model> {
    catalog::models(PROVIDER_ID)
}

/// Requests go to the Pi endpoint unless the model names another base URL, which lets tests
/// and self-hosted routing reach a loopback server. Discovery uses the same override.
fn root(model: &Model) -> String {
    if model.base_url.is_empty() || model.base_url == catalog::BASE_URL {
        ENDPOINT.into()
    } else {
        model.base_url.trim_end_matches('/').into()
    }
}
fn endpoints(root: &str) -> Endpoints {
    if root == ENDPOINT {
        Endpoints::production()
    } else {
        Endpoints {
            load: vec![root.into()],
            ..Endpoints::production()
        }
    }
}

/// Holds the pooled client only. Credentials arrive per request.
pub struct AntigravityProvider {
    client: reqwest::Client,
    models: Vec<Model>,
}

#[async_trait::async_trait]
impl Provider for AntigravityProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }
    fn models(&self) -> &[Model] {
        &self.models
    }
    async fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: &SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        let (producer, stream) = create_assistant_message_event_stream();
        let root = root(model);
        let source = Live {
            net: Arc::new(Net {
                client: self.client.clone(),
                endpoints: endpoints(&root),
                user_agent: transport::default_user_agent(),
            }),
            root,
            idle: options.stream_idle_timeout(),
        };
        // Pi omits the reasoning option for "off"; only real levels reach the catalog.
        let reasoning = match options.reasoning {
            None | Some(ThinkingLevel::Off) => Value::Null,
            Some(level) => serde_json::to_value(level).unwrap_or(Value::Null),
        };
        let request_options = json!({
            "apiKey": options.api_key,
            "reasoning": reasoning,
            "maxTokens": options.max_tokens,
            "sessionId": options.session_id,
        });
        let model = serde_json::to_value(model).unwrap_or(Value::Null);
        let context = serde_json::to_value(context).unwrap_or(Value::Null);
        let caller = options.signal.clone();
        tokio::spawn(async move {
            let mut sink = events::Emitter { producer };
            driver::run(
                &source,
                &model,
                &context,
                &request_options,
                caller,
                &mut sink,
            )
            .await;
        });
        stream
    }
}

fn create(
    context: &ProviderContext,
    model: &Model,
    _options: &SimpleStreamOptions,
) -> Result<Arc<dyn Provider>, String> {
    Ok(Arc::new(AntigravityProvider {
        client: context.client(&root(model))?,
        models: models(),
    }))
}

/// The streaming registration. The macro emits a `register` function, so it gets a module.
mod streaming {
    crate::registration::register_provider!(
        super::PROVIDER_ID,
        super::catalog::BASE_URL,
        super::catalog::API,
        serde_json::json!({"id": super::PROVIDER_ID, "models": super::models(), "requiresAuth": true}),
        super::create
    );
}

/// Declares the streaming provider and its OAuth flow. Streaming never depends on OAuth.
pub(crate) fn register(api: &PluginApi) -> i32 {
    let result = streaming::register(api);
    if result != 0 {
        return result;
    }
    // A host without the OAuth slot keeps the provider; login then needs another credential.
    let Some(register_oauth) = api.register_oauth_provider else {
        return 0;
    };
    let manifest = json!({
        "id": PROVIDER_ID,
        "displayName": "Google Antigravity",
        "supportsPkce": true,
    })
    .to_string();
    register_oauth(
        StbStringRef::from_str(&manifest),
        oauth_request,
        free_string,
        std::ptr::null_mut::<c_void>(),
    )
}

/// Host action callback: `{action, providerId, input}` in, one standard envelope out.
extern "C" fn oauth_request(
    request: StbStringRef,
    out: *mut StbString,
    _user_data: *mut c_void,
) -> i32 {
    if out.is_null() {
        return 2;
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the host keeps the request buffer valid until this callback returns.
        let text = unsafe { request.as_str() }.to_owned();
        oauth_action(&text)
    }));
    let (status, body) = match result {
        Ok(Ok(value)) => (0, value),
        Ok(Err(message)) => (1, error_envelope(&message)),
        Err(_) => (1, error_envelope("Antigravity OAuth panicked")),
    };
    // SAFETY: `out` is the host's exclusive output slot, checked non-null above.
    unsafe { *out = StbString::from_string(body.to_string()) };
    status
}

fn error_envelope(message: &str) -> Value {
    json!({"version": 1, "kind": "error", "payload": {"message": message}})
}

fn oauth_action(text: &str) -> Result<Value, String> {
    let request: Value =
        serde_json::from_str(text).map_err(|_| "invalid OAuth request".to_owned())?;
    let input = &request["input"];
    match request["action"].as_str() {
        Some("begin") => begin(),
        Some("exchange") => exchange(input["value"].as_str().unwrap_or_default()),
        Some("refresh") => refresh(&input["credential"]),
        Some("revoke") => revoke(&input["credential"]),
        _ => Err("OAuth action must be one of begin, exchange, refresh, revoke".into()),
    }
}

fn begin() -> Result<Value, String> {
    let auth::Authorization { url, state, login } = auth::authorize(ClientSource::environment())?;
    shared()
        .logins
        .lock()
        .unwrap()
        .insert(state, login, Instant::now());
    Ok(json!({
        "version": 1,
        "kind": "interaction",
        "payload": {"display": {
            "title": "Connect Google Antigravity",
            "url": url,
            "instructions": ["Open the URL, sign in, then paste the callback URL or the code."],
        }},
    }))
}

fn exchange(pasted: &str) -> Result<Value, String> {
    let pasted = pasted.to_owned();
    // Redemption runs inside the worker, before any request; a rejected input sends nothing.
    block_on_oauth(move |net| async move {
        exchange_credential(
            &net,
            ClientSource::environment(),
            &shared().logins,
            &pasted,
            Instant::now(),
        )
        .await
    })
}

/// Redeems a login and packs the credential the host stores. The access value carries the
/// project the login bound, so a later stream can use it without a process-local cache.
async fn exchange_credential(
    net: &Net,
    source: ClientSource,
    logins: &Mutex<auth::PendingLogins>,
    pasted: &str,
    now: Instant,
) -> Result<Value, String> {
    let result = auth::complete(net, source, &net.endpoints, logins, pasted, now).await?;
    if result["type"] != "success" {
        return Err("Antigravity OAuth exchange failed".into());
    }
    let refresh = result["refresh"].as_str().unwrap_or_default().to_owned();
    let access = result["access"].as_str().unwrap_or_default().to_owned();
    if access.is_empty() {
        return Err("Antigravity OAuth exchange returned no access token".into());
    }
    let project = result["projectId"].as_str().unwrap_or_default();
    shared()
        .credentials
        .lock()
        .unwrap()
        .remember(&access, &refresh);
    Ok(credential_envelope(
        &auth::format_access(&access, project),
        &refresh,
        &result["expires"],
    ))
}

fn refresh(credential: &Value) -> Result<Value, String> {
    let packed = credential["refresh"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let old_access = credential["access"].as_str().unwrap_or_default().to_owned();
    if packed.is_empty() {
        return Err("Antigravity has no stored refresh credential".into());
    }
    let old_token = auth::parse_access(&old_access).0.to_owned();
    block_on_oauth(move |net| async move {
        refresh_credential(&net, ClientSource::environment(), &old_token, &packed).await
    })
}

/// Refreshes the packed refresh token. The refresh token keeps the project segments, so the new
/// access is stored with the same binding, and a credential stored before the binding gains it.
async fn refresh_credential(
    net: &Net,
    source: ClientSource,
    old_token: &str,
    packed: &str,
) -> Result<Value, String> {
    let refreshed = auth::refresh(net, source, &net.endpoints, packed).await?;
    let access = refreshed["access"].as_str().unwrap_or_default().to_owned();
    let refresh = refreshed["refresh"].as_str().unwrap_or_default().to_owned();
    if access.is_empty() {
        return Err("Antigravity token refresh returned no access token".into());
    }
    {
        let mut credentials = shared().credentials.lock().unwrap();
        credentials.forget(old_token);
        credentials.remember(&access, &refresh);
    }
    let project = auth::bound_project(&refresh);
    Ok(credential_envelope(
        &auth::format_access(&access, &project),
        &refresh,
        &refreshed["expires"],
    ))
}

fn revoke(credential: &Value) -> Result<Value, String> {
    let packed = credential["refresh"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let access = credential["access"].as_str().unwrap_or_default().to_owned();
    block_on_oauth(move |net| async move { revoke_credential(&net, &packed, &access).await })?;
    Ok(json!({"version": 1, "kind": "success", "payload": {}}))
}

/// Revokes the refresh token, or the plain access token when none is stored. A stored access
/// may carry a project binding, which is never sent.
async fn revoke_credential(net: &Net, packed: &str, access: &str) -> Result<(), String> {
    let token = auth::parse_access(access).0;
    auth::revoke(net, &net.endpoints, packed, token).await?;
    shared().credentials.lock().unwrap().forget(token);
    Ok(())
}

/// Host credential envelope. `expires_at` is whole seconds; Pi's `expires` is milliseconds.
fn credential_envelope(access: &str, refresh: &str, expires_ms: &Value) -> Value {
    let expires_at = expires_ms.as_f64().map_or(0, |ms| (ms / 1000.0) as i64);
    json!({
        "version": 1,
        "kind": "credential",
        "payload": {"access": access, "refresh": refresh, "expires_at": expires_at},
    })
}

/// Runs one OAuth exchange on its own thread and runtime, so a host runtime is never nested.
fn block_on_oauth<T, F, Fut>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(Net) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, String>>,
{
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        runtime.block_on(async move {
            let client = rpi_ai::http::build_client(&Endpoints::production().token, None, None)?;
            work(Net {
                client,
                endpoints: Endpoints::production(),
                user_agent: transport::default_user_agent(),
            })
            .await
        })
    })
    .join()
    .map_err(|_| "Antigravity OAuth worker stopped".to_owned())?
}
