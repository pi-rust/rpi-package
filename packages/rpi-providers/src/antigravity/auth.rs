//! Pi OAuth and packed credentials. The host owns persistence; this module owns the wire protocol.
use super::json::{finite_number, string, trim, truthy};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    sync::Mutex,
    time::{Duration, Instant},
};
use url::Url;

/// Build-time and runtime variable names for the Antigravity OAuth client. The package
/// ships no client identity: builds and runtimes must supply both or OAuth fails closed.
pub const CLIENT_ID_ENV: &str = "RPI_ANTIGRAVITY_OAUTH_CLIENT_ID";
pub const CLIENT_SECRET_ENV: &str = "RPI_ANTIGRAVITY_OAUTH_CLIENT_SECRET";
pub const REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";
pub const AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const LOAD_ENDPOINTS: [&str; 2] = [
    "https://daily-cloudcode-pa.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];
const SCOPES: [&str; 5] = [
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
];

/// Client values captured by `option_env!` when this crate is compiled.
#[derive(Clone, Copy)]
pub struct BuildClient {
    pub id: Option<&'static str>,
    pub secret: Option<&'static str>,
}
impl BuildClient {
    pub const COMPILED: BuildClient = BuildClient {
        id: option_env!("RPI_ANTIGRAVITY_OAUTH_CLIENT_ID"),
        secret: option_env!("RPI_ANTIGRAVITY_OAUTH_CLIENT_SECRET"),
    };
}
#[cfg(test)]
impl BuildClient {
    pub const NONE: BuildClient = BuildClient {
        id: None,
        secret: None,
    };
}

/// Where the OAuth client comes from. Production uses `environment()`; tests inject
/// explicit values so their results never depend on how the crate was built.
#[derive(Clone, Copy)]
pub struct ClientSource {
    pub build: BuildClient,
    pub runtime: fn(&str) -> Option<String>,
}
impl ClientSource {
    pub fn environment() -> Self {
        Self {
            build: BuildClient::COMPILED,
            runtime: runtime_variable,
        }
    }
    /// Build-time values win over runtime variables; empty or blank values count as unset.
    pub fn resolve(self) -> Result<OAuthClient, String> {
        let pick = |build: Option<&str>, name: &str| {
            build
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .or_else(|| (self.runtime)(name).filter(|value| !value.trim().is_empty()))
        };
        match (
            pick(self.build.id, CLIENT_ID_ENV),
            pick(self.build.secret, CLIENT_SECRET_ENV),
        ) {
            (Some(id), Some(secret)) => Ok(OAuthClient { id, secret }),
            _ => Err(format!(
                "Antigravity OAuth is not configured: set {CLIENT_ID_ENV} and {CLIENT_SECRET_ENV} when building rpi-providers, or export both in the runtime environment."
            )),
        }
    }
}
fn runtime_variable(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// A resolved OAuth client. Only `ClientSource::resolve` constructs one, and it is
/// deliberately not `Debug`, so the secret cannot reach a log through formatting.
pub struct OAuthClient {
    id: String,
    secret: String,
}
#[cfg(test)]
impl OAuthClient {
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Token endpoints. Production uses `Endpoints::production()`; unit tests point the
/// same code at a loopback server. Nothing in the host or a request can change these.
#[derive(Clone, Debug)]
pub struct Endpoints {
    pub token: String,
    pub userinfo: String,
    pub revoke: String,
    pub load: Vec<String>,
}
impl Endpoints {
    pub fn production() -> Self {
        Self {
            token: "https://oauth2.googleapis.com/token".into(),
            userinfo: "https://www.googleapis.com/oauth2/v1/userinfo?alt=json".into(),
            revoke: "https://oauth2.googleapis.com/revoke".into(),
            load: LOAD_ENDPOINTS.iter().map(|value| (*value).into()).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RefreshParts {
    pub refresh_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_project_id: Option<String>,
}
pub fn parse_refresh(value: &str) -> RefreshParts {
    let mut parts = value.split('|');
    RefreshParts {
        refresh_token: parts.next().unwrap_or_default().into(),
        project_id: parts
            .next()
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        managed_project_id: parts
            .next()
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
    }
}
pub fn format_refresh(parts: &RefreshParts) -> String {
    let base = format!(
        "{}|{}",
        parts.refresh_token,
        parts.project_id.as_deref().unwrap_or_default()
    );
    match parts
        .managed_project_id
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        Some(managed) => format!("{base}|{managed}"),
        None => base,
    }
}
/// The access value the host stores: the bare token, plus `|<project>` once a login or refresh
/// has bound a project. Credentials stored before the binding are bare tokens.
pub fn format_access(token: &str, project: &str) -> String {
    if project.is_empty() {
        token.into()
    } else {
        format!("{token}|{project}")
    }
}
/// The bare token that is sent to Google, and the project bound to it when one is stored.
pub fn parse_access(value: &str) -> (&str, Option<&str>) {
    match value.split_once('|') {
        Some((token, project)) => (token, Some(project).filter(|project| !project.is_empty())),
        None => (value, None),
    }
}
/// The project a packed refresh resolves to: the managed project when one is recorded.
pub fn bound_project(packed: &str) -> String {
    let parts = parse_refresh(packed);
    parts
        .managed_project_id
        .or(parts.project_id)
        .unwrap_or_default()
}
/// Pi stores expiry in milliseconds; the host envelope carries whole seconds.
pub fn expires_at(request_ms: f64, seconds: &Value) -> f64 {
    let seconds = seconds.as_f64().unwrap_or(3600.0);
    if seconds.is_nan() || seconds <= 0.0 {
        request_ms
    } else {
        request_ms + seconds * 1000.0
    }
}

/// Access-token to packed-refresh bridge, keyed by the bare token. Pi's provider framework passes
/// only the access value to the stream. A bound access value names its project itself, so the
/// bridge only supplies the refresh-token context for a plain (unbound) access, and it is lost on
/// restart.
#[derive(Default)]
pub struct CredentialCache(IndexMap<String, String>);
impl CredentialCache {
    pub fn remember(&mut self, access: &str, refresh: &str) {
        if access.is_empty() {
            return;
        }
        if self.0.shift_remove(access).is_none() && self.0.len() >= 4 {
            self.0.shift_remove_index(0);
        }
        self.0.insert(access.into(), refresh.into());
    }
    pub fn get(&self, access: &str) -> Option<&str> {
        self.0.get(access).map(String::as_str)
    }
    pub fn forget(&mut self, access: &str) {
        self.0.shift_remove(access);
    }
}

/// Issued logins stay redeemable for this long.
pub const PENDING_TTL: Duration = Duration::from_secs(10 * 60);
/// Issued logins kept at once; issuing another one evicts the oldest.
pub const PENDING_LIMIT: usize = 8;
pub const NO_LOGIN: &str = "Start Antigravity login before pasting the code";
pub const MISMATCH: &str = "This login code belongs to a different login attempt; start again";

/// The PKCE verifier and project of one issued login. They stay in process and never reach
/// the login URL, the redirect or the host.
pub struct PendingLogin {
    pub verifier: String,
    pub project: String,
}

/// Input from the user: a bare authorization code, or a callback URL with its query.
pub enum Pasted {
    Bare(String),
    Callback { code: String, state: Option<String> },
}
impl Pasted {
    pub fn code(&self) -> &str {
        match self {
            Pasted::Bare(code) | Pasted::Callback { code, .. } => code,
        }
    }
}
pub fn parse_pasted(input: &str) -> Pasted {
    let text = trim(input);
    match Url::parse(text) {
        Ok(url) if url.query().is_some() => {
            let get = |name: &str| {
                url.query_pairs()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.into_owned())
                    .filter(|value| !value.is_empty())
            };
            Pasted::Callback {
                code: get("code").unwrap_or_default(),
                state: get("state"),
            }
        }
        _ => Pasted::Bare(text.to_owned()),
    }
}

/// Issued login attempts keyed by state. Each entry is single-use and expires after
/// `PENDING_TTL`; the table holds at most `PENDING_LIMIT` entries.
#[derive(Default)]
pub struct PendingLogins(IndexMap<String, Issued>);
struct Issued {
    login: PendingLogin,
    at: Instant,
}
impl PendingLogins {
    pub fn insert(&mut self, state: String, login: PendingLogin, now: Instant) {
        self.prune(now);
        while self.0.len() >= PENDING_LIMIT {
            self.0.shift_remove_index(0);
        }
        self.0.insert(state, Issued { login, at: now });
    }
    /// Takes the login a pasted input belongs to. A callback must carry a live state; a bare
    /// code takes the most recent live login. The entry is gone once taken, so a failed
    /// exchange cannot be retried against it, and a rejected input takes nothing.
    pub fn redeem(&mut self, pasted: &Pasted, now: Instant) -> Result<PendingLogin, String> {
        self.prune(now);
        let issued = match pasted {
            Pasted::Bare(_) => {
                let Some(latest) = self.0.keys().next_back().cloned() else {
                    return Err(NO_LOGIN.into());
                };
                self.0.shift_remove(&latest)
            }
            Pasted::Callback { state, .. } => state
                .as_deref()
                .and_then(|state| self.0.shift_remove(state)),
        };
        issued
            .map(|issued| issued.login)
            .ok_or_else(|| MISMATCH.to_owned())
    }
    fn prune(&mut self, now: Instant) {
        self.0
            .retain(|_, issued| now.saturating_duration_since(issued.at) < PENDING_TTL);
    }
}

/// A login URL, the state the redirect must return, and the login that redeems it.
pub struct Authorization {
    pub url: String,
    pub state: String,
    pub login: PendingLogin,
}
pub fn authorize(source: ClientSource) -> Result<Authorization, String> {
    let mut verifier_entropy = [0; 32];
    let mut state_entropy = [0; 32];
    getrandom::getrandom(&mut verifier_entropy).map_err(|error| error.to_string())?;
    getrandom::getrandom(&mut state_entropy).map_err(|error| error.to_string())?;
    authorize_with_entropy(&verifier_entropy, &state_entropy, source)
}
/// Entropy is injected only at the generation boundary; production uses the OS CSPRNG.
/// Fails before building any URL when the client is not configured.
// Deliberate divergence from Pi: `state` is a nonce, so the verifier never travels in the URL.
pub fn authorize_with_entropy(
    verifier_entropy: &[u8; 32],
    state_entropy: &[u8; 32],
    source: ClientSource,
) -> Result<Authorization, String> {
    let client = source.resolve()?;
    let verifier = URL_SAFE_NO_PAD.encode(verifier_entropy);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = URL_SAFE_NO_PAD.encode(state_entropy);
    let mut url = Url::parse(AUTHORIZE_URL).unwrap();
    url.query_pairs_mut().extend_pairs([
        ("client_id", client.id.as_str()),
        ("response_type", "code"),
        ("redirect_uri", REDIRECT_URI),
        ("scope", &SCOPES.join(" ")),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
        ("access_type", "offline"),
        ("prompt", "consent"),
    ]);
    Ok(Authorization {
        url: url.into(),
        state,
        // Begin binds no project; the exchange discovers one when the login has none.
        login: PendingLogin {
            verifier,
            project: String::new(),
        },
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub timeout_ms: u64,
}
pub trait Body: Send {
    fn text(self) -> impl Future<Output = Result<String, String>> + Send;
}
pub struct Reply<B> {
    pub status: u16,
    pub status_text: String,
    pub body: B,
}
impl<B> Reply<B> {
    pub(crate) fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}
/// The HTTP surface the token and project protocols need. Production uses reqwest.
pub trait Port: Sync {
    type Body: Body;
    fn send(
        &self,
        request: Request,
    ) -> impl Future<Output = Result<Reply<Self::Body>, String>> + Send;
    fn now_ms(&self) -> f64;
    fn user_agent(&self) -> String;
}
fn form(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().copied())
        .finish()
}
fn prop(value: &Value, key: &str) -> String {
    value
        .get(key)
        .map(string)
        .unwrap_or_else(|| "undefined".into())
}
pub(crate) async fn json_body(body: impl Body) -> Result<Value, String> {
    let text = body.text().await?;
    // serde's message names the position only, never the response text.
    serde_json::from_str(&text).map_err(|error| format!("invalid JSON response: {error}"))
}
pub fn bootstrap_headers(access: &str, user_agent: String) -> Vec<(String, String)> {
    vec![
        ("User-Agent".into(), user_agent),
        ("Authorization".into(), format!("Bearer {access}")),
        ("Content-Type".into(), "application/json".into()),
    ]
}

/// Refreshes a packed credential. The returned `refresh` keeps the project segments.
pub async fn refresh(
    port: &impl Port,
    source: ClientSource,
    endpoints: &Endpoints,
    packed: &str,
) -> Result<Value, String> {
    let client = source.resolve()?;
    let (refresh, suffix) = packed
        .find('|')
        .map_or((packed, ""), |index| (&packed[..index], &packed[index..]));
    let started = port.now_ms();
    let response = port
        .send(Request {
            url: endpoints.token.clone(),
            method: "POST".into(),
            timeout_ms: 15_000,
            headers: vec![(
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            )],
            body: Some(form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("client_id", client.id.as_str()),
                ("client_secret", client.secret.as_str()),
            ])),
        })
        .await?;
    if !response.ok() {
        // Google's body can echo the request; report the status only.
        let _ = response.body.text().await;
        return Err(format!(
            "Antigravity token refresh failed ({} {})",
            response.status, response.status_text
        ));
    }
    let payload = json_body(response.body).await?;
    let rotated = payload
        .get("refresh_token")
        .filter(|value| !value.is_null())
        .map(string)
        .unwrap_or_else(|| refresh.into());
    let mut result = json!({"refresh":format!("{rotated}{suffix}")});
    if let Some(access) = payload.get("access_token") {
        result["access"] = access.clone();
    }
    result["expires"] = finite_number(expires_at(started, &payload["expires_in"]));
    Ok(result)
}

/// Revokes the refresh token, or the access token when none is stored.
pub async fn revoke(
    port: &impl Port,
    endpoints: &Endpoints,
    packed: &str,
    access: &str,
) -> Result<(), String> {
    let stored = parse_refresh(packed).refresh_token;
    let token = if stored.is_empty() { access } else { &stored };
    if token.is_empty() {
        return Err("Antigravity has no credential to revoke".into());
    }
    post_revoke(port, endpoints, token).await
}
async fn post_revoke(port: &impl Port, endpoints: &Endpoints, token: &str) -> Result<(), String> {
    let response = port
        .send(Request {
            url: endpoints.revoke.clone(),
            method: "POST".into(),
            timeout_ms: 15_000,
            headers: vec![(
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            )],
            body: Some(form(&[("token", token)])),
        })
        .await?;
    if response.ok() {
        Ok(())
    } else {
        let _ = response.body.text().await;
        Err(format!(
            "Antigravity token revocation failed ({} {})",
            response.status, response.status_text
        ))
    }
}

/// Redeems the pasted input against the pending logins, then exchanges its code. A rejected
/// input never reaches the port.
pub async fn complete(
    port: &impl Port,
    source: ClientSource,
    endpoints: &Endpoints,
    logins: &Mutex<PendingLogins>,
    pasted: &str,
    now: Instant,
) -> Result<Value, String> {
    let pasted = parse_pasted(pasted);
    if pasted.code().is_empty() {
        return Err("Antigravity OAuth input is empty".into());
    }
    let login = logins.lock().unwrap().redeem(&pasted, now)?;
    Ok(exchange(port, source, endpoints, pasted.code(), &login).await)
}

/// Exchanges a code for a credential with the verifier and project of the login it redeemed.
pub async fn exchange(
    port: &impl Port,
    source: ClientSource,
    endpoints: &Endpoints,
    code: &str,
    login: &PendingLogin,
) -> Value {
    match exchange_inner(port, source, endpoints, code, login).await {
        Ok(value) => value,
        Err(error) => json!({"type":"failed","error":error}),
    }
}
async fn exchange_inner(
    port: &impl Port,
    source: ClientSource,
    endpoints: &Endpoints,
    code: &str,
    login: &PendingLogin,
) -> Result<Value, String> {
    let client = source.resolve()?;
    let started = port.now_ms();
    let response = port
        .send(Request {
            url: endpoints.token.clone(),
            method: "POST".into(),
            timeout_ms: 15_000,
            headers: vec![
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded;charset=UTF-8".into(),
                ),
                ("Accept".into(), "*/*".into()),
                ("User-Agent".into(), port.user_agent()),
            ],
            body: Some(form(&[
                ("client_id", client.id.as_str()),
                ("client_secret", client.secret.as_str()),
                ("code", code),
                ("grant_type", "authorization_code"),
                ("redirect_uri", REDIRECT_URI),
                ("code_verifier", &login.verifier),
            ])),
        })
        .await?;
    if !response.ok() {
        let _ = response.body.text().await;
        return Ok(json!({"type":"failed","error":format!(
            "Antigravity token exchange failed ({} {})", response.status, response.status_text)}));
    }
    let tokens = json_body(response.body).await?;
    let response = port
        .send(Request {
            url: endpoints.userinfo.clone(),
            method: "GET".into(),
            timeout_ms: 15_000,
            headers: vec![
                (
                    "Authorization".into(),
                    format!("Bearer {}", prop(&tokens, "access_token")),
                ),
                ("User-Agent".into(), port.user_agent()),
            ],
            body: None,
        })
        .await?;
    // Profile details are display-only; a failed lookup does not block the login.
    if !response.ok() {
        let _ = response.body.text().await;
    }
    if !truthy(&tokens["refresh_token"]) {
        return Ok(json!({"type":"failed","error":"Missing refresh token in response"}));
    }
    let project = if login.project.is_empty() {
        discover_login_project(port, endpoints, &prop(&tokens, "access_token")).await?
    } else {
        login.project.clone()
    };
    let mut result =
        json!({"type":"success","refresh":format!("{}|{project}",prop(&tokens,"refresh_token"))});
    if let Some(access) = tokens.get("access_token") {
        result["access"] = access.clone();
    }
    result["expires"] = finite_number(expires_at(started, &tokens["expires_in"]));
    result["projectId"] = project.into();
    Ok(result)
}
async fn discover_login_project(
    port: &impl Port,
    endpoints: &Endpoints,
    access: &str,
) -> Result<String, String> {
    for endpoint in &endpoints.load {
        let attempt = async {
            let response = port
                .send(Request {
                    url: format!("{endpoint}/v1internal:loadCodeAssist"),
                    method: "POST".into(),
                    timeout_ms: 10_000,
                    headers: bootstrap_headers(access, port.user_agent()),
                    body: Some("{\"metadata\":{\"ideType\":\"ANTIGRAVITY\"}}".into()),
                })
                .await?;
            if !response.ok() {
                let _ = response.body.text().await;
                return Ok(None);
            }
            let payload = json_body(response.body).await?;
            Ok::<_, String>(
                payload["cloudaicompanionProject"]
                    .as_str()
                    .or_else(|| payload["cloudaicompanionProject"]["id"].as_str())
                    .filter(|project| !project.is_empty())
                    .map(str::to_owned),
            )
        }
        .await;
        // A failed endpoint falls through to the next one, as Pi's discovery does.
        if let Ok(Some(project)) = attempt {
            return Ok(project);
        }
    }
    Ok(String::new())
}
