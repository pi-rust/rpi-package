//! MCP transport primitives owned by the extension.
//!
//! The current adapter keeps transports synchronous because the existing RPI
//! package tool ABI is synchronous. A future connection manager can run these
//! primitives on a dedicated worker without changing the host ABI.

use crate::jsonrpc::{Notification, Request, Response};
use crate::kit::{http_client, validate_public_url};
use crate::McpServerConfig;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

#[derive(Debug)]
pub struct StdioTransport {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl StdioTransport {
    pub fn start(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        cwd: Option<&str>,
    ) -> Result<Self, String> {
        let mut command_builder = Command::new(command);
        command_builder.args(args).envs(env);
        if let Some(cwd) = cwd {
            command_builder.current_dir(cwd);
        }
        let mut child = command_builder
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("failed to start MCP stdio server: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "MCP stdin unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "MCP stdout unavailable".to_string())?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    pub fn notify(&mut self, notification: &Notification) -> Result<(), String> {
        let line = serde_json::to_string(notification)
            .map_err(|error| format!("encode MCP notification: {error}"))?;
        writeln!(self.stdin, "{line}")
            .map_err(|error| format!("write MCP notification: {error}"))?;
        self.stdin
            .flush()
            .map_err(|error| format!("flush MCP notification: {error}"))
    }

    pub fn request(&mut self, request: &Request) -> Result<Response, String> {
        let line = serde_json::to_string(request)
            .map_err(|error| format!("encode MCP request: {error}"))?;
        writeln!(self.stdin, "{line}").map_err(|error| format!("write MCP request: {error}"))?;
        self.stdin
            .flush()
            .map_err(|error| format!("flush MCP request: {error}"))?;
        let mut response = String::new();
        self.stdout
            .read_line(&mut response)
            .map_err(|error| format!("read MCP response: {error}"))?;
        if response.trim().is_empty() {
            return Err("MCP server closed stdout".into());
        }
        serde_json::from_str(response.trim())
            .map_err(|error| format!("decode MCP response: {error}"))
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn http_request(
    config: &McpServerConfig,
    request: &Request,
    timeout_seconds: u64,
) -> Result<Response, String> {
    let crate::McpServerConfig::Http(server) = config else {
        return Err("HTTP transport requires an HTTP MCP server config".into());
    };
    let url = validate_public_url(&server.url)?;
    let client = http_client(timeout_seconds)?;
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in &server.headers {
        let name = reqwest::header::HeaderName::try_from(name.as_str())
            .map_err(|error| format!("invalid MCP header name: {error}"))?;
        let value = reqwest::header::HeaderValue::try_from(value.as_str())
            .map_err(|error| format!("invalid MCP header value: {error}"))?;
        headers.insert(name, value);
    }
    let response = client
        .post(url)
        .headers(headers)
        .json(request)
        .send()
        .map_err(|error| format!("MCP HTTP request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("MCP HTTP server returned {}", response.status()));
    }
    response
        .json()
        .map_err(|error| format!("decode MCP HTTP response: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_http_for_stdio_config() {
        let config = McpServerConfig::Stdio(crate::StdioServer {
            command: "fake".into(),
            args: vec![],
            env: Default::default(),
            cwd: None,
            enabled: true,
            timeout: 1,
            exposure: Default::default(),
        });
        let error = http_request(&config, &Request::new(1, "tools/list", None), 1).unwrap_err();
        assert!(error.contains("HTTP transport"));
    }
}
