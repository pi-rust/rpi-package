//! TLS termination for the WebSocket transport.
//!
//! The listener never generates or fetches credentials: the operator supplies an
//! existing PEM certificate chain and private key, and every failure is reported
//! without echoing file contents.

use rustls::ServerConfig;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// PEM files backing `wss://`. `client_ca` enables mTLS when set.
#[derive(Debug, Clone)]
pub struct TlsFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub client_ca: Option<PathBuf>,
}

impl TlsFiles {
    pub fn new(cert: impl Into<PathBuf>, key: impl Into<PathBuf>) -> Self {
        Self {
            cert: cert.into(),
            key: key.into(),
            client_ca: None,
        }
    }

    pub fn with_client_ca(mut self, ca: impl Into<PathBuf>) -> Self {
        self.client_ca = Some(ca.into());
        self
    }

    /// Load this file set, keeping the paths for later rotation.
    pub fn load(&self) -> Result<Arc<ServerConfig>, String> {
        load_server_config(&self.cert, &self.key, self.client_ca.as_deref())
    }
}

/// Load a certificate chain and key, optionally requiring client certificates
/// signed by `client_ca`.
pub fn load_server_config(
    cert: &Path,
    key: &Path,
    client_ca: Option<&Path>,
) -> Result<Arc<ServerConfig>, String> {
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

    let builder = match client_ca {
        Some(path) => {
            let verifier =
                rustls::server::WebPkiClientVerifier::builder(Arc::new(client_roots(path)?))
                    .build()
                    .map_err(|_| {
                        "TLS client CA is not usable for verifying client certificates".to_string()
                    })?;
            // A client CA means client certificates are mandatory; there is no
            // "verify when offered" mode because that silently accepts anyone.
            ServerConfig::builder().with_client_cert_verifier(verifier)
        }
        None => ServerConfig::builder().with_no_client_auth(),
    };
    builder
        .with_single_cert(chain, private_key)
        // A certificate/key mismatch is only detected at the handshake, so the
        // message here stays factual about what was rejected.
        .map(Arc::new)
        .map_err(|_| "TLS certificate chain or key is not usable for serving".to_string())
}

/// Trust anchors for client certificates; every certificate in the file is a root.
fn client_roots(path: &Path) -> Result<rustls::RootCertStore, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("cannot read TLS client CA {}: {error}", path.display()))?;
    let mut roots = rustls::RootCertStore::empty();
    for certificate in rustls_pemfile::certs(&mut bytes.as_slice()) {
        let certificate =
            certificate.map_err(|_| "TLS client CA has an invalid certificate".to_string())?;
        roots
            .add(certificate)
            .map_err(|_| "TLS client CA certificate is not usable as a root".to_string())?;
    }
    if roots.is_empty() {
        return Err("TLS client CA file has no PEM certificates".into());
    }
    Ok(roots)
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
        let error = load_server_config(&temp_path("absent.pem"), &temp_path("absent.key"), None)
            .expect_err("must fail");
        assert!(error.contains("cannot read TLS certificate"), "{error}");
    }

    #[test]
    fn a_missing_client_ca_is_reported() {
        let cert = temp_path("pair-cert.pem");
        let key = temp_path("pair-key.pem");
        let (cert_pem, key_pem) = generated_pair();
        std::fs::write(&cert, cert_pem).unwrap();
        std::fs::write(&key, key_pem).unwrap();
        let error = load_server_config(&cert, &key, Some(&temp_path("absent-ca.pem")))
            .expect_err("must fail");
        assert!(error.contains("cannot read TLS client CA"), "{error}");
        let _ = std::fs::remove_file(&cert);
        let _ = std::fs::remove_file(&key);
    }

    #[test]
    fn an_empty_client_ca_is_rejected() {
        let cert = temp_path("empty-ca-cert.pem");
        let key = temp_path("empty-ca-key.pem");
        let ca = temp_path("empty-ca.pem");
        let (cert_pem, key_pem) = generated_pair();
        std::fs::write(&cert, cert_pem).unwrap();
        std::fs::write(&key, key_pem).unwrap();
        std::fs::write(&ca, b"not a certificate").unwrap();
        let error = load_server_config(&cert, &key, Some(&ca)).expect_err("must fail");
        assert!(error.contains("no PEM certificates"), "{error}");
        let _ = std::fs::remove_file(&cert);
        let _ = std::fs::remove_file(&key);
        let _ = std::fs::remove_file(&ca);
    }

    /// Self-signed PEM pair; the shared test helper for these cases.
    fn generated_pair() -> (String, String) {
        let generated =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert");
        (generated.cert.pem(), generated.key_pair.serialize_pem())
    }

    #[test]
    fn non_pem_input_is_rejected() {
        let cert = temp_path("garbage.pem");
        let key = temp_path("garbage.key");
        std::fs::write(&cert, b"not a certificate").unwrap();
        std::fs::write(&key, b"not a key").unwrap();
        let error = load_server_config(&cert, &key, None).expect_err("must fail");
        assert!(error.contains("no PEM certificates"), "{error}");
        let _ = std::fs::remove_file(&cert);
        let _ = std::fs::remove_file(&key);
    }
}
