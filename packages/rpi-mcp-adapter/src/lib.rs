#![forbid(unsafe_code)]

mod config;
mod connection;
mod control;
mod http;
mod jsonrpc;
mod kit;
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
pub use rpi_mcp_core::{DiscoveredServer, McpRuntime, RequestContext, ResourceUpdate};
pub use runtime::{
    call_from_config, call_from_config_with_timeout, close_from_config, discover_from_config,
    resources_from_config, shutdown_connections, take_resource_updates,
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

static RUNTIME: std::sync::OnceLock<rpi_plugin_sdk::PluginApi> = std::sync::OnceLock::new();

/// Dispatch a host runtime action (used to append the resource-update message).
fn call_runtime(id: rpi_plugin_sdk::RuntimeActionId, args: Value) -> Result<Value, String> {
    let runtime = *RUNTIME.get().ok_or("mcp runtime unavailable")?;
    let mut out = rpi_plugin_sdk::StbString::empty();
    // Bind the JSON to a local: `StbStringRef` borrows it for the call, so an
    // inline `&args.to_string()` would drop the buffer immediately and hand the
    // host a dangling pointer.
    let input_json = args.to_string();
    let input = rpi_plugin_sdk::StbStringRef::from_str(&input_json);
    let rc = (runtime.runtime_action)(id.into(), input, &mut out, runtime.user_data);
    let text = out.to_string_lossy();
    (runtime.free_string)(out);
    if rc != 0 {
        return Err(if text.is_empty() {
            "runtime action failed".into()
        } else {
            text
        });
    }
    if text.trim().is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_str(&text).map_err(|e| format!("invalid runtime response: {e}"))
    }
}

fn mcp_resource(params: &Value, control: &Control) -> Result<Value, String> {
    Ok(kit::text_result(
        runtime::resources(params, control)?.to_string(),
    ))
}

/// TurnEnd: surface any resource updates read since the last turn. TurnEnd is a
/// turn boundary, so the appended message never splits an assistant `tool_calls`
/// from its `toolResult`s (the HTTP 400 that motivated this timing).
extern "C" fn turn_end(_: rpi_plugin_sdk::StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    match std::panic::catch_unwind(|| {
        runtime::flush_resource_updates(|message| {
            call_runtime(
                rpi_plugin_sdk::RuntimeActionId::AppendEntry,
                json!({"message": message}),
            )
            .map(|_| ())
        })
    }) {
        Ok(Ok(())) => 0,
        _ => 1,
    }
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
tool_entry!(resource_execute, mcp_resource);
extern "C" fn tool_poll(
    handle: rpi_plugin_sdk::StepHandle,
    callback: Option<rpi_plugin_sdk::ToolPartialCb>,
    data: *mut std::ffi::c_void,
) -> rpi_plugin_sdk::StepResult {
    kit::poll(handle, callback, data)
}
extern "C" fn tool_cancel(handle: rpi_plugin_sdk::StepHandle) {
    kit::cancel(handle)
}
extern "C" fn tool_destroy(handle: rpi_plugin_sdk::StepHandle) {
    kit::destroy(handle)
}
extern "C" fn session_shutdown(
    _: rpi_plugin_sdk::StablePluginEvent,
    _: *mut std::ffi::c_void,
) -> i32 {
    std::panic::catch_unwind(shutdown_connections)
        .map(|_| 0)
        .unwrap_or(1)
}
// The SDK generates a safe C export that forwards the host-owned API pointer
// to its unsafe registration helper. Keep that SDK-macro lint exception local.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
mod plugin_entrypoint {
    use super::*;
    // The SDK entrypoint validates the API and contains registration panics.
    rpi_plugin_sdk::export_plugin!(|api| {
        let Some(register) = api.register_tool else {
            return 1;
        };
        // A reload must not leave old workers or sessions owned by this DLL.
        shutdown_connections();
        let tools: [(&str, &str, &str, rpi_plugin_sdk::ToolExecuteFn); 4] = [
    ("mcp_request", "Send a raw HTTP JSON-RPC request. For managed MCP sessions use mcp_call or mcp_list.",
     r#"{"type":"object","properties":{"url":{"type":"string"},"method":{"type":"string"},"params":{},"id":{"type":["string","integer"]},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":300}},"required":["url","method"]}"#, request_execute),
    ("mcp_call", "Call a tool using a persistent stdio or HTTP MCP connection. Use mcp_list to discover tool names and argument schemas. Supply configPath and server, or url.",
     r#"{"type":"object","properties":{"url":{"type":"string"},"configPath":{"type":"string"},"server":{"type":"string"},"tool":{"type":"string"},"arguments":{"type":"object"},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":300}},"required":["tool"],"oneOf":[{"required":["configPath","server"],"not":{"required":["url"]}},{"required":["url"],"not":{"anyOf":[{"required":["configPath"]},{"required":["server"]}]}}]}"#, call_execute),
    ("mcp_list", "List configured MCP servers, or discover a server's tools and argument schemas. Set action=close to release a server connection. Supply configPath, configPath/server, or url.",
     r#"{"type":"object","properties":{"url":{"type":"string"},"configPath":{"type":"string"},"server":{"type":"string"},"action":{"type":"string","enum":["list","close"]},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":300}},"oneOf":[{"required":["configPath"],"not":{"required":["url"]}},{"required":["url"],"not":{"anyOf":[{"required":["configPath"]},{"required":["server"]}]}}]}"#, list_execute),
    ("mcp_resource", "List, read, subscribe to, or unsubscribe from an MCP server's resources. Subscribing makes the server push resources/updated notifications, which are read and shown to you at the next turn. Supply configPath+server, or url.",
     r#"{"type":"object","properties":{"url":{"type":"string"},"configPath":{"type":"string"},"server":{"type":"string"},"action":{"type":"string","enum":["list","read","subscribe","unsubscribe"]},"uri":{"type":"string"},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":300}},"oneOf":[{"required":["configPath"],"not":{"required":["url"]}},{"required":["url"],"not":{"anyOf":[{"required":["configPath"]},{"required":["server"]}]}}]}"#, resource_execute),
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
        // Remember the host API so the TurnEnd handler can append resource updates.
        let _ = RUNTIME.set(*api);
        if let Some(register) = api.register_event_handler {
            // SessionShutdown is a marker without session identity: release all
            // connections in this extension instance.
            if register(
                rpi_plugin_sdk::EventTag::SessionShutdown,
                session_shutdown,
                std::ptr::null_mut(),
            ) != 0
            {
                return 1;
            }
            // TurnEnd: show resource updates read since the last turn (a safe boundary).
            if register(
                rpi_plugin_sdk::EventTag::TurnEnd,
                turn_end,
                std::ptr::null_mut(),
            ) != 0
            {
                return 1;
            }
        }
        0
    });
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
