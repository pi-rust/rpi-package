//! SDK context and compatibility facade. Connections belong to this adapter runtime.
use crate::{Control, DiscoveredServer, ResourceUpdate, ServerInfo};
use rpi_mcp_core::{ClientIdentity, CoreRequestContext, McpRuntime};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
};
fn default_runtime() -> &'static McpRuntime {
    static DEFAULT: OnceLock<McpRuntime> = OnceLock::new();
    DEFAULT.get_or_init(|| {
        McpRuntime::with_identity(ClientIdentity::new(
            "rpi-mcp-adapter",
            env!("CARGO_PKG_VERSION"),
        ))
    })
}
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
fn context(params: &Value) -> CoreRequestContext {
    let scope = host_scope(params);
    remember_scope(&scope);
    CoreRequestContext {
        scope,
        cwd: host_cwd(params),
    }
}
pub(crate) fn call(params: &Value, control: &Control) -> Result<Value, String> {
    default_runtime().call(&context(params), params, control)
}
pub(crate) fn list(params: &Value, control: &Control) -> Result<Value, String> {
    default_runtime().list(&context(params), params, control)
}
pub(crate) fn resources(params: &Value, control: &Control) -> Result<Value, String> {
    default_runtime().resources(&context(params), params, control)
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
    rpi_mcp_core::connection::check_tool_result(&result)?;
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
pub fn take_resource_updates(scope: Option<&str>) -> Vec<ResourceUpdate> {
    default_runtime().take_resource_updates(scope)
}
pub fn shutdown_connections() {
    default_runtime().shutdown_connections()
}
pub(crate) fn flush_resource_updates(
    append: impl FnOnce(Value) -> Result<(), String>,
) -> Result<(), String> {
    flush_resource_updates_with(default_runtime(), append)
}

pub(crate) fn flush_resource_updates_with(
    runtime: &McpRuntime,
    append: impl FnOnce(Value) -> Result<(), String>,
) -> Result<(), String> {
    let scope = current_scope();
    let updates = runtime.take_resource_update_batch(scope.as_deref());
    if updates.is_empty() {
        return Ok(());
    }
    let mut text = String::from("🔔 MCP resource update(s):");
    for update in updates.updates() {
        text.push_str(&format!(
            "\n\n--- {} (server {})\n{}",
            update.uri, update.key, update.text
        ));
    }
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let result = append(json!({
        "kind": "custom",
        "role": "custom",
        "content": [{"type":"text","text": text}],
        "data": {"customType": "mcp-resources", "display": true},
        "timestamp": timestamp,
    }));
    if result.is_err() && current_scope() == scope {
        updates.retry();
    }
    result
}
