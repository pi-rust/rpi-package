//! Provider-neutral MCP connection protocol helpers.
//!
//! Transport ownership stays in this package. These helpers deliberately only
//! construct and validate MCP messages, so stdio/HTTP can share the same flow.

use crate::jsonrpc::{self, Request, Response, Tool};
use serde_json::{json, Value};

pub const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    pub protocol_version: String,
    pub name: String,
    pub version: String,
}

pub fn initialize_request(id: u64, client_name: &str, client_version: &str) -> Request {
    Request::new(
        id,
        "initialize",
        Some(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": client_name, "version": client_version}
        })),
    )
}

pub fn initialized_notification() -> jsonrpc::Notification {
    jsonrpc::Notification {
        jsonrpc: "2.0".into(),
        method: "notifications/initialized".into(),
        params: None,
    }
}

pub fn parse_initialize(response: Response) -> Result<ServerInfo, String> {
    let result = response_result(response)?;
    let protocol_version = result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| "MCP initialize response has no protocolVersion".to_string())?;
    if !["2024-11-05", "2025-03-26", PROTOCOL_VERSION].contains(&protocol_version) {
        return Err(format!(
            "unsupported MCP protocol version {protocol_version:?}"
        ));
    }
    if result
        .get("capabilities")
        .and_then(|v| v.get("tools"))
        .is_none()
    {
        return Err("MCP server does not advertise the tools capability".into());
    }
    let info = result
        .get("serverInfo")
        .ok_or_else(|| "MCP initialize response has no serverInfo".to_string())?;
    Ok(ServerInfo {
        protocol_version: protocol_version.to_string(),
        name: info
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        version: info
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
    })
}

pub fn tools_list_request(id: u64) -> Request {
    Request::new(id, "tools/list", Some(json!({})))
}

pub fn parse_tools_list(response: Response) -> Result<Vec<Tool>, String> {
    jsonrpc::tool_list(&response_result(response)?)
}

pub fn tools_call_request(id: u64, name: &str, arguments: Value) -> Request {
    Request::new(
        id,
        "tools/call",
        Some(json!({"name": name, "arguments": arguments})),
    )
}

pub fn parse_tools_call(response: Response) -> Result<String, String> {
    let result = response_result(response)?;
    check_tool_result(&result)?;
    Ok(jsonrpc::call_result_text(&result))
}

pub(crate) fn check_tool_result(result: &Value) -> Result<(), String> {
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(format!(
            "MCP tool execution failed: {}",
            jsonrpc::call_result_text(result)
        ));
    }
    Ok(())
}

pub(crate) fn response_result(response: Response) -> Result<Value, String> {
    if let Some(error) = response.error {
        return Err(format!(
            "MCP JSON-RPC error {}: {}",
            error.code, error.message
        ));
    }
    response
        .result
        .ok_or_else(|| "MCP response has neither result nor error".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_initialize_and_tool_calls() {
        let initialize = initialize_request(1, "rpi-mcp", "0.1");
        assert_eq!(initialize.method, "initialize");
        assert_eq!(initialize.params.unwrap()["clientInfo"]["name"], "rpi-mcp");
        let call = tools_call_request(2, "read_file", json!({"path":"README.md"}));
        assert_eq!(call.params.unwrap()["name"], "read_file");
    }

    #[test]
    fn parses_initialize_and_reports_jsonrpc_errors() {
        let info = parse_initialize(Response {
            jsonrpc: "2.0".into(),
            id: 1,
            result: Some(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "serverInfo": {"name":"fake","version":"1"}, "capabilities":{"tools":{}}
            })),
            error: None,
        })
        .unwrap();
        assert_eq!(info.name, "fake");
        let error = parse_tools_call(Response {
            jsonrpc: "2.0".into(),
            id: 2,
            result: None,
            error: Some(crate::jsonrpc::ErrorObject {
                code: -1,
                message: "nope".into(),
                data: None,
            }),
        })
        .unwrap_err();
        assert!(error.contains("nope"));
    }
}
