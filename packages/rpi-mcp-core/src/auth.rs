//! Optional trusted credential injection. Defaults do not implement OAuth.
use crate::Control;
use std::{fmt, future::Future, path::PathBuf, pin::Pin, time::Instant};

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct AuthBinding {
    pub namespace: u64,
    pub scope: String,
    pub cwd: PathBuf,
    pub target: String,
    pub resource: String,
    pub generation: u64,
}
impl AuthBinding {
    pub fn new(
        namespace: u64,
        scope: String,
        cwd: PathBuf,
        target: String,
        resource: String,
        generation: u64,
    ) -> Result<Self, AuthError> {
        let cwd = std::fs::canonicalize(cwd).map_err(|_| AuthError::IdentityMismatch)?;
        let url = crate::http::validate_mcp_url(&resource).map_err(|_| AuthError::UnsafeUrl)?;
        if url.fragment().is_some() || url.as_str() != resource {
            return Err(AuthError::IdentityMismatch);
        }
        Ok(Self {
            namespace,
            scope,
            cwd,
            target,
            resource,
            generation,
        })
    }
}
impl fmt::Debug for AuthBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthBinding([redacted])")
    }
}

#[derive(Clone)]
pub struct CredentialLease {
    binding: std::sync::Arc<AuthBinding>,
    id: String,
    issuer: String,
    scopes: Vec<String>,
    token: reqwest::header::HeaderValue,
    expires_at: Option<Instant>,
}
impl CredentialLease {
    pub fn new(
        binding: &AuthBinding,
        id: String,
        issuer: String,
        scopes: Vec<String>,
        token: &str,
        expires_at: Option<Instant>,
    ) -> Result<Self, AuthError> {
        let base = token.trim_end_matches('=');
        if id.is_empty()
            || issuer.is_empty()
            || base.is_empty()
            || !base
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~+/".contains(&b))
            || expires_at.is_some_and(|end| end <= Instant::now())
        {
            return Err(AuthError::Rejected);
        }
        let mut token = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| AuthError::Rejected)?;
        token.set_sensitive(true);
        Ok(Self {
            binding: std::sync::Arc::new(binding.clone()),
            id,
            issuer,
            scopes,
            token,
            expires_at,
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn issuer(&self) -> &str {
        &self.issuer
    }
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }
    pub(crate) fn header(
        &self,
        binding: &AuthBinding,
    ) -> Result<reqwest::header::HeaderValue, AuthError> {
        if self.binding.as_ref() != binding {
            return Err(AuthError::IdentityMismatch);
        }
        if self.expires_at.is_some_and(|end| end <= Instant::now()) {
            return Err(AuthError::Rejected);
        }
        Ok(self.token.clone())
    }
}
impl fmt::Debug for CredentialLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CredentialLease([redacted])")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    Cancelled,
    Timeout,
    Discovery,
    UnsafeUrl,
    IdentityMismatch,
    Registration,
    InvalidState,
    Exchange,
    Scope,
    Rejected,
    Provider,
}
impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MCP authentication {:?}", self)
    }
}
impl std::error::Error for AuthError {}
#[derive(Debug)]
pub enum AuthDecision {
    Unavailable,
    Declined,
    Credential(CredentialLease),
}
pub enum AuthReason<'a> {
    CachedOnly,
    Challenge {
        challenge: &'a BearerChallenge,
        rejected_lease: Option<&'a str>,
    },
}
impl fmt::Debug for AuthReason<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CachedOnly => "CachedOnly",
            Self::Challenge { .. } => "Challenge([redacted])",
        })
    }
}
#[derive(Debug)]
pub struct AuthRequest<'a> {
    pub binding: &'a AuthBinding,
    pub reason: AuthReason<'a>,
    pub control: &'a Control,
    pub deadline: Instant,
}
pub type AuthFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AuthDecision, AuthError>> + Send + 'a>>;
/// Trusted embedding service. CachedOnly must not prompt. Challenge invalidates only
/// the rejected lease. Cache isolation and refresh single-flight are provider-owned,
/// keyed by the entire binding. Futures must yield and be drop/cancellation safe:
/// own and clean up resources, never detach tasks, commit late grants or re-enter
/// this connection. Discovery, issuer/account policy and real exchange are caller-owned.
pub trait CredentialProvider: Send + Sync {
    fn credential<'a>(&'a self, request: AuthRequest<'a>) -> AuthFuture<'a>;

    /// Noninteractive notification of a resource's rejection, before any challenge
    /// handling (including malformed challenges and the final 401). Invalidate
    /// only this exact binding/lease ID, never a newer concurrent grant. Must be
    /// bounded, nonblocking and synchronous: no I/O, prompts or detached work.
    /// Optional for existing providers; closing a transport does not call this
    /// method or revoke an account grant.
    fn rejected_lease(&self, _binding: &AuthBinding, _lease_id: &str) {}
}

#[derive(Clone)]
pub struct BearerChallenge {
    pub resource_metadata: Option<String>,
    pub scope: Option<String>,
    pub error: Option<String>,
}
impl fmt::Debug for BearerChallenge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BearerChallenge([redacted])")
    }
}
fn token(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}
/// Parse all challenge fields, with quoted-string escapes and scheme boundaries.
/// Ambiguous Bearer challenges and duplicate parameters fail closed.
pub fn parse_bearer_challenge(
    headers: &reqwest::header::HeaderMap,
) -> Result<Option<BearerChallenge>, AuthError> {
    let mut total = 0;
    let mut count = 0;
    let mut bearer = None;
    for header in headers.get_all(reqwest::header::WWW_AUTHENTICATE) {
        total += header.as_bytes().len();
        if total > 16384 {
            return Err(AuthError::Rejected);
        }
        let s = header.as_bytes();
        let mut i = 0;
        // Quoted strings deliberately accept valid UTF-8 (including obs-text),
        // not arbitrary invalid byte strings. HTAB is legal HTTP whitespace.
        std::str::from_utf8(s).map_err(|_| AuthError::Rejected)?;
        if s.iter().any(|b| (*b < 32 && *b != b'\t') || *b == 127) {
            return Err(AuthError::Rejected);
        }
        while i < s.len() {
            while i < s.len() && (matches!(s[i], b' ' | b'\t') || s[i] == b',') {
                i += 1;
            }
            if i == s.len() {
                break;
            }
            count += 1;
            if count > 16 {
                return Err(AuthError::Rejected);
            }
            let start = i;
            while i < s.len() && token(s[i]) {
                i += 1;
            }
            if start == i {
                return Err(AuthError::Rejected);
            }
            let is_bearer = s[start..i].eq_ignore_ascii_case(b"bearer");
            if i < s.len() && !matches!(s[i], b' ' | b'\t' | b',') {
                return Err(AuthError::Rejected);
            }
            // token68 credentials on unrelated schemes are opaque, not params.
            let mut probe = i;
            while probe < s.len() && matches!(s[probe], b' ' | b'\t') {
                probe += 1;
            }
            let begin = probe;
            while probe < s.len()
                && (s[probe].is_ascii_alphanumeric() || b"-._~+/".contains(&s[probe]))
            {
                probe += 1;
            }
            let base_end = probe;
            while probe < s.len() && s[probe] == b'=' {
                probe += 1;
            }
            while probe < s.len() && matches!(s[probe], b' ' | b'\t') {
                probe += 1;
            }
            if !is_bearer && base_end > begin && (probe == s.len() || s[probe] == b',') {
                i = probe;
                continue;
            }
            let mut params = std::collections::BTreeMap::new();
            loop {
                while i < s.len() && matches!(s[i], b' ' | b'\t') {
                    i += 1;
                }
                if i == s.len() {
                    break;
                }
                if s[i] == b',' {
                    i += 1;
                    continue;
                }
                let start = i;
                while i < s.len() && token(s[i]) {
                    i += 1;
                }
                if start == i {
                    return Err(AuthError::Rejected);
                }
                let end = i;
                while i < s.len() && matches!(s[i], b' ' | b'\t') {
                    i += 1;
                }
                if i == s.len() || s[i] != b'=' {
                    i = start;
                    break;
                }
                i += 1;
                while i < s.len() && matches!(s[i], b' ' | b'\t') {
                    i += 1;
                }
                let value = if i < s.len() && s[i] == b'"' {
                    let mut bytes = Vec::new();
                    i += 1;
                    let mut closed = false;
                    while i < s.len() {
                        let b = s[i];
                        i += 1;
                        if b == b'"' {
                            closed = true;
                            break;
                        }
                        let b = if b == b'\\' {
                            if i == s.len() {
                                return Err(AuthError::Rejected);
                            }
                            let b = s[i];
                            i += 1;
                            b
                        } else {
                            b
                        };
                        bytes.push(b);
                    }
                    if !closed {
                        return Err(AuthError::Rejected);
                    }
                    String::from_utf8(bytes).map_err(|_| AuthError::Rejected)?
                } else {
                    let start = i;
                    while i < s.len() && token(s[i]) {
                        i += 1;
                    }
                    if start == i {
                        return Err(AuthError::Rejected);
                    }
                    String::from_utf8(s[start..i].to_vec()).map_err(|_| AuthError::Rejected)?
                };
                if params
                    .insert(
                        std::str::from_utf8(&s[start..end])
                            .map_err(|_| AuthError::Rejected)?
                            .to_ascii_lowercase(),
                        value,
                    )
                    .is_some()
                {
                    return Err(AuthError::Rejected);
                }
                while i < s.len() && matches!(s[i], b' ' | b'\t') {
                    i += 1;
                }
                if i < s.len() && s[i] != b',' {
                    return Err(AuthError::Rejected);
                }
            }
            if is_bearer {
                if bearer.is_some() {
                    return Err(AuthError::Rejected);
                }
                let resource_metadata = params.remove("resource_metadata");
                if let Some(value) = &resource_metadata {
                    let url = url::Url::parse(value).map_err(|_| AuthError::UnsafeUrl)?;
                    if url.scheme() != "https"
                        || url.host_str().is_none()
                        || !url.username().is_empty()
                        || url.password().is_some()
                        || url.fragment().is_some()
                    {
                        return Err(AuthError::UnsafeUrl);
                    }
                }
                bearer = Some(BearerChallenge {
                    resource_metadata,
                    scope: params.remove("scope"),
                    error: params.remove("error"),
                });
            }
        }
    }
    Ok(bearer)
}
