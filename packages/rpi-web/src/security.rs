//! Password and same-origin protection for a public rpi-web deployment.
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;
use uuid::Uuid;

const SESSION_SECONDS: u64 = 12 * 60 * 60;

pub struct Security {
    hosts: HashSet<String>,
    password: Option<[u8; 32]>,
    sessions: Mutex<HashMap<String, Instant>>,
    attempts: Mutex<Vec<Instant>>,
}

impl Security {
    pub fn new(hosts: impl IntoIterator<Item = String>, password: Option<&str>) -> Self {
        Self {
            hosts: hosts.into_iter().collect(),
            password: password.map(digest),
            sessions: Mutex::new(HashMap::new()),
            attempts: Mutex::new(Vec::new()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.password.is_some()
    }

    pub fn host_allowed(&self, headers: &HeaderMap) -> bool {
        headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|host| self.hosts.contains(host))
    }

    pub fn origin_allowed(&self, headers: &HeaderMap) -> bool {
        let Some(origin) = headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
        else {
            return true;
        };
        let Some(authority) = origin
            .strip_prefix("https://")
            .or_else(|| origin.strip_prefix("http://"))
        else {
            return false;
        };
        self.hosts.contains(authority.trim_end_matches('/'))
    }

    pub async fn authorized(&self, headers: &HeaderMap) -> bool {
        if !self.enabled() {
            return true;
        }
        let Some(token) = cookie(headers) else {
            return false;
        };
        let now = Instant::now();
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, expiry| *expiry > now);
        sessions.contains_key(token)
    }

    pub async fn login(&self, password: &str) -> Result<String, StatusCode> {
        let now = Instant::now();
        let mut attempts = self.attempts.lock().await;
        attempts.retain(|time| now.duration_since(*time) < Duration::from_secs(60));
        if attempts.len() >= 10 {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        attempts.push(now);
        let matches = self
            .password
            .as_ref()
            .is_some_and(|expected| bool::from(expected.ct_eq(&digest(password))));
        if !matches {
            return Err(StatusCode::UNAUTHORIZED);
        }
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, expiry| *expiry > now);
        if sessions.len() >= 64 {
            return Err(StatusCode::CONFLICT);
        }
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        sessions.insert(token.clone(), now + Duration::from_secs(SESSION_SECONDS));
        Ok(token)
    }

    pub async fn logout(&self, headers: &HeaderMap) {
        if let Some(token) = cookie(headers) {
            self.sessions.lock().await.remove(token);
        }
    }
}

fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

fn cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|value| value.trim().strip_prefix("rpi_web_session="))
}

fn request_marker_present(headers: &HeaderMap, method: &str) -> bool {
    matches!(method, "GET" | "HEAD" | "OPTIONS")
        || headers
            .get("x-rpi-web")
            .and_then(|value| value.to_str().ok())
            == Some("1")
}

pub async fn guard(
    State(security): State<Arc<Security>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let headers = request.headers().clone();
    let method = request.method().as_str();
    let path = request.uri().path();
    let is_auth = path == "/api/v1/auth";
    let is_health = path == "/api/v1/health";
    let host_allowed = security.host_allowed(&headers);
    let origin_allowed = security.origin_allowed(&headers);
    let allowed = host_allowed
        && origin_allowed
        && request_marker_present(&headers, method)
        && (is_auth
            || is_health
            || !path.starts_with("/api/")
            || security.authorized(&headers).await);
    let mut response = if allowed {
        next.run(request).await
    } else {
        let status = if host_allowed && origin_allowed && request_marker_present(&headers, method) {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::FORBIDDEN
        };
        (status, Json(json!({"error": "unauthorized"}))).into_response()
    };
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
        .headers_mut()
        .insert("x-frame-options", HeaderValue::from_static("DENY"));
    response
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub password: String,
}

pub async fn status_for(security: &Security, headers: &HeaderMap) -> Json<serde_json::Value> {
    Json(json!({
        "required": security.enabled(),
        "authenticated": security.authorized(headers).await
    }))
}

pub async fn login_for(
    security: &Security,
    headers: &HeaderMap,
    request: LoginRequest,
) -> Response {
    if !security.enabled() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "password authentication is not configured"})),
        )
            .into_response();
    }
    let token = match security.login(&request.password).await {
        Ok(token) => token,
        Err(status) => {
            return (status, Json(json!({"error": "authentication failed"}))).into_response()
        }
    };
    let secure = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin.starts_with("https://"));
    let secure_suffix = if secure { "; Secure" } else { "" };
    let mut response = Json(json!({"authenticated": true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        format!(
            "rpi_web_session={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={SESSION_SECONDS}{secure_suffix}"
        )
        .parse()
        .expect("valid session cookie"),
    );
    response
}

pub async fn logout_for(security: &Security, headers: &HeaderMap) -> Response {
    security.logout(headers).await;
    let mut response = Json(json!({"authenticated": false})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("rpi_web_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header;

    #[tokio::test]
    async fn password_sessions_are_scoped_and_logout_revokes_them() {
        let security = Security::new(["example.test".to_string()], Some("secret"));
        assert_eq!(security.login("wrong").await, Err(StatusCode::UNAUTHORIZED));
        let token = security.login("secret").await.unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("rpi_web_session={token}").parse().unwrap(),
        );
        assert!(security.authorized(&headers).await);
        let response = logout_for(&security, &headers).await;
        assert!(response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0"));
        assert!(!security.authorized(&headers).await);
    }

    #[test]
    fn origins_must_match_an_allowed_host() {
        let security = Security::new(["example.test".to_string()], None);
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "https://example.test/".parse().unwrap());
        assert!(security.origin_allowed(&headers));
        headers.insert(header::ORIGIN, "https://evil.test".parse().unwrap());
        assert!(!security.origin_allowed(&headers));
    }

    #[test]
    fn non_get_requests_require_the_rpc_marker() {
        let mut headers = HeaderMap::new();
        assert!(!request_marker_present(&headers, "POST"));
        headers.insert("x-rpi-web", "1".parse().unwrap());
        assert!(request_marker_present(&headers, "POST"));
    }
}
