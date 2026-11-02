//! ACP-only configuration conversion. Never format client values in errors.
use rpi_extcore::external_tools::{HttpToolServer, StdioToolServer, ToolServer};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

const MAX_SERVERS: usize = 16;
const MAX_NAME: usize = 200;
const MAX_STRING: usize = 16 * 1024;
const MAX_ARGS: usize = 128;
const MAX_ENV: usize = 128;
const MAX_CONFIG_BYTES: usize = 256 * 1024;

/// None means omitted (retain the config of an already attached session).
/// Unknown extra ACP metadata is harmless; transport-bearing fields are not.
pub(crate) fn parse(
    params: &Map<String, Value>,
    cwd: &Path,
) -> Result<Option<Vec<ToolServer>>, String> {
    let Some(value) = params.get("mcpServers") else {
        return Ok(None);
    };
    let servers = value.as_array().ok_or("mcpServers must be an array")?;
    if servers.len() > MAX_SERVERS {
        return Err("mcpServers exceeds 16 servers".into());
    }
    // Count the encoded form without allocating a second, possibly huge secret buffer.
    let mut encoded = EncodedLimit(0);
    serde_json::to_writer(&mut encoded, value)
        .map_err(|_| "mcpServers exceeds 256 KiB encoded configuration".to_string())?;
    if !cwd.is_absolute() || !cwd.is_dir() {
        return Err("cwd must be an existing absolute directory".into());
    }
    let mut names = HashSet::new();
    let mut result = Vec::with_capacity(servers.len());
    for server in servers {
        let server = server
            .as_object()
            .ok_or("mcpServers entries must be objects")?;
        // HTTP must have an explicit type; only stdio may be implicit.
        let transport = match server.get("type") {
            None => "stdio",
            Some(Value::String(kind)) if kind == "stdio" || kind == "http" => kind.as_str(),
            _ => return Err("invalid MCP transport".into()),
        };
        if let Some(kind) = server.get("transport") {
            if kind.as_str() != Some(transport) {
                return Err("conflicting or invalid MCP transport".into());
            }
        }
        if transport == "stdio" && (server.contains_key("url") || server.contains_key("headers")) {
            return Err("stdio MCP server cannot contain URL or headers".into());
        }
        if transport == "http"
            && ["command", "args", "env"]
                .iter()
                .any(|key| server.contains_key(*key))
        {
            return Err("HTTP MCP server cannot contain stdio fields".into());
        }
        let name = string(
            server.get("name"),
            MAX_NAME,
            false,
            "invalid MCP server name",
        )?;
        if !names.insert(name.to_owned()) {
            return Err("duplicate MCP server name".into());
        }
        if transport == "http" {
            let url = string(server.get("url"), MAX_STRING, false, "invalid MCP HTTP URL")?;
            let entries = server
                .get("headers")
                .and_then(Value::as_array)
                .ok_or("MCP headers must be an array")?;
            if entries.len() > 128 {
                return Err("MCP headers exceeds 128 entries".into());
            }
            let mut headers = BTreeMap::new();
            for entry in entries {
                let entry = entry
                    .as_object()
                    .ok_or("MCP header entries must be objects")?;
                let key = string(
                    entry.get("name"),
                    MAX_STRING,
                    false,
                    "invalid MCP header name",
                )?
                .to_ascii_lowercase();
                let value = string(
                    entry.get("value"),
                    MAX_STRING,
                    true,
                    "invalid MCP header value",
                )?;
                // Check before inserting; core owns token/control/forbidden checks.
                if headers.contains_key(&key) {
                    return Err("duplicate MCP header name".into());
                }
                headers.insert(key, value.to_owned());
            }
            let target = ToolServer::Http(HttpToolServer {
                name: name.to_owned(),
                url: url.to_owned(),
                headers,
            });
            target
                .validate()
                .map_err(|_| "invalid MCP HTTP configuration".to_owned())?;
            result.push(target);
            continue;
        }
        let command = string(
            server.get("command"),
            MAX_STRING,
            false,
            "invalid MCP command",
        )?;
        let command = if command.contains(['/', '\\']) && !Path::new(command).is_absolute() {
            // ACP clients can use either separator. Normalize for the local platform.
            let relative = command.replace(['/', '\\'], std::path::MAIN_SEPARATOR_STR);
            cwd.join(relative)
                .into_os_string()
                .into_string()
                .map_err(|_| "invalid MCP command path")?
        } else {
            command.to_owned() // Bare names deliberately retain PATH lookup.
        };
        if command.len() > MAX_STRING {
            return Err("resolved MCP command path is too long".into());
        }
        let args = server
            .get("args")
            .and_then(Value::as_array)
            .ok_or("MCP args must be an array")?;
        if args.len() > MAX_ARGS {
            return Err("MCP args exceeds 128 entries".into());
        }
        let args = args
            .iter()
            .map(|arg| {
                string(Some(arg), MAX_STRING, true, "invalid MCP argument").map(str::to_owned)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let entries = server
            .get("env")
            .and_then(Value::as_array)
            .ok_or("MCP env must be an array")?;
        if entries.len() > MAX_ENV {
            return Err("MCP env exceeds 128 entries".into());
        }
        let mut env = BTreeMap::new();
        for entry in entries {
            let entry = entry.as_object().ok_or("MCP env entries must be objects")?;
            let name = string(entry.get("name"), MAX_NAME, false, "invalid MCP env name")?;
            if name.contains('=') {
                return Err("invalid MCP env name".into());
            }
            let value = string(
                entry.get("value"),
                MAX_STRING,
                true,
                "invalid MCP env value",
            )?;
            if env.insert(name.to_owned(), value.to_owned()).is_some() {
                return Err("duplicate MCP env name".into());
            }
        }
        result.push(ToolServer::Stdio(StdioToolServer {
            name: name.to_owned(),
            command,
            args,
            env,
        }));
    }
    Ok(Some(result))
}

fn string<'a>(
    value: Option<&'a Value>,
    max: usize,
    empty: bool,
    error: &str,
) -> Result<&'a str, String> {
    value
        .and_then(Value::as_str)
        .filter(|text| {
            text.len() <= max && !text.contains('\0') && (empty || !text.trim().is_empty())
        })
        .ok_or_else(|| error.to_owned())
}

struct EncodedLimit(usize);
impl std::io::Write for EncodedLimit {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        if self.0 > MAX_CONFIG_BYTES {
            return Err(std::io::Error::other("configuration size limit"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stdio(server: &ToolServer) -> &StdioToolServer {
        match server {
            ToolServer::Stdio(server) => server,
            _ => panic!("expected stdio"),
        }
    }
    fn server() -> Value {
        json!({"name":"fixture", "command":"fixture-bin", "args":["", "--flag"],
            "env":[{"name":"TOKEN", "value":"secret-canary"}], "_meta":{"ignored":true}})
    }
    fn parse_value(value: Value) -> Result<Option<Vec<ToolServer>>, String> {
        parse(
            &json!({"mcpServers":value}).as_object().unwrap().clone(),
            &std::env::current_dir().unwrap(),
        )
    }
    fn rejects(value: Value) {
        let error = match parse_value(value) {
            Err(error) => error,
            Ok(_) => panic!("accepted invalid config"),
        };
        assert!(!error.contains("secret-canary"));
    }

    #[test]
    fn omitted_and_empty_are_distinct_and_metadata_is_ignored() {
        assert!(parse(&Map::new(), Path::new("not-used")).unwrap().is_none());
        assert!(parse_value(json!([])).unwrap().unwrap().is_empty());
        let servers = parse_value(json!([server()])).unwrap().unwrap();
        assert_eq!(stdio(&servers[0]).command, "fixture-bin");
        assert_eq!(stdio(&servers[0]).args, ["", "--flag"]);
        assert_eq!(stdio(&servers[0]).env["TOKEN"], "secret-canary");
    }

    #[test]
    fn command_paths_are_anchored_to_explicit_cwd() {
        let cwd = std::env::current_dir().unwrap();
        for relative in ["./bin/mcp", ".\\bin\\mcp"] {
            let mut value = server();
            value["command"] = json!(relative);
            let servers = parse_value(json!([value])).unwrap().unwrap();
            assert_eq!(
                Path::new(&stdio(&servers[0]).command),
                cwd.join("./bin/mcp")
            );
        }
        let mut value = server();
        value["command"] = json!(cwd.join("absolute-mcp"));
        let servers = parse_value(json!([value])).unwrap().unwrap();
        assert_eq!(
            Path::new(&stdio(&servers[0]).command),
            cwd.join("absolute-mcp")
        );
        assert!(parse(
            json!({"mcpServers":[]}).as_object().unwrap(),
            Path::new("relative")
        )
        .is_err());
    }

    #[test]
    fn only_stdio_transport_is_accepted() {
        for field in ["type", "transport"] {
            for transport in [
                json!("http"),
                json!("sse"),
                json!("other"),
                Value::Null,
                json!({"type":"stdio"}),
            ] {
                let mut value = server();
                value[field] = transport;
                rejects(json!([value]));
            }
            let mut value = server();
            value[field] = json!("stdio");
            assert!(parse_value(json!([value])).is_ok());
        }
        for field in ["url", "headers"] {
            let mut value = server();
            value[field] = json!("secret-canary");
            rejects(json!([value]));
        }
    }

    #[test]
    fn malformed_null_duplicate_and_nul_configs_fail_without_secrets() {
        for value in [
            Value::Null,
            json!({}),
            json!([null]),
            json!([server(), server()]),
        ] {
            rejects(value);
        }
        for field in ["name", "command", "args", "env"] {
            for invalid in [Value::Null, json!(42), json!("secret-canary\0")] {
                let mut value = server();
                value[field] = invalid;
                rejects(json!([value]));
            }
            let mut value = server();
            value.as_object_mut().unwrap().remove(field);
            rejects(json!([value]));
        }
        for field in ["name", "command"] {
            let mut value = server();
            value[field] = json!(" \t");
            rejects(json!([value]));
        }
        for env in [
            json!({"TOKEN":"secret-canary"}),
            json!([null]),
            json!([{"name":"TOKEN"}]),
            json!([{"name":"A=B","value":"secret-canary"}]),
            json!([{"name":"T\0","value":"secret-canary"}]),
            json!([{"name":"T","value":"secret-canary\0"}]),
            json!([{"name":"T","value":7}]),
            json!([{"name":"T","value":"secret-canary"},{"name":"T","value":"secret-canary"}]),
        ] {
            let mut value = server();
            value["env"] = env;
            rejects(json!([value]));
        }
        let mut value = server();
        value["args"] = json!(["secret-canary\0"]);
        rejects(json!([value]));
    }

    fn http() -> Value {
        json!({"type":"http", "name":"network", "url":"http://127.0.0.1:9876/mcp?token=secret-canary",
            "headers":[{"name":"Authorization", "value":"secret-canary"}], "_meta":{"ok":true}})
    }

    #[test]
    fn http_shape_and_header_canonicalization() {
        let first = parse_value(json!([http()])).unwrap().unwrap();
        let mut value = http();
        value["transport"] = json!("http");
        value["headers"][0]["name"] = json!("aUtHoRiZaTiOn");
        let second = parse_value(json!([value])).unwrap().unwrap();
        assert!(first == second);
        let ToolServer::Http(http_server) = &first[0] else {
            panic!("expected HTTP")
        };
        assert_eq!(http_server.headers["authorization"], "secret-canary");
        for field in ["command", "args", "env"] {
            let mut value = http();
            value[field] = Value::Null;
            rejects(json!([value]));
        }
        for field in ["name", "url", "headers"] {
            for invalid in [Value::Null, json!(42)] {
                let mut value = http();
                value[field] = invalid;
                rejects(json!([value]));
            }
            let mut value = http();
            value.as_object_mut().unwrap().remove(field);
            rejects(json!([value]));
        }
        for (kind, transport) in [
            ("http", "stdio"),
            ("stdio", "http"),
            ("sse", "sse"),
            ("other", "http"),
        ] {
            let mut value = http();
            value["type"] = json!(kind);
            value["transport"] = json!(transport);
            rejects(json!([value]));
        }
        let mut value = http();
        value.as_object_mut().unwrap().remove("type");
        value["transport"] = json!("http");
        rejects(json!([value]));
        let mut value = http();
        value["headers"] = json!([]);
        assert!(parse_value(json!([value])).is_ok());
        let mut value = server();
        value["env"] = json!([{"name":"EMPTY", "value":""}]);
        assert!(parse_value(json!([value])).is_ok());
    }

    #[test]
    fn http_url_security_is_validated_before_contact_with_fixed_errors() {
        for url in [
            "",
            "relative",
            "//127.0.0.1/mcp",
            "file:///secret-canary",
            "ftp://localhost/mcp",
            "http:///mcp",
            "http://",
            "http://user:secret-canary@localhost/mcp",
            "http://@localhost/mcp",
            "http://localhost/mcp#secret-canary",
            "http://localhost/#",
            " http://localhost/mcp",
            "http://localhost/mcp ",
            "http://local
host/mcp",
            "http://localhost/	secret-canary",
            "http://localhost:bad/mcp",
            "http://[broken/mcp",
        ] {
            let mut value = http();
            value["url"] = json!(url);
            rejects(json!([value]));
        }
        for url in [
            "http://localhost/mcp",
            "https://localhost:443/mcp?key=secret-canary",
            "http://[::1]:123/mcp",
        ] {
            let mut value = http();
            value["url"] = json!(url);
            assert!(parse_value(json!([value])).is_ok());
        }
    }

    #[test]
    fn http_header_security_and_bounds() {
        for name in [
            "Host",
            "Accept",
            "Content-Type",
            "Content-Length",
            "Content-Encoding",
            "Accept-Encoding",
            "Mcp-Session-Id",
            "MCP-Protocol-Version",
            "Last-Event-ID",
            "Connection",
            "Keep-Alive",
            "Proxy-Connection",
            "Transfer-Encoding",
            "TE",
            "Trailer",
            "Upgrade",
            "Expect",
            "Proxy-Authorization",
            "Proxy-Authenticate",
            "",
            "bad name",
            "bad:name",
            "bad	name",
            "näme",
        ] {
            for name in [name.to_owned(), name.to_ascii_uppercase()] {
                let mut value = http();
                value["headers"] = json!([{"name":name,"value":"secret-canary"}]);
                rejects(json!([value]));
            }
        }
        for headers in [
            json!({}),
            json!([null]),
            json!([{"name":"X-Key"}]),
            json!([{"value":"secret-canary"}]),
            json!([{"name":7,"value":"secret-canary"}]),
            json!([{"name":"X-Key","value":7}]),
            json!([{"name":"X-Key","value":"a"},{"name":"x-KEY","value":"b"}]),
            json!([{"name":"x".repeat(MAX_STRING + 1),"value":""}]),
            json!([{"name":"X-Key","value":"x".repeat(MAX_STRING + 1)}]),
        ] {
            let mut value = http();
            value["headers"] = headers;
            rejects(json!([value]));
        }
        for control in (0u8..32).chain([127]) {
            let mut value = http();
            value["headers"][0]["value"] = json!(format!("secret-canary{}", char::from(control)));
            rejects(json!([value]));
        }
        let mut value = http();
        value["headers"] = json!((0..128)
            .map(|i| json!({"name":format!("x-{i}"),"value":""}))
            .collect::<Vec<_>>());
        assert!(parse_value(json!([value.clone()])).is_ok());
        value["headers"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"extra","value":""}));
        rejects(json!([value]));
        let mut value = http();
        value["headers"] = json!((0..17)
            .map(|i| json!({"name":format!("x-{i}"),"value":"x".repeat(MAX_STRING)}))
            .collect::<Vec<_>>());
        rejects(json!([value]));
    }

    #[test]
    fn counts_strings_and_encoded_size_are_bounded() {
        let mut servers = Vec::new();
        for index in 0..MAX_SERVERS {
            let mut value = server();
            value["name"] = json!(format!("s{index}"));
            servers.push(value);
        }
        assert!(parse_value(json!(servers)).is_ok());
        let mut extra = server();
        extra["name"] = json!("extra");
        servers.push(extra);
        rejects(json!(servers));
        for (field, value) in [
            ("name", json!("n".repeat(MAX_NAME + 1))),
            ("command", json!("c".repeat(MAX_STRING + 1))),
            ("args", json!(vec![""; MAX_ARGS + 1])),
            ("args", json!(["a".repeat(MAX_STRING + 1)])),
            (
                "env",
                json!(vec![
                    json!({"name":"T","value":"secret-canary"});
                    MAX_ENV + 1
                ]),
            ),
            (
                "env",
                json!([{"name":"T","value":"s".repeat(MAX_STRING + 1)}]),
            ),
            (
                "env",
                json!([{"name":"n".repeat(MAX_NAME + 1),"value":"secret-canary"}]),
            ),
        ] {
            let mut s = server();
            s[field] = value;
            rejects(json!([s]));
        }
        let mut s = server();
        s["args"] = json!(vec!["a".repeat(MAX_STRING); 16]);
        rejects(json!([s]));
        let mut s = server();
        s["_meta"] = json!("x".repeat(MAX_CONFIG_BYTES));
        rejects(json!([s]));
    }
}
