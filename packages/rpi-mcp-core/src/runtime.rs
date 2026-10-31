//! Persistent connections scoped to the host session and configuration identity.
use crate::configured::{validate_tool_name, Catalog};
use crate::connection::{
    initialize_request, initialized_notification, parse_initialize, response_result,
    tools_call_request,
};
use crate::control::{Control, Deadline};
use crate::http::HttpTransport;
use crate::{
    ConfiguredStdio, DiscoveryLimits, McpConfig, McpServerConfig, Request, Response, ServerInfo,
    StdioTransport, Tool,
};
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
    Http(HttpTransport),
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
    // Opaque identity only: targets never own a Session, Entry, or runtime.
    generation: Arc<()>,
}
impl Session {
    fn start(
        config: &McpServerConfig,
        timeout: u64,
        control: &Control,
        notifications: &SyncSender<Value>,
        identity: &ClientIdentity,
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
            McpServerConfig::Http(s) => {
                Transport::Http(HttpTransport::with_user_agent(s, &identity.user_agent())?)
            }
        };
        transport.timeout(timeout);
        let info = parse_initialize(transport.request(
            &initialize_request(1, &identity.name, &identity.version),
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
            generation: Arc::new(()),
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
        let response = self
            .transport
            .request(&Request::new(id, method, Some(params)), control);
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
enum SessionAccess<'a> {
    Legacy,
    Discover,
    Existing(&'a Arc<()>),
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

/// Identity supplied by the embedding adapter, independent of the core crate version.
#[derive(Debug, Clone)]
pub struct ClientIdentity {
    pub name: String,
    pub version: String,
}
impl ClientIdentity {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }
    pub fn user_agent(&self) -> String {
        format!("{}/{}", self.name, self.version)
    }
}
impl Default for ClientIdentity {
    fn default() -> Self {
        Self::new("rpi-mcp-core", env!("CARGO_PKG_VERSION"))
    }
}

/// Request authority is explicit; the core never interprets SDK host context.
#[derive(Debug, Clone)]
pub struct CoreRequestContext {
    pub scope: String,
    pub cwd: PathBuf,
}
impl Default for CoreRequestContext {
    fn default() -> Self {
        Self {
            scope: "library".into(),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }
}

#[derive(Default)]
struct State {
    sessions: Mutex<Sessions>,
    subscriptions: Mutex<Subscriptions>,
    pending: Mutex<Vec<QueuedResourceUpdate>>,
    identity: ClientIdentity,
}

/// Caller-owned runtime. Drop cancels and joins its connections and owned workers.
/// Use `Arc<McpRuntime>` when sharing ownership; this type is deliberately not Clone.
pub struct McpRuntime {
    state: Arc<State>,
}
impl Default for McpRuntime {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for McpRuntime {
    fn drop(&mut self) {
        self.shutdown_connections();
    }
}

/// Neutral delivery lease; retry validates subscription lifetime and read revision.
/// The weak reference does not own the runtime.
pub struct ResourceUpdateBatch {
    updates: Vec<QueuedResourceUpdate>,
    state: Weak<State>,
}
impl ResourceUpdateBatch {
    pub fn updates(&self) -> impl Iterator<Item = &ResourceUpdate> {
        self.updates.iter().map(|q| &q.update)
    }
    pub fn is_empty(&self) -> bool {
        self.updates.is_empty()
    }
    pub fn retry(self) {
        if let Some(state) = self.state.upgrade() {
            state.restore_pending_batch(self.updates);
        }
    }
}
impl McpRuntime {
    pub fn new() -> Self {
        Self::with_identity(ClientIdentity::default())
    }
    pub fn with_identity(identity: ClientIdentity) -> Self {
        Self {
            state: Arc::new(State {
                identity,
                ..State::default()
            }),
        }
    }
    pub fn call(
        &self,
        context: &CoreRequestContext,
        params: &Value,
        control: &Control,
    ) -> Result<Value, String> {
        self.state.call(context, params, control)
    }
    pub fn list(
        &self,
        context: &CoreRequestContext,
        params: &Value,
        control: &Control,
    ) -> Result<Value, String> {
        self.state.list(context, params, control)
    }
    pub fn resources(
        &self,
        context: &CoreRequestContext,
        params: &Value,
        control: &Control,
    ) -> Result<Value, String> {
        self.state.resources(context, params, control)
    }
    /// Discover an in-memory target, admitting calls only after every page validates.
    pub fn discover_stdio(
        &self,
        target: &ConfiguredStdio,
        limits: DiscoveryLimits,
        control: &Control,
    ) -> Result<DiscoveredServer, String> {
        let deadline = Deadline::new(target.config.timeout, "stdio target lock", control);
        let mut admission = lock_until(&target.generation, &deadline)?;
        // A failed rediscovery must not leave the old catalog callable.
        *admission = None;
        let mut catalog = Catalog::new(limits)?;
        self.state.with_session_access(
            target.key.clone(),
            McpServerConfig::Stdio(target.config.clone()),
            target.config.timeout,
            control,
            None,
            SessionAccess::Discover,
            |session, control, _| {
                let mut cursor = None;
                loop {
                    control.check()?;
                    let params = cursor
                        .as_ref()
                        .map(|c| json!({"cursor":c}))
                        .unwrap_or_else(|| json!({}));
                    let result = session
                        .request("tools/list", params, control)
                        .and_then(response_result)
                        .map_err(|_| "MCP stdio tools/list failed")?;
                    cursor = catalog.page(&result)?;
                    if cursor.is_none() {
                        break;
                    }
                }
                control.check()?;
                *admission = Some(session.generation.clone());
                Ok(DiscoveredServer {
                    name: target.name.clone(),
                    info: session.info.clone(),
                    tools: catalog.tools,
                })
            },
        )
    }

    /// Call only the connection admitted by discovery. Never reconnect, requery,
    /// replay, or project the raw MCP result (including `isError`).
    pub fn call_stdio(
        &self,
        target: &ConfiguredStdio,
        tool: &str,
        arguments: Value,
        control: &Control,
    ) -> Result<Value, String> {
        control.check()?;
        validate_tool_name(tool)?;
        if !arguments.is_object() {
            return Err("MCP arguments must be an object".into());
        }
        let deadline = Deadline::new(target.config.timeout, "stdio target lock", control);
        let admission = lock_until(&target.generation, &deadline)?;
        let generation = admission
            .as_ref()
            .ok_or("MCP stdio target requires discovery")?;
        self.state.with_session_access(
            target.key.clone(),
            McpServerConfig::Stdio(target.config.clone()),
            target.config.timeout,
            control,
            None,
            SessionAccess::Existing(generation),
            |session, control, _| {
                let request = tools_call_request(0, tool, arguments);
                session
                    .request(
                        "tools/call",
                        request.params.unwrap_or_else(|| json!({})),
                        control,
                    )
                    .and_then(response_result)
                    .map_err(|_| "MCP stdio tools/call failed".into())
            },
        )
    }

    pub fn shutdown_connections(&self) {
        self.state.shutdown_connections();
    }
    pub fn take_resource_update_batch(&self, scope: Option<&str>) -> ResourceUpdateBatch {
        self.state.take_pending_batch(scope)
    }
    pub fn take_resource_updates(&self, scope: Option<&str>) -> Vec<ResourceUpdate> {
        self.state.take_pending_updates(scope)
    }
    pub fn call_from_config(
        &self,
        config_path: &str,
        server_name: &str,
        tool_name: &str,
        arguments: Value,
    ) -> Result<String, String> {
        self.state
            .call_from_config(config_path, server_name, tool_name, arguments)
    }
    pub fn call_from_config_with_timeout(
        &self,
        config_path: &str,
        server_name: &str,
        tool_name: &str,
        arguments: Value,
        timeout: Option<u64>,
    ) -> Result<String, String> {
        self.state.call_from_config_with_timeout(
            config_path,
            server_name,
            tool_name,
            arguments,
            timeout,
        )
    }
    pub fn discover_from_config(
        &self,
        config_path: &str,
        server_name: &str,
    ) -> Result<DiscoveredServer, String> {
        self.state.discover_from_config(config_path, server_name)
    }
    pub fn close_from_config(&self, config_path: &str, server_name: &str) -> Result<(), String> {
        self.state.close_from_config(config_path, server_name)
    }
    pub fn resources_from_config(
        &self,
        config_path: &str,
        server_name: &str,
        action: &str,
        uri: Option<&str>,
    ) -> Result<Value, String> {
        self.state
            .resources_from_config(config_path, server_name, action, uri)
    }
}

impl State {
    fn sessions(&self) -> &Mutex<Sessions> {
        &self.sessions
    }
    fn subscriptions(&self) -> &Mutex<Subscriptions> {
        &self.subscriptions
    }
    fn pending(&self) -> &Mutex<Vec<QueuedResourceUpdate>> {
        &self.pending
    }

    pub fn resources_from_config(
        self: &Arc<Self>,
        config_path: &str,
        server_name: &str,
        action: &str,
        uri: Option<&str>,
    ) -> Result<Value, String> {
        let mut params = json!({"configPath":config_path,"server":server_name,"action":action});
        if let Some(uri) = uri {
            params["uri"] = json!(uri);
        }
        self.resources(&CoreRequestContext::default(), &params, &Control::default())
    }

    pub fn close_from_config(
        self: &Arc<Self>,
        config_path: &str,
        server_name: &str,
    ) -> Result<(), String> {
        self.list(
            &CoreRequestContext::default(),
            &json!({"configPath":config_path,"server":server_name,"action":"close"}),
            &Control::default(),
        )
        .map(|_| ())
    }

    pub fn discover_from_config(
        self: &Arc<Self>,
        config_path: &str,
        server_name: &str,
    ) -> Result<DiscoveredServer, String> {
        let value = self.list(
            &CoreRequestContext::default(),
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
            },
            tools: serde_json::from_value(value["tools"].clone()).map_err(|e| e.to_string())?,
        })
    }

    pub fn call_from_config_with_timeout(
        self: &Arc<Self>,
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
        let result = self.call(&CoreRequestContext::default(), &params, &Control::default())?;
        crate::connection::check_tool_result(&result)?;
        Ok(crate::call_result_text(&result))
    }

    pub fn call_from_config(
        self: &Arc<Self>,
        config_path: &str,
        server_name: &str,
        tool_name: &str,
        arguments: Value,
    ) -> Result<String, String> {
        self.call_from_config_with_timeout(config_path, server_name, tool_name, arguments, None)
    }

    pub(crate) fn list(
        self: &Arc<Self>,
        context: &CoreRequestContext,
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
            let (_, config) = self.load_config(context, params)?;
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
        let (key, name, config, timeout) = self.target(context, params)?;
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
                json!({"server":name, "serverInfo":{"name":session.info.name,"version":session.info.version},"protocolVersion":session.info.protocol_version,"tools":tools}),
            )
        },
    )
    }

    pub(crate) fn call(
        self: &Arc<Self>,
        context: &CoreRequestContext,
        params: &Value,
        control: &Control,
    ) -> Result<Value, String> {
        let tool = crate::kit::string_param(params, "tool")?;
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

    fn target(
        self: &Arc<Self>,
        context: &CoreRequestContext,
        params: &Value,
    ) -> Result<(String, String, McpServerConfig, u64), String> {
        let scope = context.scope.clone();
        let (name, identity, mut config) = if params.get("configPath").is_some() {
            if params.get("url").is_some() {
                return Err("specify configPath/server or url, not both".into());
            }
            let name = crate::kit::string_param(params, "server")?;
            let (path, config) = self.load_config(context, params)?;
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
            let url = crate::http::validate_mcp_url(&crate::kit::string_param(params, "url")?)?
                .to_string();
            let config = McpServerConfig::Http(crate::HttpServer {
                url: url.clone(),
                headers: Default::default(),
                enabled: true,
                timeout: 60,
                exposure: Default::default(),
            });
            ("http".into(), url, config)
        };
        let key = serde_json::to_string(&(scope, identity, &name)).map_err(|e| e.to_string())?;
        let (enabled, default_timeout) = match &config {
            McpServerConfig::Stdio(s) => (s.enabled, s.timeout),
            McpServerConfig::Http(s) => (s.enabled, s.timeout),
        };
        if !enabled {
            self.close_key(&key)?;
            return Err(format!("MCP server {name:?} is disabled"));
        }
        if let McpServerConfig::Stdio(server) = &mut config {
            // A configured cwd is relative to the config file; otherwise use host project cwd.
            if let Some(cwd) = &server.cwd {
                if Path::new(cwd).is_relative() {
                    let file = self.config_file(context, params)?;
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
        let timeout = match params.get("timeoutSeconds") {
            None => default_timeout.clamp(1, 300),
            Some(value) => value
                .as_u64()
                .filter(|v| (1..=300).contains(v))
                .ok_or("timeoutSeconds must be an integer from 1 to 300")?,
        };
        Ok((key, name, config, timeout))
    }

    fn load_config(
        self: &Arc<Self>,
        context: &CoreRequestContext,
        params: &Value,
    ) -> Result<(PathBuf, McpConfig), String> {
        let path = self.config_file(context, params)?;
        let text = std::fs::read_to_string(&path).map_err(|e| format!("read MCP config: {e}"))?;
        Ok((path, McpConfig::parse(&text)?))
    }

    fn config_file(
        self: &Arc<Self>,
        context: &CoreRequestContext,
        params: &Value,
    ) -> Result<PathBuf, String> {
        let path = params
            .get("configPath")
            .and_then(Value::as_str)
            .ok_or("missing configPath")?;
        std::fs::canonicalize(context.cwd.clone().join(path))
            .map_err(|e| format!("read MCP config {path:?}: {e}"))
    }

    fn release_entry(self: &Arc<Self>, key: &str, entry: &Arc<Entry>) -> Result<(), String> {
        {
            let mut state = entry
                .session
                .lock()
                .map_err(|_| "MCP connection lock poisoned")?;
            state.session.take();
        }
        // Keep the stopped entry registered while joining: an old notification
        // must not resolve this key to a newly opened connection and read from it.
        self.retire_entry(entry);
        let mut map = self
            .sessions()
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

    fn close_key(self: &Arc<Self>, key: &str) -> Result<(), String> {
        let entry = {
            let map = self
                .sessions()
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

    pub fn shutdown_connections(self: &Arc<Self>) {
        let entries = if let Ok(map) = self.sessions().lock() {
            // Keep stopped entries registered until their active operations have
            // released the connection lock. New calls cannot reopen the same key
            // while its old subscription lifetimes are still being retired.
            for entry in map.values() {
                entry.stop.cancel();
            }
            map.clone()
        } else {
            return;
        };
        for (key, entry) in entries {
            let _ = self.release_entry(&key, &entry);
        }
    }

    fn retire_entry(self: &Arc<Self>, entry: &Arc<Entry>) {
        entry.stop.cancel();
        // Serialize joins too: another close/shutdown must not remove this entry
        // and allow a replacement while the first caller is still joining it.
        if let Ok(mut slot) = entry.dispatcher.lock() {
            if let Some(handle) = slot.take() {
                let _ = handle.join();
            }
        }
    }

    fn with_session<T>(
        self: &Arc<Self>,
        key: String,
        config: McpServerConfig,
        timeout: u64,
        control: &Control,
        skip_restore_uri: Option<&str>,
        run: impl FnOnce(&mut Session, &Control, bool) -> Result<T, String>,
    ) -> Result<T, String> {
        self.with_session_access(
            key,
            config,
            timeout,
            control,
            skip_restore_uri,
            SessionAccess::Legacy,
            run,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_session_access<T>(
        self: &Arc<Self>,
        key: String,
        config: McpServerConfig,
        timeout: u64,
        control: &Control,
        skip_restore_uri: Option<&str>,
        access: SessionAccess<'_>,
        run: impl FnOnce(&mut Session, &Control, bool) -> Result<T, String>,
    ) -> Result<T, String> {
        control.check()?;
        let entry = {
            let mut sessions = self
                .sessions()
                .lock()
                .map_err(|_| "MCP session registry poisoned")?;
            // Existing-only access must fail before creating any entry/worker.
            if matches!(access, SessionAccess::Existing(_)) && !sessions.contains_key(&key) {
                return Err(
                    "MCP stdio discovery connection is retired; rediscover required".into(),
                );
            }
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
        if let SessionAccess::Existing(expected) = &access {
            // Check under the same lock used for config replacement/retirement.
            // A stale target cannot mutate config, restart, or reach the callback.
            if state.config.as_ref() != Some(&config)
                || !state.session.as_ref().is_some_and(|session| {
                    !session.failed && Arc::ptr_eq(&session.generation, expected)
                })
            {
                return Err("MCP stdio discovery connection is stale; rediscover required".into());
            }
        }
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
            let mut session = Session::start(
                &config,
                timeout,
                &control,
                &entry.notifications,
                &self.identity,
            )
            .map_err(|error| match access {
                SessionAccess::Legacy => error,
                _ => "MCP stdio initialization failed".into(),
            })?;
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
        let result = run(session, &control, new_connection);
        if session.failed {
            state.session.take();
        } // Do not retry tool calls: they may have side effects.
        result
    }

    pub(crate) fn resources(
        self: &Arc<Self>,
        context: &CoreRequestContext,
        params: &Value,
        control: &Control,
    ) -> Result<Value, String> {
        control.check()?;
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list");
        if !matches!(action, "list" | "read" | "subscribe" | "unsubscribe") {
            return Err("mcp_resource action must be list, read, subscribe or unsubscribe".into());
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

        let uri = crate::kit::string_param(params, "uri")?;
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

    fn read_resource_by_key(self: &Arc<Self>, key: &str, uri: &str) -> Result<String, String> {
        let entry = self
            .sessions()
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

    fn dispatch_notification_with(
        self: &Arc<Self>,
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
            let map = self
                .subscriptions()
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if !map
                .get(key)
                .and_then(|uris| uris.get(&uri))
                .is_some_and(|current| Arc::ptr_eq(current, &token))
            {
                return;
            }
            // Keep the subscription lock through enqueue, synchronized with retirement.
            let mut pending = self.pending().lock().unwrap_or_else(|p| p.into_inner());
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

    fn dispatch_notification(self: &Arc<Self>, key: &str, value: &Value) {
        self.dispatch_notification_with(key, value, |key, uri| self.read_resource_by_key(key, uri));
    }

    fn spawn_dispatcher(
        self: &Arc<Self>,
        key: String,
        receiver: Receiver<Value>,
        stop: Control,
    ) -> JoinHandle<()> {
        let state = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("mcp-notify".into())
            .spawn(move || loop {
                if stop.check().is_err() {
                    break;
                }
                match receiver.recv_timeout(Duration::from_millis(100)) {
                    Ok(value) => {
                        let Some(state) = state.upgrade() else { break };
                        state.dispatch_notification(&key, &value);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            })
            .expect("spawn MCP notification dispatcher")
    }

    fn restore_subscriptions(
        self: &Arc<Self>,
        key: &str,
        session: &mut Session,
        control: &Control,
        skip_uri: Option<&str>,
    ) -> Result<(), String> {
        let uris = {
            let mut map = self
                .subscriptions()
                .lock()
                .unwrap_or_else(|p| p.into_inner());
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

    fn clear_subscriptions(self: &Arc<Self>, key: &str) {
        let mut map = self
            .subscriptions()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        map.remove(key);
        self.clear_pending_for_key(key);
    }

    fn remove_subscription(
        self: &Arc<Self>,
        key: &str,
        uri: &str,
        expected: Option<&Subscription>,
    ) {
        let mut map = self
            .subscriptions()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
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

    fn subscription(self: &Arc<Self>, key: &str, uri: &str) -> Option<Subscription> {
        self.subscriptions()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
            .and_then(|uris| uris.get(uri))
            .cloned()
    }

    fn add_subscription(self: &Arc<Self>, key: &str, uri: &str) -> Subscription {
        let token = Arc::new(AtomicU64::new(0));
        let mut map = self
            .subscriptions()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        map.entry(key.to_string())
            .or_default()
            .insert(uri.to_string(), token.clone());
        // Rotation starts a new delivery lifetime. Purge queued content under the
        // same lock used by enqueue and retirement, as well as invalidating reads.
        self.clear_pending_for_uri(key, uri);
        token
    }

    fn clear_pending_for_uri(self: &Arc<Self>, key: &str, uri: &str) {
        self.pending()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|queued| !(queued.update.key == key && queued.update.uri == uri));
    }

    fn clear_pending_for_key(self: &Arc<Self>, key: &str) {
        self.pending()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|queued| queued.update.key != key);
    }

    fn restore_pending_batch(self: &Arc<Self>, updates: Vec<QueuedResourceUpdate>) {
        // Retirement and enqueue use this same lock order. A failed append must
        // never resurrect an unsubscribed URI, a closed connection or an older
        // subscription lifetime. A newer read supersedes an older failed delivery,
        // even if another flush has already drained and delivered that newer read.
        let map = self
            .subscriptions()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut pending = self.pending().lock().unwrap_or_else(|p| p.into_inner());
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

    fn take_pending_batch(self: &Arc<Self>, scope: Option<&str>) -> ResourceUpdateBatch {
        let drained =
            std::mem::take(&mut *self.pending().lock().unwrap_or_else(|p| p.into_inner()));
        let updates = match scope {
            Some(scope) => drained
                .into_iter()
                .filter(|queued| queued.update.scope == scope)
                .collect(),
            None => drained,
        };
        ResourceUpdateBatch {
            updates,
            state: Arc::downgrade(self),
        }
    }

    pub(crate) fn take_pending_updates(
        self: &Arc<Self>,
        scope: Option<&str>,
    ) -> Vec<ResourceUpdate> {
        self.take_pending_batch(scope)
            .updates
            .into_iter()
            .map(|queued| queued.update)
            .collect()
    }
}

fn scope_of_key(key: &str) -> String {
    serde_json::from_str::<Value>(key)
        .ok()
        .and_then(|value| value.get(0).and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default()
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn typed_target() -> ConfiguredStdio {
        let config = serde_json::from_value(json!({
            "command":"must-not-be-spawned-core-unit-test", "timeout":1
        }))
        .unwrap();
        ConfiguredStdio::new(&CoreRequestContext::default(), "typed", config)
            .ok()
            .unwrap()
    }

    #[test]
    fn typed_calls_require_discovery_and_never_create_registry_entries() {
        let runtime = McpRuntime::new();
        let target = typed_target();
        let control = Control::default();
        assert!(runtime
            .call_stdio(&target, "any tool/中文", json!({}), &control)
            .unwrap_err()
            .contains("requires discovery"));
        // A token from another runtime (or a retired registry key) is not authority
        // to open a transport. The invalid executable must never be attempted.
        *target.generation.lock().unwrap() = Some(Arc::new(()));
        assert!(runtime
            .call_stdio(&target, "any", json!({}), &control)
            .unwrap_err()
            .contains("retired"));
        assert!(runtime.state.sessions.lock().unwrap().is_empty());
        assert!(runtime
            .call_stdio(&target, "any", json!([]), &control)
            .unwrap_err()
            .contains("object"));
        control.cancel();
        assert!(runtime
            .call_stdio(&target, "any", json!({}), &control)
            .unwrap_err()
            .contains("cancelled"));
    }

    #[test]
    fn typed_failed_or_changed_session_cannot_reconnect_or_mutate_config() {
        let runtime = McpRuntime::new();
        let target = typed_target();
        *target.generation.lock().unwrap() = Some(Arc::new(()));
        let (notifications, _receiver) = mpsc::sync_channel(1);
        let mut previous_config = target.config.clone();
        previous_config.command = "different-command".into();
        let entry = Arc::new(Entry {
            session: Mutex::new(ConnectionState {
                config: Some(McpServerConfig::Stdio(previous_config.clone())),
                session: None,
            }),
            stop: Control::default(),
            notifications,
            dispatcher: Mutex::new(None),
        });
        runtime
            .state
            .sessions
            .lock()
            .unwrap()
            .insert(target.key.clone(), entry.clone());
        for config in [previous_config.clone(), target.config.clone()] {
            entry.session.lock().unwrap().config = Some(McpServerConfig::Stdio(config.clone()));
            assert!(runtime
                .call_stdio(&target, "any", json!({}), &Control::default())
                .unwrap_err()
                .contains("stale"));
            let state = entry.session.lock().unwrap();
            assert_eq!(state.config.as_ref(), Some(&McpServerConfig::Stdio(config)));
            assert!(state.session.is_none());
        }
        runtime.shutdown_connections();
        assert!(runtime
            .call_stdio(&target, "any", json!({}), &Control::default())
            .unwrap_err()
            .contains("retired"));
    }

    #[test]
    fn typed_target_does_not_retain_its_runtime_and_clone_invalidation_is_shared() {
        let runtime = McpRuntime::new();
        let state = Arc::downgrade(&runtime.state);
        let target = typed_target();
        let clone = target.clone();
        *target.generation.lock().unwrap() = Some(Arc::new(()));
        // Reject invalid discovery limits before touching the transport, and
        // invalidate all cloned admissions rather than leaving an old catalog live.
        assert!(runtime
            .discover_stdio(
                &target,
                DiscoveryLimits {
                    max_tools: 1,
                    max_catalog_bytes: 0,
                },
                &Control::default()
            )
            .is_err());
        assert!(clone.generation.lock().unwrap().is_none());
        drop(runtime);
        assert!(state.upgrade().is_none());
        drop((target, clone));
    }

    #[test]
    fn resource_text_flattens_text_and_notes_blobs() {
        let text = resource_text(&json!({"contents": [
            {"uri": "res://a", "mimeType": "text/plain", "text": "hello"},
            {"uri": "res://b", "blob": "QUJD"}]}));
        assert!(text.contains("res://a"));
        assert!(text.contains("hello"));
        assert!(text.contains("[blob, 4 base64 chars]"), "got: {text}");
    }

    #[test]
    fn resource_text_bounds_regular_and_unexpected_responses() {
        let huge = "中".repeat(MAX_RESOURCE_TEXT);
        for response in [
            json!({"contents":[{"uri":"mem://large","text":huge}]}),
            json!({"unexpected":huge}),
        ] {
            let text = resource_text(&response);
            assert!(
                text.len() <= MAX_RESOURCE_TEXT + "\n[truncated]".len(),
                "unbounded resource response: {} bytes",
                text.len()
            );
            assert!(text.ends_with("\n[truncated]"));
        }
    }

    fn queue(runtime: &Arc<State>, scope: &str, key: &str, uri: &str, text: &str) {
        let token = runtime.subscription(key, uri).unwrap_or_default();
        runtime
            .pending()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(QueuedResourceUpdate::new(
                ResourceUpdate {
                    scope: scope.into(),
                    key: key.into(),
                    uri: uri.into(),
                    text: text.into(),
                },
                token,
            ));
    }

    fn clear_queue(runtime: &Arc<State>) {
        runtime
            .pending()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    #[test]
    fn a_new_subscription_lifetime_drops_the_previous_queued_content() {
        let runtime = Arc::new(State::default());
        clear_queue(&runtime);
        const KEY: &str = r#"["session:renew","config","server"]"#;
        const URI: &str = "mem://renew";
        runtime.add_subscription(KEY, URI);
        queue(&runtime, "session:renew", KEY, URI, "obsolete content");
        runtime.add_subscription(KEY, URI);
        let obsolete = runtime.take_pending_updates(None);
        queue(&runtime, "session:renew", KEY, URI, "fresh content");
        let fresh = runtime.take_pending_updates(None);
        runtime.clear_subscriptions(KEY);
        assert!(
            obsolete.is_empty(),
            "old subscription content survived renewal: {obsolete:?}"
        );
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].text, "fresh content");
    }

    fn assert_teardown_retires_late_subscription(shutdown: bool) {
        let runtime = Arc::new(State::default());
        clear_queue(&runtime);
        const KEY: &str = r#"["session:closing","config","server"]"#;
        const URI: &str = "mem://closing";
        let (notifications, _receiver) = mpsc::sync_channel(1);
        let entry = Arc::new(Entry {
            session: Mutex::new(ConnectionState::default()),
            stop: Control::default(),
            notifications,
            dispatcher: Mutex::new(None),
        });
        runtime
            .sessions()
            .lock()
            .unwrap()
            .insert(KEY.into(), entry.clone());
        // Subscribe mutates its local lifetime under this connection lock,
        // before its RPC observes cancellation. Pause it immediately before
        // that mutation, then start teardown on another thread.
        let state = entry.session.lock().unwrap();
        let worker_runtime = runtime.clone();
        let closing = std::thread::spawn(move || {
            let runtime = worker_runtime;
            if shutdown {
                runtime.shutdown_connections();
            } else {
                runtime.close_key(KEY).unwrap();
            }
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while entry.stop.check().is_ok() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let cancelled = entry.stop.check().is_err();
        runtime.add_subscription(KEY, URI);
        queue(&runtime, "session:closing", KEY, URI, "late content");
        drop(state);
        closing.join().unwrap();
        let retained = runtime.subscription(KEY, URI).is_some();
        let updates = runtime.take_pending_updates(None);
        runtime.clear_subscriptions(KEY);
        assert!(cancelled, "teardown did not cancel the active operation");
        assert!(
            !retained,
            "teardown left an in-flight subscription registered"
        );
        assert!(
            updates.is_empty(),
            "teardown left queued content: {updates:?}"
        );
        assert!(!runtime.sessions().lock().unwrap().contains_key(KEY));
    }

    #[test]
    fn close_retires_subscriptions_after_in_flight_operations_exit() {
        assert_teardown_retires_late_subscription(false);
    }

    #[test]
    fn closing_connection_stays_registered_until_its_dispatcher_exits() {
        let runtime = Arc::new(State::default());
        const KEY: &str = r#"["session:dispatcher-close","config","server"]"#;
        let stop = Control::default();
        let worker_stop = stop.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let dispatcher = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while worker_stop.check().is_ok() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            ready_tx.send(()).unwrap();
            // Model a notification already being processed when close begins.
            let _ = resume_rx.recv_timeout(Duration::from_secs(3));
        });
        let (notifications, _receiver) = mpsc::sync_channel(1);
        let entry = Arc::new(Entry {
            session: Mutex::new(ConnectionState::default()),
            stop,
            notifications,
            dispatcher: Mutex::new(Some(dispatcher)),
        });
        runtime
            .sessions()
            .lock()
            .unwrap()
            .insert(KEY.into(), entry.clone());
        let worker_runtime = runtime.clone();
        let closing = std::thread::spawn(move || worker_runtime.close_key(KEY));
        let ready = ready_rx.recv_timeout(Duration::from_secs(3));
        // Removing the key early lets a new call replace it while the old
        // dispatcher can still resolve it in read_resource_by_key.
        let retained = runtime
            .sessions()
            .lock()
            .unwrap()
            .get(KEY)
            .is_some_and(|current| Arc::ptr_eq(current, &entry));
        let _ = resume_tx.send(());
        closing.join().unwrap().unwrap();
        assert!(ready.is_ok(), "dispatcher fixture did not observe close");
        assert!(
            retained,
            "a replacement could open before the old dispatcher exited"
        );
        assert!(!runtime.sessions().lock().unwrap().contains_key(KEY));
    }

    #[test]
    fn shutdown_retires_subscriptions_after_in_flight_operations_exit() {
        // Shutdown intentionally affects every registry entry. Isolate this
        // test from parallel HTTP lifecycle tests that also use the registry.
        const CHILD: &str = "RPI_MCP_SHUTDOWN_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            assert_teardown_retires_late_subscription(true);
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime::tests::shutdown_retires_subscriptions_after_in_flight_operations_exit",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn repeated_retirement_cannot_clear_a_replacement_connections_subscriptions() {
        let runtime = Arc::new(State::default());
        clear_queue(&runtime);
        const KEY: &str = r#"["session:replacement","config","server"]"#;
        const URI: &str = "mem://replacement";
        let make_entry = || {
            let (notifications, _receiver) = mpsc::sync_channel(1);
            Arc::new(Entry {
                session: Mutex::new(ConnectionState::default()),
                stop: Control::default(),
                notifications,
                dispatcher: Mutex::new(None),
            })
        };
        let old = make_entry();
        runtime
            .sessions()
            .lock()
            .unwrap()
            .insert(KEY.into(), old.clone());
        runtime.close_key(KEY).unwrap();
        let current = make_entry();
        runtime
            .sessions()
            .lock()
            .unwrap()
            .insert(KEY.into(), current.clone());
        let token = runtime.add_subscription(KEY, URI);
        queue(&runtime, "session:replacement", KEY, URI, "new content");
        // A concurrent shutdown may have captured the old entry before close
        // finished, then reach its cleanup after the key has been reopened.
        runtime.release_entry(KEY, &old).unwrap();
        let retained = runtime
            .subscription(KEY, URI)
            .is_some_and(|current| Arc::ptr_eq(&current, &token));
        let still_registered = runtime
            .sessions()
            .lock()
            .unwrap()
            .get(KEY)
            .is_some_and(|entry| Arc::ptr_eq(entry, &current));
        let updates = runtime.take_pending_updates(None);
        runtime.close_key(KEY).unwrap();
        assert!(retained, "old cleanup cleared the replacement subscription");
        assert!(
            still_registered,
            "old cleanup removed the replacement connection"
        );
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].text, "new content");
    }

    #[test]
    fn in_flight_read_cannot_enqueue_after_subscription_retirement() {
        let runtime = Arc::new(State::default());
        const KEY: &str = r#"["session:race","config","server"]"#;
        const URI: &str = "mem://race";
        let notification = json!({"method":"notifications/resources/updated","params":{"uri":URI}});
        for action in ["unsubscribe", "close-and-reopen", "replace"] {
            clear_queue(&runtime);
            runtime.add_subscription(KEY, URI);
            let (ready_tx, ready_rx) = mpsc::channel();
            let (resume_tx, resume_rx) = mpsc::channel();
            let value = notification.clone();
            let worker_runtime = runtime.clone();
            let worker = std::thread::spawn(move || {
                let runtime = worker_runtime;
                runtime.dispatch_notification_with(KEY, &value, |_, _| {
                    // Pause a successful read just before the dispatcher can
                    // enqueue it, reproducing the original retirement race.
                    ready_tx.send(()).unwrap();
                    resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok("obsolete content".into())
                });
            });
            ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            match action {
                "unsubscribe" => runtime.remove_subscription(KEY, URI, None),
                "close-and-reopen" => {
                    runtime.clear_subscriptions(KEY);
                    runtime.add_subscription(KEY, URI);
                }
                "replace" => {
                    runtime.add_subscription(KEY, URI);
                }
                _ => unreachable!(),
            }
            queue(&runtime, "session:other", "other-key", URI, "unrelated");
            resume_tx.send(()).unwrap();
            worker.join().unwrap();
            let updates = runtime.take_pending_updates(None);
            assert_eq!(
                updates.len(),
                1,
                "{action}: stale read was queued: {updates:?}"
            );
            assert_eq!(updates[0].text, "unrelated");

            if action != "unsubscribe" {
                runtime.dispatch_notification_with(KEY, &notification, |_, _| {
                    Ok("fresh content".into())
                });
                let updates = runtime.take_pending_updates(None);
                assert_eq!(
                    updates.len(),
                    1,
                    "{action}: new subscription stopped working"
                );
                assert_eq!(updates[0].text, "fresh content");
            }
            runtime.clear_subscriptions(KEY);
        }
    }

    #[test]
    fn stale_failure_cannot_remove_a_new_subscription_or_its_updates() {
        let runtime = Arc::new(State::default());
        clear_queue(&runtime);
        let old = runtime.add_subscription("key", "mem://a");
        let current = runtime.add_subscription("key", "mem://a");
        queue(&runtime, "session:a", "key", "mem://a", "fresh");
        runtime.remove_subscription("key", "mem://a", Some(&old));
        assert!(Arc::ptr_eq(
            &runtime.subscription("key", "mem://a").unwrap(),
            &current
        ));
        assert_eq!(runtime.take_pending_updates(None).len(), 1);
        runtime.remove_subscription("key", "mem://a", Some(&current));
        assert!(runtime.subscription("key", "mem://a").is_none());
    }

    /// Regression: the queue was global and `TurnEnd` drained all of it, so
    /// session A's resource contents could land in session B's context.
    #[test]
    fn queued_updates_are_scoped_to_the_session_that_produced_them() {
        let runtime = Arc::new(State::default());
        clear_queue(&runtime);
        queue(&runtime, "session:a", "key-a", "mem://a", "A");
        queue(&runtime, "session:b", "key-b", "mem://b", "B");

        // The host is serving B: only B's update may reach the model.
        let served = runtime.take_pending_updates(Some("session:b"));
        assert_eq!(served.len(), 1, "got {served:?}");
        assert_eq!(served[0].text, "B");
        // A's update was dropped, not deferred into the next drain.
        assert!(runtime.take_pending_updates(Some("session:a")).is_empty());
        clear_queue(&runtime);
    }

    /// A host with no session id (older, or an ephemeral session) still gets
    /// everything rather than nothing.
    #[test]
    fn unscoped_host_still_receives_updates() {
        let runtime = Arc::new(State::default());
        clear_queue(&runtime);
        queue(&runtime, "library", "key-l", "mem://l", "L");
        let served = runtime.take_pending_updates(None);
        assert_eq!(served.len(), 1);
        clear_queue(&runtime);
    }

    /// Closing a connection (or unsubscribing one URI) drops exactly its queued
    /// updates, and leaves other connections' alone.
    #[test]
    fn close_and_unsubscribe_drop_only_their_own_updates() {
        let runtime = Arc::new(State::default());
        clear_queue(&runtime);
        queue(&runtime, "session:a", "key-a", "mem://a", "A");
        queue(&runtime, "session:b", "key-b", "mem://b", "B");

        // Unsubscribing one URI drops exactly that update.
        runtime.clear_pending_for_uri("key-a", "mem://a");
        let remaining = runtime.take_pending_updates(None);
        assert_eq!(remaining.len(), 1, "got {remaining:?}");
        assert_eq!(remaining[0].text, "B");

        // Closing one connection drops all of its updates, not the other's.
        queue(&runtime, "session:a", "key-a", "mem://other", "A2");
        queue(&runtime, "session:b", "key-b", "mem://b", "B2");
        runtime.clear_pending_for_key("key-a");
        let remaining = runtime.take_pending_updates(None);
        assert_eq!(remaining.len(), 1, "got {remaining:?}");
        assert_eq!(remaining[0].text, "B2");
        clear_queue(&runtime);
    }

    /// The scope stored with an update is the one embedded in its connection key
    /// (`[scope, identity, name]`), which is what the filter above compares.
    #[test]
    fn scope_is_read_back_out_of_the_connection_key() {
        let key = serde_json::to_string(&("session:abc", "/tmp/mcp.json", "fake")).unwrap();
        assert_eq!(scope_of_key(&key), "session:abc");
        assert_eq!(scope_of_key("not json"), "");
    }
}
