use std::io::Read;

mod config;
mod connection;
mod jsonrpc;
mod kit;
mod runtime;
mod transport;

use crate::kit::{http_client, string_param, validate_public_url};
pub use config::{Exposure, HttpServer, McpConfig, McpServerConfig, StdioServer};
pub use connection::{
    initialize_request, initialized_notification, parse_initialize, parse_tools_call,
    parse_tools_list, tools_call_request, tools_list_request, ServerInfo, PROTOCOL_VERSION,
};
pub use jsonrpc::{
    call_result_text, tool_list, ErrorObject, Notification, Request, Response, Tool,
};
pub use runtime::{call_from_config, discover_from_config, DiscoveredServer};
use serde_json::Value;
pub use transport::{http_request, StdioTransport};

fn mcp_call(params: &Value) -> Result<String, String> {
    if let (Some(config_path), Some(server_name)) = (
        params.get("configPath").and_then(Value::as_str),
        params.get("server").and_then(Value::as_str),
    ) {
        let tool = string_param(params, "tool")?;
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));
        return call_from_config(config_path, server_name, &tool, arguments);
    }
    let url = validate_public_url(&string_param(params, "url")?)?;
    let tool = string_param(params, "tool")?;
    if tool.trim().is_empty() || tool.len() > 200 {
        return Err("MCP tool must contain 1-200 characters".into());
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    let timeout = params
        .get("timeoutSeconds")
        .and_then(Value::as_u64)
        .unwrap_or(15);
    let request = tools_call_request(1, &tool, arguments);
    let response = http_request(
        &McpServerConfig::Http(HttpServer {
            url: url.to_string(),
            headers: Default::default(),
            enabled: true,
            timeout,
            exposure: Exposure::Direct,
        }),
        &request,
        timeout,
    )?;
    parse_tools_call(response)
}

fn mcp_request(params: &Value) -> Result<String, String> {
    let url = validate_public_url(&string_param(params, "url")?)?;
    let method = string_param(params, "method")?;
    if method.trim().is_empty() || method.len() > 200 {
        return Err("MCP method must contain 1-200 characters".into());
    }
    let id = params.get("id").cloned().unwrap_or(Value::from(1));
    let call_params = params
        .get("params")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": call_params,
    });
    let timeout = params
        .get("timeoutSeconds")
        .and_then(Value::as_u64)
        .unwrap_or(15);
    let mut response = http_client(timeout)?
        .post(url)
        .json(&request)
        .send()
        .map_err(|err| format!("MCP request failed: {err}"))?;
    let status = response.status();
    let mut body = String::new();
    response
        .by_ref()
        .take(1_048_577)
        .read_to_string(&mut body)
        .map_err(|err| format!("failed to read MCP response: {err}"))?;
    if body.len() > 1_048_576 {
        return Err("MCP response exceeded the 1 MiB limit".into());
    }
    if !status.is_success() {
        return Err(format!("MCP server returned {status}: {body}"));
    }
    let json: Value = serde_json::from_str(&body)
        .map_err(|err| format!("MCP server returned invalid JSON: {err}"))?;
    Ok(json.to_string())
}

export_two_tool_plugin!(
    mcp_request,
    "mcp_request",
    "Send a bounded JSON-RPC 2.0 request to an HTTP MCP server.",
    r#"{"type":"object","properties":{"url":{"type":"string"},"method":{"type":"string"},"params":{},"id":{},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":30}},"required":["url","method"]}"#,
    mcp_call,
    "mcp_call",
    "Call a named tool on an HTTP MCP server through a stable dispatcher.",
    r#"{"type":"object","properties":{"url":{"type":"string"},"configPath":{"type":"string"},"server":{"type":"string"},"tool":{"type":"string"},"arguments":{"type":"object"},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":30}},"required":["tool"]}"#
);

#[cfg(test)]
mod dispatcher_tests {
    use super::*;

    #[test]
    fn dispatcher_requires_tool_name() {
        let error = mcp_call(&serde_json::json!({"url":"https://example.com/mcp"})).unwrap_err();
        assert!(error.contains("tool"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_private_mcp_endpoint() {
        let value = serde_json::json!({"url":"http://127.0.0.1:3000","method":"tools/list"});
        assert!(mcp_request(&value).unwrap_err().contains("not allowed"));
    }
}
