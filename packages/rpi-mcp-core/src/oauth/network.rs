//! Restricted, credential-free OAuth HTTP transport. No connection or DNS cache is
//! shared across requests: the checked address set is pinned into each client.
use crate::{AuthError, Control};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fmt,
    future::Future,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::Arc,
    time::Instant,
};
use url::{Host, Url};

const MAX_URL: usize = 8 * 1024;
const MAX_BODY: usize = 64 * 1024;

pub struct NetworkPolicy {
    origins: HashSet<String>,
    private: HashMap<String, HashSet<IpAddr>>,
    development: HashSet<String>,
}
impl NetworkPolicy {
    pub fn new(origins: Vec<String>) -> Result<Self, AuthError> {
        let mut allowed = HashSet::new();
        for origin in origins {
            let url = origin_url(&origin)?;
            if url.scheme() != "https" {
                return Err(AuthError::UnsafeUrl);
            }
            allowed.insert(url.origin().ascii_serialization());
        }
        Ok(Self {
            origins: allowed,
            private: HashMap::new(),
            development: HashSet::new(),
        })
    }
    /// Authorize only this HTTPS origin and this exact set of resolved addresses.
    /// All answers, not just the connector's preferred answer, must be in the set.
    pub fn allow_private_origin(
        &mut self,
        origin: &str,
        addresses: Vec<IpAddr>,
    ) -> Result<(), AuthError> {
        let url = origin_url(origin)?;
        if url.scheme() != "https" || addresses.is_empty() {
            return Err(AuthError::UnsafeUrl);
        }
        let key = url.origin().ascii_serialization();
        self.origins.insert(key.clone());
        self.private.insert(key, addresses.into_iter().collect());
        Ok(())
    }
    /// Explicit development exception, never a hostname or wildcard exception.
    pub fn allow_development_origin(&mut self, origin: &str) -> Result<(), AuthError> {
        let url = origin_url(origin)?;
        if url.scheme() != "http" || !literal_ip(&url).is_some_and(|ip| ip.is_loopback()) {
            return Err(AuthError::UnsafeUrl);
        }
        self.development.insert(url.origin().ascii_serialization());
        Ok(())
    }
    fn check_url(&self, url: &Url) -> Result<(), AuthError> {
        validate_url(url)?;
        let key = url.origin().ascii_serialization();
        if let Some(Host::Domain(host)) = url.host() {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            if !self.private.contains_key(&key)
                && (!host.contains('.')
                    || [
                        "localhost",
                        "local",
                        "internal",
                        "home",
                        "arpa",
                        "onion",
                        "invalid",
                        "test",
                    ]
                    .iter()
                    .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}"))))
            {
                return Err(AuthError::UnsafeUrl);
            }
        }
        if (url.scheme() == "https" && self.origins.contains(&key))
            || (url.scheme() == "http" && self.development.contains(&key))
        {
            Ok(())
        } else {
            Err(AuthError::UnsafeUrl)
        }
    }
    fn check_addresses(&self, url: &Url, addresses: &[SocketAddr]) -> Result<(), AuthError> {
        self.check_url(url)?;
        let key = url.origin().ascii_serialization();
        let valid = !addresses.is_empty()
            && addresses.iter().all(|addr| {
                if let Some(grant) = self.private.get(&key) {
                    grant.contains(&addr.ip())
                } else if self.development.contains(&key) {
                    Some(addr.ip()) == literal_ip(url)
                } else {
                    public_ip(addr.ip())
                }
            });
        if valid {
            Ok(())
        } else {
            Err(AuthError::UnsafeUrl)
        }
    }
}
impl fmt::Debug for NetworkPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NetworkPolicy([redacted])")
    }
}

/// Trusted caller-supplied DNS service. Each request resolves anew; all returned
/// addresses are policy-checked and pinned into that request's connector.
/// Dropping its future does not promise termination of OS resolver threads.
pub trait Resolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, AuthError>> + Send + 'a>>;
}

pub struct OAuthNetwork {
    policy: NetworkPolicy,
    roots: Vec<reqwest::Certificate>,
    resolver: Option<Arc<dyn Resolver>>,
}
impl fmt::Debug for OAuthNetwork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthNetwork([redacted])")
    }
}
pub struct NetworkReply {
    pub status: u16,
    pub json: Value,
}
impl fmt::Debug for NetworkReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetworkReply")
            .field("status", &self.status)
            .field("json", &"[redacted]")
            .finish()
    }
}
impl OAuthNetwork {
    pub fn new(policy: NetworkPolicy) -> Result<Self, AuthError> {
        Ok(Self {
            policy,
            roots: Vec::new(),
            resolver: None,
        })
    }
    /// Validate resource approval without fetching or resolving its hostname.
    /// HTTP requires an explicit literal-loopback development grant.
    pub fn validate_resource_url(&self, url: &Url) -> Result<(), AuthError> {
        self.policy.check_url(url)
    }
    /// Additional trust anchors supplied by the embedding caller. Certificate
    /// chain and hostname verification remain enabled.
    pub fn with_root_certificates(mut self, roots: Vec<reqwest::Certificate>) -> Self {
        self.roots = roots;
        self
    }
    /// Install a trusted resolver, not an untrusted metadata-provided service.
    pub fn with_resolver(mut self, resolver: Arc<dyn Resolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }
    pub async fn get_json(
        &self,
        url: &Url,
        control: &Control,
        deadline: Instant,
    ) -> Result<NetworkReply, AuthError> {
        self.request(url, Payload::Get, control, deadline).await
    }
    pub async fn post_json(
        &self,
        url: &Url,
        body: &Value,
        control: &Control,
        deadline: Instant,
    ) -> Result<NetworkReply, AuthError> {
        self.request(url, Payload::Json(body), control, deadline)
            .await
    }
    pub async fn post_form(
        &self,
        url: &Url,
        body: &[(String, String)],
        control: &Control,
        deadline: Instant,
    ) -> Result<NetworkReply, AuthError> {
        self.request(url, Payload::Form(body), control, deadline)
            .await
    }
    async fn request(
        &self,
        url: &Url,
        payload: Payload<'_>,
        control: &Control,
        deadline: Instant,
    ) -> Result<NetworkReply, AuthError> {
        // The request future, including DNS waiting, connection, upload and body
        // reads, is dropped on cancellation/deadline. OS resolver work may outlive
        // its dropped lookup future; this is not an OS DNS-thread cancellation API.
        bounded(control, deadline, async {
            self.policy.check_url(url)?;
            let port = url.port_or_known_default().ok_or(AuthError::UnsafeUrl)?;
            let addresses: Vec<SocketAddr> = match literal_ip(url) {
                Some(ip) => vec![SocketAddr::new(ip, port)],
                None => {
                    let host = url.host_str().ok_or(AuthError::UnsafeUrl)?;
                    if let Some(resolver) = &self.resolver {
                        resolver.resolve(host, port).await?
                    } else {
                        tokio::net::lookup_host((host, port))
                            .await
                            .map_err(|_| AuthError::Discovery)?
                            .collect()
                    }
                }
            };
            self.policy.check_addresses(url, &addresses)?;
            let mut builder = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .referer(false)
                .resolve_to_addrs(url.host_str().ok_or(AuthError::UnsafeUrl)?, &addresses);
            for root in &self.roots {
                builder = builder.add_root_certificate(root.clone());
            }
            let client = builder.build().map_err(|_| AuthError::Provider)?;
            let request = match payload {
                Payload::Get => client.get(url.clone()),
                Payload::Json(body) => {
                    let bytes = serde_json::to_vec(body).map_err(|_| AuthError::Exchange)?;
                    if bytes.len() > MAX_BODY {
                        return Err(AuthError::Exchange);
                    }
                    client
                        .post(url.clone())
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .body(bytes)
                }
                Payload::Form(body) => {
                    let mut encoded = String::new();
                    for (key, value) in body {
                        // Check before extending, then check escaped length too.
                        if key.len().saturating_add(value.len()) > MAX_BODY {
                            return Err(AuthError::Exchange);
                        }
                        let pair = url::form_urlencoded::Serializer::new(String::new())
                            .append_pair(key, value)
                            .finish();
                        if !encoded.is_empty() {
                            encoded.push('&');
                        }
                        if encoded.len().saturating_add(pair.len()) > MAX_BODY {
                            return Err(AuthError::Exchange);
                        }
                        encoded.push_str(&pair);
                    }
                    client
                        .post(url.clone())
                        .header(
                            reqwest::header::CONTENT_TYPE,
                            "application/x-www-form-urlencoded",
                        )
                        .body(encoded)
                }
            };
            let mut response = request
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(|_| AuthError::Exchange)?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) {
                return Err(AuthError::Exchange);
            }
            require_json_content_type(response.headers())?;
            if response
                .content_length()
                .is_some_and(|n| n > MAX_BODY as u64)
            {
                return Err(AuthError::Exchange);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| AuthError::Exchange)? {
                if chunk.len() > MAX_BODY - bytes.len() {
                    return Err(AuthError::Exchange);
                }
                bytes.extend_from_slice(&chunk);
            }
            let json = serde_json::from_slice(&bytes).map_err(|_| AuthError::Exchange)?;
            // Status interpretation belongs to the provider (including OAuth 400).
            Ok(NetworkReply { status, json })
        })
        .await
    }
}
fn require_json_content_type(headers: &reqwest::header::HeaderMap) -> Result<(), AuthError> {
    let mut values = headers.get_all(reqwest::header::CONTENT_TYPE).iter();
    let value = values.next().ok_or(AuthError::Exchange)?;
    if values.next().is_some() || value.as_bytes().len() > 1024 {
        return Err(AuthError::Exchange);
    }
    let media: mime::Mime = value
        .to_str()
        .map_err(|_| AuthError::Exchange)?
        .parse()
        .map_err(|_| AuthError::Exchange)?;
    if media.type_() == mime::APPLICATION
        && (media.subtype() == mime::JSON || media.suffix() == Some(mime::JSON))
        && media.subtype() != mime::STAR
    {
        Ok(())
    } else {
        Err(AuthError::Exchange)
    }
}

enum Payload<'a> {
    Get,
    Json(&'a Value),
    Form(&'a [(String, String)]),
}

async fn bounded<T>(
    control: &Control,
    deadline: Instant,
    work: impl Future<Output = Result<T, AuthError>>,
) -> Result<T, AuthError> {
    control.check().map_err(|_| AuthError::Cancelled)?;
    if Instant::now() >= deadline {
        return Err(AuthError::Timeout);
    }
    tokio::select! {
        biased;
        _ = control.cancelled() => Err(AuthError::Cancelled),
        _ = tokio::time::sleep_until(deadline.into()) => Err(AuthError::Timeout),
        result = work => {
            control.check().map_err(|_| AuthError::Cancelled)?;
            if Instant::now() >= deadline { return Err(AuthError::Timeout); }
            result
        }
    }
}
fn literal_ip(url: &Url) -> Option<IpAddr> {
    match url.host()? {
        Host::Ipv4(ip) => Some(ip.into()),
        Host::Ipv6(ip) => Some(ip.into()),
        Host::Domain(_) => None,
    }
}
fn validate_url(url: &Url) -> Result<(), AuthError> {
    if url.as_str().len() > MAX_URL
        || !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query_pairs().any(|(key, _)| {
            matches!(
                key.to_ascii_lowercase().as_str(),
                "access_token" | "refresh_token" | "client_secret" | "code" | "code_verifier"
            )
        })
    {
        return Err(AuthError::UnsafeUrl);
    }
    Ok(())
}
fn origin_url(origin: &str) -> Result<Url, AuthError> {
    let url = Url::parse(origin).map_err(|_| AuthError::UnsafeUrl)?;
    validate_url(&url)?;
    if url.path() != "/" || url.query().is_some() {
        return Err(AuthError::UnsafeUrl);
    }
    Ok(url)
}

/// RFC9728: insert the well-known suffix *before* the resource path.
pub fn protected_metadata_url(resource: &Url) -> Result<Url, AuthError> {
    metadata_url(resource, "/.well-known/oauth-protected-resource", false)
}
/// RFC8414 (not OIDC): insert the well-known suffix before the issuer path.
pub fn authorization_metadata_url(issuer: &Url) -> Result<Url, AuthError> {
    metadata_url(issuer, "/.well-known/oauth-authorization-server", true)
}
fn metadata_url(source: &Url, suffix: &str, issuer: bool) -> Result<Url, AuthError> {
    validate_url(source)?;
    if source.scheme() != "https" || (issuer && source.query().is_some()) {
        return Err(AuthError::UnsafeUrl);
    }
    let mut result = source.clone();
    let path = if source.path() == "/" {
        ""
    } else {
        source.path()
    };
    result.set_path(&format!("{suffix}{path}"));
    result.set_query(None);
    validate_url(&result)?;
    Ok(result)
}

// Conservative globally routable unicast policy; deliberately excludes special
// purpose allocations even if some suballocations have globally reachable uses.
fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let n = u32::from(ip);
            ![
                (0x00000000, 8),
                (0x0a000000, 8),
                (0x64400000, 10),
                (0x7f000000, 8),
                (0xa9fe0000, 16),
                (0xac100000, 12),
                (0xc0000000, 24),
                (0xc0000200, 24),
                (0xc01fc400, 24), // AS112
                (0xc034c100, 24), // AMT
                (0xc0586300, 24),
                (0xc0a80000, 16),
                (0xc0af3000, 24), // direct delegation AS112
                (0xc6120000, 15),
                (0xc6336400, 24),
                (0xcb007100, 24),
                (0xe0000000, 3),
            ]
            .iter()
            .any(|&(base, bits)| n >> (32 - bits) == base >> (32 - bits))
        }
        IpAddr::V6(ip) => {
            let n = u128::from(ip);
            // Only 2000::/3 global unicast, minus IETF special, documentation,
            // deprecated 6to4 (which embeds an unchecked IPv4), and new docs.
            n >> 125 == 1
                && ![
                    (0x2001u128 << 112, 23),
                    (0x20010db8u128 << 96, 32),
                    (0x2002u128 << 112, 16),
                    (0x2620004f8000u128 << 80, 48), // AS112
                    (0x3ff0u128 << 112, 12),
                ]
                .iter()
                .any(|&(base, bits)| n >> (128 - bits) == base >> (128 - bits))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn special_and_mixed_addresses_fail_closed() {
        for text in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.0.0.8",
            "192.0.2.1",
            "192.31.196.1",
            "192.52.193.1",
            "192.175.48.1",
            "2620:4f:8000::1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:8.8.8.8",
            "::ffff:127.0.0.1",
            "64:ff9b::808:808",
            "100::1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "2001::1",
            "2001:2::1",
            "2001:db8::1",
            "2002:808:808::1",
            "3fff::1",
        ] {
            assert!(!public_ip(text.parse().unwrap()), "{text}");
        }
        for text in [
            "8.8.8.8",
            "1.1.1.1",
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
        ] {
            assert!(public_ip(text.parse().unwrap()), "{text}");
        }
        let url = Url::parse("https://as.example/token").unwrap();
        let mut policy = NetworkPolicy::new(vec!["https://as.example".into()]).unwrap();
        let good = "8.8.8.8:443".parse().unwrap();
        let bad = "127.0.0.1:443".parse().unwrap();
        assert!(policy.check_addresses(&url, &[good]).is_ok());
        assert!(policy.check_addresses(&url, &[]).is_err());
        assert!(policy.check_addresses(&url, &[good, bad]).is_err());
        policy
            .allow_private_origin("https://as.example", vec![bad.ip()])
            .unwrap();
        assert!(policy.check_addresses(&url, &[bad]).is_ok());
        assert!(policy.check_addresses(&url, &[good, bad]).is_err());
        assert!(policy
            .check_addresses(&Url::parse("https://as.example:444/").unwrap(), &[bad])
            .is_err());
    }

    #[tokio::test]
    async fn dns_phase_future_uses_same_control_and_deadline() {
        // This proves the guard drops pending work, not OS DNS threads.
        // Real TLS connector pinning and fresh lookup are tested in oauth_tls.rs.
        struct PendingDrop<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for PendingDrop<'_> {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let dropped = std::sync::atomic::AtomicBool::new(false);
        let control = Control::default();
        let work = async {
            let _guard = PendingDrop(&dropped);
            std::future::pending::<Result<(), AuthError>>().await
        };
        assert_eq!(
            bounded(&control, Instant::now() + Duration::from_millis(20), work).await,
            Err(AuthError::Timeout)
        );
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            control.cancel();
        };
        let work = bounded(
            &control,
            Instant::now() + Duration::from_secs(2),
            std::future::pending::<Result<(), AuthError>>(),
        );
        let (_, result) = tokio::join!(cancel, work);
        assert_eq!(result, Err(AuthError::Cancelled));
    }
}
