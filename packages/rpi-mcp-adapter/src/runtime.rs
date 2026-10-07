//! Persistent connections scoped to the host session and configuration identity.
use crate::connection::{
    initialize_request, initialized_notification, parse_initialize, response_result,
    tools_call_request,
};
use crate::control::{Control, Deadline};
use crate::http::HttpTransport;
use crate::{McpConfig, McpServerConfig, Request, Response, ServerInfo, StdioTransport, Tool};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, TryLockError};

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
    config: McpServerConfig,
    transport: Transport,
    info: ServerInfo,
    next_id: u64,
    failed: bool,
}
impl Session {
    fn start(config: &McpServerConfig, timeout: u64, control: &Control) -> Result<Self, String> {
        control.check()?;
        let mut transport = match config {
            McpServerConfig::Stdio(s) => Transport::Stdio(StdioTransport::start(
                &s.command,
                &s.args,
                &s.env,
                s.cwd.as_deref(),
            )?),
            McpServerConfig::Http(s) => Transport::Http(HttpTransport::new(s)?),
        };
        transport.timeout(timeout);
        let info = parse_initialize(transport.request(
            &initialize_request(1, "rpi-mcp-adapter", env!("CARGO_PKG_VERSION")),
            control,
        )?)?;
        if let Transport::Http(http) = &mut transport {
            http.version = Some(info.protocol_version.clone());
        }
        transport.notify(control)?;
        Ok(Self {
            config: config.clone(),
            transport,
            info,
            next_id: 2,
            failed: false,
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
struct Entry {
    session: Mutex<Option<Session>>,
    stop: Control,
}
type Sessions = BTreeMap<String, Arc<Entry>>;
static SESSIONS: OnceLock<Mutex<Sessions>> = OnceLock::new();
fn sessions() -> &'static Mutex<Sessions> {
    SESSIONS.get_or_init(Default::default)
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
fn with_session<T>(
    key: String,
    config: McpServerConfig,
    timeout: u64,
    control: &Control,
    run: impl FnOnce(&mut Session, &Control) -> Result<T, String>,
) -> Result<T, String> {
    let entry = {
        let mut sessions = sessions()
            .lock()
            .map_err(|_| "MCP session registry poisoned")?;
        if !sessions.contains_key(&key) && sessions.len() >= 64 {
            return Err("MCP connection limit (64) reached; close unused connections with mcp_list action=close".into());
        }
        sessions
            .entry(key)
            .or_insert_with(|| {
                Arc::new(Entry {
                    session: Mutex::new(None),
                    stop: Control::default(),
                })
            })
            .clone()
    };
    let control = control.linked(&entry.stop);
    let deadline = Deadline::new(timeout, "connection lock", &control);
    let mut state = lock_until(&entry.session, &deadline)?;
    if state.as_ref().is_some_and(|s| s.config != config) {
        state.take();
    }
    if state.is_none() {
        *state = Some(Session::start(&config, timeout, &control)?);
    }
    let session = state.as_mut().ok_or("MCP session unavailable")?;
    session.transport.timeout(timeout);
    let result = run(session, &control);
    if session.failed {
        state.take();
    } // Do not retry tool calls: they may have side effects.
    result
}

pub fn shutdown_connections() {
    let entries = if let Ok(mut map) = sessions().lock() {
        std::mem::take(&mut *map)
    } else {
        return;
    };
    for entry in entries.values() {
        entry.stop.cancel();
    }
    for entry in entries.into_values() {
        if let Ok(mut state) = entry.session.lock() {
            state.take();
        }
    }
}
fn close_key(key: &str) -> Result<(), String> {
    let entry = sessions()
        .lock()
        .map_err(|_| "MCP session registry poisoned")?
        .remove(key);
    if let Some(entry) = entry {
        entry.stop.cancel();
        entry
            .session
            .lock()
            .map_err(|_| "MCP connection lock poisoned")?
            .take();
    }
    Ok(())
}

fn host_scope(params: &Value) -> String {
    let host = params.get(rpi_plugin_sdk::HOST_CONTEXT_KEY);
    if let Some(id) = host
        .and_then(|v| v.get("sessionId"))
        .and_then(Value::as_str)
    {
        return format!("session:{id}");
    }
    if let Some(cwd) = host.and_then(|v| v.get("cwd")).and_then(Value::as_str) {
        return format!(
            "project:{}",
            std::fs::canonicalize(cwd)
                .unwrap_or_else(|_| PathBuf::from(cwd))
                .to_string_lossy()
        );
    }
    "library".into()
}

fn host_cwd(params: &Value) -> PathBuf {
    params
        .get(rpi_plugin_sdk::HOST_CONTEXT_KEY)
        .and_then(|v| v.get("cwd"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}
fn config_file(params: &Value) -> Result<PathBuf, String> {
    let path = params
        .get("configPath")
        .and_then(Value::as_str)
        .ok_or("missing configPath")?;
    std::fs::canonicalize(host_cwd(params).join(path))
        .map_err(|e| format!("read MCP config {path:?}: {e}"))
}
fn load_config(params: &Value) -> Result<(PathBuf, McpConfig), String> {
    let path = config_file(params)?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read MCP config: {e}"))?;
    Ok((path, McpConfig::parse(&text)?))
}
fn target(params: &Value) -> Result<(String, String, McpServerConfig, u64), String> {
    let scope = host_scope(params);
    let (name, identity, mut config) = if params.get("configPath").is_some() {
        if params.get("url").is_some() {
            return Err("specify configPath/server or url, not both".into());
        }
        let name = crate::kit::string_param(params, "server")?;
        let (path, config) = load_config(params)?;
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
        let url =
            crate::http::validate_mcp_url(&crate::kit::string_param(params, "url")?)?.to_string();
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
        close_key(&key)?;
        return Err(format!("MCP server {name:?} is disabled"));
    }
    if let McpServerConfig::Stdio(server) = &mut config {
        // A configured cwd is relative to the config file; otherwise use host project cwd.
        if let Some(cwd) = &server.cwd {
            if Path::new(cwd).is_relative() {
                let file = config_file(params)?;
                server.cwd = Some(
                    file.parent()
                        .unwrap_or(Path::new("."))
                        .join(cwd)
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        } else {
            server.cwd = Some(host_cwd(params).to_string_lossy().into_owned());
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

pub(crate) fn call(params: &Value, control: &Control) -> Result<Value, String> {
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
    let (key, _, config, timeout) = target(params)?;
    with_session(key, config, timeout, control, |session, control| {
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
    })
}
pub(crate) fn list(params: &Value, control: &Control) -> Result<Value, String> {
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
        let (_, config) = load_config(params)?;
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
    let (key, name, config, timeout) = target(params)?;
    if action == "close" {
        close_key(&key)?;
        return Ok(json!({"server":name,"closed":true}));
    }
    with_session(key, config, timeout, control, |session, control| {
        let tools = session.tools(control)?;
        Ok(
            json!({"server":name, "serverInfo":{"name":session.info.name,"version":session.info.version},"protocolVersion":session.info.protocol_version,"tools":tools}),
        )
    })
}

pub fn call_from_config(
    config_path: &str,
    server_name: &str,
    tool_name: &str,
    arguments: Value,
) -> Result<String, String> {
    call_from_config_with_timeout(config_path, server_name, tool_name, arguments, None)
}
pub fn call_from_config_with_timeout(
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
    let result = call(&params, &Control::default())?;
    crate::connection::check_tool_result(&result)?;
    Ok(crate::call_result_text(&result))
}
pub fn discover_from_config(
    config_path: &str,
    server_name: &str,
) -> Result<DiscoveredServer, String> {
    let value = list(
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
pub fn close_from_config(config_path: &str, server_name: &str) -> Result<(), String> {
    list(
        &json!({"configPath":config_path,"server":server_name,"action":"close"}),
        &Control::default(),
    )
    .map(|_| ())
}
