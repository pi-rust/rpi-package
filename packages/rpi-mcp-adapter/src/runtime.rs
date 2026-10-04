//! Configuration-driven one-shot MCP runtime for the stable dispatcher.

use crate::connection::{
    initialize_request, parse_initialize, parse_tools_call, parse_tools_list, tools_call_request,
    tools_list_request,
};
use crate::transport::{http_request, StdioTransport};
use crate::{McpConfig, McpServerConfig, ServerInfo, Tool};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredServer {
    pub name: String,
    pub info: ServerInfo,
    pub tools: Vec<Tool>,
}

pub fn discover_from_config(
    config_path: &str,
    server_name: &str,
) -> Result<DiscoveredServer, String> {
    let text = std::fs::read_to_string(config_path)
        .map_err(|error| format!("read MCP config {config_path:?}: {error}"))?;
    let config = McpConfig::parse(&text)?;
    let server = config
        .servers
        .get(server_name)
        .ok_or_else(|| format!("MCP server {server_name:?} is not configured"))?;
    if !is_enabled(server) {
        return Err(format!("MCP server {server_name:?} is disabled"));
    }
    let (info, tools) = match server {
        McpServerConfig::Stdio(server) => {
            let mut transport = StdioTransport::start(
                &server.command,
                &server.args,
                &server.env,
                server.cwd.as_deref(),
            )?;
            let info = initialize_with_stdio(&mut transport)?;
            let tools = parse_tools_list(transport.request(&tools_list_request(2))?)?;
            (info, tools)
        }
        McpServerConfig::Http(server) => {
            let config = McpServerConfig::Http(server.clone());
            let info = parse_initialize(http_request(
                &config,
                &initialize_request(1, "rpi-mcp-adapter", env!("CARGO_PKG_VERSION")),
                server.timeout,
            )?)?;
            let tools = parse_tools_list(http_request(
                &config,
                &tools_list_request(2),
                server.timeout,
            )?)?;
            (info, tools)
        }
    };
    Ok(DiscoveredServer {
        name: server_name.to_string(),
        info,
        tools,
    })
}

pub fn call_from_config(
    config_path: &str,
    server_name: &str,
    tool_name: &str,
    arguments: Value,
) -> Result<String, String> {
    if tool_name.trim().is_empty() || tool_name.len() > 200 {
        return Err("MCP tool must contain 1-200 characters".into());
    }
    let text = std::fs::read_to_string(config_path)
        .map_err(|error| format!("read MCP config {config_path:?}: {error}"))?;
    let config = McpConfig::parse(&text)?;
    let server = config
        .servers
        .get(server_name)
        .ok_or_else(|| format!("MCP server {server_name:?} is not configured"))?;
    if !is_enabled(server) {
        return Err(format!("MCP server {server_name:?} is disabled"));
    }

    match server {
        McpServerConfig::Stdio(server) => {
            let mut transport = StdioTransport::start(
                &server.command,
                &server.args,
                &server.env,
                server.cwd.as_deref(),
            )?;
            let info = initialize_with_stdio(&mut transport)?;
            let _ = info;
            let tools = parse_tools_list(transport.request(&tools_list_request(2))?)?;
            ensure_tool(&tools, tool_name)?;
            parse_tools_call(transport.request(&tools_call_request(3, tool_name, arguments))?)
        }
        McpServerConfig::Http(server) => {
            let config = McpServerConfig::Http(server.clone());
            let info = parse_initialize(http_request(
                &config,
                &initialize_request(1, "rpi-mcp-adapter", env!("CARGO_PKG_VERSION")),
                server.timeout,
            )?)?;
            let _ = info;
            let tools = parse_tools_list(http_request(
                &config,
                &tools_list_request(2),
                server.timeout,
            )?)?;
            ensure_tool(&tools, tool_name)?;
            parse_tools_call(http_request(
                &config,
                &tools_call_request(3, tool_name, arguments),
                server.timeout,
            )?)
        }
    }
}

fn initialize_with_stdio(transport: &mut StdioTransport) -> Result<crate::ServerInfo, String> {
    let info = parse_initialize(transport.request(&initialize_request(
        1,
        "rpi-mcp-adapter",
        env!("CARGO_PKG_VERSION"),
    ))?)?;
    transport.notify(&crate::initialized_notification())?;
    Ok(info)
}

fn ensure_tool(tools: &[crate::Tool], name: &str) -> Result<(), String> {
    if tools.iter().any(|tool| tool.name == name) {
        Ok(())
    } else {
        Err(format!("MCP server does not expose tool {name:?}"))
    }
}

fn is_enabled(server: &McpServerConfig) -> bool {
    match server {
        McpServerConfig::Stdio(server) => server.enabled,
        McpServerConfig::Http(server) => server.enabled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_config_and_tool() {
        let error = call_from_config("missing-mcp.json", "docs", "read", Value::Null).unwrap_err();
        assert!(error.contains("read MCP config"));
    }
}
