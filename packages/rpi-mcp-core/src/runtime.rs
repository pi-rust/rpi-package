//! Persistent connections scoped to the host session and configuration identity.
use crate::connection::{
    initialize_request, initialized_notification, parse_initialize, response_result,
    tools_call_request,
};
use crate::control::{Control, Deadline};
use crate::http::HttpTransport;
use crate::RequestContext;
use crate::{McpConfig, McpServerConfig, Request, Response, ServerInfo, StdioTransport, Tool};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredServer {
    pub name: String,
    pub info: ServerInfo,
    pub tools: Vec<Tool>,
}

#[derive(Debug)]
enum Transport {
    Stdio(StdioTransport),
    Http(Box<HttpTransport>),
}
impl Transport {
    fn request(&mut self, request: &Request, control: &Control) -> Result<Response, String> {
        match self {
            Self::Stdio(t) => t.request_control(request, control),
            Self::Http(t) => t.request(request, control),
        }
    }
    fn notify(&mut self, control: &Control) -> Result<(), String> {
        match self {
            Self::Stdio(t) => t.notify_control(&initialized_notification(), control),
            Self::Http(t) => t.notify(&initialized_notification(), control),
        }
    }
    fn timeout(&mut self, timeout: u64) {
        match self {
            Self::Stdio(t) => t.set_timeout(timeout),
            Self::Http(t) => t.set_timeout(timeout),
        }
    }
}
#[derive(Debug)]
struct Session {
    transport: Transport,
    info: ServerInfo,
    next_id: u64,
    failed: bool,
    sampling_context: Option<crate::SamplingContext>,
    callbacks: Option<Arc<crate::callback::Callbacks>>,
    timeout: u64,
}
impl Session {
    fn start(
        config: &McpServerConfig,
        timeout: u64,
        control: &Control,
        notifications: &SyncSender<Value>,
        auth: Option<(crate::AuthBinding, Arc<dyn crate::CredentialProvider>)>,
        sampling: Option<(crate::SamplingOptions, crate::SamplingContext)>,
    ) -> Result<Self, String> {
        control.check()?;
        let mut transport = match config {
            McpServerConfig::Stdio(s) => Transport::Stdio(StdioTransport::start(
                &s.command,
                &s.args,
                &s.env,
                s.cwd.as_deref(),
                Some(notifications.clone()),
            )?),
            McpServerConfig::Http(s) => Transport::Http(Box::new(match auth {
                Some((binding, provider)) => HttpTransport::with_auth(s, binding, provider)?,
                None => HttpTransport::new(s)?,
            })),
        };
        let (callbacks, sampling_context) = match sampling {
            Some((options, context)) => (Some(Arc::new(crate::callback::Callbacks::new(options)?)), Some(context)),
            None => (None, None),
        };
        match &mut transport {
            Transport::Stdio(t) => t.set_callbacks(callbacks.clone()),
            Transport::Http(t) => t.set_callbacks(callbacks.clone()),
        }
        transport.timeout(timeout);
        let mut initialize = initialize_request(1, "rpi-mcp-adapter", env!("CARGO_PKG_VERSION"));
        if callbacks.is_some() { initialize.params.as_mut().unwrap()["capabilities"] = json!({"sampling":{}}); }
        let info = parse_initialize(transport.request(
            &initialize,
            control,
        )?)?;
        if let Transport::Http(http) = &mut transport {
            http.version = Some(info.protocol_version.clone());
        }
        transport.notify(control)?;
        Ok(Self {
            transport,
            info,
            next_id: 2,
            failed: false,
            callbacks,
            sampling_context,
            timeout,
        })
    }
    fn request(
        &mut self,
        method: &str,
        params: Value,
        control: &Control,
    ) -> Result<Response, String> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("MCP request ID exhausted")?;
        if ["tools/call", "resources/read", "prompts/get"].contains(&method) {
            if let (Some(callbacks), Some(context)) = (&self.callbacks, &self.sampling_context) {
                let mut context = context.clone();
                context.operation = format!("{method}:{id}");
                context.deadline = std::time::Instant::now() + Duration::from_secs(self.timeout);
                callbacks.activate(context, control.clone());
            }
        }
        let response = self
            .transport
            .request(&Request::new(id, method, Some(params)), control);
        if let Some(callbacks) = &self.callbacks { callbacks.retire(); }
        if response.is_err() {
            self.failed = true;
        }
        response
    }
    fn tools(&mut self, control: &Control) -> Result<Vec<Tool>, String> {
        let mut tools = Vec::new();
        let mut cursor = None;
        let mut seen = HashSet::new();
        for _ in 0..100 {
            let params = cursor
                .as_ref()
                .map(|v| json!({"cursor":v}))
                .unwrap_or_else(|| json!({}));
            let result = response_result(self.request("tools/list", params, control)?)?;
            tools.extend(crate::tool_list(&result)?);
            if tools.len() > 16_384 {
                return Err("MCP tool list exceeded 16384 tools".into());
            }
            match result.get("nextCursor") {
                None => return Ok(tools),
                Some(Value::String(next)) => {
                    if !seen.insert(next.clone()) {
                        return Err("MCP tools/list returned a repeated pagination cursor".into());
                    }
                    cursor = Some(next.clone());
                }
                _ => return Err("MCP tools/list nextCursor must be a string".into()),
            }
        }
        Err("MCP tools/list exceeded 100 pages".into())
    }
}
// Keep the last configuration even after a failed transport is dropped, so
// a config edit cannot replay the old server's subscriptions onto a new server.
#[derive(Default)]
struct ConnectionState {
    config: Option<McpServerConfig>,
    session: Option<Session>,
}

struct Entry {
    session: Mutex<ConnectionState>,
    stop: Control,
    /// Notifications from this connection's reader, consumed by its dispatcher.
    /// Owned by the entry so a re-opened session reuses the same dispatcher.
    notifications: SyncSender<Value>,
    /// The dispatcher thread, joined on close so a released connection leaves no
    /// thread behind. `stop` is cancelled first, which unblocks its wait.
    dispatcher: Mutex<Option<JoinHandle<()>>>,
}
type Sessions = BTreeMap<String, Arc<Entry>>;

// ---------------------------------------------------------------------------
// Resource subscriptions and pushed updates
// ---------------------------------------------------------------------------

// Each subscription lifetime gets its own identity, so a read begun before
// unsubscribe/close can never be accepted by a later subscription to the same URI.
// Pointer identity distinguishes subscription lifetimes; the counter identifies
// the newest read, including one delivered by an overlapping flush.
type Subscription = Arc<AtomicU64>;
type Subscriptions = HashMap<String, HashMap<String, Subscription>>;

/// A resource read in response to `notifications/resources/updated`, waiting to
/// be shown to the model. Drained at the turn boundary (`TurnEnd`) so injection
/// never splits an assistant `tool_calls` from its `toolResult`s.
#[derive(Debug, Clone)]
pub struct ResourceUpdate {
    /// Session scope this update belongs to (see [`scope_of_key`]). The host
    /// serves one session at a time, so an update produced under a different
    /// scope must never be injected into the current one.
    pub scope: String,
    pub key: String,
    pub uri: String,
    pub text: String,
}
struct QueuedResourceUpdate {
    update: ResourceUpdate,
    subscription: Subscription,
    revision: u64,
}

impl QueuedResourceUpdate {
    fn new(update: ResourceUpdate, subscription: Subscription) -> Self {
        let revision = subscription.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        Self {
            update,
            subscription,
            revision,
        }
    }
}

const MAX_PENDING: usize = 64;
const MAX_RESOURCE_TEXT: usize = 20_000;

/// The session scope embedded in a connection key (`[scope, identity, name]`).
fn scope_of_key(key: &str) -> String {
    serde_json::from_str::<Value>(key)
        .ok()
        .and_then(|value| value.get(0).and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default()
}

/// Flatten a `resources/read` result's `contents` into text (blobs are noted by
/// size, not inlined).
fn resource_text(result: &Value) -> String {
    let Some(contents) = result.get("contents").and_then(Value::as_array) else {
        return bound_resource_text(result.to_string());
    };
    let mut out = String::new();
    for item in contents {
        let uri = item.get("uri").and_then(Value::as_str).unwrap_or("");
        let body = if let Some(text) = item.get("text").and_then(Value::as_str) {
            text.to_string()
        } else if let Some(blob) = item.get("blob").and_then(Value::as_str) {
            format!("[blob, {} base64 chars]", blob.len())
        } else {
            continue;
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("{uri}\n{body}"));
    }
    bound_resource_text(out)
}

fn bound_resource_text(mut out: String) -> String {
    if out.len() > MAX_RESOURCE_TEXT {
        let mut end = MAX_RESOURCE_TEXT;
        while end > 0 && !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push_str("\n[truncated]");
    }
    out
}

fn lock_until<'a, T>(
    mutex: &'a Mutex<T>,
    deadline: &Deadline,
) -> Result<MutexGuard<'a, T>, String> {
    loop {
        deadline.check()?;
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(_)) => return Err("MCP connection lock poisoned".into()),
            Err(TryLockError::WouldBlock) => std::thread::sleep(deadline.slice()?),
        }
    }
}

/// Stop a released entry and join its dispatcher. Cancelling `stop` first makes
/// the dispatcher's 100 ms wait return promptly, and it also aborts any
/// in-flight background `resources/read` (whose control is linked to it).
fn retire_entry(entry: &Arc<Entry>) {
    entry.stop.cancel();
    // Serialize joins too: another close/shutdown must not remove this entry
    // and allow a replacement while the first caller is still joining it.
    if let Ok(mut slot) = entry.dispatcher.lock() {
        if let Some(handle) = slot.take() {
            let _ = handle.join();
        }
    }
}

fn config_file(context: &RequestContext, params: &Value) -> Result<PathBuf, String> {
    let path = params
        .get("configPath")
        .and_then(Value::as_str)
        .ok_or("missing configPath")?;
    std::fs::canonicalize(context.cwd.clone().join(path))
        .map_err(|e| format!("read MCP config {path:?}: {e}"))
}
fn load_config(context: &RequestContext, params: &Value) -> Result<(PathBuf, McpConfig), String> {
    let path = config_file(context, params)?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read MCP config: {e}"))?;
    Ok((path, McpConfig::parse(&text)?))
}

/// Connection state owned by one runtime instance; never a global registry.
static AUTH_IDS: AtomicU64 = AtomicU64::new(1);

struct AuthTarget {
    config: McpServerConfig,
    key: String,
}

pub struct RuntimeState {
    sessions: Mutex<Sessions>,
    subscriptions: Mutex<Subscriptions>,
    pending: Mutex<Vec<QueuedResourceUpdate>>,
    weak: Weak<RuntimeState>,
    auth: Option<Arc<dyn crate::CredentialProvider>>,
    namespace: u64,
    sampling_namespace: String,
    sampling: Option<crate::SamplingOptions>,
    auth_targets: Mutex<HashMap<String, AuthTarget>>,
}

/// Owner of connections, dispatchers and subscription lifetimes.
/// Dropping it cancels operations, kills children and joins owned workers.
pub struct McpRuntime {
    state: Arc<RuntimeState>,
}
impl Default for McpRuntime {
    fn default() -> Self {
        Self::new()
    }
}
impl McpRuntime {
    pub fn new() -> Self {
        Self::with_providers(None, None)
    }
    pub fn with_auth(provider: Arc<dyn crate::CredentialProvider>) -> Self {
        Self::with_providers(Some(provider), None)
    }
    pub fn with_providers(auth: Option<Arc<dyn crate::CredentialProvider>>, sampling: Option<crate::SamplingOptions>) -> Self {
        let mut random = [0u8; 32];
        let random_ok = getrandom::getrandom(&mut random).is_ok();
        let sampling = sampling.filter(|_| random_ok);
        let sampling_namespace = random.iter().map(|b| format!("{b:02x}")).collect();
        Self {
            state: Arc::new_cyclic(|weak| RuntimeState {
                sessions: Mutex::default(),
                subscriptions: Mutex::default(),
                pending: Mutex::default(),
                weak: weak.clone(),
                auth,
                sampling,
                sampling_namespace,
                auth_targets: Mutex::default(),
                namespace: AUTH_IDS.fetch_add(1, Ordering::Relaxed),
            }),
        }
    }
}
impl std::ops::Deref for McpRuntime {
    type Target = RuntimeState;
    fn deref(&self) -> &RuntimeState {
        &self.state
    }
}
impl Drop for McpRuntime {
    fn drop(&mut self) {
        self.shutdown_connections();
    }
}

impl RuntimeState {
    pub fn take_pending_updates(&self, scope: Option<&str>) -> Vec<ResourceUpdate> {
        self.take_pending_batch(scope)
            .into_iter()
            .map(|queued| queued.update)
            .collect()
    }

    /// Drain the queued updates, keeping only those for `scope`.
    ///
    /// Updates belonging to any other scope are dropped: they were produced for a
    /// session the host is no longer serving, and injecting them would leak one
    /// session's resource contents into another's context. Passing `None`
    /// drains every scope; the embedding adapter chooses the active scope.
    fn take_pending_batch(&self, scope: Option<&str>) -> Vec<QueuedResourceUpdate> {
        let drained = std::mem::take(&mut *self.pending.lock().unwrap_or_else(|p| p.into_inner()));
        match scope {
            Some(scope) => drained
                .into_iter()
                .filter(|queued| queued.update.scope == scope)
                .collect(),
            None => drained,
        }
    }

    /// Deliver a neutral batch. Failed delivery is retried only if its scope and
    /// subscription lifetime remain active. Presentation belongs to the caller.
    pub fn deliver_resource_updates(
        &self,
        current_scope: impl Fn() -> Option<String>,
        append: impl FnOnce(Vec<ResourceUpdate>) -> Result<(), String>,
    ) -> Result<(), String> {
        let scope = current_scope();
        let updates = self.take_pending_batch(scope.as_deref());
        if updates.is_empty() {
            return Ok(());
        }
        let result = append(updates.iter().map(|queued| queued.update.clone()).collect());
        if result.is_err() && current_scope() == scope {
            self.restore_pending_batch(updates);
        }
        result
    }

    fn restore_pending_batch(&self, updates: Vec<QueuedResourceUpdate>) {
        // Retirement and enqueue use this same lock order. A failed append must
        // never resurrect an unsubscribed URI, a closed connection or an older
        // subscription lifetime. A newer read supersedes an older failed delivery,
        // even if another flush has already drained and delivered that newer read.
        let map = self.subscriptions.lock().unwrap_or_else(|p| p.into_inner());
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        let mut retry = Vec::new();
        for queued in updates {
            if pending.len() + retry.len() >= MAX_PENDING {
                break;
            }
            if map
                .get(&queued.update.key)
                .and_then(|uris| uris.get(&queued.update.uri))
                .is_some_and(|current| {
                    Arc::ptr_eq(current, &queued.subscription)
                        && current.load(Ordering::Relaxed) == queued.revision
                })
            {
                retry.push(queued);
            }
        }
        retry.append(&mut *pending);
        *pending = retry;
    }

    /// Drop queued updates for one connection (close, unsubscribe, failed subscribe).
    fn clear_pending_for_key(&self, key: &str) {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|queued| queued.update.key != key);
    }

    /// Drop queued updates for one connection + URI.
    fn clear_pending_for_uri(&self, key: &str, uri: &str) {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|queued| !(queued.update.key == key && queued.update.uri == uri));
    }

    fn add_subscription(&self, key: &str, uri: &str) -> Subscription {
        let token = Arc::new(AtomicU64::new(0));
        let mut map = self.subscriptions.lock().unwrap_or_else(|p| p.into_inner());
        map.entry(key.to_string())
            .or_default()
            .insert(uri.to_string(), token.clone());
        // Rotation starts a new delivery lifetime. Purge queued content under the
        // same lock used by enqueue and retirement, as well as invalidating reads.
        self.clear_pending_for_uri(key, uri);
        token
    }

    fn subscription(&self, key: &str, uri: &str) -> Option<Subscription> {
        self.subscriptions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
            .and_then(|uris| uris.get(uri))
            .cloned()
    }

    /// Always lock subscriptions before pending, and hold both while invalidating
    /// and purging. Enqueue uses the same order, so nothing can slip in after purge.
    /// An expected token prevents a failed older operation retiring a newer lifetime.
    fn remove_subscription(&self, key: &str, uri: &str, expected: Option<&Subscription>) {
        let mut map = self.subscriptions.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(expected) = expected {
            if !map
                .get(key)
                .and_then(|uris| uris.get(uri))
                .is_some_and(|current| Arc::ptr_eq(current, expected))
            {
                return;
            }
        }
        if let Some(uris) = map.get_mut(key) {
            uris.remove(uri);
            if uris.is_empty() {
                map.remove(key);
            }
        }
        self.clear_pending_for_uri(key, uri);
    }

    fn clear_subscriptions(&self, key: &str) {
        let mut map = self.subscriptions.lock().unwrap_or_else(|p| p.into_inner());
        map.remove(key);
        self.clear_pending_for_key(key);
    }

    /// Re-register only resource subscriptions when a transport is recreated.
    /// User tool calls are never replayed. Rotate identities and purge the old
    /// connection's queued reads atomically before accepting early notifications.
    fn restore_subscriptions(
        &self,
        key: &str,
        session: &mut Session,
        control: &Control,
        skip_uri: Option<&str>,
    ) -> Result<(), String> {
        let uris = {
            let mut map = self.subscriptions.lock().unwrap_or_else(|p| p.into_inner());
            let mut uris = Vec::new();
            if let Some(subscribed) = map.get_mut(key) {
                for (uri, token) in subscribed {
                    // An explicit subscribe/unsubscribe handles this URI once the
                    // connection is ready; do not issue a duplicate RPC.
                    if skip_uri == Some(uri.as_str()) {
                        continue;
                    }
                    *token = Arc::new(AtomicU64::new(0));
                    uris.push(uri.clone());
                }
            }
            self.clear_pending_for_key(key);
            uris.sort();
            uris
        };
        for uri in uris {
            response_result(session.request(
                "resources/subscribe",
                json!({"uri":uri}),
                control,
            )?)?;
        }
        Ok(())
    }

    /// One dispatcher per connection. It ends when every sender is gone (entry
    /// removed and session dropped) or when the connection is stopped, whichever
    /// happens first — the stop check is what lets `close`/`shutdown` join it
    /// promptly instead of waiting on the reader.
    fn spawn_dispatcher(
        &self,
        key: String,
        receiver: Receiver<Value>,
        stop: Control,
    ) -> JoinHandle<()> {
        let weak = self.weak.clone();
        std::thread::Builder::new()
            .name("mcp-notify".into())
            .spawn(move || loop {
                if stop.check().is_err() {
                    break;
                }
                match receiver.recv_timeout(Duration::from_millis(100)) {
                    Ok(value) => {
                        let Some(state) = weak.upgrade() else {
                            break;
                        };
                        state.dispatch_notification(&key, &value);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            })
            .expect("spawn MCP notification dispatcher")
    }

    fn dispatch_notification(&self, key: &str, value: &Value) {
        self.dispatch_notification_with(key, value, |key, uri| self.read_resource_by_key(key, uri));
    }

    fn dispatch_notification_with(
        &self,
        key: &str,
        value: &Value,
        read: impl FnOnce(&str, &str) -> Result<String, String>,
    ) {
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if method != "notifications/resources/updated" {
            return;
        }
        let Some(uri) = value
            .get("params")
            .and_then(|params| params.get("uri"))
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return;
        };
        // A subscription is marked *before* its `subscribe` request goes out, so a
        // server that pushes the first update while the handshake is still in flight
        // is accepted here rather than dropped. A failed subscribe rolls the mark
        // back (and purges anything queued in the meantime).
        let Some(token) = self.subscription(key, &uri) else {
            return;
        };
        if let Ok(text) = read(key, &uri) {
            let map = self.subscriptions.lock().unwrap_or_else(|p| p.into_inner());
            if !map
                .get(key)
                .and_then(|uris| uris.get(&uri))
                .is_some_and(|current| Arc::ptr_eq(current, &token))
            {
                return;
            }
            // Keep the subscription lock through enqueue, synchronized with retirement.
            let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            if pending.len() < MAX_PENDING {
                pending.push(QueuedResourceUpdate::new(
                    ResourceUpdate {
                        scope: scope_of_key(key),
                        key: key.to_string(),
                        uri,
                        text,
                    },
                    token,
                ));
            }
        }
    }

    /// Read a resource on an existing connection (used by the dispatcher). Never
    /// creates a session: if the connection is gone, the update is dropped. Runs on
    /// the dispatcher thread, not the reader, so it can await the reply.
    ///
    /// The control is linked to the connection's `stop`, so `close`/`shutdown`
    /// cancels an in-flight read instead of waiting for its timeout — and the lock
    /// wait below shares that same deadline.
    fn read_resource_by_key(&self, key: &str, uri: &str) -> Result<String, String> {
        let entry = self
            .sessions
            .lock()
            .map_err(|_| "MCP session registry poisoned")?
            .get(key)
            .cloned()
            .ok_or("MCP connection is gone")?;
        let control = Control::default().linked(&entry.stop);
        let deadline = Deadline::new(120, "resources/read", &control);
        let mut state = lock_until(&entry.session, &deadline)?;
        let session = state.session.as_mut().ok_or("MCP connection is closed")?;
        let result = session
            .request("resources/read", json!({"uri": uri}), &control)
            .and_then(response_result);
        if session.failed {
            state.session.take();
        }
        result.map(|result| resource_text(&result))
    }

    /// `resources/list` | `read` | `subscribe` | `unsubscribe`.
    pub fn resources(
        &self,
        context: &RequestContext,
        params: &Value,
        control: &Control,
    ) -> Result<Value, String> {
        control.check()?;
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list");
        if action == "templates/list" {
            return serde_json::to_value(self.resource_templates_list(context, params, control)?)
                .map_err(|e| e.to_string());
        }
        if !matches!(action, "list" | "read" | "subscribe" | "unsubscribe") {
            return Err(
                "mcp_resource action must be list, templates/list, read, subscribe or unsubscribe"
                    .into(),
            );
        }
        let (key, name, config, timeout) = self.target(context, params)?;

        if action == "list" {
            return self.with_session(
                key,
                config,
                timeout,
                control,
                None,
                |session, control, _| {
                    let mut resources = Vec::new();
                    let mut cursor = None;
                    let mut seen = HashSet::new();
                    for _ in 0..100 {
                        let params = cursor
                            .as_ref()
                            .map(|c| json!({"cursor": c}))
                            .unwrap_or_else(|| json!({}));
                        let result =
                            response_result(session.request("resources/list", params, control)?)?;
                        if let Some(items) = result.get("resources").and_then(Value::as_array) {
                            resources.extend(items.iter().cloned());
                        }
                        if resources.len() > 16_384 {
                            return Err("MCP resource list exceeded 16384 resources".into());
                        }
                        match result.get("nextCursor") {
                            None => return Ok(json!({"server": name, "resources": resources})),
                            Some(Value::String(next)) => {
                                if !seen.insert(next.clone()) {
                                    return Err(
                                        "MCP resources/list returned a repeated pagination cursor"
                                            .into(),
                                    );
                                }
                                cursor = Some(next.clone());
                            }
                            _ => {
                                return Err("MCP resources/list nextCursor must be a string".into())
                            }
                        }
                    }
                    Err("MCP resources/list exceeded 100 pages".into())
                },
            );
        }

        let uri = string_param(params, "uri")?;
        if action == "unsubscribe" {
            let previous = self.subscription(&key, &uri);
            let mut retired = false;
            let _ = self.with_session(
                key.clone(),
                config,
                timeout,
                control,
                Some(&uri),
                |session, control, _| {
                    // Serialize subscription changes with their RPCs on this connection.
                    // Invalidate before sending, so even a failed remote unsubscribe
                    // stops local delivery and a read cannot outlive the purge.
                    self.remove_subscription(&key, &uri, None);
                    retired = true;
                    let result = session
                        .request("resources/unsubscribe", json!({"uri": uri}), control)
                        .and_then(response_result);
                    // A rejected unsubscribe can leave the remote registration alive
                    // after local delivery is retired. Reconnect rather than reusing
                    // that mismatched state; other URIs will be restored normally.
                    if result.is_err() {
                        session.failed = true;
                    }
                    result
                },
            );
            // If connecting/locking failed, retire only the lifetime we observed;
            // never remove a fresh subscription created by a concurrent call.
            if !retired {
                if let Some(previous) = previous {
                    self.remove_subscription(&key, &uri, Some(&previous));
                }
            }
            return Ok(json!({"server": name, "uri": uri, "unsubscribed": true}));
        }
        if action == "subscribe" {
            if matches!(&config, McpServerConfig::Http(_)) {
                return Err(
                "mcp_resource subscribe is not supported for HTTP servers yet (no push channel)"
                    .into(),
            );
            }
            self.with_session(
                key.clone(),
                config,
                timeout,
                control,
                Some(&uri),
                |session, control, new_connection| {
                    // On an existing transport, a mark means the server is already
                    // subscribed. A fresh transport skips restoration for this URI
                    // and needs exactly one RPC below, with its new local lifetime
                    // installed before any early notification can arrive.
                    let already_subscribed =
                        !new_connection && self.subscription(&key, &uri).is_some();
                    // Register before the RPC so early notifications are accepted, while
                    // holding the connection lock so subscribe/unsubscribe stay ordered.
                    let token = self.add_subscription(&key, &uri);
                    if already_subscribed {
                        return Ok(json!({}));
                    }
                    let subscribed = session
                        .request("resources/subscribe", json!({"uri": uri}), control)
                        .and_then(response_result);
                    if subscribed.is_err() {
                        self.remove_subscription(&key, &uri, Some(&token));
                    }
                    subscribed
                },
            )?;
            return Ok(json!({"server": name, "uri": uri, "subscribed": true}));
        }
        // read — flatten and bound the contents: a large blob must not reach the
        // model through the tool result.
        self.with_session(
            key,
            config,
            timeout,
            control,
            None,
            |session, control, _| {
                let result = response_result(session.request(
                    "resources/read",
                    json!({"uri": uri}),
                    control,
                )?)?;
                Ok(json!({"server": name, "uri": uri, "text": resource_text(&result)}))
            },
        )
    }

    fn with_session<T>(
        &self,
        key: String,
        config: McpServerConfig,
        timeout: u64,
        control: &Control,
        skip_restore_uri: Option<&str>,
        run: impl FnOnce(&mut Session, &Control, bool) -> Result<T, String>,
    ) -> Result<T, String> {
        let entry = {
            // Admission and retirement share identity -> sessions lock order.
            // Never hold either registry lock during provider I/O or joins.
            let targets = self
                .auth_targets
                .lock()
                .map_err(|_| "auth identity registry poisoned")?;
            if (self.auth.is_some() || self.sampling.is_some()) && !targets.values().any(|target| target.key == key) {
                return Err("MCP authentication identity retired".into());
            }
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| "MCP session registry poisoned")?;
            if !sessions.contains_key(&key) && sessions.len() >= 64 {
                return Err("MCP connection limit (64) reached; close unused connections with mcp_list action=close".into());
            }
            match sessions.get(&key) {
                Some(entry) => entry.clone(),
                None => {
                    let (notifications, receiver) = mpsc::sync_channel(64);
                    let stop = Control::default();
                    let dispatcher = self.spawn_dispatcher(key.clone(), receiver, stop.clone());
                    let entry = Arc::new(Entry {
                        session: Mutex::new(ConnectionState::default()),
                        stop,
                        notifications,
                        dispatcher: Mutex::new(Some(dispatcher)),
                    });
                    sessions.insert(key.clone(), Arc::clone(&entry));
                    entry
                }
            }
        };
        let control = control.linked(&entry.stop);
        let deadline = Deadline::new(timeout, "connection lock", &control);
        let mut state = lock_until(&entry.session, &deadline)?;
        if state.config.as_ref() != Some(&config) {
            self.clear_subscriptions(&key);
            state.session.take();
            state.config = Some(config.clone());
        }
        if state.session.as_ref().is_some_and(|session| session.failed) {
            state.session.take();
        }
        let new_connection = state.session.is_none();
        if new_connection {
            let auth = match (&self.auth, &config) {
                (Some(provider), McpServerConfig::Http(server)) => {
                    let (scope, target, name, cwd, _, generation): (
                        String,
                        String,
                        String,
                        PathBuf,
                        u64,
                        u64,
                    ) = serde_json::from_str(&key)
                        .map_err(|_| "invalid auth connection identity")?;
                    let target = serde_json::to_string(&(target, name))
                        .map_err(|_| "invalid auth target")?;
                    let binding = crate::AuthBinding::new(
                        self.namespace,
                        scope,
                        cwd,
                        target,
                        crate::http::validate_mcp_url(&server.url)?.to_string(),
                        generation,
                    )
                    .map_err(|e| e.to_string())?;
                    Some((binding, provider.clone()))
                }
                _ => None,
            };
            let sampling = if let Some(options) = &self.sampling {
                let (scope, target, server, cwd, _, generation): (String, String, String, PathBuf, u64, u64) =
                    serde_json::from_str(&key).map_err(|_| "invalid sampling identity")?;
                Some((options.clone(), crate::SamplingContext {
                    namespace: self.sampling_namespace.clone(), scope, target, server, cwd, generation,
                    operation: "initialize".into(), deadline: deadline.end,
                }))
            } else { None };
            let mut session =
                Session::start(&config, timeout, &control, &entry.notifications, auth, sampling)?;
            if let Err(error) =
                self.restore_subscriptions(&key, &mut session, &control, skip_restore_uri)
            {
                if session.failed || control.check().is_err() {
                    return Err(format!("MCP resource subscription restoration interrupted: {error}; the next call will retry"));
                }
                self.clear_subscriptions(&key);
                return Err(format!("MCP resource subscriptions could not be restored: {error}; subscriptions cleared, subscribe again"));
            }
            state.session = Some(session);
        }
        let session = state.session.as_mut().ok_or("MCP session unavailable")?;
        session.transport.timeout(timeout);
        session.timeout = timeout;
        let result = run(session, &control, new_connection);
        if session.failed {
            state.session.take();
        } // Do not retry tool calls: they may have side effects.
        result
    }

    pub fn shutdown_connections(&self) {
        let entries = {
            let mut targets = self.auth_targets.lock().unwrap_or_else(|p| p.into_inner());
            targets.clear();
            let map = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            // Revoke admission and snapshot owners atomically; draining is outside
            // both registries, so unrelated targets can continue making progress.
            for entry in map.values() {
                entry.stop.cancel();
            }
            map.clone()
        };
        for (key, entry) in entries {
            let _ = self.release_entry(&key, &entry);
        }
    }

    fn close_key(&self, key: &str) -> Result<(), String> {
        let entry = {
            let mut targets = self
                .auth_targets
                .lock()
                .map_err(|_| "auth identity registry poisoned")?;
            targets.retain(|_, target| target.key != key);
            let map = self
                .sessions
                .lock()
                .map_err(|_| "MCP session registry poisoned")?;
            match map.get(key) {
                Some(entry) => {
                    entry.stop.cancel();
                    Some(entry.clone())
                }
                None => {
                    self.clear_subscriptions(key);
                    None
                }
            }
        };
        if let Some(entry) = entry {
            self.release_entry(key, &entry)?;
        }
        Ok(())
    }

    /// Drain active operations before purging local lifetimes. Keep removal and
    /// purge atomic with respect to opening a replacement, and let only the owner
    /// of the registered entry purge it (concurrent close/shutdown may join twice).
    fn release_entry(&self, key: &str, entry: &Arc<Entry>) -> Result<(), String> {
        {
            let mut state = entry
                .session
                .lock()
                .map_err(|_| "MCP connection lock poisoned")?;
            state.session.take();
            state.config.take();
        }
        // Keep the stopped entry registered while joining: an old notification
        // must not resolve this key to a newly opened connection and read from it.
        retire_entry(entry);
        let mut map = self
            .sessions
            .lock()
            .map_err(|_| "MCP session registry poisoned")?;
        if map
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, entry))
        {
            self.clear_subscriptions(key);
            map.remove(key);
        }
        Ok(())
    }

    fn target(
        &self,
        context: &RequestContext,
        params: &Value,
    ) -> Result<(String, String, McpServerConfig, u64), String> {
        self.resolve_target(context, params, true)
    }

    fn resolve_target(
        &self,
        context: &RequestContext,
        params: &Value,
        allocate: bool,
    ) -> Result<(String, String, McpServerConfig, u64), String> {
        let scope = context.scope.clone();
        let (name, identity, mut config) = if params.get("configPath").is_some() {
            if params.get("url").is_some() {
                return Err("specify configPath/server or url, not both".into());
            }
            let name = string_param(params, "server")?;
            let (path, config) = load_config(context, params)?;
            let server = config
                .servers
                .get(&name)
                .ok_or_else(|| format!("MCP server {name:?} is not configured"))?
                .clone();
            (name, path.to_string_lossy().into_owned(), server)
        } else {
            if params.get("server").is_some() {
                return Err("server requires configPath".into());
            }
            let url = crate::http::validate_mcp_url(&string_param(params, "url")?)?.to_string();
            let config = McpServerConfig::Http(crate::HttpServer {
                url: url.clone(),
                headers: Default::default(),
                enabled: true,
                timeout: 60,
                exposure: Default::default(),
            });
            ("http".into(), url, config)
        };
        let (enabled, default_timeout) = match &config {
            McpServerConfig::Stdio(s) => (s.enabled, s.timeout),
            McpServerConfig::Http(s) => (s.enabled, s.timeout),
        };
        let timeout = match params.get("timeoutSeconds") {
            None => default_timeout.clamp(1, 300),
            Some(value) => value
                .as_u64()
                .filter(|v| (1..=300).contains(v))
                .ok_or("timeoutSeconds must be an integer from 1 to 300")?,
        };
        let key = if self.auth.is_some() || self.sampling.is_some() {
            let cwd = std::fs::canonicalize(&context.cwd)
                .map_err(|_| "MCP authentication cwd unavailable")?;
            let base = serde_json::to_string(&(&scope, &identity, &name, &cwd, self.namespace))
                .map_err(|_| "invalid auth identity")?;
            let (key, retired) = {
                let mut targets = self
                    .auth_targets
                    .lock()
                    .map_err(|_| "auth identity registry poisoned")?;
                if !allocate || !enabled {
                    // Unknown close is a lookup only, including after config edits.
                    (
                        targets
                            .get(&base)
                            .map(|t| t.key.clone())
                            .unwrap_or_default(),
                        None,
                    )
                } else {
                    let retired = if targets.get(&base).is_some_and(|t| t.config != config) {
                        targets.remove(&base)
                    } else {
                        None
                    };
                    if !targets.contains_key(&base) {
                        if targets.len() >= 128 {
                            return Err("MCP authenticated target limit reached".into());
                        }
                        let generation = AUTH_IDS.fetch_add(1, Ordering::Relaxed);
                        let key = serde_json::to_string(&(
                            &scope,
                            &identity,
                            &name,
                            &cwd,
                            self.namespace,
                            generation,
                        ))
                        .map_err(|_| "invalid auth identity")?;
                        targets.insert(
                            base.clone(),
                            AuthTarget {
                                config: config.clone(),
                                key,
                            },
                        );
                    }
                    (targets[&base].key.clone(), retired)
                }
            };
            // Removing the old record is the retirement linearization point:
            // old resolved keys cannot register a session from this point on.
            // Drain its owner without holding the global identity registry.
            if let Some(retired) = retired {
                self.close_key(&retired.key)?;
            }
            key
        } else {
            serde_json::to_string(&(scope, identity, &name)).map_err(|e| e.to_string())?
        };
        if !enabled && allocate {
            self.close_key(&key)?;
            return Err(format!("MCP server {name:?} is disabled"));
        }
        if let McpServerConfig::Stdio(server) = &mut config {
            // A configured cwd is relative to the config file; otherwise use host project cwd.
            if let Some(cwd) = &server.cwd {
                if Path::new(cwd).is_relative() {
                    let file = config_file(context, params)?;
                    server.cwd = Some(
                        file.parent()
                            .unwrap_or(Path::new("."))
                            .join(cwd)
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            } else {
                server.cwd = Some(context.cwd.clone().to_string_lossy().into_owned());
            }
        }
        Ok((key, name, config, timeout))
    }

    pub fn call(
        &self,
        context: &RequestContext,
        params: &Value,
        control: &Control,
    ) -> Result<Value, String> {
        let tool = string_param(params, "tool")?;
        if tool.trim().is_empty() || tool.len() > 200 {
            return Err("MCP tool must contain 1-200 characters".into());
        }
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !arguments.is_object() {
            return Err("MCP arguments must be an object".into());
        }
        let (key, _, config, timeout) = self.target(context, params)?;
        self.with_session(
            key,
            config,
            timeout,
            control,
            None,
            |session, control, _| {
                let tools = session.tools(control)?;
                if !tools.iter().any(|t| t.name == tool) {
                    return Err(format!("MCP server does not expose tool {tool:?}"));
                }
                let request = tools_call_request(0, &tool, arguments);
                response_result(session.request(
                    "tools/call",
                    request.params.unwrap_or_else(|| json!({})),
                    control,
                )?)
            },
        )
    }

    pub fn list(
        &self,
        context: &RequestContext,
        params: &Value,
        control: &Control,
    ) -> Result<Value, String> {
        control.check()?;
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list");
        if !matches!(action, "list" | "close") {
            return Err("mcp_list action must be list or close".into());
        }
        if params.get("configPath").is_some() && params.get("server").is_none() {
            if action != "list" {
                return Err("close requires a server or url".into());
            }
            let (_, config) = load_config(context, params)?;
            let servers: Vec<Value> = config
                .servers
                .iter()
                .map(|(name, config)| {
                    let (transport, enabled) = match config {
                        McpServerConfig::Stdio(s) => ("stdio", s.enabled),
                        McpServerConfig::Http(s) => ("http", s.enabled),
                    };
                    json!({"server":name, "transport":transport, "enabled":enabled})
                })
                .collect();
            return Ok(json!({"servers":servers}));
        }
        let (key, name, config, timeout) =
            self.resolve_target(context, params, action != "close")?;
        if action == "close" {
            self.close_key(&key)?;
            return Ok(json!({"server":name,"closed":true}));
        }
        self.with_session(
        key,
        config,
        timeout,
        control,
        None,
        |session, control, _| {
            let tools = session.tools(control)?;
            Ok(
                json!({"server":name, "serverInfo":{"name":session.info.name,"version":session.info.version},"protocolVersion":session.info.protocol_version,"capabilities":session.info.capabilities,"tools":tools}),
            )
        },
    )
    }

    pub fn call_from_config(
        &self,
        config_path: &str,
        server_name: &str,
        tool_name: &str,
        arguments: Value,
    ) -> Result<String, String> {
        self.call_from_config_with_timeout(config_path, server_name, tool_name, arguments, None)
    }

    pub fn call_from_config_with_timeout(
        &self,
        config_path: &str,
        server_name: &str,
        tool_name: &str,
        arguments: Value,
        timeout: Option<u64>,
    ) -> Result<String, String> {
        let mut params = json!({"configPath":config_path,"server":server_name,"tool":tool_name,"arguments":arguments});
        if let Some(timeout) = timeout {
            params["timeoutSeconds"] = timeout.into();
        }
        let result = self.call(&RequestContext::default(), &params, &Control::default())?;
        crate::connection::check_tool_result(&result)?;
        Ok(crate::call_result_text(&result))
    }

    pub fn discover_from_config(
        &self,
        config_path: &str,
        server_name: &str,
    ) -> Result<DiscoveredServer, String> {
        let value = self.list(
            &RequestContext::default(),
            &json!({"configPath":config_path,"server":server_name}),
            &Control::default(),
        )?;
        Ok(DiscoveredServer {
            name: server_name.into(),
            info: ServerInfo {
                name: value["serverInfo"]["name"]
                    .as_str()
                    .unwrap_or("unknown")
                    .into(),
                version: value["serverInfo"]["version"]
                    .as_str()
                    .unwrap_or("unknown")
                    .into(),
                protocol_version: value["protocolVersion"].as_str().unwrap_or("").into(),
                capabilities: value["capabilities"].clone(),
            },
            tools: serde_json::from_value(value["tools"].clone()).map_err(|e| e.to_string())?,
        })
    }

    pub fn close_from_config(&self, config_path: &str, server_name: &str) -> Result<(), String> {
        self.list(
            &RequestContext::default(),
            &json!({"configPath":config_path,"server":server_name,"action":"close"}),
            &Control::default(),
        )
        .map(|_| ())
    }

    /// `mcp_resource`'s actions against a configured server (`list`/`read`/
    /// `subscribe`/`unsubscribe`), for embedders and integration tests.
    pub fn resources_from_config(
        &self,
        config_path: &str,
        server_name: &str,
        action: &str,
        uri: Option<&str>,
    ) -> Result<Value, String> {
        let mut params = json!({"configPath":config_path,"server":server_name,"action":action});
        if let Some(uri) = uri {
            params["uri"] = json!(uri);
        }
        self.resources(&RequestContext::default(), &params, &Control::default())
    }

    /// Drain the resource updates queued for `scope` — the same queue the `TurnEnd`
    /// injection path consumes. `None` drains everything (a host with no session id).
    pub fn take_resource_updates(&self, scope: Option<&str>) -> Vec<ResourceUpdate> {
        self.take_pending_updates(scope)
    }
}

fn string_param(params: &Value, name: &str) -> Result<String, String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("missing {name}"))
}

#[path = "catalog.rs"]
mod catalog;

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "runtime_auth_lifecycle_tests.rs"]
mod auth_lifecycle_tests;
