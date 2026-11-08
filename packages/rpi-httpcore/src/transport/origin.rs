use axum::{
    extract::State,
    http::{header, HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json, Router,
};
use serde_json::json;
use std::{collections::HashSet, sync::Arc};

/// Shared Host/Origin and mutation-marker policy for HTTP owners.
#[derive(Clone)]
pub struct HostOriginPolicy {
    hosts: Arc<HashSet<String>>,
}

impl HostOriginPolicy {
    pub fn new(hosts: impl IntoIterator<Item = String>) -> Self {
        Self {
            hosts: Arc::new(hosts.into_iter().collect()),
        }
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

    pub fn request_marker_present(headers: &HeaderMap, method: &str) -> bool {
        matches!(method, "GET" | "HEAD" | "OPTIONS")
            || headers
                .get("x-rpi-web")
                .and_then(|value| value.to_str().ok())
                == Some("1")
    }
}

/// Apply Host/Origin and mutation-marker protection to an arbitrary owner
/// router. Authentication remains a separate policy layer.
pub fn same_origin_router(routes: Router, policy: HostOriginPolicy) -> Router {
    routes.layer(axum::middleware::from_fn_with_state(
        policy,
        same_origin_guard,
    ))
}

async fn same_origin_guard(
    State(policy): State<HostOriginPolicy>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let allowed = policy.host_allowed(request.headers())
        && policy.origin_allowed(request.headers())
        && HostOriginPolicy::request_marker_present(request.headers(), request.method().as_str());
    let response = if allowed {
        next.run(request).await
    } else {
        (StatusCode::FORBIDDEN, Json(json!({"error":"forbidden"}))).into_response()
    };
    with_security_headers(response)
}

pub(crate) fn with_security_headers(mut response: Response) -> Response {
    response.headers_mut().insert(
        "x-content-type-options",
        "nosniff".parse().expect("valid response header"),
    );
    response.headers_mut().insert(
        "referrer-policy",
        "no-referrer".parse().expect("valid response header"),
    );
    response.headers_mut().insert(
        "x-frame-options",
        "DENY".parse().expect("valid response header"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_origin_and_mutation_checks_are_shared_by_owner_adapters() {
        let policy = HostOriginPolicy::new(["127.0.0.1:3030".to_owned()]);
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:3030".parse().unwrap());
        headers.insert(header::ORIGIN, "http://127.0.0.1:3030".parse().unwrap());
        assert!(policy.host_allowed(&headers));
        assert!(policy.origin_allowed(&headers));
        assert!(HostOriginPolicy::request_marker_present(&headers, "GET"));
        assert!(!HostOriginPolicy::request_marker_present(&headers, "POST"));
        headers.insert("x-rpi-web", "1".parse().unwrap());
        assert!(HostOriginPolicy::request_marker_present(&headers, "POST"));
    }

    #[test]
    fn malformed_or_cross_origin_headers_are_rejected() {
        let policy = HostOriginPolicy::new(["localhost:3030".to_owned()]);
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "localhost:3030".parse().unwrap());
        headers.insert(header::ORIGIN, "https://attacker.invalid".parse().unwrap());
        assert!(!policy.origin_allowed(&headers));
        headers.insert(header::ORIGIN, "null".parse().unwrap());
        assert!(!policy.origin_allowed(&headers));
    }
}
