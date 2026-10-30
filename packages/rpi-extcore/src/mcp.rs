//! Session-owned MCP resources behind native generic external tools.
//!
//! Attach before prompting; keep exactly one binding per host. Configuration is
//! ephemeral metadata, never transcript data. Native retains permission/run
//! semantics. Dropping a binding signals cleanup; `shutdown` also joins it.
use crate::{
    host::HostSession,
    mcp_external::{Driver, ExternalConfig},
};
use futures_util::FutureExt;
pub use rpi_mcp_core::{oauth, AuthError, Control as McpControl, CredentialProvider};
use rpi_mcp_core::{Control, McpRuntime, RequestContext};
pub use rpi_mcp_core::{Exposure, HttpServer, McpServerConfig, StdioServer};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{
    sync::{broadcast, oneshot},
    task::{JoinHandle, JoinSet},
};

/// Explicit session context. Relative server cwd paths are resolved by MCP core
/// against `cwd`; no ambient plugin/session identity is consulted.
#[derive(Clone, Debug)]
pub struct McpConfig {
    pub scope: String,
    pub cwd: PathBuf,
    pub servers: BTreeMap<String, McpServerConfig>,
    /// Overall discovery deadline, clamped to 1..=300 seconds.
    pub discovery_timeout: Duration,
}

/// Explicit trusted runtime injection; absent by default (header-only HTTP).
#[derive(Default, Clone)]
pub struct McpOptions {
    pub credential_provider: Option<Arc<dyn CredentialProvider>>,
    /// Explicit trusted opt-in for initialized resources/prompts model tools.
    /// False preserves the existing tools-only native surface.
    pub catalog: bool,
    /// Explicit executor AND authorizer; disabled by default.
    pub sampling: Option<rpi_mcp_core::sampling::SamplingOptions>,
    /// Retains provisional workers when an attachment future is dropped. Without
    /// an explicit owner, binding Drop only signals cancellation (legacy RAII).
    pub owner: Option<McpOwner>,
}

/// Caller-owned cleanup boundary, shared across attachment/reconfiguration.
/// Shutdown permanently closes admission; individual binding shutdown does not.
#[derive(Default, Clone)]
pub struct McpOwner {
    state: Arc<Mutex<OwnerState>>,
}
#[derive(Default)]
struct OwnerState {
    closed: bool,
    workers: Vec<Arc<Worker>>,
}
impl std::fmt::Debug for McpOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("McpOwner([redacted])")
    }
}
impl McpOwner {
    // Admission and spawn are one synchronous critical section: shutdown cannot
    // miss a provisional worker, and no work starts after the owner closes.
    fn register(&self, start: impl FnOnce() -> Arc<Worker>) -> Result<Arc<Worker>, String> {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Err("MCP owner is closed".into());
        }
        state.workers.retain(|worker| !worker.reap_finished());
        if state.workers.len() >= 128 {
            return Err("MCP owner exceeds 128 workers".into());
        }
        let worker = start();
        state.workers.push(worker.clone());
        Ok(worker)
    }

    /// Cancel every provisional/published worker before joining any. Cancelling
    /// this future never takes a live handle out of the shared registry; retrying
    /// joins the same workers. All workers are joined even when one returns Err.
    pub async fn shutdown(&self) -> Result<(), String> {
        let workers = {
            let mut state = self.state.lock().unwrap();
            state.closed = true;
            for worker in &state.workers {
                worker.control.cancel();
            }
            state.workers.clone()
        };
        let mut result = Ok(());
        for worker in workers {
            let joined = worker.join().await;
            if result.is_ok() {
                result = joined;
            }
        }
        self.state
            .lock()
            .unwrap()
            .workers
            .retain(|w| !w.reap_finished());
        result
    }
}

struct Worker {
    control: Control,
    finished: tokio::task::AbortHandle,
    state: tokio::sync::Mutex<WorkerState>,
}
struct WorkerState {
    handle: Option<JoinHandle<Result<(), String>>>,
    result: Option<Result<(), String>>,
}
impl Worker {
    fn new(control: Control, handle: JoinHandle<Result<(), String>>) -> Arc<Self> {
        Arc::new(Self {
            control,
            finished: handle.abort_handle(),
            state: tokio::sync::Mutex::new(WorkerState {
                handle: Some(handle),
                result: None,
            }),
        })
    }
    fn completed(
        state: &mut WorkerState,
        result: Result<Result<(), String>, tokio::task::JoinError>,
    ) -> Result<(), String> {
        let result = result
            .unwrap_or_else(|_| Err("MCP binding worker failed (diagnostics withheld)".into()));
        state.handle = None;
        state.result = Some(result.clone());
        result
    }
    fn reap_finished(&self) -> bool {
        let Ok(mut state) = self.state.try_lock() else {
            return false;
        };
        if let Some(handle) = state.handle.as_mut() {
            if !handle.is_finished() {
                return false;
            }
            // Only poll a terminal handle; retain it if it is unexpectedly Pending.
            let Some(result) = handle.now_or_never() else {
                return false;
            };
            let _ = Self::completed(&mut state, result);
        }
        true
    }
    async fn join(&self) -> Result<(), String> {
        let mut state = self.state.lock().await;
        match state.handle.as_mut() {
            Some(handle) => {
                let result = handle.await;
                Self::completed(&mut state, result)
            }
            None => state.result.clone().unwrap_or(Ok(())),
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.control.cancel();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolDescriptor {
    pub native_name: String,
    pub server: String,
    pub remote_name: String,
    pub description: String,
    pub input_schema: Value,
}

/// The creator owns this guard. No task retains the guard or a strong host.
pub struct McpBinding {
    control: Control,
    worker: Option<Arc<Worker>>,
    descriptors: Vec<ToolDescriptor>,
}
impl Drop for McpBinding {
    fn drop(&mut self) {
        self.control.cancel();
    }
}
impl McpBinding {
    pub async fn attach(host: Arc<HostSession>, config: McpConfig) -> Result<Self, String> {
        attach(host, config).await
    }
    pub fn descriptors(&self) -> &[ToolDescriptor] {
        &self.descriptors
    }
    /// True only when discovery configured at least one model-facing tool.
    pub fn is_active(&self) -> bool {
        !self.descriptors.is_empty()
    }
    /// Surface terminal overflow/transport failures without replaying calls.
    pub fn is_finished(&self) -> bool {
        self.worker
            .as_ref()
            .map_or(true, |w| w.finished.is_finished())
    }
    pub async fn shutdown(&mut self) -> Result<(), String> {
        self.control.cancel();
        // Keep ownership if this shutdown future is itself cancelled.
        let result = match self.worker.as_ref() {
            Some(worker) => worker.join().await,
            None => Ok(()),
        };
        self.worker = None;
        result
    }
}

/// Subscribe before discovery/configuration. Empty/disabled config is a true
/// no-op and does not require or advertise native external-tool capability.
pub async fn attach(host: Arc<HostSession>, config: McpConfig) -> Result<McpBinding, String> {
    attach_with_options(host, config, McpOptions::default()).await
}

pub async fn attach_with_options(
    host: Arc<HostSession>,
    config: McpConfig,
    options: McpOptions,
) -> Result<McpBinding, String> {
    attach_inner(host, config, None, options).await
}

pub(crate) async fn attach_external(
    host: Arc<HostSession>,
    config: McpConfig,
    external: ExternalConfig,
) -> Result<McpBinding, String> {
    attach_external_with_options(host, config, external, McpOptions::default()).await
}

pub(crate) async fn attach_external_with_options(
    host: Arc<HostSession>,
    config: McpConfig,
    external: ExternalConfig,
    options: McpOptions,
) -> Result<McpBinding, String> {
    let backend = external.backend.clone();
    let result = match Driver::new(external, &config) {
        Ok(driver) => attach_inner(host, config, Some(Arc::new(driver)), options).await,
        Err(error) => Err(error),
    };
    // No worker owns early validation failures. Shutdown is idempotent, so
    // attach failure also covers errors already cleaned up by the worker.
    if result.is_err() {
        let _ = backend.shutdown().await;
    }
    result
}

async fn attach_inner(
    host: Arc<HostSession>,
    config: McpConfig,
    external: Option<Arc<Driver>>,
    mut options: McpOptions,
) -> Result<McpBinding, String> {
    let control = Control::default();
    let mut binding = McpBinding {
        control: control.clone(),
        worker: None,
        descriptors: vec![],
    };
    let enabled = config.servers.values().any(|s| match s {
        McpServerConfig::Stdio(s) => s.enabled,
        McpServerConfig::Http(s) => s.enabled,
    });
    if !enabled && external.is_none() {
        return Ok(binding);
    }
    if config.scope.trim().is_empty() || !config.cwd.is_absolute() || !config.cwd.is_dir() {
        return Err("MCP requires a nonempty scope and absolute existing cwd".into());
    }
    for command in ["configure_external_tools", "external_tool_response"] {
        if !host.supports_command(command) {
            return Err(format!("native host does not support {command}"));
        }
    }
    let events = host.subscribe_raw();
    let (ready_tx, ready_rx) = oneshot::channel();
    let weak = Arc::downgrade(&host);
    let owner = options.owner.take(); // Never retain the registry from its worker.
    let worker_control = control.clone();
    let start = || {
        Worker::new(
            worker_control,
            tokio::spawn(async move {
                run(weak, config, events, control, ready_tx, external, options).await
            }),
        )
    };
    binding.worker = Some(match owner {
        Some(owner) => owner.register(start)?,
        None => start(),
    });
    match ready_rx
        .await
        .unwrap_or_else(|_| Err("MCP binding stopped during attach".into()))
    {
        Ok(descriptors) => binding.descriptors = descriptors,
        Err(error) => {
            // Ready failure is not cleanup acknowledgement. Join private runtime
            // discovery/transports before returning; owner survives caller cancel.
            let _ = binding.shutdown().await;
            return Err(error);
        }
    }
    Ok(binding)
}

// Runtime errors may include remote stderr, credentials, or configuration values.
// Do not forward or log them across the core boundary. Keep locally generated
// validation errors separate so their useful static diagnostics survive.
fn runtime_error(_error: String) -> String {
    "MCP runtime operation failed (remote diagnostics withheld)".into()
}

pub(crate) fn tool_name(server: &str, tool: &str) -> String {
    let mut hash = Sha256::new();
    hash.update((server.len() as u64).to_be_bytes());
    hash.update(server.as_bytes());
    hash.update(tool.as_bytes());
    // 240 digest bits plus namespace: deterministic, ASCII, exactly 64 bytes.
    format!("mcp_{:x}", hash.finalize())[..64].to_string()
}

fn definitions(tools: &[ToolDescriptor]) -> Vec<Value> {
    tools.iter().map(|t| json!({"name":t.native_name,"description":t.description,"inputSchema":t.input_schema})).collect()
}

async fn send(host: &Weak<HostSession>, value: Value) -> Result<Value, String> {
    let host = host.upgrade().ok_or("native host dropped")?;
    let response = host.send_timeout(value, Duration::from_secs(5)).await?;
    if response["success"] == false {
        return Err("native rejected MCP external tool command".into());
    }
    Ok(response)
}

async fn run(
    host: Weak<HostSession>,
    config: McpConfig,
    mut events: broadcast::Receiver<Value>,
    control: Control,
    ready: oneshot::Sender<Result<Vec<ToolDescriptor>, String>>,
    external: Option<Arc<Driver>>,
    options: McpOptions,
) -> Result<(), String> {
    // RAII file stays alive through every blocking operation and runtime cleanup.
    // tempfile uses exclusive creation and mode 0600 on Unix; Windows inherits
    // the user's temp directory ACL. Never log config or expose its path.
    let mut file = match tempfile::Builder::new()
        .prefix("rpi-mcp-")
        .suffix(".json")
        .tempfile()
    {
        Ok(file) => file,
        Err(e) => {
            let error = format!("cannot create private MCP metadata: {e}");
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
    };
    let runtime = Arc::new(McpRuntime::with_providers(options.credential_provider, options.sampling));
    let mut configure_attempted = false;
    let result = async {
        let raw = rpi_mcp_core::McpConfig { servers: config.servers.clone() };
        let bytes = serde_json::to_vec(&raw).map_err(|e| e.to_string())?;
        rpi_mcp_core::McpConfig::parse(std::str::from_utf8(&bytes).map_err(|e| e.to_string())?).map_err(runtime_error)?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.flush().map_err(|e| e.to_string())?;
        let path = file.path().to_path_buf();
        let context = RequestContext { scope: config.scope.clone(), cwd: config.cwd.clone() };
        let rt = runtime.clone();
        let ctx = context.clone();
        let cancel = control.clone();
        let discovery_path = path.clone();
        let external_discovery = external.clone();
        let external_context = context.clone();
        let external_cancel = control.clone();
        let catalog = options.catalog;
        let mut discover = tokio::spawn(async move {
        let mut descriptors = tokio::task::spawn_blocking(move || {
            let mut descriptors = Vec::new();
            let mut names = HashSet::new();
            for (server, target) in &config.servers {
                let enabled = match target { McpServerConfig::Stdio(s) => s.enabled, McpServerConfig::Http(s) => s.enabled };
                if !enabled { continue; }
                cancel.check()?;
                let params = json!({"configPath":discovery_path,"server":server});
                if catalog {
                    let info = rt.server_info(&ctx, &params, &cancel).map_err(runtime_error)?;
                    for descriptor in crate::mcp_catalog::descriptors(server, &info) {
                        if !names.insert(descriptor.native_name.clone()) { return Err("duplicate/colliding MCP tool identity".into()); }
                        if descriptors.len() >= 128 { return Err("MCP discovery exceeds 128 native tools".into()); }
                        descriptors.push(descriptor);
                    }
                    // Catalog-only servers must not receive unsupported tools/list.
                    if !info.capabilities.get("tools").is_some_and(Value::is_object) { continue; }
                }
                let result = rt.list(&ctx, &params, &cancel).map_err(runtime_error)?;
                for tool in rpi_mcp_core::tool_list(&result).map_err(runtime_error)? {
                    let native_name = tool_name(server, &tool.name);
                    if !names.insert(native_name.clone()) { return Err("duplicate/colliding MCP tool identity".to_string()); }
                    if descriptors.len() >= 128 { return Err("MCP discovery exceeds 128 native tools".into()); }
                    descriptors.push(ToolDescriptor { native_name, server: server.clone(), remote_name: tool.name, description: tool.description, input_schema: tool.input_schema });
                }
            }
            if serde_json::to_vec(&definitions(&descriptors)).map_err(|e| e.to_string())?.len() > 256 * 1024 {
                return Err("MCP definitions exceed 256 KiB".into());
            }
            Ok::<_, String>(descriptors)
        }).await.map_err(|_| "MCP discovery worker failed".to_string())??;
        if let Some(external) = external_discovery {
            descriptors.extend(external.discover(&external_context, &external_cancel).await?);
        }
        let mut names = HashSet::new();
        if descriptors.len() > 128 { return Err("MCP discovery exceeds 128 native tools".into()); }
        if descriptors.iter().any(|tool| !names.insert(tool.native_name.clone())) {
            return Err("duplicate/colliding MCP tool identity".into());
        }
        if serde_json::to_vec(&definitions(&descriptors)).map_err(|e| e.to_string())?.len() > 256 * 1024 {
            return Err("MCP definitions exceed 256 KiB".into());
        }
        Ok::<_, String>(descriptors)
        });
        let discovery_deadline = tokio::time::sleep(config.discovery_timeout.clamp(Duration::from_secs(1), Duration::from_secs(300)));
        tokio::pin!(discovery_deadline);
        let descriptors = loop {
            tokio::select! {
                result = &mut discover => break result.map_err(|_| "MCP discovery worker failed".to_string())?,
                _ = control.cancelled() => { control.cancel(); let _ = discover.await; return Err("MCP discovery cancelled".into()); },
                _ = &mut discovery_deadline => {
                    control.cancel(); let _ = discover.await; return Err("MCP discovery timed out".into());
                }
                event = events.recv() => {
                    let error = match event {
                        Ok(e) if e["type"] == "host_closed" => Some("native host closed during MCP discovery".to_string()),
                        Ok(e) if e["type"] == "external_tool_request" => Some("external tool request before MCP configuration".to_string()),
                        Err(e) => Some(format!("MCP discovery event stream lost: {e}")),
                        _ => None,
                    };
                    if let Some(error) = error {
                        control.cancel(); let _ = discover.await; return Err(error);
                    }
                }
            }
        }?;
        control.check()?;
        if !descriptors.is_empty() {
            configure_attempted = true;
            send(&host, json!({"type":"configure_external_tools","tools":definitions(&descriptors)})).await?;
        }
        Ok::<_, String>((descriptors, path, context))
    }.await;
    let result = match result {
        Err(error) => {
            let _ = ready.send(Err(error.clone()));
            if configure_attempted {
                // An acknowledgement timeout can leave configuration applied.
                if let Some(host) = host.upgrade() {
                    host.terminate().await;
                }
            }
            Err(error)
        }
        Ok((descriptors, path, context)) => {
            let active = !descriptors.is_empty();
            let _ = ready.send(Ok(descriptors.clone()));
            let result = serve(
                &host,
                events,
                &control,
                ServeContext {
                    runtime: runtime.clone(),
                    context,
                    path,
                    descriptors,
                    external: external.clone(),
                },
            )
            .await;
            if active {
                // Reconfiguration is idle-only. If a live native run prevents
                // removal, stop the host rather than leave dangling tools.
                if send(&host, json!({"type":"configure_external_tools","tools":[]}))
                    .await
                    .is_err()
                {
                    if let Some(host) = host.upgrade() {
                        host.terminate().await;
                    }
                }
            }
            result
        }
    };
    control.cancel();
    // Runtime shutdown joins owned transports/process trees off the async lane.
    let _ = tokio::task::spawn_blocking(move || runtime.shutdown_connections()).await;
    let external_result = match external {
        Some(external) => external.shutdown().await,
        None => Ok(()),
    };
    drop(file);
    result.and(external_result)
}

struct ServeContext {
    runtime: Arc<McpRuntime>,
    context: RequestContext,
    path: PathBuf,
    descriptors: Vec<ToolDescriptor>,
    external: Option<Arc<Driver>>,
}

async fn serve(
    host: &Weak<HostSession>,
    mut events: broadcast::Receiver<Value>,
    control: &Control,
    config: ServeContext,
) -> Result<(), String> {
    let ServeContext {
        runtime,
        context,
        path,
        descriptors,
        external,
    } = config;
    let tools: HashMap<_, _> = descriptors
        .into_iter()
        .map(|t| (t.native_name.clone(), t))
        .collect();
    let mut calls = JoinSet::new();
    let mut pending: HashMap<String, Control> = HashMap::new();
    let mut closed = HashSet::new();
    let mut seen = HashSet::new();
    let result = loop {
        tokio::select! {
            biased;
            _ = control.cancelled() => break Ok(()),
            event = events.recv() => {
                let event = match event { Ok(e) => e, Err(e) => break Err(format!("MCP native event stream lost: {e}")) };
                match event["type"].as_str() {
                    Some("host_closed") => break Ok(()),
                    Some("external_tool_request_closed") => {
                        if let Some(id) = event["requestId"].as_str() {
                            if let Some(cancel) = pending.get(id) { cancel.cancel(); closed.insert(id.to_string()); }
                        }
                    }
                    Some("external_tool_request") => {
                        let Some(id) = event["requestId"].as_str().filter(|s| !s.is_empty() && s.len() <= 256) else { break Err("MCP request invalid requestId (1..256 bytes required)".into()); };
                        if seen.contains(id) { continue; } // never replay execution
                        if seen.len() >= 65_536 { break Err("MCP request deduplication capacity exceeded".into()); }
                        seen.insert(id.to_string());
                        if pending.len() >= 64 { break Err("MCP pending request capacity exceeded".into()); }
                        let cancel = Control::default().linked(control);
                        pending.insert(id.to_string(), cancel.clone());
                        let rt = runtime.clone(); let ctx = context.clone(); let path = path.clone();
                        let tool = event["toolName"].as_str().and_then(|name| tools.get(name)).cloned();
                        let arguments = event.get("arguments").cloned().unwrap_or_else(|| json!({}));
                        let id = id.to_string();
                        let external = external.clone();
                        calls.spawn(async move {
                            let result = match tool {
                                Some(tool) if external.as_ref().is_some_and(|backend| backend.contains(&tool.server)) => {
                                    external.as_ref().unwrap().call(&ctx, tool, arguments, id.clone(), &cancel).await
                                }
                                Some(tool) if crate::mcp_catalog::is_catalog(&tool) => {
                                    let cancel = cancel.clone();
                                    tokio::task::spawn_blocking(move || crate::mcp_catalog::execute(&rt, &ctx, &path, &tool, arguments, &cancel))
                                        .await.unwrap_or_else(|_| Err("MCP catalog worker failed".into()))
                                }
                                Some(tool) => {
                                    let cancel = cancel.clone();
                                    tokio::task::spawn_blocking(move || rt.call(&ctx, &json!({"configPath":path,"server":tool.server,"tool":tool.remote_name,"arguments":arguments}), &cancel).map_err(runtime_error))
                                        .await.unwrap_or_else(|_| Err("MCP call worker failed".into()))
                                }
                                None => Err("unknown MCP external tool".into()),
                            };
                            (id, result, cancel.check().is_err())
                        });
                    }
                    _ => {}
                }
            }
            completion = calls.join_next(), if !calls.is_empty() => {
                let (id, result, cancelled) = match completion { Some(Ok(v)) => v, _ => break Err("MCP call worker failed".into()) };
                pending.remove(&id);
                if closed.remove(&id) { continue; }
                let response = if cancelled { json!({"type":"external_tool_response","requestId":id,"cancelled":true}) }
                else { match result.and_then(native_result) {
                    Ok(result) => json!({"type":"external_tool_response","requestId":id,"result":result}),
                    Err(message) => json!({"type":"external_tool_response","requestId":id,"error":{"message":message}}),
                }};
                if let Err(error) = send(host, response).await { break Err(error); }
            }
        }
    };
    for cancel in pending.values() {
        cancel.cancel();
    }
    while calls.join_next().await.is_some() {}
    if result.is_err() {
        // Fail closed: outstanding native requests must not hang after overflow.
        if let Some(host) = host.upgrade() {
            host.terminate().await;
        }
    }
    result
}

fn native_result(result: Value) -> Result<Value, String> {
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .ok_or("MCP result missing content array")?;
    if content.iter().any(|c| match c["type"].as_str() {
        Some("text") => !c["text"].is_string(),
        Some("image") => !c["data"].is_string() || !c["mimeType"].is_string(),
        _ => true,
    }) {
        return Err(
            "MCP result contains unsupported native content (only text/image supported)".into(),
        );
    }
    if serde_json::to_vec(&result)
        .map_err(|e| e.to_string())?
        .len()
        > 1024 * 1024 - 4096
    {
        return Err("MCP result exceeds native 1 MiB frame limit".into());
    }
    // Preserve isError, annotations, structuredContent and unknown metadata.
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle_worker() -> Arc<Worker> {
        let control = Control::default();
        let cancel = control.clone();
        Worker::new(
            control,
            tokio::spawn(async move {
                cancel.cancelled().await;
                Ok(())
            }),
        )
    }

    #[tokio::test]
    async fn owner_bounds_admission_and_reclaims_joined_and_finished_slots() {
        let owner = McpOwner::default();
        for _ in 0..260 {
            let worker = owner.register(idle_worker).unwrap();
            worker.control.cancel();
            worker.join().await.unwrap();
        }
        for _ in 0..260 {
            let worker = owner
                .register(|| Worker::new(Control::default(), tokio::spawn(async { Ok(()) })))
                .unwrap();
            while !worker.finished.is_finished() {
                tokio::task::yield_now().await;
            }
        }
        // Completed-but-unjoined handles are polled/reaped during admission.
        for _ in 0..128 {
            owner.register(idle_worker).unwrap();
        }
        assert!(owner
            .register(|| panic!("full registry must not start work"))
            .is_err());
        owner.shutdown().await.unwrap();
        assert!(owner
            .register(|| panic!("closed owner must not start work"))
            .is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_registration_and_shutdown_cannot_resurrect_workers() {
        for _ in 0..20 {
            let owner = McpOwner::default();
            let mut tasks = JoinSet::new();
            for _ in 0..32 {
                let owner = owner.clone();
                tasks.spawn(async move { owner.register(idle_worker) });
            }
            owner.shutdown().await.unwrap();
            while let Some(result) = tasks.join_next().await {
                if let Ok(worker) = result.unwrap() {
                    assert!(worker.finished.is_finished());
                }
            }
            assert!(owner.state.lock().unwrap().workers.is_empty());
        }
    }

    #[tokio::test]
    async fn shutdown_cancels_all_before_join_and_redacts_panics() {
        let owner = McpOwner::default();
        let first_cancel = Control::default();
        let second_cancel = Control::default();
        let wait_second = second_cancel.clone();
        owner
            .register(|| {
                Worker::new(
                    first_cancel,
                    tokio::spawn(async move {
                        wait_second.cancelled().await;
                        panic!("secret provider material");
                    }),
                )
            })
            .unwrap();
        owner
            .register(|| Worker::new(second_cancel, tokio::spawn(async { Ok(()) })))
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(2), owner.shutdown())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error, "MCP binding worker failed (diagnostics withheld)");
        assert!(owner.state.lock().unwrap().workers.is_empty());
    }

    #[test]
    fn runtime_errors_withhold_remote_diagnostics() {
        for error in [
            "stdio transport finished: stderr: Authorization: Bearer secret-token",
            "invalid configuration: https://user:password@example.com/?api_key=secret",
            "remote RPC error: credential=secret",
        ] {
            assert_eq!(
                runtime_error(error.into()),
                "MCP runtime operation failed (remote diagnostics withheld)"
            );
        }
        assert_eq!(
            native_result(json!({})).unwrap_err(),
            "MCP result missing content array"
        );
    }

    #[test]
    fn names_and_native_result_validation() {
        let name = tool_name("ab", "c/?!");
        assert_eq!(name.len(), 64);
        assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        assert_ne!(tool_name("ab", "c"), tool_name("a", "bc"));
        let result = json!({"content":[],"isError":true,"structuredContent":{"x":1},"_meta":{"trace":"kept"}});
        assert_eq!(native_result(result.clone()).unwrap(), result);
        assert!(native_result(json!({"content":[{"type":"resource","resource":{}}]})).is_err());
        assert!(
            native_result(json!({"content":[{"type":"text","text":"x".repeat(1024*1024)}]}))
                .is_err()
        );
    }
}
