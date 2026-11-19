//! TLS termination for the WebSocket transport.
//!
//! The listener never generates or fetches credentials: the operator supplies an
//! existing PEM certificate chain and private key, and every failure is reported
//! without echoing file contents.

use rustls::ServerConfig;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// PEM files backing `wss://`.
#[derive(Debug, Clone)]
pub struct TlsFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
}

impl TlsFiles {
    pub fn new(cert: impl Into<PathBuf>, key: impl Into<PathBuf>) -> Self {
        Self {
            cert: cert.into(),
            key: key.into(),
        }
    }
}

/// Load a certificate chain and key into a rustls server configuration.
pub fn load_server_config(cert: &Path, key: &Path) -> Result<Arc<ServerConfig>, String> {
    let cert_bytes = std::fs::read(cert)
        .map_err(|error| format!("cannot read TLS certificate {}: {error}", cert.display()))?;
    let key_bytes = std::fs::read(key)
        .map_err(|error| format!("cannot read TLS key {}: {error}", key.display()))?;

    let chain = rustls_pemfile::certs(&mut cert_bytes.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "TLS certificate file has no PEM certificates".to_string())?;
    if chain.is_empty() {
        return Err("TLS certificate file has no PEM certificates".into());
    }
    let private_key = rustls_pemfile::private_key(&mut key_bytes.as_slice())
        .map_err(|_| "TLS key file has no PEM private key".to_string())?
        .ok_or_else(|| "TLS key file has no PEM private key".to_string())?;

    ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, private_key)
        .map(Arc::new)
        .map_err(|_| "TLS certificate and key do not form a valid pair".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("rpi-server-tls-{}-{name}", std::process::id()));
        path
    }

    #[test]
    fn missing_files_fail_without_echoing_contents() {
        let error = load_server_config(&temp_path("absent.pem"), &temp_path("absent.key"))
            .expect_err("must fail");
        assert!(error.contains("cannot read TLS certificate"), "{error}");
    }

    #[test]
    fn non_pem_input_is_rejected() {
        let cert = temp_path("garbage.pem");
        let key = temp_path("garbage.key");
        std::fs::write(&cert, b"not a certificate").unwrap();
        std::fs::write(&key, b"not a key").unwrap();
        let error = load_server_config(&cert, &key).expect_err("must fail");
        assert!(error.contains("no PEM certificates"), "{error}");
        let _ = std::fs::remove_file(&cert);
        let _ = std::fs::remove_file(&key);
    }
}
