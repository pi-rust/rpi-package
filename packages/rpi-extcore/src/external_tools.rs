//! Session-owned, protocol-neutral MCP stdio tools for the native RPC host.
use rpi_mcp_core::{
    ConfiguredStdio, Control, CoreRequestContext, DiscoveryLimits, McpRuntime, StdioServer,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinHandle};

/// Explicit stdio configuration. Deliberately not Debug: environment values may be secrets.
#[derive(Clone, PartialEq, Eq)]
pub struct StdioToolServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

const MAX_CALLS: usize = 64;
const MAX_TOOLS: usize = 128;
const MAX_CATALOG: usize = 256 * 1024;
const MAX_FRAME: usize = 1024 * 1024;

#[cfg(test)]
#[path = "external_tools_tests.rs"]
mod tests;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Configuration {
    pub servers: Vec<StdioToolServer>,
    pub cwd: PathBuf,
    pub scope: String,
}

impl Configuration {
    fn targets(&self) -> Result<Vec<ConfiguredStdio>, String> {
        if self.servers.len() > 16
            || !self.cwd.is_absolute()
            || self.scope.trim().is_empty()
            || self.scope.contains('\0')
        {
            return Err("invalid external tool configuration".into());
        }
        let context = CoreRequestContext {
            cwd: self.cwd.clone(),
            scope: self.scope.clone(),
        };
        let cwd = self.cwd.to_str().ok_or("external tool cwd must be UTF-8")?;
        let mut names = HashSet::new();
        let mut bytes = 0usize;
        self.servers
            .iter()
            .map(|s| {
                if !names.insert(&s.name)
                    || s.name.len() > 200
                    || s.args.len() > 256
                    || s.env.len() > 256
                {
                    return Err("invalid external tool configuration".into());
                }
                // Bound the encoded configuration, without ever writing a transient config file.
                bytes = bytes.saturating_add(
                    json!({"name":s.name,"command":s.command,"args":s.args,"env":s.env})
                        .to_string()
                        .len(),
                );
                if bytes > MAX_CATALOG {
                    return Err("external tool configuration is too large".into());
                }
                ConfiguredStdio::new(
                    &context,
                    &s.name,
                    StdioServer {
                        command: s.command.clone(),
                        args: s.args.clone(),
                        env: s.env.clone(),
                        cwd: Some(cwd.into()),
                        enabled: true,
                        timeout: 45,
                        exposure: Default::default(),
                    },
                )
                .map_err(|_| "invalid external tool configuration".into())
            })
            .collect()
    }
}

struct Binding {
    target: ConfiguredStdio,
    tool: String,
}
pub(crate) struct Catalog {
    pub definitions: Vec<Value>,
    bindings: HashMap<String, Binding>,
}

fn native_name(server: &str, tool: &str) -> String {
    // Length-delimited tuple avoids ambiguous separator concatenations.
    let mut hash = Sha256::new();
    for part in [server, tool] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("mcp_{:x}", hash.finalize())[..64].to_string()
}

fn discover(
    runtime: &McpRuntime,
    config: &Configuration,
    targets: Vec<ConfiguredStdio>,
    control: &Control,
) -> Result<Catalog, String> {
    let mut catalog = Catalog {
        definitions: Vec::new(),
        bindings: HashMap::new(),
    };
    for (server, target) in config.servers.iter().zip(targets) {
        control.check()?;
        let found = runtime
            .discover_stdio(
                &target,
                DiscoveryLimits {
                    max_tools: MAX_TOOLS - catalog.definitions.len(),
                    max_catalog_bytes: MAX_CATALOG,
                },
                control,
            )
            .map_err(|_| "external tool discovery failed".to_string())?;
        for tool in found.tools {
            if catalog.definitions.len() >= MAX_TOOLS
                || !tool.input_schema.is_object()
                || tool.input_schema["type"] != "object"
            {
                return Err("invalid external tool catalog".into());
            }
            let name = native_name(&server.name, &tool.name);
            let description = format!(
                "MCP server: {}\nMCP tool: {}\n{}",
                server.name, tool.name, tool.description
            );
            if description.len() > 8 * 1024 {
                return Err("external tool description is too large".into());
            }
            catalog.definitions.push(
                json!({"name":name,"description":description,"inputSchema":tool.input_schema}),
            );
            if catalog
                .bindings
                .insert(
                    name,
                    Binding {
                        target: target.clone(),
                        tool: tool.name,
                    },
                )
                .is_some()
            {
                return Err("duplicate external tool identity".into());
            }
            if serde_json::to_vec(&catalog.definitions)
                .map_err(|_| "invalid external tool catalog")?
                .len()
                > MAX_CATALOG
            {
                return Err("external tool catalog is too large".into());
            }
        }
    }
    Ok(catalog)
}

/// Drop cancels the blocking discovery and retains its join before shutting down
/// the candidate runtime. Never abandon a spawn_blocking operation on timeout.
pub(crate) struct Candidate {
    runtime: Option<Arc<McpRuntime>>,
    control: Control,
    task: Option<JoinHandle<Result<Catalog, String>>>,
}
impl Candidate {
    pub fn start(config: Configuration) -> Result<Self, String> {
        let targets = config.targets()?;
        let runtime = Arc::new(McpRuntime::new());
        let control = Control::default();
        let r = runtime.clone();
        let c = control.clone();
        let task = tokio::task::spawn_blocking(move || discover(&r, &config, targets, &c));
        Ok(Self {
            runtime: Some(runtime),
            control,
            task: Some(task),
        })
    }
    pub async fn discover(&mut self) -> Result<Catalog, String> {
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            self.task.as_mut().expect("candidate task"),
        )
        .await
        .map_err(|_| "external tool discovery timed out".to_string())?
        .map_err(|_| "external tool discovery failed".to_string())?;
        self.task.take();
        result
    }
    pub fn install(
        mut self,
        catalog: Catalog,
        commands: crate::host::BridgeCommands,
    ) -> Arc<Bridge> {
        Bridge::start(
            self.runtime.take().expect("candidate runtime"),
            catalog,
            commands,
        )
    }
}
impl Drop for Candidate {
    fn drop(&mut self) {
        self.control.cancel();
        let task = self.task.take();
        if let Some(runtime) = self.runtime.take() {
            tokio::spawn(async move {
                if let Some(task) = task {
                    let _ = task.await;
                }
                let _ = tokio::task::spawn_blocking(move || runtime.shutdown_connections()).await;
            });
        }
    }
}

struct Invocation {
    id: String,
    tool: String,
    arguments: Value,
    control: Control,
}
struct Active {
    stopped: bool,
    calls: HashMap<String, Control>,
}

/// Owned only by HostSession; the native reader holds a Weak reference. The
/// supervisor owns independent state, never an Arc<Bridge> or Arc<HostSession>.
pub(crate) struct Bridge {
    active: Arc<Mutex<Active>>,
    stop: Control,
    requests: mpsc::Sender<Invocation>,
    join: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}
impl Bridge {
    fn start(
        runtime: Arc<McpRuntime>,
        catalog: Catalog,
        commands: crate::host::BridgeCommands,
    ) -> Arc<Self> {
        let (requests, mut receive) = mpsc::channel::<Invocation>(MAX_CALLS);
        let active = Arc::new(Mutex::new(Active {
            stopped: false,
            calls: HashMap::new(),
        }));
        let stop = Control::default();
        let task_stop = stop.clone();
        let task_active = active.clone();
        let join = tokio::spawn(async move {
            let bindings = Arc::new(catalog.bindings);
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = task_stop.cancelled() => break,
                    Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                    request = receive.recv(), if tasks.len() < MAX_CALLS => {
                        let Some(request) = request else { break };
                        let runtime = runtime.clone();
                        let bindings = bindings.clone();
                        let commands = commands.clone();
                        let active = task_active.clone();
                        tasks.spawn(async move {
                            invoke(runtime, bindings, request, commands, active).await;
                        });
                    }
                }
            }
            {
                let mut active = task_active.lock().expect("external calls");
                active.stopped = true;
                for control in active.calls.values() {
                    control.cancel();
                }
                active.calls.clear();
            }
            receive.close();
            while let Some(request) = receive.recv().await {
                request.control.cancel();
            }
            while tasks.join_next().await.is_some() {}
            // The last runtime Arc is dropped on this blocking cleanup worker too.
            let _ = tokio::task::spawn_blocking(move || runtime.shutdown_connections()).await;
        });
        Arc::new(Self {
            active,
            stop,
            requests,
            join: tokio::sync::Mutex::new(Some(join)),
        })
    }
    pub fn downgrade(this: &Arc<Self>) -> Weak<Self> {
        Arc::downgrade(this)
    }
    /// Synchronous, nonblocking reader routing. Overflow is authoritative failure.
    pub fn route(&self, value: &Value) -> Result<(), ()> {
        match value["type"].as_str() {
            Some("external_tool_request_closed") => {
                let id = value["requestId"].as_str().ok_or(())?;
                if let Some(control) = self.active.lock().expect("external calls").calls.remove(id)
                {
                    control.cancel();
                }
            }
            Some("external_tool_request") => {
                let id = value["requestId"]
                    .as_str()
                    .filter(|id| !id.is_empty() && id.len() <= 256)
                    .ok_or(())?;
                let tool = value["toolName"].as_str().ok_or(())?;
                let mut active = self.active.lock().expect("external calls");
                if active.stopped
                    || active.calls.len() >= MAX_CALLS
                    || active.calls.contains_key(id)
                {
                    return Err(());
                }
                let control = Control::default();
                active.calls.insert(id.into(), control.clone());
                self.requests
                    .try_send(Invocation {
                        id: id.into(),
                        tool: tool.into(),
                        arguments: value["arguments"].clone(),
                        control,
                    })
                    .map_err(|_| ())?;
            }
            Some("agent_settled") => self.cancel_calls(),
            _ => {}
        }
        Ok(())
    }
    pub fn cancel_calls(&self) {
        let mut active = self.active.lock().expect("external calls");
        for (_, control) in active.calls.drain() {
            control.cancel();
        }
    }
    pub fn close(&self) {
        self.active.lock().expect("external calls").stopped = true;
        self.stop.cancel();
        self.cancel_calls();
    }
    pub async fn join(&self) {
        let mut join = self.join.lock().await;
        if let Some(task) = join.as_mut() {
            let _ = task.await;
        }
        join.take();
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.close();
    }
}

struct CancelOnDrop(Control);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn invoke(
    runtime: Arc<McpRuntime>,
    bindings: Arc<HashMap<String, Binding>>,
    request: Invocation,
    commands: crate::host::BridgeCommands,
    active: Arc<Mutex<Active>>,
) {
    let Invocation {
        id,
        tool,
        arguments,
        control,
    } = request;
    let _cancel = CancelOnDrop(control.clone());
    if control.check().is_err() {
        return;
    }
    let outcome = if arguments.is_object() {
        if let Some(binding) = bindings.get(&tool) {
            let target = binding.target.clone();
            let tool = binding.tool.clone();
            // Keep the native request lifetime separate from this call budget.
            // MCP links its own entry-stop flag, so a secondary parent link would
            // be overwritten. The select below explicitly cancels this primary.
            let call_control = Control::default();
            let blocking_control = call_control.clone();
            let mut call = tokio::task::spawn_blocking(move || {
                runtime.call_stdio(&target, &tool, arguments, &blocking_control)
            });
            let result = tokio::select! {
                biased;
                _ = control.cancelled() => { call_control.cancel(); let _ = call.await; return; },
                result = tokio::time::timeout(Duration::from_secs(45), &mut call) => match result {
                    Ok(Ok(result)) => result.map_err(|_| "external tool call failed"),
                    Ok(Err(_)) => Err("external tool call failed"),
                    Err(_) => { call_control.cancel(); let _ = call.await; Err("external tool call timed out") }
                }
            };
            result.and_then(validate_result)
        } else {
            Err("unknown external tool")
        }
    } else {
        Err("external tool arguments must be an object")
    };
    let mut frame = json!({"type":"external_tool_response","requestId":id});
    match outcome {
        Ok(result) => frame["result"] = result,
        Err(error) => frame["error"] = json!(error),
    }
    // Include the newline and native correlation UUID added by dispatch.
    if frame.to_string().len() + 128 > MAX_FRAME {
        frame = json!({"type":"external_tool_response","requestId":id,"error":"external tool result is too large"});
    }
    let live = {
        let state = active.lock().expect("external calls");
        !state.stopped
            && state
                .calls
                .get(&id)
                .is_some_and(|c| Arc::ptr_eq(&c.0, &control.0))
            && control.check().is_ok()
    };
    if live {
        commands.respond(frame, control.clone()).await;
    }
    // Keep the entry until native closure; closure synchronously cancels queued
    // output as well as calls. This also enforces the native outstanding limit.
}

fn validate_result(value: Value) -> Result<Value, &'static str> {
    let object = value.as_object().ok_or("invalid external tool result")?;
    let content = object
        .get("content")
        .and_then(Value::as_array)
        .ok_or("invalid external tool content")?;
    use base64::Engine;
    for block in content {
        match block["type"].as_str() {
            Some("text")
                if block["text"].is_string()
                    && block
                        .get("textSignature")
                        .map_or(true, |v| v.is_null() || v.is_string()) => {}
            Some("image")
                if block["data"].as_str().is_some_and(|data| {
                    base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .is_ok()
                }) && block["mimeType"]
                    .as_str()
                    .is_some_and(|v| v.starts_with("image/") && v.len() > 6) => {}
            _ => return Err("unsupported external tool content block"),
        }
    }
    if object.get("isError").is_some_and(|v| !v.is_boolean())
        || object
            .get("structuredContent")
            .is_some_and(|v| !v.is_object())
    {
        return Err("invalid external tool result metadata");
    }
    Ok(value)
}
