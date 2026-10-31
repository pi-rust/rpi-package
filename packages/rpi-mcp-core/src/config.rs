//! Pi-compatible MCP server configuration owned by the extension.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
pub struct McpConfig {
    #[serde(default, rename = "mcpServers")]
    pub servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum McpServerConfig {
    Stdio(StdioServer),
    Http(HttpServer),
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct StdioServer {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub exposure: Exposure,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct HttpServer {
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub exposure: Exposure,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Exposure {
    #[default]
    Codemode,
    Deferred,
    Direct,
    Hidden,
}

fn default_true() -> bool {
    true
}
fn default_timeout() -> u64 {
    60
}

impl McpConfig {
    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Self =
            serde_json::from_str(text).map_err(|error| format!("invalid mcp config: {error}"))?;
        for (name, server) in &config.servers {
            validate_name(name)?;
            match server {
                McpServerConfig::Stdio(server) if server.command.trim().is_empty() => {
                    return Err(format!("MCP server {name:?} has an empty command"));
                }
                McpServerConfig::Http(server) if server.url.trim().is_empty() => {
                    return Err(format!("MCP server {name:?} has an empty URL"));
                }
                _ => {}
            }
        }
        Ok(config)
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(format!("invalid MCP server name {name:?}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pi_mcp_servers_shape() {
        let config = McpConfig::parse(
            r#"{
          "mcpServers": {
            "docs": {"type":"stdio", "command":"node", "args":["server.js"]},
            "remote": {"url":"https://example.com/mcp", "exposure":"direct"}
          }
        }"#,
        )
        .unwrap();
        assert_eq!(config.servers.len(), 2);
        assert!(matches!(config.servers["docs"], McpServerConfig::Stdio(_)));
        assert!(matches!(config.servers["remote"], McpServerConfig::Http(_)));
    }

    #[test]
    fn rejects_invalid_server_name() {
        let error = McpConfig::parse(r#"{"mcpServers":{"bad.name":{"command":"x"}}}"#).unwrap_err();
        assert!(error.contains("invalid MCP server name"));
    }
}
