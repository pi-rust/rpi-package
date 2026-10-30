//! SDK context/lifecycle mapping and legacy library API. Runtime state lives in core.
use crate::{Control, DiscoveredServer, ResourceUpdate, ServerInfo};
use rpi_mcp_core::{McpRuntime, RequestContext};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

fn instance() -> &'static McpRuntime {
    static INSTANCE: OnceLock<McpRuntime> = OnceLock::new();
    INSTANCE.get_or_init(McpRuntime::new)
}
fn context(params: &Value) -> RequestContext {
    let scope = host_scope(params);
    remember_scope(&scope);
    RequestContext {
        scope,
        cwd: host_cwd(params),
    }
}
pub(crate) fn call(params: &Value, control: &Control) -> Result<Value, String> {
    instance().call(&context(params), params, control)
}
pub(crate) fn list(params: &Value, control: &Control) -> Result<Value, String> {
    instance().list(&context(params), params, control)
}
pub(crate) fn resources(params: &Value, control: &Control) -> Result<Value, String> {
    instance().resources(&context(params), params, control)
}
pub fn shutdown_connections() {
    instance().shutdown_connections();
}
fn take_pending_updates(scope: Option<&str>) -> Vec<ResourceUpdate> {
    instance().take_resource_updates(scope)
}
pub(crate) fn flush_resource_updates(
    append: impl FnOnce(Value) -> Result<(), String>,
) -> Result<(), String> {
    instance().deliver_resource_updates(current_scope, |updates| {
        let mut text = String::from("🔔 MCP resource update(s):");
        for update in updates {
            text.push_str(&format!(
                "

--- {} (server {})
{}",
                update.uri, update.key, update.text
            ));
        }
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        append(
            json!({"kind":"custom","role":"custom","content":[{"type":"text","text":text}],
            "data":{"customType":"mcp-resources","display":true},"timestamp":timestamp}),
        )
    })
}
/// The session scope of the most recent tool call.
///
/// Fallback for older embedders without the SDK session environment variable.
/// Current hosts publish the active session on every hot switch, even when no
/// MCP tool has been called in the new session.
static CURRENT_SCOPE: OnceLock<Mutex<Option<String>>> = OnceLock::new();
fn current_scope_slot() -> &'static Mutex<Option<String>> {
    CURRENT_SCOPE.get_or_init(|| Mutex::new(None))
}
fn remember_scope(scope: &str) {
    *current_scope_slot()
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Some(scope.to_string());
}
pub(crate) fn current_scope() -> Option<String> {
    if let Some(id) = std::env::var_os(rpi_plugin_sdk::SESSION_ID_ENV) {
        // A present but invalid id must fail closed, never reuse the old scope
        // or fall back to the unscoped queue drain.
        return Some(
            id.to_str()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(|id| format!("session:{id}"))
                .unwrap_or_default(),
        );
    }
    current_scope_slot()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
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
            capabilities: value["capabilities"].clone(),
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

/// `mcp_resource`'s actions against a configured server (`list`/`read`/
/// `subscribe`/`unsubscribe`), for embedders and integration tests.
pub fn resources_from_config(
    config_path: &str,
    server_name: &str,
    action: &str,
    uri: Option<&str>,
) -> Result<Value, String> {
    let mut params = json!({"configPath":config_path,"server":server_name,"action":action});
    if let Some(uri) = uri {
        params["uri"] = json!(uri);
    }
    resources(&params, &Control::default())
}

/// Drain the resource updates queued for `scope` — the same queue the `TurnEnd`
/// injection path consumes. `None` drains everything (a host with no session id).
pub fn take_resource_updates(scope: Option<&str>) -> Vec<ResourceUpdate> {
    take_pending_updates(scope)
}
