//! reqwest transport for the Cloud Code endpoints. Credentials enter per physical send only.
use super::{
    auth::{self, Endpoints, Port, Reply},
    catalog,
    driver::{self, Deadline, Response, Secrets},
    metadata::SessionStore,
    project::ProjectPort,
    request::{self, PreparedRequest},
    shared,
};
use serde_json::{json, Value};
use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const IDLE_MESSAGE: &str =
    "Antigravity stream stopped sending data before the idle time limit. Try again later.";

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
impl ProjectPort for Net {
    fn wait(&self, milliseconds: u64) -> impl Future<Output = ()> + Send {
        tokio::time::sleep(Duration::from_millis(milliseconds))
    }
    fn endpoints(&self) -> &Endpoints {
        &self.endpoints
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
            .map_err(|error| format!("Antigravity stream read failed: {error}"))
    }
}

/// The per-request source: resolves credentials, prepares the body and performs one send.
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
    fn next_id(&self) -> String {
        uuid_v4()
    }
    fn sessions(&self) -> &Mutex<SessionStore> {
        &shared().sessions
    }
    async fn open(
        &self,
        model: &Value,
        context: &Value,
        options: &Value,
        deadline: &Deadline,
        secrets: &mut Secrets,
    ) -> Result<Response<LiveBody>, String> {
        let stored = options["apiKey"]
            .as_str()
            .filter(|key| !key.is_empty())
            .ok_or("Missing Antigravity OAuth access token")?;
        // The stored value is `<token>` or `<token>|<project>`; only the token leaves the process.
        let (token, bound) = auth::parse_access(stored);
        if token.is_empty() {
            return Err("Missing Antigravity OAuth access token".into());
        }
        let token = token.to_owned();
        secrets.credential(&token);
        if catalog::is_retired(model["id"].as_str().unwrap_or_default()) {
            return Err(catalog::RETIRED_MESSAGE.into());
        }
        let project = match bound {
            // The login or refresh that stored this access already bound the project, so the
            // stream uses it without project discovery, even after a restart.
            Some(project) => project.to_owned(),
            None => {
                let packed = shared()
                    .credentials
                    .lock()
                    .unwrap()
                    .get(&token)
                    .unwrap_or_default()
                    .to_owned();
                let parts = auth::parse_refresh(&packed);
                for project in [parts.project_id, parts.managed_project_id]
                    .into_iter()
                    .flatten()
                {
                    secrets.project(&project);
                }
                let resolved = deadline
                    .wait(shared().projects.ensure(
                        self.net.clone(),
                        json!({"type":"oauth","refresh":packed,"access":token,"expires":now_ms() + 60_000}),
                    ))
                    .await?;
                resolved["effectiveProjectId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            }
        };
        secrets.project(&project);
        let prepared: PreparedRequest = {
            let mut sessions = shared().sessions.lock().unwrap();
            request::prepare(
                model,
                context,
                options,
                &mut sessions,
                now_ms(),
                &mut || uuid_v4(),
            )?
        };
        deadline.check()?;
        let response = deadline
            .wait(self.dispatch(&prepared, &token, &project))
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
        prepared: &PreparedRequest,
        token: &str,
        project: &str,
    ) -> Result<reqwest::Response, String> {
        // Pi sends `Accept-Encoding: gzip`; the workspace reqwest has no decoder, so none is requested.
        self.net
            .client
            .post(format!(
                "{}/v1internal:streamGenerateContent?alt=sse",
                self.root
            ))
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("User-Agent", self.net.user_agent.as_str())
            .body(prepared.body(project))
            .send()
            .await
            .map_err(|error| format!("Antigravity request failed: {error}"))
    }
}

/// The Antigravity CLI user agent Pi sends (`buildAntigravityHarnessUserAgent`), without a
/// per-request credential.
pub fn default_user_agent() -> String {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "windows",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "x86" => "386",
        "aarch64" => "arm64",
        other => other,
    };
    format!("antigravity/cli/1.3.5 (aidev_client; os_type={platform}; arch={arch}; cl=998863838; auth_method=consumer)")
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

/// Random RFC 4122 version-4 identifier from the OS CSPRNG.
pub fn uuid_v4() -> String {
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
