//! Streamable HTTP: session headers, JSON/SSE responses and cancellable I/O.
use crate::control::{Control, Deadline, MAX_MESSAGE};
use crate::jsonrpc::{Notification, Request, Response};
use crate::transport::{client_reply, decode_response, validate_message};
use crate::{HttpServer, McpServerConfig};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use std::time::Duration;

pub fn validate_mcp_url(input: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(input).map_err(|e| format!("invalid MCP URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("MCP URL must use http or https and include a host".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("use configured headers instead of embedded URL credentials".into());
    }
    Ok(url)
}

#[derive(Debug)]
pub struct HttpTransport {
    client: Client,
    runtime: tokio::runtime::Runtime,
    url: url::Url,
    session: Option<String>,
    pub(crate) version: Option<String>,
    timeout: u64,
}

impl HttpTransport {
    pub fn new(server: &HttpServer) -> Result<Self, String> {
        let url = validate_mcp_url(&server.url)?;
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in &server.headers {
            headers.insert(
                reqwest::header::HeaderName::try_from(name.as_str()).map_err(|e| e.to_string())?,
                reqwest::header::HeaderValue::try_from(value.as_str())
                    .map_err(|e| e.to_string())?,
            );
        }
        let client = Client::builder()
            .pool_max_idle_per_host(0)
            .tcp_nodelay(true)
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("rpi-mcp-adapter/0.1")
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?,
            client,
            url,
            session: None,
            version: None,
            timeout: server.timeout.clamp(1, 300),
        })
    }
    pub fn set_timeout(&mut self, seconds: u64) {
        self.timeout = seconds.clamp(1, 300);
    }
    fn post(&self, value: &Value) -> reqwest::RequestBuilder {
        let mut request = self
            .client
            .post(self.url.clone())
            .header("Accept", "application/json, text/event-stream")
            .json(value);
        if let Some(session) = &self.session {
            request = request.header("Mcp-Session-Id", session);
        }
        if let Some(version) = &self.version {
            request = request.header("MCP-Protocol-Version", version);
        }
        request
    }
    fn run<T>(
        &self,
        deadline: &Deadline,
        future: impl std::future::Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        deadline.check()?;
        self.runtime.block_on(async {
            tokio::select! {
                result = future => result,
                _ = deadline.control.cancelled() => Err("MCP request cancelled".into()),
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline.end)) =>
                    Err(format!("MCP request {} timed out after {} seconds", deadline.method, deadline.seconds)),
            }
        })
    }
    pub(crate) fn raw(&mut self, value: Value, control: &Control) -> Result<Value, String> {
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("reply")
            .to_string();
        let deadline = Deadline::new(self.timeout, &method, control);
        let id = value.get("id").cloned();
        let is_initialize = method == "initialize";
        let result = self.run(&deadline, self.exchange(&value));
        let (response, session) = match result {
            Ok(result) => result,
            Err(error) => {
                if error.contains("cancelled") || error.contains("timed out") {
                    if let Some(id) = id {
                        let cancel = json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id,"reason":error}});
                        self.runtime.block_on(async {
                            let _ = self
                                .post(&cancel)
                                .timeout(Duration::from_millis(200))
                                .send()
                                .await;
                        });
                    }
                }
                return Err(error);
            }
        };
        if is_initialize {
            self.session = session;
        }
        Ok(response)
    }
    async fn exchange(&self, value: &Value) -> Result<(Value, Option<String>), String> {
        if serde_json::to_vec(value).map_err(|e| e.to_string())?.len() > MAX_MESSAGE {
            return Err("MCP request exceeded the 16 MiB limit".into());
        }
        let mut response = self
            .post(value)
            .send()
            .await
            .map_err(|e| format!("MCP HTTP request failed: {e}"))?;
        if response.status() == StatusCode::NOT_FOUND && self.session.is_some() {
            return Err(
                "MCP HTTP session expired (404); connection will be reinitialized on the next call"
                    .into(),
            );
        }
        if !response.status().is_success() {
            return Err(format!("MCP HTTP server returned {}", response.status()));
        }
        let session = response
            .headers()
            .get("Mcp-Session-Id")
            .map(|v| v.to_str().map(String::from))
            .transpose()
            .map_err(|e| format!("invalid MCP session header: {e}"))?;
        if let Some(session) = &session {
            if session.is_empty() || !session.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
                return Err("invalid MCP session ID".into());
            }
        }
        let Some(id) = value.get("id") else {
            if response.status() != StatusCode::ACCEPTED {
                return Err("MCP notification must return HTTP 202".into());
            }
            return Ok((Value::Null, session));
        };
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if content_type == "application/json" {
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| format!("read MCP HTTP response: {e}"))?
            {
                if bytes.len() + chunk.len() > MAX_MESSAGE {
                    return Err("MCP response exceeded the 16 MiB limit".into());
                }
                bytes.extend_from_slice(&chunk);
            }
            let result: Value = serde_json::from_slice(&bytes)
                .map_err(|e| format!("decode MCP HTTP response: {e}"))?;
            check_raw_response(&result, id)?;
            return Ok((result, session));
        }
        if content_type != "text/event-stream" {
            return Err(format!("unsupported MCP HTTP content type: {content_type}"));
        }
        let mut decoder = SseDecoder::default();
        let mut total = 0usize;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("read MCP SSE response: {e}"))?
        {
            total += chunk.len();
            if total > MAX_MESSAGE {
                return Err("MCP response exceeded the 16 MiB limit".into());
            }
            for data in decoder.feed(&chunk)? {
                let message: Value = serde_json::from_str(&data)
                    .map_err(|e| format!("decode MCP SSE event: {e}"))?;
                validate_message(&message)?;
                if message.get("method").is_some() {
                    if let Some(reply) = client_reply(&message) {
                        let mut reply_request = self.post(&reply);
                        // A server may ping during initialization after assigning
                        // the session header but before InitializeResult arrives.
                        if self.session.is_none() {
                            if let Some(session) = &session {
                                reply_request = reply_request.header("Mcp-Session-Id", session);
                            }
                        }
                        let response = reply_request
                            .send()
                            .await
                            .map_err(|e| format!("reply to MCP server: {e:?}"))?;
                        if response.status() != StatusCode::ACCEPTED {
                            return Err("MCP server did not accept client reply".into());
                        }
                    }
                } else if message.get("id") == Some(id) {
                    check_raw_response(&message, id)?;
                    return Ok((message, session));
                }
            }
        }
        Err("MCP SSE stream ended before matching response".into())
    }
    pub(crate) fn request(
        &mut self,
        request: &Request,
        control: &Control,
    ) -> Result<Response, String> {
        let value = self.raw(
            serde_json::to_value(request).map_err(|e| e.to_string())?,
            control,
        )?;
        decode_response(value, request.id)
    }
    pub(crate) fn notify(
        &mut self,
        notification: &Notification,
        control: &Control,
    ) -> Result<(), String> {
        self.raw(
            serde_json::to_value(notification).map_err(|e| e.to_string())?,
            control,
        )
        .map(|_| ())
    }
    pub fn close(&mut self) {
        if let Some(session) = self.session.take() {
            self.runtime.block_on(async {
                let mut request = self
                    .client
                    .delete(self.url.clone())
                    .header("Mcp-Session-Id", session)
                    .timeout(Duration::from_secs(1));
                if let Some(version) = &self.version {
                    request = request.header("MCP-Protocol-Version", version);
                }
                let _ = request.send().await;
            });
        }
    }
}
impl Drop for HttpTransport {
    fn drop(&mut self) {
        self.close();
    }
}

fn check_raw_response(value: &Value, id: &Value) -> Result<(), String> {
    validate_message(value)?;
    if value.get("id") != Some(id) {
        return Err("MCP response ID does not match request".into());
    }
    if value.get("result").is_some() == value.get("error").is_some() {
        return Err("MCP response must contain exactly one of result or error".into());
    }
    Ok(())
}

/// Incremental SSE decoder: arbitrary byte boundaries, CR/LF/CRLF and multiline data.
#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
    data: Vec<String>,
    first: bool,
    skip_lf: bool,
}
impl SseDecoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>, String> {
        let mut events = Vec::new();
        for &byte in bytes {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if byte == b'\n' || byte == b'\r' {
                self.skip_lf = byte == b'\r';
                let mut line = String::from_utf8(std::mem::take(&mut self.buffer))
                    .map_err(|e| format!("invalid SSE UTF-8: {e}"))?;
                if !self.first {
                    self.first = true;
                    line = line.trim_start_matches('\u{feff}').into();
                }
                if line.is_empty() {
                    if !self.data.is_empty() {
                        events.push(self.data.join("\n"));
                        self.data.clear();
                    }
                } else if let Some(data) = line.strip_prefix("data:") {
                    self.data
                        .push(data.strip_prefix(' ').unwrap_or(data).into());
                } else if line == "data" {
                    self.data.push(String::new());
                }
            } else {
                self.buffer.push(byte);
            }
        }
        Ok(events)
    }
}

pub fn http_request(
    config: &McpServerConfig,
    request: &Request,
    timeout_seconds: u64,
) -> Result<Response, String> {
    let McpServerConfig::Http(server) = config else {
        return Err("HTTP transport requires an HTTP MCP server config".into());
    };
    let mut transport = HttpTransport::new(server)?;
    transport.set_timeout(timeout_seconds);
    transport.request(request, &Control::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sse_handles_split_utf8_crlf_and_multiline() {
        let mut decoder = SseDecoder::default();
        let input = "\u{feff}: keepalive\r\ndata: {\r\ndata: \"text\":\"你好\"}\r\n\r\n";
        let mut events = Vec::new();
        for byte in input.as_bytes() {
            events.extend(decoder.feed(&[*byte]).unwrap());
        }
        assert_eq!(events, vec!["{\n\"text\":\"你好\"}"]);
    }
    #[test]
    fn local_mcp_urls_are_supported() {
        for url in [
            "http://localhost:8931/mcp",
            "http://127.0.0.1/mcp",
            "http://[::1]/mcp",
            "http://192.168.1.2/mcp",
        ] {
            assert!(validate_mcp_url(url).is_ok());
        }
        assert!(validate_mcp_url("file:///tmp/mcp").is_err());
    }
}
