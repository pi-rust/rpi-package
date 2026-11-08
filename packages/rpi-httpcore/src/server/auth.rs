use super::{AppState, HttpReply};
use crate::config::AppConfig;
use crate::security;
use axum::{
    extract::{Json, State},
    response::IntoResponse,
};
use rpi_extcore::{
    reply::IntoReply,
    service::{self, AuthRefreshRequest},
};
use serde_json::Value;
use std::net::SocketAddr;

pub(super) fn configured_password(config: &AppConfig) -> Option<String> {
    let (primary, legacy) = if config.service_name == "rpi-web" {
        ("RPI_WEB_PASSWORD", "RPI_HTTP_PASSWORD")
    } else {
        ("RPI_HTTP_PASSWORD", "RPI_WEB_PASSWORD")
    };
    std::env::var(primary)
        .or_else(|_| std::env::var(legacy))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn allowed_hosts(config: &AppConfig) -> Vec<String> {
    let (primary, legacy) = if config.service_name == "rpi-web" {
        ("RPI_WEB_ALLOWED_HOSTS", "RPI_HTTP_ALLOWED_HOSTS")
    } else {
        ("RPI_HTTP_ALLOWED_HOSTS", "RPI_WEB_ALLOWED_HOSTS")
    };
    std::env::var(primary)
        .or_else(|_| std::env::var(legacy))
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|host| !host.is_empty())
                .map(str::to_string)
                .collect()
        })
        .filter(|hosts: &Vec<String>| !hosts.is_empty())
        .or_else(|| config.allowed_hosts.clone())
        .unwrap_or_else(|| {
            let mut hosts = vec![
                format!("127.0.0.1:{}", config.port),
                format!("localhost:{}", config.port),
            ];
            if let Ok(ip) = config.bind.parse::<std::net::IpAddr>() {
                if ip.is_ipv6() {
                    hosts.push(format!("[::1]:{}", config.port));
                }
                if !ip.is_unspecified() {
                    let authority = SocketAddr::new(ip, config.port).to_string();
                    if !hosts.contains(&authority) {
                        hosts.push(authority);
                    }
                }
            }
            hosts
        })
}

pub(super) async fn auth_status(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Json<Value> {
    security::status_for(&state.security, &headers).await
}

pub(super) async fn auth_login(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(request): Json<security::LoginRequest>,
) -> impl IntoResponse {
    security::login_for(&state.security, &headers, request).await
}

pub(super) async fn auth_logout(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    security::logout_for(&state.security, &headers).await
}

pub(super) async fn refresh_auth(
    State(state): State<AppState>,
    Json(request): Json<AuthRefreshRequest>,
) -> axum::response::Response {
    HttpReply(
        service::refresh_auth(&state.backend, request)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/api/v1/auth",
            axum::routing::get(auth_status)
                .post(auth_login)
                .delete(auth_logout),
        )
        .route("/api/v1/auth/refresh", axum::routing::post(refresh_auth))
}
