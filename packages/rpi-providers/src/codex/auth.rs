//! OpenAI Codex (ChatGPT OAuth) wire protocol, ported from pi-ai's
//! `auth/oauth/openai-codex.ts` to the rpi-providers SDK 0.8.3 surface.
//!
//! The client identity is the public Codex CLI client, exactly as upstream pi-ai
//! ships it; there is no client secret. The host owns persistence; this module
//! owns the wire protocol and never sees stored credentials outside a call.
use super::json::{finite_number, trim};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    sync::Mutex,
    time::{Duration, Instant},
};
use url::Url;

/// The public Codex CLI client, as in pi-ai (`CLIENT_ID`).
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Shared with the Codex CLI; the login instructions ask the user to paste the callback URL.
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
pub const SCOPE: &str = "openid profile email offline_access";
/// JWT claim carrying the ChatGPT account binding.
pub const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";

/// The OAuth endpoints. Production uses `Endpoints::production()`; unit tests point
/// the same code at a loopback server. Nothing in a request can change these.
#[derive(Clone, Debug)]
pub struct Endpoints {
    pub token: String,
}
impl Endpoints {
    pub fn production() -> Self {
        Self {
            token: TOKEN_URL.into(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
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
/// The HTTP surface the token protocol needs. Production uses reqwest.
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
pub(crate) async fn json_body(body: impl Body) -> Result<serde_json::Value, String> {
    let text = body.text().await?;
    // serde's message names the position only, never the response text.
    serde_json::from_str(&text).map_err(|error| format!("invalid JSON response: {error}"))
}

/// Issued logins stay redeemable for this long.
pub const PENDING_TTL: Duration = Duration::from_secs(10 * 60);
/// Issued logins kept at once; issuing another one evicts the oldest.
pub const PENDING_LIMIT: usize = 8;
pub const NO_LOGIN: &str = "Start OpenAI Codex login before pasting the code";
pub const MISMATCH: &str = "This login code belongs to a different login attempt; start again";

/// The PKCE verifier of one issued login. It stays in process and never reaches
/// the login URL, the redirect or the host.
#[derive(Debug)]
pub struct PendingLogin {
    pub verifier: String,
}

/// Input from the user: a bare authorization code, a callback URL with its query,
/// `code#state`, or a `code=...` parameter string. Mirrors `parseAuthorizationInput`.
pub struct Pasted {
    pub code: String,
    pub state: Option<String>,
}
impl Pasted {
    pub fn parse(input: &str) -> Self {
        let value = trim(input);
        if value.is_empty() {
            return Self {
                code: String::new(),
                state: None,
            };
        }
        if let Ok(url) = Url::parse(value) {
            let get = |name: &str| {
                url.query_pairs()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.into_owned())
                    .filter(|value| !value.is_empty())
            };
            return Self {
                code: get("code").unwrap_or_default(),
                state: get("state"),
            };
        }
        if let Some((code, state)) = value.split_once('#') {
            return Self {
                code: code.to_owned(),
                state: Some(state.to_owned()).filter(|state| !state.is_empty()),
            };
        }
        if value.contains("code=") {
            let params: std::collections::HashMap<_, _> =
                url::form_urlencoded::parse(value.as_bytes())
                    .into_owned()
                    .collect();
            return Self {
                code: params.get("code").cloned().unwrap_or_default(),
                state: params.get("state").cloned(),
            };
        }
        Self {
            code: value.to_owned(),
            state: None,
        }
    }
}

/// Issued login attempts keyed by state. Each entry is single-use and expires after
/// `PENDING_TTL`; the table holds at most `PENDING_LIMIT` entries.
#[derive(Default)]
pub struct PendingLogins(indexmap::IndexMap<String, Issued>);
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
    /// Takes the login a pasted input belongs to. A pasted state must match a live
    /// login exactly; a bare code takes the most recent live login. The entry is
    /// gone once taken, so a failed exchange cannot be retried against it.
    pub fn redeem(&mut self, pasted: &Pasted, now: Instant) -> Result<PendingLogin, String> {
        self.prune(now);
        let issued = match &pasted.state {
            Some(state) => self.0.shift_remove(state),
            None => {
                let Some(latest) = self.0.keys().next_back().cloned() else {
                    return Err(NO_LOGIN.into());
                };
                self.0.shift_remove(&latest)
            }
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
/// Entropy is injected only at the generation boundary; production uses the OS CSPRNG.
pub fn authorize_with_entropy(
    verifier_entropy: &[u8; 32],
    state_entropy: &[u8; 32],
) -> Authorization {
    let verifier = URL_SAFE_NO_PAD.encode(verifier_entropy);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = URL_SAFE_NO_PAD.encode(state_entropy);
    let mut url = Url::parse(AUTHORIZE_URL).expect("pinned authorize URL");
    url.query_pairs_mut().extend_pairs([
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPE),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "pi"),
    ]);
    Authorization {
        url: url.into(),
        state,
        login: PendingLogin { verifier },
    }
}
pub fn authorize() -> Result<Authorization, String> {
    let mut verifier_entropy = [0; 32];
    let mut state_entropy = [0; 32];
    getrandom::getrandom(&mut verifier_entropy).map_err(|error| error.to_string())?;
    getrandom::getrandom(&mut state_entropy).map_err(|error| error.to_string())?;
    Ok(authorize_with_entropy(&verifier_entropy, &state_entropy))
}

/// The ChatGPT account id bound into an access-token JWT, if present.
pub fn account_id(access_token: &str) -> Option<String> {
    let mut parts = access_token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    parts.next()?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value[JWT_CLAIM_PATH]["chatgpt_account_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}
/// A signed loopback JWT for tests: header.payload.signature, none of it validated.
#[cfg(test)]
pub fn test_jwt(account_id: &str) -> String {
    let part =
        |value: serde_json::Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap());
    format!(
        "{}.{}.signature",
        part(serde_json::json!({"alg": "none"})),
        part(serde_json::json!({JWT_CLAIM_PATH: {"chatgpt_account_id": account_id}})),
    )
}

/// Reads a token response. Pi requires all three fields; the account id must come
/// out of the access JWT or the credential is unusable for the Codex backend.
pub fn read_token_response(
    payload: &serde_json::Value,
    operation: &str,
    now_ms: f64,
) -> Result<serde_json::Value, String> {
    let access = payload
        .get("access_token")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("OpenAI Codex token {operation} response missing access_token"))?;
    let refresh = payload
        .get("refresh_token")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("OpenAI Codex token {operation} response missing refresh_token"))?;
    let expires_in = payload
        .get("expires_in")
        .and_then(|value| value.as_f64())
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or_else(|| format!("OpenAI Codex token {operation} response missing expires_in"))?;
    if account_id(access).is_none() {
        return Err(format!(
            "OpenAI Codex token {operation} response carries no ChatGPT account id"
        ));
    }
    Ok(serde_json::json!({
        "access": access,
        "refresh": refresh,
        "expires": finite_number(now_ms + expires_in * 1000.0),
    }))
}

/// Exchanges a code for tokens with the login's verifier.
pub async fn exchange(
    port: &impl Port,
    endpoints: &Endpoints,
    code: &str,
    login: &PendingLogin,
) -> Result<serde_json::Value, String> {
    let response = port
        .send(Request {
            url: endpoints.token.clone(),
            method: "POST".into(),
            timeout_ms: 15_000,
            headers: vec![
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                ),
                ("Accept".into(), "*/*".into()),
                ("User-Agent".into(), port.user_agent()),
            ],
            body: Some(form(&[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("code", code),
                ("code_verifier", &login.verifier),
                ("redirect_uri", REDIRECT_URI),
            ])),
        })
        .await?;
    if !response.ok() {
        // The body can echo the request; report the status only.
        let _ = response.body.text().await;
        return Err(format!(
            "OpenAI Codex token exchange failed ({} {})",
            response.status, response.status_text
        ));
    }
    let payload = json_body(response.body).await?;
    read_token_response(&payload, "exchange", port.now_ms())
}

/// Refreshes a stored refresh token. OpenAI rotates the refresh token on every call.
pub async fn refresh(
    port: &impl Port,
    endpoints: &Endpoints,
    refresh_token: &str,
) -> Result<serde_json::Value, String> {
    let response = port
        .send(Request {
            url: endpoints.token.clone(),
            method: "POST".into(),
            timeout_ms: 15_000,
            headers: vec![
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                ),
                ("User-Agent".into(), port.user_agent()),
            ],
            body: Some(form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", CLIENT_ID),
            ])),
        })
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            return Err(format!("OpenAI Codex token refresh error: {error}"));
        }
    };
    if !response.ok() {
        let _ = response.body.text().await;
        return Err(format!(
            "OpenAI Codex token refresh failed ({} {})",
            response.status, response.status_text
        ));
    }
    let payload = json_body(response.body).await?;
    read_token_response(&payload, "refresh", port.now_ms())
}

/// Redeems the pasted input against the pending logins, then exchanges its code. A
/// rejected input never reaches the port.
pub async fn complete(
    port: &impl Port,
    endpoints: &Endpoints,
    logins: &Mutex<PendingLogins>,
    pasted: &str,
    now: Instant,
) -> Result<serde_json::Value, String> {
    let pasted = Pasted::parse(pasted);
    if pasted.code.is_empty() {
        return Err("OpenAI Codex OAuth input is empty".into());
    }
    let login = logins.lock().unwrap().redeem(&pasted, now)?;
    exchange(port, endpoints, &pasted.code, &login).await
}
