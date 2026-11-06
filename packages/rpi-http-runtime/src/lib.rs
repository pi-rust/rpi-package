//! Shared HTTP authentication/authorization. No workspace owner or Agent is created.
use axum::{
    extract::DefaultBodyLimit,
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tokio::sync::watch;
pub mod pairing;
pub mod security;
pub mod sharing;
pub use pairing::QrAuthorization;
pub use sharing::SharingControl;

pub struct AccessState {
    pub security: security::Security,
    pub shutdown: watch::Receiver<bool>,
    pub node_id: String,
    pub sharing: SharingControl,
    pub pairing: Arc<pairing::Pairing>,
}

pub fn auth_router<S: Clone + Send + Sync + 'static>(state: Arc<AccessState>) -> Router<S> {
    Router::new()
        .route(
            "/api/v1/auth",
            get(security::auth)
                .post(security::login)
                .delete(security::logout),
        )
        .route(
            "/api/v1/auth/qr",
            post(security::issue_qr).layer(DefaultBodyLimit::max(1024)),
        )
        .route(
            "/api/v1/auth/qr/redeem",
            post(security::redeem_qr).layer(DefaultBodyLimit::max(1024)),
        )
        .with_state(state)
}

/// Never log this or put it in a QR payload.
/// # Errors
/// Returns an error when the OS random source fails.
pub fn random_secret() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    Ok(hex(&bytes))
}

/// A phone-reachable advertised origin; never accepts credentials or secrets.
/// # Errors
/// Rejects unreachable local addresses, non-origins and URLs containing secrets.
pub fn public_address(value: &str) -> Result<(String, String), String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("尚未配置分享地址，请填写手机可达的完整 HTTP(S) 地址。".into());
    }
    let url = url::Url::parse(value)
        .map_err(|_| "分享地址无效，请填写完整 HTTP(S) 地址，例如 http://192.168.1.8:14180/。")?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("分享地址必须是无密码、无查询参数的 HTTP(S) origin".into());
    }
    let host = url.host_str().ok_or("missing public host")?;
    let plain = host.trim_matches(['[', ']']);
    if plain.eq_ignore_ascii_case("localhost")
        || plain.ends_with(".localhost")
        || plain
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
    {
        return Err("请填写手机可达地址，而不是 localhost、回环或通配监听地址".into());
    }
    let authority = if let Some(port) = url.port() {
        format!("{host}:{port}")
    } else {
        host.to_owned()
    };
    Ok((url.to_string(), authority))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(&mut out, "{byte:02x}");
            out
        })
}
