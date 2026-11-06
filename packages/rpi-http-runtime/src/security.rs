use crate::{
    sharing::{Access, SharingControl},
    AccessState as ServerState,
};
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::{watch, Mutex};

const SESSION_SECONDS: u64 = 12 * 60 * 60;

struct SessionEntry {
    principal: String,
    expires_at: Option<Instant>,
    access: Access,
    revoked: watch::Sender<bool>,
}
impl SessionEntry {
    fn new(token: &str, expires_at: Instant, access: Access) -> Self {
        let (revoked, _) = watch::channel(false);
        Self {
            // The owner-operation identity is deliberately derived from, but is
            // never the same as, the bearer cookie.
            principal: crate::hex(&Sha256::digest(token.as_bytes())),
            expires_at: Some(expires_at),
            access,
            revoked,
        }
    }
    fn is_live(&self) -> bool {
        !*self.revoked.borrow() && self.expires_at.is_none_or(|at| at > Instant::now())
    }
    fn revoke(&self) {
        self.revoked.send_replace(true);
    }
}

/// A revocable, non-bearer view of an authenticated session.
///
/// This intentionally has no `Debug` implementation and does not retain the
/// original cookie. The principal is an opaque identity for owner registries.
#[derive(Clone)]
pub struct AuthLease {
    entry: Arc<SessionEntry>,
}
impl AuthLease {
    pub fn principal(&self) -> &str {
        &self.entry.principal
    }
    pub fn access(&self) -> Access {
        self.entry.access
    }
    pub fn is_valid(&self, sharing: &SharingControl) -> bool {
        self.entry.is_live() && sharing.permits(self.entry.access)
    }
    /// Wait until this lease is revoked, expires, sharing changes, or the
    /// server shutdown receiver is closed/signalled.
    pub async fn invalidated(&self, sharing: &SharingControl, shutdown: &watch::Receiver<bool>) {
        // Subscribe before checking current values, otherwise a revoke between
        // the check and subscribe would be considered already seen and lost.
        let mut revoked = self.entry.revoked.subscribe();
        let mut changes = sharing.subscribe();
        let mut stopped = shutdown.clone();
        if *stopped.borrow() || !self.is_valid(sharing) {
            return;
        }
        let deadline = async {
            if let Some(at) = self.entry.expires_at {
                tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(deadline);
        tokio::select! {
            _ = revoked.changed() => {},
            _ = changes.changed() => {},
            _ = stopped.changed() => {},
            _ = &mut deadline => {},
        }
    }
}

pub struct Security {
    password: [u8; 32],
    cookie_name: String,
    hosts: HashSet<String>,
    sessions: Mutex<HashMap<String, Arc<SessionEntry>>>,
    attempts: Mutex<Vec<Instant>>,
    // Only a separately bound loopback listener can construct this mode. The
    // public password/cookie listener never accepts a local-control header.
    control: Option<[u8; 32]>,
}
impl Security {
    pub fn from_password_digest(
        digest: [u8; 32],
        hosts: Vec<String>,
        node_id: &str,
    ) -> Result<Self, String> {
        let mut security = Self::new("", hosts, node_id)?;
        security.password = digest;
        Ok(security)
    }
    pub fn new(password: &str, hosts: Vec<String>, node_id: &str) -> Result<Self, String> {
        let hosts: HashSet<_> = hosts.into_iter().map(|s| s.to_ascii_lowercase()).collect();
        if hosts
            .iter()
            .any(|h| h.is_empty() || h.contains(['/', '*', '@', '\r', '\n']))
        {
            return Err("invalid HTTP Host allowlist".into());
        }
        Ok(Self {
            password: Sha256::digest(password.as_bytes()).into(),
            cookie_name: format!(
                "rpi_work_desktop_{}",
                crate::hex(&Sha256::digest(node_id.as_bytes())[..8])
            ),
            hosts,
            sessions: Mutex::new(HashMap::new()),
            attempts: Mutex::new(Vec::new()),
            control: None,
        })
    }
    pub fn new_control(
        secret: &str,
        hosts: Vec<String>,
        node_id: &str,
        access: Access,
    ) -> Result<Self, String> {
        if secret.len() < 32 {
            return Err("Local control capability is too short".into());
        }
        let mut security = Self::new(secret, hosts, node_id)?;
        let digest: [u8; 32] = Sha256::digest(secret.as_bytes()).into();
        let mut entry = SessionEntry::new(secret, Instant::now(), access);
        entry.expires_at = None;
        security
            .sessions
            .get_mut()
            .insert("local-control".into(), Arc::new(entry));
        security.control = Some(digest);
        Ok(security)
    }
    pub fn is_control(&self) -> bool {
        self.control.is_some()
    }
    fn host(&self, headers: &HeaderMap) -> bool {
        headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|h| self.hosts.contains(&h.to_ascii_lowercase()))
    }
    fn origin(headers: &HeaderMap) -> bool {
        let Some(origin) = headers.get(header::ORIGIN) else {
            return true;
        };
        let Ok(origin) = origin.to_str() else {
            return false;
        };
        let Some(host) = origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
        else {
            return false;
        };
        !host.contains(['/', '?', '#', '@'])
            && headers
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|h| h.eq_ignore_ascii_case(host))
    }
    pub async fn authenticate(&self, headers: &HeaderMap, generation: u64) -> Option<AuthLease> {
        let token = if let Some(expected) = &self.control {
            let supplied = headers.get("x-rpi-node-control")?.to_str().ok()?;
            let digest: [u8; 32] = Sha256::digest(supplied.as_bytes()).into();
            if !bool::from(expected.ct_eq(&digest)) {
                return None;
            }
            "local-control"
        } else {
            cookie(headers, &self.cookie_name)?
        };
        let now = Instant::now();
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, entry| {
            let keep = entry.expires_at.is_none_or(|at| at > now);
            if !keep {
                entry.revoke();
            }
            keep
        });
        sessions.get(token).and_then(|entry| {
            (entry.access.generation == generation && entry.is_live()).then(|| AuthLease {
                entry: entry.clone(),
            })
        })
    }
    pub async fn authorized(&self, headers: &HeaderMap, generation: u64) -> bool {
        self.authenticate(headers, generation).await.is_some()
    }
    /// Run a synchronous owner-side commit only while this exact session entry
    /// is still present, live, and in the current sharing epoch.
    pub async fn while_authorized<T>(
        &self,
        lease: &AuthLease,
        sharing: &SharingControl,
        commit: impl FnOnce() -> T,
    ) -> Option<T> {
        let sessions = self.sessions.lock().await;
        let entry = sessions
            .values()
            .find(|candidate| Arc::ptr_eq(candidate, &lease.entry))?
            .clone();
        sharing
            .while_current(entry.access, || {
                // while_current already holds sharing's read lease. Do not
                // recursively read it: a waiting policy writer could deadlock.
                if !entry.is_live() {
                    return None;
                }
                Some(commit())
            })
            .flatten()
    }
}
pub fn authentication_required() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error":"authentication required"})),
    )
        .into_response()
}
pub fn lease_failure(state: &ServerState, lease: &AuthLease) -> Response {
    if *state.shutdown.borrow() {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    } else if !state.sharing.permits(lease.access()) {
        sharing_disabled()
    } else {
        authentication_required()
    }
}
fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|s| s.trim().strip_prefix(&format!("{name}=")))
}
pub async fn guard(
    State(state): State<Arc<ServerState>>,
    request: Request,
    next: Next,
) -> Response {
    let no_store = request.uri().path().starts_with("/api/")
        || request.uri().path() == "/.well-known/rpi-work-entry";
    let mut response = checked_guard(State(state), request, next).await;
    if no_store {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    }
    response
}
async fn checked_guard(
    State(state): State<Arc<ServerState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let security = &state.security;
    if *state.shutdown.borrow() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if !security.host(request.headers()) || !Security::origin(request.headers()) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"Host or Origin is not allowed"})),
        )
            .into_response();
    }
    let marker = matches!(request.method().as_str(), "GET" | "HEAD" | "OPTIONS")
        || request.headers().get("x-rpi-web").is_some_and(|v| v == "1");
    if !marker {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"missing request marker"})),
        )
            .into_response();
    }
    let path = request.uri().path();
    let api = path.starts_with("/api/");
    let workspace_api = api && path != "/api/v1/health";
    let mut changes = state.sharing.subscribe();
    let access = *changes.borrow_and_update();
    if workspace_api && !access.enabled {
        return sharing_disabled();
    }
    let protected = security.is_control()
        || api
            && !matches!(
                path,
                "/api/v1/auth" | "/api/v1/health" | "/api/v1/auth/qr/redeem"
            );
    let lease = if protected {
        security
            .authenticate(request.headers(), access.generation)
            .await
    } else {
        None
    };
    if protected && lease.is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"authentication required"})),
        )
            .into_response();
    }
    if let Some(lease) = &lease {
        request.extensions_mut().insert(lease.clone());
    }
    request.extensions_mut().insert(access);
    let mut response = if let Some(lease) = &lease {
        tokio::select! {
            biased;
            _ = lease.invalidated(&state.sharing, &state.shutdown) => return lease_failure(&state, lease),
            response = next.run(request) => response,
        }
    } else if workspace_api {
        tokio::select! {
            biased;
            _ = changes.changed() => return sharing_disabled(),
            response = next.run(request) => response,
        }
    } else {
        next.run(request).await
    };
    // A change can land between producing a response and yielding to select.
    if *state.shutdown.borrow() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if workspace_api && !state.sharing.permits(access) {
        return sharing_disabled();
    }
    if let Some(lease) = &lease {
        if !lease.is_valid(&state.sharing) {
            return lease_failure(&state, lease);
        }
    }
    response
        .headers_mut()
        .insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    if api {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    }
    response
}
pub async fn auth(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Json<serde_json::Value> {
    Json(
        json!({"required":true,"authenticated":state.security.authorized(&headers, state.sharing.snapshot().generation).await}),
    )
}
#[derive(Deserialize)]
pub struct Login {
    password: String,
}
pub async fn login(
    State(state): State<Arc<ServerState>>,
    axum::extract::Extension(access): axum::extract::Extension<Access>,
    Json(body): Json<Login>,
) -> Response {
    let now = Instant::now();
    let mut attempts = state.security.attempts.lock().await;
    attempts.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
    if attempts.len() >= 10 {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    attempts.push(now);
    drop(attempts);
    let digest: [u8; 32] = Sha256::digest(body.password.as_bytes()).into();
    if !bool::from(state.security.password.ct_eq(&digest)) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(token) = crate::random_secret() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let mut sessions = state.security.sessions.lock().await;
    let inserted = state
        .sharing
        .while_current(access, || issue_session(&mut sessions, &token, access, now));
    match inserted {
        Some(Ok(())) => {}
        Some(Err(status)) => return status.into_response(),
        None => return sharing_disabled(),
    }
    session_response(&state.security, &token)
}
fn issue_session(
    sessions: &mut HashMap<String, Arc<SessionEntry>>,
    token: &str,
    access: Access,
    now: Instant,
) -> Result<(), StatusCode> {
    sessions.retain(|_, entry| {
        let keep = entry.expires_at.is_none_or(|at| at > now)
            && entry.access.generation == access.generation;
        if !keep {
            entry.revoke();
        }
        keep
    });
    if sessions.len() >= 64 {
        return Err(StatusCode::CONFLICT);
    }
    if let Some(previous) = sessions.get(token) {
        previous.revoke();
    }
    sessions.insert(
        token.to_owned(),
        Arc::new(SessionEntry::new(
            token,
            now + Duration::from_secs(SESSION_SECONDS),
            access,
        )),
    );
    Ok(())
}
fn session_response(security: &Security, token: &str) -> Response {
    let value = format!(
        "{}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={SESSION_SECONDS}",
        security.cookie_name
    );
    (
        [(header::SET_COOKIE, value)],
        Json(json!({"required":true,"authenticated":true})),
    )
        .into_response()
}
pub async fn issue_qr(
    State(state): State<Arc<ServerState>>,
    axum::extract::Extension(access): axum::extract::Extension<Access>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if !body.as_object().is_some_and(serde_json::Map::is_empty) {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }
    match state.pairing.issue(&state.sharing, access) {
        Ok(authorization) => Json(authorization).into_response(),
        Err(_) if !state.sharing.permits(access) => sharing_disabled(),
        Err(_) => StatusCode::CONFLICT.into_response(),
    }
}

pub async fn redeem_qr(
    State(state): State<Arc<ServerState>>,
    axum::extract::Extension(access): axum::extract::Extension<Access>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(ticket) = body
        .as_object()
        .filter(|object| object.len() == 1)
        .and_then(|object| object.get("ticket"))
        .and_then(serde_json::Value::as_str)
    else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    let Ok(token) = crate::random_secret() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let mut sessions = state.security.sessions.lock().await;
    match state.pairing.redeem(&state.sharing, access, ticket, || {
        issue_session(&mut sessions, &token, access, Instant::now())
    }) {
        Ok(()) => session_response(&state.security, &token),
        Err(StatusCode::FORBIDDEN) => sharing_disabled(),
        Err(status) => status.into_response(),
    }
}

pub async fn logout(State(state): State<Arc<ServerState>>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie(&headers, &state.security.cookie_name) {
        let mut sessions = state.security.sessions.lock().await;
        if let Some(entry) = sessions.get(token) {
            entry.revoke();
        }
        sessions.remove(token);
    }
    (
        [(
            header::SET_COOKIE,
            format!(
                "{}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0",
                state.security.cookie_name
            ),
        )],
        Json(json!({"required":true,"authenticated":false})),
    )
        .into_response()
}

pub fn sharing_disabled() -> Response {
    (StatusCode::FORBIDDEN, [(header::CACHE_CONTROL,"no-store")], Json(json!({"error":"宿主已关闭共享，请在这台电脑上开启允许共享", "code":"sharing_disabled"}))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn private_capability_cannot_promote_a_public_listener_or_cookie() {
        let policy = SharingControl::new(true);
        let secret = "isolated-parent-capability-0123456789";
        let private = Security::new_control(secret, vec![], "private", policy.snapshot()).unwrap();
        let public = Security::new(secret, vec![], "public").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-rpi-node-control", secret.parse().unwrap());
        let lease = private.authenticate(&headers, 0).await.unwrap();
        assert!(private
            .while_authorized(&lease, &policy, || true)
            .await
            .unwrap());
        assert!(lease.entry.expires_at.is_none());
        assert!(public.authenticate(&headers, 0).await.is_none());
        headers.insert("x-rpi-node-control", "wrong-capability".parse().unwrap());
        assert!(private.authenticate(&headers, 0).await.is_none());
        assert!(private
            .authenticate(&super::tests::headers(&private, "local-control"), 0)
            .await
            .is_none());
        let (shutdown, stopped) = watch::channel(false);
        shutdown.send_replace(true);
        tokio::time::timeout(
            Duration::from_millis(50),
            lease.invalidated(&policy, &stopped),
        )
        .await
        .unwrap();
    }

    fn entry(token: &str, expires_at: Instant, access: Access) -> Arc<SessionEntry> {
        Arc::new(SessionEntry::new(token, expires_at, access))
    }

    fn headers(security: &Security, token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("{}={token}", security.cookie_name).parse().unwrap(),
        );
        headers
    }

    #[tokio::test]
    async fn logout_revokes_lease_before_removing_session() {
        let security = Security::new("test-only", vec!["localhost".into()], "logout").unwrap();
        let policy = SharingControl::new(true);
        let access = policy.snapshot();
        let token = "logout-token";
        let mut sessions = security.sessions.lock().await;
        let session = entry(token, Instant::now() + Duration::from_secs(60), access);
        sessions.insert(token.into(), session.clone());
        drop(sessions);
        let lease = security
            .authenticate(&headers(&security, token), 0)
            .await
            .unwrap();
        session.revoke();
        security.sessions.lock().await.remove(token);
        assert!(!lease.is_valid(&policy));
        assert!(*session.revoked.borrow());
    }

    #[tokio::test]
    async fn expired_session_is_rejected_and_revoked_without_sleeping() {
        let security = Security::new("test-only", vec!["localhost".into()], "expiry").unwrap();
        let policy = SharingControl::new(true);
        let access = policy.snapshot();
        let token = "expired-token";
        let session = entry(token, Instant::now() - Duration::from_secs(1), access);
        security
            .sessions
            .lock()
            .await
            .insert(token.into(), session.clone());
        assert!(security
            .authenticate(&headers(&security, token), 0)
            .await
            .is_none());
        assert!(*session.revoked.borrow());
        assert!(security.sessions.lock().await.is_empty());
    }

    #[tokio::test]
    async fn lease_waits_for_logout_expiry_sharing_and_shutdown() {
        let security = Security::new("test-only", vec!["localhost".into()], "wait").unwrap();
        let policy = SharingControl::new(true);
        let access = policy.snapshot();
        let token = "wait-token";
        security.sessions.lock().await.insert(
            token.into(),
            entry(token, Instant::now() + Duration::from_secs(60), access),
        );
        let lease = security
            .authenticate(&headers(&security, token), 0)
            .await
            .unwrap();
        let (shutdown, receiver) = watch::channel(false);
        let waiting = lease.clone();
        let policy_for_task = policy.clone();
        let task = tokio::spawn(async move {
            waiting.invalidated(&policy_for_task, &receiver).await;
        });
        tokio::task::yield_now().await;
        policy.set_enabled(false);
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        drop(shutdown);
    }

    #[tokio::test]
    async fn generation_and_principal_are_isolated() {
        let security = Security::new("test-only", vec!["localhost".into()], "isolation").unwrap();
        let policy = SharingControl::new(true);
        let access = policy.snapshot();
        let mut sessions = security.sessions.lock().await;
        sessions.insert(
            "first".into(),
            entry("first", Instant::now() + Duration::from_secs(60), access),
        );
        sessions.insert(
            "second".into(),
            entry("second", Instant::now() + Duration::from_secs(60), access),
        );
        drop(sessions);
        let first = security
            .authenticate(&headers(&security, "first"), 0)
            .await
            .unwrap();
        let second = security
            .authenticate(&headers(&security, "second"), 0)
            .await
            .unwrap();
        assert_ne!(first.principal(), second.principal());
        assert_ne!(first.principal(), "first");
        assert_eq!(first.access().generation, access.generation);
        policy.set_enabled(false);
        policy.set_enabled(true);
        assert!(!first.is_valid(&policy));
        assert!(!second.is_valid(&policy));
    }

    #[tokio::test]
    async fn while_authorized_does_not_run_for_invalid_lease() {
        let security = Security::new("test-only", vec!["localhost".into()], "commit").unwrap();
        let policy = SharingControl::new(true);
        let access = policy.snapshot();
        let token = "commit-token";
        let session = entry(token, Instant::now() + Duration::from_secs(60), access);
        security
            .sessions
            .lock()
            .await
            .insert(token.into(), session.clone());
        let lease = security
            .authenticate(&headers(&security, token), 0)
            .await
            .unwrap();
        session.revoke();
        let mut ran = false;
        assert_eq!(
            security
                .while_authorized(&lease, &policy, || {
                    ran = true;
                    1
                })
                .await,
            None
        );
        assert!(!ran);
    }

    #[tokio::test]
    async fn valid_commit_runs_once_but_revoke_and_off_on_fence_it() {
        let security =
            Security::new("test-only", vec!["localhost".into()], "commit-valid").unwrap();
        let policy = SharingControl::new(true);
        let access = policy.snapshot();
        let token = "valid-token";
        let session = entry(token, Instant::now() + Duration::from_secs(60), access);
        security
            .sessions
            .lock()
            .await
            .insert(token.into(), session.clone());
        let lease = security
            .authenticate(&headers(&security, token), 0)
            .await
            .unwrap();
        assert_eq!(
            security.while_authorized(&lease, &policy, || 17).await,
            Some(17)
        );
        policy.set_enabled(false);
        policy.set_enabled(true);
        assert_eq!(
            security
                .while_authorized(&lease, &policy, || panic!("stale commit"))
                .await,
            None::<()>
        );
        session.revoke();
        assert_eq!(
            security
                .while_authorized(&lease, &policy, || panic!("revoked commit"))
                .await,
            None::<()>
        );
    }

    #[tokio::test]
    async fn invalidation_observes_already_revoked_deadlines_and_shutdown() {
        let policy = SharingControl::new(true);
        let access = policy.snapshot();
        let (shutdown, receiver) = watch::channel(false);
        let revoked = entry("revoked", Instant::now() + Duration::from_secs(60), access);
        let lease = AuthLease {
            entry: revoked.clone(),
        };
        revoked.revoke();
        tokio::time::timeout(
            Duration::from_secs(1),
            lease.invalidated(&policy, &receiver),
        )
        .await
        .unwrap();
        let expired = AuthLease {
            entry: entry("expired", Instant::now() - Duration::from_secs(1), access),
        };
        tokio::time::timeout(
            Duration::from_secs(1),
            expired.invalidated(&policy, &receiver),
        )
        .await
        .unwrap();
        let expiring = AuthLease {
            entry: entry(
                "expiring",
                Instant::now() + Duration::from_millis(5),
                access,
            ),
        };
        tokio::time::timeout(
            Duration::from_secs(1),
            expiring.invalidated(&policy, &receiver),
        )
        .await
        .unwrap();
        assert!(!expiring.is_valid(&policy));
        let live = AuthLease {
            entry: entry("live", Instant::now() + Duration::from_secs(60), access),
        };
        let waiting = live.invalidated(&policy, &receiver);
        tokio::pin!(waiting);
        tokio::select! {
            biased;
            _ = &mut waiting => panic!("live lease invalidated early"),
            _ = tokio::task::yield_now() => {},
        }
        shutdown.send_replace(true);
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn late_old_epoch_auth_does_not_erase_new_login() {
        let security = Security::new("test-only", vec!["localhost".into()], "epoch-test").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("{}=new-token", security.cookie_name)
                .parse()
                .unwrap(),
        );
        let policy = crate::SharingControl::new(true);
        let current = policy.snapshot();
        security.sessions.lock().await.insert(
            "new-token".into(),
            entry(
                "new-token",
                Instant::now() + Duration::from_secs(60),
                Access {
                    enabled: true,
                    generation: current.generation + 2,
                },
            ),
        );
        assert!(!security.authorized(&headers, 0).await);
        assert!(security.authorized(&headers, current.generation + 2).await);
        let old = policy.snapshot();
        policy.set_enabled(false);
        policy.set_enabled(true);
        assert_eq!(
            policy.while_current(old, || panic!("stale login committed")),
            None::<()>
        );
        assert!(security.authorized(&headers, current.generation + 2).await);
    }
}
