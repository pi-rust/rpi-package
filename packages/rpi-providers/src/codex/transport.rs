//! reqwest transport for the Codex backend. Credentials enter per physical send only.
use super::{
    auth::{self, Endpoints, Port, Reply},
    convert,
    driver::{self, Deadline, Response, Secrets},
};
use rpi_ai::types::ThinkingLevel;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const IDLE_MESSAGE: &str =
    "OpenAI Codex stream stopped sending data before the idle time limit. Try again later.";

/// reqwest client plus endpoints for one provider instance. Holds no credentials.
#[derive(Clone)]
pub struct Net {
    pub client: reqwest::Client,
    pub endpoints: Endpoints,
    pub user_agent: String,
}
impl Net {
    async fn request(&self, request: auth::Request) -> Result<reqwest::Response, String> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|error| error.to_string())?;
        let mut builder = self
            .client
            .request(method, &request.url)
            .timeout(Duration::from_millis(request.timeout_ms));
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        builder.send().await.map_err(|error| error.to_string())
    }
}
pub struct NetBody(reqwest::Response);
impl auth::Body for NetBody {
    async fn text(self) -> Result<String, String> {
        self.0.text().await.map_err(|error| error.to_string())
    }
}
impl Port for Net {
    type Body = NetBody;
    async fn send(&self, request: auth::Request) -> Result<Reply<NetBody>, String> {
        let response = self.request(request).await?;
        Ok(Reply {
            status: response.status().as_u16(),
            status_text: response
                .status()
                .canonical_reason()
                .unwrap_or_default()
                .into(),
            body: NetBody(response),
        })
    }
    fn now_ms(&self) -> f64 {
        now_ms() as f64
    }
    fn user_agent(&self) -> String {
        self.user_agent.clone()
    }
}

/// Body reader for one streamed response. A silent connection fails after `idle`.
pub struct LiveBody {
    response: reqwest::Response,
    idle: Option<Duration>,
}
impl driver::Body for LiveBody {
    async fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
        let chunk = match self.idle {
            Some(idle) => tokio::time::timeout(idle, self.response.chunk())
                .await
                .map_err(|_| IDLE_MESSAGE.to_owned())?,
            None => self.response.chunk().await,
        };
        chunk
            .map(|bytes| bytes.map(|bytes| bytes.to_vec()))
            .map_err(|error| format!("OpenAI Codex stream read failed: {error}"))
    }
}

/// One Codex "session id" per host session id, matching Pi's stable codex session id.
fn codex_session_id(session_id: &str) -> String {
    static SESSIONS: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
    let sessions = SESSIONS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut sessions = sessions.lock().unwrap();
    if sessions.len() > 64 {
        sessions.clear();
    }
    sessions
        .entry(session_id.to_owned())
        .or_insert_with(super::mod_uuid_v4)
        .clone()
}

/// The per-request source: resolves the account from the access JWT, builds the body
/// and performs one send.
pub struct Live {
    pub net: Arc<Net>,
    pub root: String,
    pub idle: Option<Duration>,
}
impl driver::Source for Live {
    type Body = LiveBody;
    fn now_ms(&self) -> i64 {
        now_ms()
    }
    async fn open(
        &self,
        model: &Value,
        context: &Value,
        options: &Value,
        deadline: &Deadline,
        secrets: &mut Secrets,
    ) -> Result<Response<LiveBody>, String> {
        let token = options["apiKey"]
            .as_str()
            .filter(|key| !key.is_empty())
            .ok_or("Missing OpenAI Codex OAuth access token")?;
        // The account header comes from the JWT, exactly as Pi's stream does; a token
        // without the claim cannot reach the Codex backend.
        let account = auth::account_id(token)
            .ok_or_else(|| "Failed to extract accountId from token".to_owned())?;
        secrets.credential(token);
        secrets.account(&account);
        let reasoning = match options["reasoning"].as_str() {
            None | Some("off") => None,
            Some(level) => serde_json::from_value::<ThinkingLevel>(json!(level)).ok(),
        };
        let session_id = options["sessionId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(codex_session_id);
        let temperature = options["temperature"].as_f64();
        let body = convert::build_request_body(
            model,
            context,
            reasoning,
            session_id.as_deref(),
            temperature,
        )?;
        deadline.check()?;
        let response = deadline
            .wait(self.dispatch(&body, token, &account, session_id.as_deref()))
            .await?;
        Ok(Response {
            status: response.status().as_u16(),
            body: LiveBody {
                response,
                idle: self.idle,
            },
        })
    }
}
impl Live {
    async fn dispatch(
        &self,
        body: &Value,
        token: &str,
        account: &str,
        session_id: Option<&str>,
    ) -> Result<reqwest::Response, String> {
        let mut request = self
            .net
            .client
            .post(responses_url(&self.root))
            .header("Authorization", format!("Bearer {token}"))
            .header("chatgpt-account-id", account)
            .header("OpenAI-Beta", "responses=experimental")
            .header("accept", "text/event-stream")
            .header("content-type", "application/json")
            .header("originator", "pi")
            .header("User-Agent", self.net.user_agent.as_str());
        if let Some(session_id) = session_id {
            request = request
                .header("session-id", session_id)
                .header("x-client-request-id", session_id);
        }
        request
            .body(serde_json::to_string(body).unwrap_or_else(|_| "{}".into()))
            .send()
            .await
            .map_err(|error| format!("OpenAI Codex request failed: {error}"))
    }
}

/// Accept the backend root, Codex base or complete Responses endpoint.
pub(super) fn responses_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/codex/responses") {
        base.to_owned()
    } else if base.ends_with("/codex") {
        format!("{base}/responses")
    } else {
        format!("{base}/codex/responses")
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}
