mod config;
mod connection;
mod control;
mod http;
mod jsonrpc;
mod kit;
mod process;
mod runtime;
mod transport;

pub use config::{Exposure, HttpServer, McpConfig, McpServerConfig, StdioServer};
pub use connection::{
    initialize_request, initialized_notification, parse_initialize, parse_tools_call,
    parse_tools_list, tools_call_request, tools_list_request, ServerInfo, PROTOCOL_VERSION,
};
pub use control::Control;
pub use jsonrpc::{
    call_result_text, tool_list, ErrorObject, Notification, Request, Response, Tool,
};
pub use runtime::{
    call_from_config, call_from_config_with_timeout, close_from_config, discover_from_config,
    shutdown_connections, DiscoveredServer,
};
use serde_json::{json, Value};
pub use transport::{http_request, StdioTransport};

fn mcp_call(params: &Value, control: &Control) -> Result<Value, String> {
    jsonrpc::agent_tool_result(&runtime::call(params, control)?)
}
fn mcp_list(params: &Value, control: &Control) -> Result<Value, String> {
    Ok(kit::text_result(
        runtime::list(params, control)?.to_string(),
    ))
}
fn mcp_request(params: &Value, control: &Control) -> Result<Value, String> {
    let url = kit::string_param(params, "url")?;
    let method = kit::string_param(params, "method")?;
    if method.trim().is_empty() || method.len() > 200 {
        return Err("MCP method must contain 1-200 characters".into());
    }
    let id = params.get("id").cloned().unwrap_or(json!(1));
    if !(id.is_string() || id.is_i64() || id.is_u64()) {
        return Err("MCP request id must be a string or integer".into());
    }
    let timeout = match params.get("timeoutSeconds") {
        None => 60,
        Some(value) => value
            .as_u64()
            .filter(|v| (1..=300).contains(v))
            .ok_or("timeoutSeconds must be an integer from 1 to 300")?,
    };
    let server = HttpServer {
        url,
        headers: Default::default(),
        enabled: true,
        timeout,
        exposure: Default::default(),
    };
    let mut transport = http::HttpTransport::new(&server)?;
    let result = transport.raw(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params.get("params").cloned().unwrap_or(json!({}))}), control)?;
    Ok(kit::text_result(result.to_string()))
}

macro_rules! tool_entry {
    ($execute:ident, $builder:ident) => {
        extern "C" fn $execute(
            _: rpi_plugin_sdk::StbStringRef,
            params: rpi_plugin_sdk::StbString,
            free: Option<rpi_plugin_sdk::FreeStringFn>,
        ) -> rpi_plugin_sdk::StepHandle {
            kit::execute(params, free, $builder)
        }
    };
}
tool_entry!(call_execute, mcp_call);
tool_entry!(list_execute, mcp_list);
tool_entry!(request_execute, mcp_request);
extern "C" fn tool_poll(
    handle: rpi_plugin_sdk::StepHandle,
    callback: Option<rpi_plugin_sdk::ToolPartialCb>,
    data: *mut std::ffi::c_void,
) -> rpi_plugin_sdk::StepResult {
    unsafe { kit::poll(handle, callback, data) }
}
extern "C" fn tool_cancel(handle: rpi_plugin_sdk::StepHandle) {
    unsafe { kit::cancel(handle) }
}
extern "C" fn tool_destroy(handle: rpi_plugin_sdk::StepHandle) {
    unsafe { kit::destroy(handle) }
}
extern "C" fn session_shutdown(
    _: rpi_plugin_sdk::StablePluginEvent,
    _: *mut std::ffi::c_void,
) -> i32 {
    std::panic::catch_unwind(shutdown_connections)
        .map(|_| 0)
        .unwrap_or(1)
}
/// # Safety
/// The host must pass a valid PluginApi pointer for this ABI version.
#[no_mangle]
pub unsafe extern "C" fn rpi_plugin_register(api: *const rpi_plugin_sdk::PluginApi) -> i32 {
    unsafe {
        rpi_plugin_sdk::register_entrypoint(api, |api| {
            let Some(register) = api.register_tool else {
                return 1;
            };
            // A reload must not leave old workers or sessions owned by this DLL.
            shutdown_connections();
            let tools: [(&str, &str, &str, rpi_plugin_sdk::ToolExecuteFn); 3] = [
            ("mcp_request", "Send a raw HTTP JSON-RPC request. For managed MCP sessions use mcp_call or mcp_list.",
             r#"{"type":"object","properties":{"url":{"type":"string"},"method":{"type":"string"},"params":{},"id":{"type":["string","integer"]},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":300}},"required":["url","method"]}"#, request_execute),
            ("mcp_call", "Call a tool using a persistent stdio or HTTP MCP connection. Use mcp_list to discover tool names and argument schemas. Supply configPath and server, or url.",
             r#"{"type":"object","properties":{"url":{"type":"string"},"configPath":{"type":"string"},"server":{"type":"string"},"tool":{"type":"string"},"arguments":{"type":"object"},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":300}},"required":["tool"],"oneOf":[{"required":["configPath","server"],"not":{"required":["url"]}},{"required":["url"],"not":{"anyOf":[{"required":["configPath"]},{"required":["server"]}]}}]}"#, call_execute),
            ("mcp_list", "List configured MCP servers, or discover a server's tools and argument schemas. Set action=close to release a server connection. Supply configPath, configPath/server, or url.",
             r#"{"type":"object","properties":{"url":{"type":"string"},"configPath":{"type":"string"},"server":{"type":"string"},"action":{"type":"string","enum":["list","close"]},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":300}},"oneOf":[{"required":["configPath"],"not":{"required":["url"]}},{"required":["url"],"not":{"anyOf":[{"required":["configPath"]},{"required":["server"]}]}}]}"#, list_execute),
        ];
            for (name, description, parameters, execute) in tools {
                let schema = kit::schema(name, description, parameters);
                let rc = register(
                    &schema,
                    execute,
                    tool_poll,
                    tool_cancel,
                    tool_destroy,
                    kit::plugin_free_string,
                );
                // The registrar consumes schema strings via plugin_free_string.
                // Only the stack schema container remains ours after this call.
                if rc != 0 {
                    return rc;
                }
            }
            // SessionShutdown is a marker without session identity: release all connections
            // in this extension instance, including library-scope fallback connections.
            if let Some(register) = api.register_event_handler {
                return register(
                    rpi_plugin_sdk::EventTag::SessionShutdown,
                    session_shutdown,
                    std::ptr::null_mut(),
                );
            }
            0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dispatcher_validates_parameters() {
        assert!(mcp_call(
            &json!({"url":"https://example.com/mcp"}),
            &Control::default()
        )
        .unwrap_err()
        .contains("tool"));
        assert!(mcp_call(
            &json!({"url":"http://localhost/mcp","tool":"x","arguments":[]}),
            &Control::default()
        )
        .unwrap_err()
        .contains("object"));
        assert!(mcp_call(
            &json!({"url":"http://localhost/mcp","tool":"x","timeoutSeconds":0}),
            &Control::default()
        )
        .unwrap_err()
        .contains("timeoutSeconds"));
    }
}

#[cfg(test)]
mod http_tests;
