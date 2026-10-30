use super::{
    callback::{bounded, check, Callback},
    network::{authorization_metadata_url, protected_metadata_url, OAuthNetwork},
};
use crate::{
    AuthBinding, AuthDecision, AuthError, AuthFuture, AuthReason, AuthRequest, Control,
    CredentialLease, CredentialProvider,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use url::Url;

/// Explicit resource-to-issuer policy. No authorization server is selected from an unknown challenge.
#[derive(Clone)]
pub struct ResourcePolicy {
    pub resource: String,
    pub issuer: String,
    pub scopes: Vec<String>,
    pub client_id: Option<String>,
    /// Trusted configuration approval of registration side effects before browser consent.
    /// Set only for the explicitly pinned resource and issuer; never derive from metadata.
    pub allow_dynamic_registration: bool,
}
impl fmt::Debug for ResourcePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResourcePolicy([redacted])")
    }
}
#[derive(Clone, Debug)]
pub struct OAuthOptions {
    pub resources: Vec<ResourcePolicy>,
    pub max_bindings: usize,
    /// Explicit initial browser destinations. This does not authorize network fetches;
    /// those remain independently constrained by NetworkPolicy.
    pub authorization_origins: Vec<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorizationOutcome {
    Accepted,
    Declined,
}
/// The actual authorization URL is intentionally accessible only to the trusted interaction.
/// It contains state and PKCE material; Debug never prints it.
pub struct AuthorizationRequest<'a> {
    pub authorization_url: Url,
    pub binding: &'a AuthBinding,
    pub issuer: &'a str,
    pub scopes: &'a [String],
    pub control: &'a Control,
    pub deadline: Instant,
}
impl fmt::Debug for AuthorizationRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthorizationRequest([redacted])")
    }
}
pub type AuthorizationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AuthorizationOutcome, AuthError>> + Send + 'a>>;
pub trait AuthorizationInteraction: Send + Sync {
    fn authorize<'a>(&'a self, request: AuthorizationRequest<'a>) -> AuthorizationFuture<'a>;
}
struct Metadata {
    authorization: Url,
    token: Url,
    registration: Option<Url>,
}
struct GrantProvenance {
    client: String,
    token_endpoint: Url,
}
struct Grant {
    lease: Option<CredentialLease>,
    valid_until: Instant,
    refresh: Option<String>,
    provenance: GrantProvenance,
}
#[derive(Default)]
struct Slot {
    gate: tokio::sync::Mutex<()>,
    grant: Mutex<Option<Grant>>,
    retired: Control,
}
/// Instance-owned memory-only cache. Full bindings (including generation) are isolation keys.
/// Admission fails at capacity instead of evicting active gates and breaking single-flight.
pub struct OAuthProvider {
    options: OAuthOptions,
    network: OAuthNetwork,
    interaction: Arc<dyn AuthorizationInteraction>,
    slots: Mutex<HashMap<AuthBinding, Arc<Slot>>>,
}
impl fmt::Debug for OAuthProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthProvider([redacted])")
    }
}
fn lock<T>(m: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, AuthError> {
    m.lock().map_err(|_| AuthError::Provider)
}
fn scopes(value: &str) -> Result<Vec<String>, AuthError> {
    let parts: Vec<String> = value.split(' ').map(str::to_owned).collect();
    if parts.is_empty()
        || parts.len() > 64
        || parts.iter().any(|s| {
            s.is_empty()
                || s.len() > 256
                || !s
                    .bytes()
                    .all(|b| b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b))
        })
    {
        return Err(AuthError::Scope);
    }
    let unique: HashSet<_> = parts.iter().collect();
    if unique.len() != parts.len() {
        return Err(AuthError::Scope);
    }
    Ok(parts)
}
fn url(value: &str) -> Result<Url, AuthError> {
    let u = Url::parse(value).map_err(|_| AuthError::UnsafeUrl)?;
    if value.len() > 8192
        || !matches!(u.scheme(), "http" | "https")
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || u.query_pairs().any(|(k, _)| {
            [
                "access_token",
                "refresh_token",
                "client_secret",
                "code",
                "code_verifier",
            ]
            .contains(&k.to_ascii_lowercase().as_str())
        })
    {
        return Err(AuthError::UnsafeUrl);
    }
    Ok(u)
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, AuthError> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(AuthError::Discovery)
}
fn contains(v: &Value, key: &str, item: &str) -> bool {
    v.get(key).and_then(Value::as_array).is_some_and(|a| {
        !a.is_empty()
            && a.len() <= 64
            && a.iter().all(|v| v.as_str().is_some_and(|s| !s.is_empty()))
            && a.iter()
                .filter_map(Value::as_str)
                .collect::<HashSet<_>>()
                .len()
                == a.len()
            && a.iter().any(|v| v.as_str() == Some(item))
    })
}
fn random() -> Result<String, AuthError> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| AuthError::Provider)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
// The shared well-known helpers intentionally require HTTPS. Explicit HTTP
// development policy still uses the identical path construction, with every
// actual fetch independently checked by OAuthNetwork.
fn metadata_url(source: &Url, authorization: bool) -> Result<Url, AuthError> {
    let mut base = source.clone();
    let development = source.scheme() == "http";
    if development {
        base.set_scheme("https").map_err(|_| AuthError::UnsafeUrl)?;
    }
    let mut result = if authorization {
        authorization_metadata_url(&base)?
    } else {
        protected_metadata_url(&base)?
    };
    if development {
        result
            .set_scheme("http")
            .map_err(|_| AuthError::UnsafeUrl)?;
        result
            .set_port(source.port())
            .map_err(|_| AuthError::UnsafeUrl)?;
    }
    Ok(result)
}
impl OAuthProvider {
    pub fn new(
        options: OAuthOptions,
        network: OAuthNetwork,
        interaction: Arc<dyn AuthorizationInteraction>,
    ) -> Result<Self, AuthError> {
        if options.max_bindings == 0
            || options.max_bindings > 4096
            || options.resources.is_empty()
            || options.resources.len() > 256
        {
            return Err(AuthError::Provider);
        }
        for origin in &options.authorization_origins {
            let u = url(origin)?;
            if u.path() != "/"
                || u.query().is_some()
                || (u.scheme() == "http"
                    && !u
                        .host_str()
                        .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                        .is_some_and(|ip| ip.is_loopback()))
            {
                return Err(AuthError::UnsafeUrl);
            }
        }
        let mut resources = HashSet::new();
        for p in &options.resources {
            if p.resource.len() > 8192 || p.issuer.len() > 8192 || !resources.insert(&p.resource) {
                return Err(AuthError::IdentityMismatch);
            }
            let resource = url(&p.resource)?;
            if resource.as_str() != p.resource {
                return Err(AuthError::IdentityMismatch);
            }
            // The protected resource receives the access token. Discovery's
            // secure policy must also approve that destination, especially the
            // exact, explicitly granted loopback-only HTTP development case.
            network.validate_resource_url(&resource)?;
            let issuer = url(&p.issuer)?;
            if issuer.query().is_some() || issuer.as_str() != p.issuer {
                return Err(AuthError::IdentityMismatch);
            }
            if p.scopes.iter().any(|s| s.contains(' ')) {
                return Err(AuthError::Scope);
            }
            scopes(&p.scopes.join(" "))?;
            if p.client_id
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.len() > 1024 || s.chars().any(char::is_control))
                || (p.client_id.is_none() && !p.allow_dynamic_registration)
            {
                return Err(AuthError::Registration);
            }
        }
        Ok(Self {
            options,
            network,
            interaction,
            slots: Mutex::new(HashMap::new()),
        })
    }
    /// Noninteractive local logout. Cancels pending work and frees admission capacity.
    /// Already returned leases and remote account grants are not revoked.
    pub fn forget_binding(&self, binding: &AuthBinding) -> Result<bool, AuthError> {
        let mut slots = lock(&self.slots)?;
        if let Some(slot) = slots.get(binding) {
            let mut grant = lock(&slot.grant)?;
            slot.retired.cancel();
            *grant = None;
            drop(grant);
            slots.remove(binding);
            return Ok(true);
        }
        Ok(false)
    }
    /// Retire all current slot generations without prompting or network I/O.
    pub fn clear(&self) -> Result<(), AuthError> {
        let mut slots = lock(&self.slots)?;
        for slot in slots.values() {
            let mut grant = lock(&slot.grant)?;
            slot.retired.cancel();
            *grant = None;
        }
        slots.clear();
        Ok(())
    }
    fn commit(slot: &Slot, grant: Grant) -> Result<AuthDecision, AuthError> {
        let mut cached = lock(&slot.grant)?;
        slot.retired.check().map_err(|_| AuthError::Cancelled)?;
        let lease = grant.lease.clone().ok_or(AuthError::Provider)?;
        *cached = Some(grant);
        Ok(AuthDecision::Credential(lease))
    }
    fn slot(&self, binding: &AuthBinding) -> Result<Arc<Slot>, AuthError> {
        let mut slots = lock(&self.slots)?;
        if let Some(s) = slots.get(binding) {
            return Ok(s.clone());
        }
        if slots.len() >= self.options.max_bindings {
            return Err(AuthError::Provider);
        }
        let s = Arc::new(Slot::default());
        slots.insert(binding.clone(), s.clone());
        Ok(s)
    }
    async fn discover(
        &self,
        p: &ResourcePolicy,
        challenge: &crate::BearerChallenge,
        c: &Control,
        end: Instant,
    ) -> Result<Metadata, AuthError> {
        if let Some(s) = &challenge.scope {
            let requested = scopes(s)?;
            if requested.iter().any(|s| !p.scopes.contains(s)) {
                return Err(AuthError::Scope);
            }
        }
        let resource = url(&p.resource)?;
        let prm = match &challenge.resource_metadata {
            Some(s) => url(s)?,
            None => metadata_url(&resource, false)?,
        };
        let reply = self.network.get_json(&prm, c, end).await?;
        if reply.status != 200 || text(&reply.json, "resource")? != p.resource {
            return Err(AuthError::IdentityMismatch);
        }
        let servers = reply
            .json
            .get("authorization_servers")
            .and_then(Value::as_array)
            .filter(|v| !v.is_empty() && v.len() <= 16)
            .ok_or(AuthError::Discovery)?;
        if !servers.iter().all(Value::is_string)
            || !servers.iter().any(|v| v.as_str() == Some(&p.issuer))
        {
            return Err(AuthError::IdentityMismatch);
        }
        if reply.json.get("bearer_methods_supported").is_some()
            && !contains(&reply.json, "bearer_methods_supported", "header")
        {
            return Err(AuthError::Discovery);
        }
        let metadata = metadata_url(&url(&p.issuer)?, true)?;
        let reply = self.network.get_json(&metadata, c, end).await?;
        let v = &reply.json;
        if reply.status != 200 || text(v, "issuer")? != p.issuer {
            return Err(AuthError::IdentityMismatch);
        }
        if !contains(v, "response_types_supported", "code")
            || !contains(v, "code_challenge_methods_supported", "S256")
            || !contains(v, "token_endpoint_auth_methods_supported", "none")
            || (v.get("grant_types_supported").is_some()
                && !contains(v, "grant_types_supported", "authorization_code"))
        {
            return Err(AuthError::Discovery);
        }
        if v.get("scopes_supported").is_some()
            && p.scopes.iter().any(|s| !contains(v, "scopes_supported", s))
        {
            return Err(AuthError::Scope);
        }
        if v.get("protected_resources").is_some()
            && !contains(v, "protected_resources", &p.resource)
        {
            return Err(AuthError::IdentityMismatch);
        }
        let authorization = url(text(v, "authorization_endpoint")?)?;
        if !self
            .options
            .authorization_origins
            .iter()
            .any(|o| Url::parse(o).is_ok_and(|u| u.origin() == authorization.origin()))
        {
            return Err(AuthError::UnsafeUrl);
        }
        Ok(Metadata {
            authorization,
            token: url(text(v, "token_endpoint")?)?,
            registration: v
                .get("registration_endpoint")
                .map(|_| text(v, "registration_endpoint").and_then(url))
                .transpose()?,
        })
    }
    fn token(
        &self,
        v: &Value,
        binding: &AuthBinding,
        p: &ResourcePolicy,
        provenance: GrantProvenance,
        old_refresh: Option<String>,
    ) -> Result<Grant, AuthError> {
        if !text(v, "token_type")
            .map_err(|_| AuthError::Exchange)?
            .eq_ignore_ascii_case("Bearer")
        {
            return Err(AuthError::Exchange);
        }
        let token = text(v, "access_token").map_err(|_| AuthError::Exchange)?;
        if token.len() > 16384 {
            return Err(AuthError::Exchange);
        }
        let granted = match v.get("scope") {
            Some(s) => scopes(s.as_str().ok_or(AuthError::Scope)?)?,
            None => p.scopes.clone(),
        };
        if p.scopes.iter().any(|s| !granted.contains(s))
            || granted.iter().any(|s| !p.scopes.contains(s))
        {
            return Err(AuthError::Scope);
        }
        // Unknown expiry is bounded to five minutes; known expiry is capped at one day.
        let lifetime = match v.get("expires_in") {
            Some(n) => n
                .as_u64()
                .filter(|n| *n > 0 && *n <= 86400)
                .ok_or(AuthError::Exchange)?,
            None => 300,
        };
        let duration = Duration::from_secs(lifetime);
        let usable = duration.saturating_sub(Duration::from_secs(30).min(duration / 10));
        let expires = Instant::now()
            .checked_add(usable)
            .ok_or(AuthError::Exchange)?;
        let refresh = match v.get("refresh_token") {
            Some(v) => Some(
                v.as_str()
                    .filter(|s| {
                        !s.is_empty() && s.len() <= 16384 && !s.chars().any(char::is_control)
                    })
                    .ok_or(AuthError::Exchange)?
                    .to_owned(),
            ),
            None => old_refresh,
        };
        let lease = CredentialLease::new(
            binding,
            random()?,
            p.issuer.clone(),
            granted,
            token,
            Some(expires),
        )?;
        Ok(Grant {
            lease: Some(lease),
            valid_until: expires,
            refresh,
            provenance,
        })
    }
    async fn code_flow(
        &self,
        req: &AuthRequest<'_>,
        p: &ResourcePolicy,
        meta: Metadata,
    ) -> Result<Option<Grant>, AuthError> {
        let callback = bounded(req.control, req.deadline, Callback::bind()).await?;
        let client = if let Some(client) = &p.client_id {
            client.clone()
        } else {
            if !p.allow_dynamic_registration {
                return Err(AuthError::Registration);
            }
            let registration = meta.registration.as_ref().ok_or(AuthError::Registration)?;
            let body = json!({"redirect_uris":[callback.redirect],"token_endpoint_auth_method":"none","grant_types":["authorization_code","refresh_token"],"response_types":["code"],"client_name":"rpi managed HTTP MCP"});
            let reply = self
                .network
                .post_json(registration, &body, req.control, req.deadline)
                .await?;
            let v = &reply.json;
            if reply.status != 201
                || text(v, "token_endpoint_auth_method").map_err(|_| AuthError::Registration)?
                    != "none"
                || !contains(v, "redirect_uris", &callback.redirect)
                || !contains(v, "grant_types", "authorization_code")
                || !contains(v, "response_types", "code")
                || v.get("client_secret").is_some()
            {
                return Err(AuthError::Registration);
            }
            text(v, "client_id")
                .map_err(|_| AuthError::Registration)?
                .to_owned()
        };
        if client.is_empty() || client.len() > 1024 || client.chars().any(char::is_control) {
            return Err(AuthError::Registration);
        }
        let state = random()?;
        let verifier = random()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut authorization = meta.authorization;
        // Endpoint-controlled query must not shadow OAuth security parameters.
        if authorization.query_pairs().any(|(k, _)| {
            [
                "response_type",
                "client_id",
                "redirect_uri",
                "scope",
                "state",
                "code_challenge",
                "code_challenge_method",
                "resource",
            ]
            .contains(&k.as_ref())
        }) {
            return Err(AuthError::Discovery);
        }
        authorization.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", client.as_str()),
            ("redirect_uri", callback.redirect.as_str()),
            ("scope", p.scopes.join(" ").as_str()),
            ("state", state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("resource", p.resource.as_str()),
        ]);
        let interact = self.interaction.authorize(AuthorizationRequest {
            authorization_url: authorization,
            binding: req.binding,
            issuer: &p.issuer,
            scopes: &p.scopes,
            control: req.control,
            deadline: req.deadline,
        });
        let receive = callback.receive(&state, &p.issuer);
        tokio::pin!(interact, receive);
        let result = bounded(req.control, req.deadline, async {
            tokio::select! {
                decision = &mut interact => match decision? { AuthorizationOutcome::Declined => Ok(None), AuthorizationOutcome::Accepted => Ok(Some(receive.await?)) },
                code = &mut receive => { let code = code?; match interact.await? { AuthorizationOutcome::Declined => Ok(None), AuthorizationOutcome::Accepted => Ok(Some(code)) } }
            }
        }).await?;
        let Some(code) = result else {
            return Ok(None);
        };
        let form = vec![
            ("grant_type".into(), "authorization_code".into()),
            ("code".into(), code),
            ("client_id".into(), client.clone()),
            ("redirect_uri".into(), callback.redirect.clone()),
            ("code_verifier".into(), verifier),
            ("resource".into(), p.resource.clone()),
        ];
        let reply = self
            .network
            .post_form(&meta.token, &form, req.control, req.deadline)
            .await?;
        if reply.status != 200 {
            return Err(AuthError::Exchange);
        }
        Ok(Some(self.token(
            &reply.json,
            req.binding,
            p,
            GrantProvenance {
                client,
                token_endpoint: meta.token,
            },
            None,
        )?))
    }
    async fn acquire(&self, req: AuthRequest<'_>) -> Result<AuthDecision, AuthError> {
        check(req.control, req.deadline)?;
        let Some(p) = self
            .options
            .resources
            .iter()
            .find(|p| p.resource == req.binding.resource)
        else {
            return Ok(AuthDecision::Unavailable);
        };
        let slot = self.slot(req.binding)?;
        bounded(
            &slot.retired,
            req.deadline,
            self.acquire_slot(&req, p, &slot),
        )
        .await
    }
    async fn acquire_slot(
        &self,
        req: &AuthRequest<'_>,
        p: &ResourcePolicy,
        slot: &Slot,
    ) -> Result<AuthDecision, AuthError> {
        if let AuthReason::Challenge {
            rejected_lease: Some(id),
            ..
        } = &req.reason
        {
            self.rejected_lease(req.binding, id);
        }
        let _gate = bounded(req.control, req.deadline, async {
            Ok(slot.gate.lock().await)
        })
        .await?;
        {
            let guard = lock(&slot.grant)?;
            if let Some(g) = guard.as_ref().filter(|g| g.valid_until > Instant::now()) {
                if let Some(l) = &g.lease {
                    return Ok(AuthDecision::Credential(l.clone()));
                }
            }
        }
        let challenge = match &req.reason {
            AuthReason::CachedOnly => return Ok(AuthDecision::Unavailable),
            AuthReason::Challenge { challenge, .. } => *challenge,
        };
        // Validate challenge even when a stored refresh grant exists.
        let meta = self
            .discover(p, challenge, req.control, req.deadline)
            .await?;
        // Take before I/O: cancellation, ambiguous outcomes or dropped futures cannot reuse a consumed refresh token.
        let previous = lock(&slot.grant)?.take();
        if let Some(mut old) = previous {
            if let Some(refresh) = old.refresh.take() {
                let form = vec![
                    ("grant_type".into(), "refresh_token".into()),
                    ("refresh_token".into(), refresh.clone()),
                    ("client_id".into(), old.provenance.client.clone()),
                    ("resource".into(), p.resource.clone()),
                    ("scope".into(), p.scopes.join(" ")),
                ];
                if old.provenance.token_endpoint != meta.token {
                    return Err(AuthError::IdentityMismatch);
                }
                let reply = self
                    .network
                    .post_form(&meta.token, &form, req.control, req.deadline)
                    .await?;
                if reply.status == 200 {
                    let grant =
                        self.token(&reply.json, req.binding, p, old.provenance, Some(refresh))?;
                    check(req.control, req.deadline)?;
                    return Self::commit(slot, grant);
                }
                if reply.status != 400
                    || reply.json.get("error").and_then(Value::as_str) != Some("invalid_grant")
                {
                    return Err(AuthError::Exchange);
                }
            }
        }
        match self.code_flow(req, p, meta).await? {
            None => Ok(AuthDecision::Declined),
            Some(grant) => {
                check(req.control, req.deadline)?;
                Self::commit(slot, grant)
            }
        }
    }
}
impl CredentialProvider for OAuthProvider {
    fn credential<'a>(&'a self, request: AuthRequest<'a>) -> AuthFuture<'a> {
        Box::pin(self.acquire(request))
    }
    fn rejected_lease(&self, binding: &AuthBinding, id: &str) {
        if let Ok(slots) = self.slots.lock() {
            if let Some(slot) = slots.get(binding) {
                if let Ok(mut grant) = slot.grant.lock() {
                    if let Some(g) = grant.as_mut() {
                        if g.lease.as_ref().is_some_and(|l| l.id() == id) {
                            g.lease = None;
                        }
                    }
                }
            }
        }
    }
}
