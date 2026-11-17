//! OpenAI Codex (ChatGPT Plus / Team OAuth) provider, ported from pi-ai's
//! `providers/openai-codex.ts`, `auth/oauth/openai-codex.ts` and
//! `api/openai-codex-responses.ts` to the rpi-providers SDK 0.8.3 surface.
//!
//! Catalog, Responses request conversion, SSE handling and OAuth live here. Pi's
//! WebSocket transport, zstd request compression and grammar tools are not ported;
//! the SSE path speaks the same wire protocol.
mod auth;
mod catalog;
mod convert;
mod driver;
mod events;
mod json;
mod redaction;
mod sse;
mod stream;
#[cfg(test)]
mod tests;
mod transport;

use auth::{Endpoints, PendingLogins};
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

pub const PROVIDER_ID: &str = "openai-codex";
/// The endpoint Pi's stream sends to when the model has no override.
pub const ENDPOINT: &str = "https://chatgpt.com/backend-api";

/// Process state: issued login attempts only. Credentials arrive per request and the
/// account id is derived from the access JWT at request time, so nothing else is cached.
fn shared() -> &'static Mutex<PendingLogins> {
    static SHARED: OnceLock<Mutex<PendingLogins>> = OnceLock::new();
    SHARED.get_or_init(|| Mutex::new(PendingLogins::default()))
}

/// The default catalog models, in Pi's order.
pub fn models() -> Vec<Model> {
    catalog::models(PROVIDER_ID)
}

/// Requests go to the Pi endpoint unless the model names another base URL, which lets
/// tests and self-hosted routing reach a loopback server.
fn root(model: &Model) -> String {
    if model.base_url.is_empty() || model.base_url == catalog::BASE_URL {
        ENDPOINT.into()
    } else {
        model.base_url.trim_end_matches('/').into()
    }
}

/// Holds the pooled client only. Credentials arrive per request.
pub struct CodexProvider {
    client: reqwest::Client,
    models: Vec<Model>,
}

#[async_trait::async_trait]
impl Provider for CodexProvider {
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
                endpoints: Endpoints::production(),
                user_agent: default_user_agent(),
            }),
            root,
            idle: options.stream_idle_timeout(),
        };
        // Pi omits the reasoning option for "off"; only real levels reach the wire map.
        let reasoning = match options.reasoning {
            None | Some(ThinkingLevel::Off) => Value::Null,
            Some(level) => serde_json::to_value(level).unwrap_or(Value::Null),
        };
        let request_options = json!({
            "apiKey": options.api_key,
            "reasoning": reasoning,
            "sessionId": options.session_id,
            "temperature": options.temperature,
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
    Ok(Arc::new(CodexProvider {
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
        "displayName": "OpenAI (ChatGPT Plus/Pro)",
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
        Err(_) => (1, error_envelope("OpenAI Codex OAuth panicked")),
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
        // OpenAI has no revoke endpoint. Success lets the host remove local credentials.
        Some("revoke") => Ok(json!({"version": 1, "kind": "success", "payload": {}})),
        _ => Err("OAuth action must be one of begin, exchange, refresh, revoke".into()),
    }
}

fn begin() -> Result<Value, String> {
    let auth::Authorization { url, state, login } = auth::authorize()?;
    shared()
        .lock()
        .unwrap()
        .insert(state, login, Instant::now());
    Ok(json!({
        "version": 1,
        "kind": "interaction",
        "payload": {"display": {
            "title": "Connect OpenAI (ChatGPT Plus/Pro)",
            "url": url,
            "instructions": ["Open the URL, sign in, then paste the callback URL or the code."],
        }},
    }))
}

fn exchange(pasted: &str) -> Result<Value, String> {
    let pasted = pasted.to_owned();
    block_on_oauth(move |net| async move {
        let result =
            auth::complete(&net, &net.endpoints, shared(), &pasted, Instant::now()).await?;
        Ok(credential_envelope(
            result["access"].as_str().unwrap_or_default(),
            result["refresh"].as_str().unwrap_or_default(),
            result["expires"].as_f64().unwrap_or(0.0) / 1000.0,
        ))
    })
}

fn refresh(credential: &Value) -> Result<Value, String> {
    let packed = credential["refresh"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    if packed.is_empty() {
        return Err("OpenAI Codex has no stored refresh credential".into());
    }
    block_on_oauth(move |net| async move {
        let refreshed = auth::refresh(&net, &net.endpoints, &packed).await?;
        Ok(credential_envelope(
            refreshed["access"].as_str().unwrap_or_default(),
            refreshed["refresh"].as_str().unwrap_or_default(),
            refreshed["expires"].as_f64().unwrap_or(0.0) / 1000.0,
        ))
    })
}

/// Host credential envelope. `expires_at` is whole seconds; Pi's `expires` is milliseconds.
fn credential_envelope(access: &str, refresh: &str, expires_at: f64) -> Value {
    json!({
        "version": 1,
        "kind": "credential",
        "payload": {"access": access, "refresh": refresh, "expires_at": expires_at as i64},
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
                user_agent: default_user_agent(),
            })
            .await
        })
    })
    .join()
    .map_err(|_| "OpenAI Codex OAuth worker stopped".to_owned())?
}

/// The Codex CLI originator Pi sends (`originator: pi` plus the CLI user agent).
pub fn default_user_agent() -> String {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "windows",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("pi/1.0.0 ({platform}-{arch})")
}

/// Random RFC 4122 version-4 identifier from the OS CSPRNG.
pub(crate) fn mod_uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("OS randomness");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}
