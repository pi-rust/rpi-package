//! ACP wire configuration only; runtime and external-tool forwarding belong to core.
use rpi_extcore::mcp::{Exposure, HttpServer, McpConfig, McpServerConfig, StdioServer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, HashSet}, path::Path, time::Duration};

#[derive(Clone, Default, PartialEq)]
pub struct Servers(pub(crate) BTreeMap<String, McpServerConfig>, pub(crate) BTreeMap<String, String>, pub(crate) BTreeMap<String, String>);
impl Servers {
    /// Static Authorization is authoritative, regardless of header casing.
    pub(crate) fn requires_managed_http_auth(&self) -> bool {
        self.0.values().any(|server| match server {
            McpServerConfig::Http(http) => http.enabled
                && !http.headers.keys().any(|name| name.eq_ignore_ascii_case("authorization")),
            McpServerConfig::Stdio(_) => false,
        })
    }
    pub fn config(self, scope: &str, cwd: &Path) -> McpConfig {
        McpConfig { scope: scope.into(), cwd: cwd.into(), servers: self.0, discovery_timeout: Duration::from_secs(60) }
    }
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value.get(key).and_then(Value::as_str).filter(|s| !s.contains('\0'))
        .ok_or_else(|| format!("MCP {key} must be a string without NUL"))
}
fn array<'a>(value: &'a Value, key: &str, required: bool) -> Result<&'a [Value], String> {
    match value.get(key) {
        None if !required => Ok(&[]),
        Some(Value::Null) => Ok(&[]),
        Some(Value::Array(items)) => Ok(items),
        _ => Err(format!("MCP {key} must be an array")),
    }
}
fn entries(value: &Value, key: &str, v2: bool) -> Result<BTreeMap<String, String>, String> {
    let mut result = BTreeMap::new();
    let mut seen = HashSet::new();
    for entry in array(value, key, !v2)? {
        let name = string(entry, "name")?;
        let val = string(entry, "value")?;
        let header = key == "headers";
        let valid = if header {
            !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                && val.bytes().all(|b| b == b'\t' || (b >= 32 && b != 127))
        } else { !name.is_empty() && !name.contains('=') };
        let identity = if header || cfg!(windows) { name.to_ascii_lowercase() } else { name.into() };
        if !valid || !seen.insert(identity) { return Err(format!("invalid or duplicate MCP {key} entry")); }
        result.insert(name.into(), val.into());
    }
    Ok(result)
}
/// v1 has implicit stdio; v2 requires a transport discriminator. `_meta` is opaque.
pub fn parse(params: &Value, v2: bool, acp: bool) -> Result<Servers, String> {
    let Some(value) = params.get("mcpServers") else { return Ok(Servers::default()); };
    let items = value.as_array().ok_or("mcpServers must be an array")?;
    if items.len() > 128 { return Err("too many MCP servers".into()); }
    let mut names = HashSet::new();
    let mut servers = BTreeMap::new();
    let mut external = BTreeMap::new();
    let mut registrations = BTreeMap::new();
    for server in items {
        let name = string(server, "name")?;
        if name.trim().is_empty() || !names.insert(name.to_owned()) { return Err("invalid or duplicate MCP server name".into()); }
        let transport = match server.get("type") {
            None if !v2 => "stdio",
            Some(Value::String(s)) => s.as_str(),
            _ => return Err("MCP transport type is required".into()),
        };
        let key = format!("acp_{:x}", Sha256::digest(name.as_bytes()));
        let config = match transport {
            "acp" => {
                if !acp { return Err("ACP MCP requires explicit unstable session RFD negotiation".into()); }
                let id = string(server, "serverId")?;
                if id.trim().is_empty() { return Err("MCP serverId must be nonempty".into()); }
                if registrations.insert(id.to_owned(), name.to_owned()).is_some_and(|old| old != name) {
                    return Err("MCP serverId cannot be rebound".into());
                }
                external.insert(key, id.to_owned());
                continue;
            }
            "stdio" => {
                let command = string(server, "command")?;
                if !Path::new(command).is_absolute() { return Err("MCP command must be an absolute executable path".into()); }
                let args = array(server, "args", !v2)?.iter().map(|v| v.as_str().filter(|s| !s.contains('\0')).map(str::to_owned).ok_or_else(|| "MCP args must contain strings without NUL".into())).collect::<Result<Vec<_>, String>>()?;
                McpServerConfig::Stdio(StdioServer { command: command.into(), args, env: entries(server, "env", v2)?, cwd: None, enabled: true, timeout: 60, exposure: Exposure::Direct })
            }
            "http" => {
                let url = string(server, "url")?;
                let parsed = url::Url::parse(url).map_err(|_| "invalid MCP HTTP URL")?;
                if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() || !parsed.username().is_empty() || parsed.password().is_some() || parsed.fragment().is_some() { return Err("invalid MCP HTTP URL".into()); }
                McpServerConfig::Http(HttpServer { url: url.into(), headers: entries(server, "headers", v2)?, enabled: true, timeout: 60, exposure: Exposure::Direct })
            }
            _ => return Err("unsupported MCP transport (stdio, HTTP and negotiated ACP are supported)".into()),
        };
        // ACP names are human-readable, while the neutral runtime uses safe keys.
        let key = format!("acp_{:x}", Sha256::digest(name.as_bytes()));
        servers.insert(key, config);
    }
    Ok(Servers(servers, external, registrations))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn managed_auth_only_for_enabled_http_without_static_authorization() {
        let command = std::env::current_exe().unwrap();
        for v2 in [false, true] {
            let cases = [
                (json!([]), false),
                (json!([{"type":"stdio", "name":"local", "command":command, "args":[], "env":[]}]), false),
                (json!([{"type":"acp", "name":"client", "serverId":"s"}]), false),
                (json!([{"type":"http", "name":"remote", "url":"https://mcp.example/mcp", "headers":[]}]), true),
                (json!([{"type":"http", "name":"remote", "url":"https://mcp.example/mcp", "headers":[{"name":"aUtHoRiZaTiOn", "value":"Bearer static"}]}]), false),
            ];
            for (items, expected) in cases {
                let mut servers = parse(&json!({"mcpServers":items}), v2, true).unwrap();
                assert_eq!(servers.requires_managed_http_auth(), expected);
                for config in servers.0.values_mut() {
                    if let McpServerConfig::Http(http) = config { http.enabled = false; }
                }
                assert!(!servers.requires_managed_http_auth());
            }
            let mixed = json!({"mcpServers":[
                {"type":"http", "name":"static", "url":"https://mcp.example/a", "headers":[{"name":"Authorization", "value":"Bearer static"}]},
                {"type":"http", "name":"managed", "url":"https://mcp.example/b", "headers":[]}
            ]});
            assert!(parse(&mixed, v2, false).unwrap().requires_managed_http_auth());
        }
    }
    #[test]
    fn pinned_shapes_and_validation() {
        let command = std::env::current_exe().unwrap();
        let stdio = json!({"name":"Human tools / one", "command":command,"args":[],"env":[{"name":"TOKEN","value":"secret","_meta":{"opaque":true}}],"_meta":null});
        assert!(parse(&json!({"mcpServers":[stdio.clone()]}),false,false).is_ok());
        assert!(parse(&json!({"mcpServers":[stdio.clone()]}),true,false).is_err());
        let mut v2 = stdio.clone(); v2["type"] = json!("stdio");
        assert!(parse(&json!({"mcpServers":[v2]}),true,false).is_ok());
        assert!(parse(&json!({"mcpServers":[stdio.clone(),stdio]}),false,false).is_err());
        for server in [json!({"name":"x","type":"sse"}),json!({"name":"x","type":"future"}),json!({"name":"x","type":"http","url":"https://example.com","headers":[{"name":"Authorization","value":"secret"},{"name":"authorization","value":"secret"}]}),json!({"name":"x","type":"http","url":"https://secret@example.com","headers":[]})] {
            let error = parse(&json!({"mcpServers":[server]}),false,false).err().unwrap(); assert!(!error.contains("secret"));
        }
        assert!(parse(&json!({"mcpServers":[{"type":"http","name":"x","url":"https://example.com"}]}),true,false).is_ok());
    }
}
