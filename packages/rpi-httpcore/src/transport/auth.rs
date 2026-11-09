use super::origin::with_security_headers;
use crate::access::{AccessState, AuthLease};
use axum::{
    extract::State,
    http::{header, HeaderMap, Request, StatusCode},
    middleware::from_fn_with_state,
    response::{IntoResponse, Response},
    Json, Router,
};
use serde_json::json;
use std::{future::Future, pin::Pin};

/// Resolve an authenticated Lease from request headers.
///
/// Implementations may use bearer credentials, cookies, mTLS metadata or an
/// owner-specific credential exchange. Returning a Lease means the provider
/// has authenticated it; the middleware still performs a final authorization
/// check before forwarding a successful response.
pub trait Authenticator: Clone + Send + Sync + 'static {
    fn authenticate(
        &self,
        access: AccessState,
        headers: HeaderMap,
    ) -> Pin<Box<dyn Future<Output = Option<AuthLease>> + Send>>;
}

impl<F, Fut> Authenticator for F
where
    F: Fn(AccessState, HeaderMap) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Option<AuthLease>> + Send + 'static,
{
    fn authenticate(
        &self,
        access: AccessState,
        headers: HeaderMap,
    ) -> Pin<Box<dyn Future<Output = Option<AuthLease>> + Send>> {
        Box::pin((self)(access, headers))
    }
}

#[derive(Clone)]
struct AuthState<A> {
    access: AccessState,
    authenticator: A,
}

/// Protect a generic owner router with a composable Lease authenticator.
///
/// The validated Lease is inserted into request extensions, allowing an owner
/// route to use `Extension<AuthLease>` without depending on cookie parsing or
/// the rpi Service type.
pub fn auth_router<A>(routes: Router, access: AccessState, authenticator: A) -> Router
where
    A: Authenticator,
{
    routes.layer(from_fn_with_state(
        AuthState {
            access,
            authenticator,
        },
        auth_middleware,
    ))
}

/// Protect a generic owner router with an `Authorization: Bearer` Lease.
pub fn bearer_auth_router(routes: Router, access: AccessState) -> Router {
    auth_router(routes, access, bearer_authenticator)
}

async fn bearer_authenticator(access: AccessState, headers: HeaderMap) -> Option<AuthLease> {
    let credential = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            let (scheme, credential) = value.split_once(' ')?;
            (scheme.eq_ignore_ascii_case("Bearer") && !credential.is_empty()).then_some(credential)
        })?;
    access.authenticate(credential).await.ok()
}

async fn auth_middleware<A>(
    State(state): State<AuthState<A>>,
    mut request: Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Response
where
    A: Authenticator,
{
    let Some(lease) = state
        .authenticator
        .authenticate(state.access.clone(), request.headers().clone())
        .await
    else {
        return unauthorized();
    };
    if !state.access.while_authorized(&lease).await {
        return unauthorized();
    }
    request.extensions_mut().insert(lease.clone());
    let response = next.run(request).await;
    if response.status().is_success() && !state.access.while_authorized(&lease).await {
        unauthorized()
    } else {
        with_security_headers(response)
    }
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
