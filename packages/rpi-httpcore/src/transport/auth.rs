use super::origin::with_security_headers;
use crate::access::{AccessState, AuthLease};
use axum::{
    extract::State,
    http::{header, Request, StatusCode},
    middleware::from_fn_with_state,
    response::{IntoResponse, Response},
    Json, Router,
};
use serde_json::json;

/// Protect a generic owner router with an `Authorization: Bearer` Lease.
///
/// The validated Lease is inserted into request extensions, allowing an owner
/// route to use `Extension<AuthLease>` without depending on cookie parsing or
/// the rpi Service type.
pub fn bearer_auth_router(routes: Router, access: AccessState) -> Router {
    routes.layer(from_fn_with_state(access, bearer_auth))
}

async fn bearer_auth(
    State(access): State<AccessState>,
    mut request: Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Response {
    let Some(credential) = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
    else {
        return unauthorized();
    };
    let Ok(lease) = access.authenticate(credential).await else {
        return unauthorized();
    };
    request.extensions_mut().insert(lease);
    with_security_headers(next.run(request).await)
}

fn unauthorized() -> Response {
    with_security_headers(
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"unauthorized"})),
        )
            .into_response(),
    )
}

/// Extract a Lease inserted by [`bearer_auth_router`] from request extensions.
pub fn lease_from_request<B>(request: &Request<B>) -> Option<&AuthLease> {
    request.extensions().get::<AuthLease>()
}
