//! Password and same-origin protection for a public rpi-web deployment.
use crate::{
    access::{AccessState, AuthLease, PrincipalId},
    transport::HostOriginPolicy,
};
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
    sync::Arc,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;

const SESSION_SECONDS: u64 = 12 * 60 * 60;

#[derive(Clone, Default)]
pub(crate) struct RequestAuthorization {
    lease: Option<AuthLease>,
}

pub struct Security {
    host_policy: HostOriginPolicy,
    password: Option<[u8; 32]>,
    access: AccessState,
    attempts: Mutex<Vec<Instant>>,
}

impl Security {
    pub fn new(hosts: impl IntoIterator<Item = String>, password: Option<&str>) -> Self {
        Self {
            host_policy: HostOriginPolicy::new(hosts),
            access: AccessState::new(password.is_some(), 64),
            password: password.map(digest),
            attempts: Mutex::new(Vec::new()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.password.is_some()
    }

    pub fn host_allowed(&self, headers: &HeaderMap) -> bool {
        self.host_policy.host_allowed(headers)
    }

    pub fn origin_allowed(&self, headers: &HeaderMap) -> bool {
        self.host_policy.origin_allowed(headers)
    }

    pub async fn authorized(&self, headers: &HeaderMap) -> bool {
        !self.enabled() || self.lease_for(headers).await.is_some()
    }

    pub(crate) async fn lease_for(&self, headers: &HeaderMap) -> Option<crate::access::AuthLease> {
        let token = cookie(headers)?;
        self.access.authenticate(token).await.ok()
    }

    pub(crate) fn access_state(&self) -> AccessState {
        self.access.clone()
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
        let principal = PrincipalId::new("password")
            .expect("the built-in password principal must be non-empty");
        let lease = self
            .access
            .issue(principal, Duration::from_secs(SESSION_SECONDS))
            .await
            .map_err(|_| StatusCode::CONFLICT)?;
        Ok(lease.credential().to_owned())
    }

    pub async fn logout(&self, headers: &HeaderMap) {
        if let Some(token) = cookie(headers) {
            self.access.revoke_credential(token).await;
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
    HostOriginPolicy::request_marker_present(headers, method)
}

pub async fn guard(
    State(security): State<Arc<Security>>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let headers = request.headers().clone();
    let method = request.method().as_str();
    let path = request.uri().path().to_owned();
    let is_auth = path == "/api/v1/auth";
    let is_health = path == "/api/v1/health";
    let host_allowed = security.host_allowed(&headers);
    let origin_allowed = security.origin_allowed(&headers);
    let lease = if security.enabled() {
        security.lease_for(&headers).await
    } else {
        None
    };
    let protected_api = !is_auth && !is_health && path.starts_with("/api/");
    let allowed = host_allowed
        && origin_allowed
        && request_marker_present(&headers, method)
        && (!protected_api || !security.enabled() || lease.is_some());
    let mut response = if allowed {
        request.extensions_mut().insert(RequestAuthorization {
            lease: lease.clone(),
        });
        if let Some(lease) = lease.clone() {
            request.extensions_mut().insert(lease);
        }
        let response = next.run(request).await;
        if protected_api && response.status().is_success() {
            if let Some(lease) = lease {
                if !security.access.while_authorized(&lease).await {
                    (
                        StatusCode::UNAUTHORIZED,
                        Json(json!({"error": "unauthorized"})),
                    )
                        .into_response()
                } else {
                    response
                }
            } else {
                response
            }
        } else {
            response
        }
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
    if is_auth || path.starts_with("/api/v1/auth/") {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub password: String,
}

pub(crate) async fn authorize_before_commit(
    security: &Security,
    authorization: &RequestAuthorization,
) -> bool {
    match &authorization.lease {
        Some(lease) => security.access.authorize_before_commit(lease).await.is_ok(),
        None => !security.enabled(),
    }
}

pub(crate) fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "unauthorized"})),
    )
        .into_response()
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
    let secure = secure_request(headers);
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

fn secure_request(headers: &HeaderMap) -> bool {
    let origin_is_secure = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin.starts_with("https://"));
    let forwarded_proto_is_secure = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("https"));
    let forwarded_is_secure = headers
        .get("forwarded")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(';').any(|part| {
                part.trim()
                    .strip_prefix("proto=")
                    .is_some_and(|proto| proto.eq_ignore_ascii_case("https"))
            })
        });
    origin_is_secure || forwarded_proto_is_secure || forwarded_is_secure
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{header, Request},
        routing::get,
        Router,
    };
    use std::sync::Arc;
    use tower::ServiceExt;

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

    #[tokio::test]
    async fn authorize_before_commit_rejects_a_revoked_lease() {
        let security = Security::new(["example.test".to_string()], Some("secret"));
        let token = security.login("secret").await.unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("rpi_web_session={token}").parse().unwrap(),
        );
        let authorization = RequestAuthorization {
            lease: security.lease_for(&headers).await,
        };
        assert!(authorize_before_commit(&security, &authorization).await);
        security.logout(&headers).await;
        assert!(!authorize_before_commit(&security, &authorization).await);
    }

    #[tokio::test]
    async fn guard_suppresses_success_after_handler_revokes_lease() {
        let security = Arc::new(Security::new(["example.test".to_string()], Some("secret")));
        let token = security.login("secret").await.unwrap();
        let handler_security = security.clone();
        let app = Router::new()
            .route(
                "/api/v1/mutation",
                get(move || {
                    let security = handler_security.clone();
                    async move {
                        security.access.revoke_all().await;
                        StatusCode::OK
                    }
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                security.clone(),
                guard,
            ));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("http://example.test/api/v1/mutation")
                    .header(header::HOST, "example.test")
                    .header(header::COOKIE, format!("rpi_web_session={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn sharing_revoke_invalidates_password_sessions_after_reopen() {
        let security = Security::new(["example.test".to_string()], Some("secret"));
        let token = security.login("secret").await.unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("rpi_web_session={token}").parse().unwrap(),
        );
        assert!(security.authorized(&headers).await);

        let closed = security.access.revoke_all().await;
        assert_eq!(closed.generation, 1);
        assert!(!security.authorized(&headers).await);

        security.access.enable_sharing().await;
        assert!(!security.authorized(&headers).await);
    }

    #[tokio::test]
    async fn secure_cookie_follows_direct_and_forwarded_https() {
        let security = Security::new(["example.test".to_string()], Some("secret"));
        for (name, value) in [
            ("origin", "https://example.test"),
            ("x-forwarded-proto", "https"),
            ("forwarded", "for=127.0.0.1;proto=https"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(name, value.parse().unwrap());
            let response = login_for(
                &security,
                &headers,
                LoginRequest {
                    password: "secret".to_string(),
                },
            )
            .await;
            assert!(response.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .contains("; Secure"));
        }
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
