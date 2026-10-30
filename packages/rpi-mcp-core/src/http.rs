//! Streamable HTTP: session headers, JSON/SSE responses and cancellable I/O.
use crate::auth::{
    parse_bearer_challenge, AuthBinding, AuthDecision, AuthReason, AuthRequest, CredentialLease,
    CredentialProvider,
};
use crate::control::{Control, Deadline, MAX_MESSAGE};
use crate::jsonrpc::{Notification, Request, Response};
use crate::transport::{client_reply, decode_response, validate_message};
use crate::{HttpServer, McpServerConfig};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use std::time::Duration;
use std::{
    cell::{Cell, RefCell},
    sync::Arc,
};

pub fn validate_mcp_url(input: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(input).map_err(|_| "invalid MCP URL")?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("MCP URL must use http or https and include a host".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("use configured headers instead of embedded URL credentials".into());
    }
    Ok(url)
}

pub struct HttpTransport {
    callbacks: Option<Arc<crate::callback::Callbacks>>,
    client: Client,
    runtime: tokio::runtime::Runtime,
    url: url::Url,
    session: Option<String>,
    pub(crate) version: Option<String>,
    timeout: u64,
    auth: Option<(AuthBinding, Arc<dyn CredentialProvider>)>,
    lease: RefCell<Option<CredentialLease>>,
}

impl std::fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HttpTransport([redacted])")
    }
}
impl HttpTransport {
    pub fn with_auth(
        server: &HttpServer,
        binding: AuthBinding,
        provider: Arc<dyn CredentialProvider>,
    ) -> Result<Self, String> {
        let mut transport = Self::new(server)?;
        if binding.resource != transport.url.as_str()
            || std::fs::canonicalize(&binding.cwd).ok().as_ref() != Some(&binding.cwd)
        {
            return Err("MCP authentication identity mismatch".into());
        }
        if !server
            .headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("authorization"))
        {
            transport.auth = Some((binding, provider));
        }
        Ok(transport)
    }
    pub fn new(server: &HttpServer) -> Result<Self, String> {
        let url = validate_mcp_url(&server.url)?;
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in &server.headers {
            let name = reqwest::header::HeaderName::try_from(name.as_str())
                .map_err(|_| "invalid MCP header name")?;
            if headers.contains_key(&name) {
                return Err("duplicate MCP header name".into());
            }
            let mut value = reqwest::header::HeaderValue::try_from(value.as_str())
                .map_err(|_| "invalid MCP header value")?;
            if name == reqwest::header::AUTHORIZATION {
                value.set_sensitive(true);
            }
            headers.insert(name, value);
        }
        let client = Client::builder()
            .pool_max_idle_per_host(0)
            .tcp_nodelay(true)
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("rpi-mcp-adapter/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| "MCP HTTP client initialization failed")?;
        Ok(Self {
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| "MCP HTTP runtime initialization failed")?,
            client,
            callbacks: None,
            auth: None,
            lease: RefCell::new(None),
            url,
            session: None,
            version: None,
            timeout: server.timeout.clamp(1, 300),
        })
    }
    pub(crate) fn set_callbacks(&mut self, callbacks: Option<Arc<crate::callback::Callbacks>>) {
        self.callbacks = callbacks;
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
        self.attach(request)
    }
    fn attach(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let (Some((binding, _)), Some(lease)) = (&self.auth, self.lease.borrow().as_ref()) {
            if let Ok(header) = lease.header(binding) {
                return request.header(reqwest::header::AUTHORIZATION, header);
            }
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
                    Err("MCP request timed out".into()),
            }
        })
    }
    pub fn raw(&mut self, value: Value, control: &Control) -> Result<Value, String> {
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("reply")
            .to_string();
        let deadline = Deadline::new(self.timeout, "HTTP", control);
        let id = value.get("id").cloned();
        let is_initialize = method == "initialize";
        let executed = Cell::new(false);
        let result = self.run(&deadline, self.exchange(&value, &deadline, &executed));
        let (response, session) = match result {
            Ok(result) => result,
            Err(error) => {
                if executed.get() && (error.contains("cancelled") || error.contains("timed out")) {
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
    async fn credential(
        &self,
        reason: AuthReason<'_>,
        deadline: &Deadline,
    ) -> Result<bool, String> {
        let Some((binding, provider)) = &self.auth else {
            return Ok(false);
        };
        let decision = provider
            .credential(AuthRequest {
                binding,
                reason,
                control: &deadline.control,
                deadline: deadline.end,
            })
            .await
            .map_err(|e| e.to_string())?;
        deadline.check()?;
        match decision {
            AuthDecision::Credential(lease) => {
                lease.header(binding).map_err(|e| e.to_string())?;
                *self.lease.borrow_mut() = Some(lease);
                Ok(true)
            }
            _ => {
                self.lease.borrow_mut().take();
                Ok(false)
            }
        }
    }
    fn reject_lease(&self) -> Option<String> {
        // Take before notifying: no RefCell borrow survives provider code and
        // cancellation cannot leave a locally reusable rejected credential.
        let lease = self.lease.borrow_mut().take()?;
        if let Some((binding, provider)) = &self.auth {
            provider.rejected_lease(binding, lease.id());
        }
        Some(lease.id().to_owned())
    }
    async fn exchange(
        &self,
        value: &Value,
        deadline: &Deadline,
        executed: &Cell<bool>,
    ) -> Result<(Value, Option<String>), String> {
        if serde_json::to_vec(value).map_err(|e| e.to_string())?.len() > MAX_MESSAGE {
            return Err("MCP request exceeded the 16 MiB limit".into());
        }
        if self.auth.is_some() {
            self.credential(AuthReason::CachedOnly, deadline).await?;
        }
        deadline.check()?;
        executed.set(true);
        let mut response = self
            .post(value)
            .send()
            .await
            .map_err(|_| "MCP HTTP request failed")?;
        if response.status() == StatusCode::UNAUTHORIZED {
            executed.set(false);
            if self.auth.is_some() {
                let rejected = self.reject_lease();
                let challenge = parse_bearer_challenge(response.headers());
                drop(response);
                let challenge = challenge
                    .map_err(|e| e.to_string())?
                    .ok_or("MCP authentication required")?;
                if !self
                    .credential(
                        AuthReason::Challenge {
                            challenge: &challenge,
                            rejected_lease: rejected.as_deref(),
                        },
                        deadline,
                    )
                    .await?
                {
                    return Err("MCP authentication required".into());
                }
                deadline.check()?;
                executed.set(true);
                response = self
                    .post(value)
                    .send()
                    .await
                    .map_err(|_| "MCP HTTP request failed")?;
                if response.status() == StatusCode::UNAUTHORIZED {
                    executed.set(false);
                    self.reject_lease();
                }
            }
        }
        if response.status() == StatusCode::NOT_FOUND && self.session.is_some() {
            return Err(
                "MCP HTTP session expired (404); connection will be reinitialized on the next call"
                    .into(),
            );
        }
        if !response.status().is_success() {
            return Err(format!(
                "MCP HTTP server returned status {}",
                response.status().as_u16()
            ));
        }
        let session = response
            .headers()
            .get("Mcp-Session-Id")
            .map(|v| v.to_str().map(String::from))
            .transpose()
            .map_err(|_| "invalid MCP session header")?;
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
                .map_err(|_| "read MCP HTTP response failed")?
            {
                if bytes.len() + chunk.len() > MAX_MESSAGE {
                    return Err("MCP response exceeded the 16 MiB limit".into());
                }
                bytes.extend_from_slice(&chunk);
            }
            let result: Value =
                serde_json::from_slice(&bytes).map_err(|_| "decode MCP HTTP response failed")?;
            check_raw_response(&result, id)?;
            return Ok((result, session));
        }
        if content_type != "text/event-stream" {
            return Err("unsupported MCP HTTP content type".into());
        }
        let (callback_tx, mut callback_rx) = tokio::sync::mpsc::channel::<Value>(8);
        let failure = crate::callback::ReplyFailure::default();
        let sink_failure = failure.clone();
        let callback_reply: crate::callback::Reply = Arc::new(move |v| {
            if callback_tx.try_send(v).is_err() { sink_failure.fail(); }
        });
        let mut decoder = SseDecoder::default();
        let mut total = 0usize;
        loop {
            failure.check()?;
            let chunk = tokio::select! {
                biased;
                _ = failure.failed() => { failure.check()?; unreachable!() },
                chunk = response.chunk() => chunk.map_err(|_| "read MCP SSE response failed")?,
                Some(reply) = callback_rx.recv() => {
                    tokio::select! {
                        biased;
                        _ = failure.failed() => { failure.check()?; unreachable!() },
                        result = self.send_callback_reply(&reply, session.as_deref()) => result?,
                    }
                    continue;
                }
            };
            let Some(chunk) = chunk else { break };

            total += chunk.len();
            if total > MAX_MESSAGE {
                return Err("MCP response exceeded the 16 MiB limit".into());
            }
            for data in decoder.feed(&chunk)? {
                failure.check()?;
                let message: Value =
                    serde_json::from_str(&data).map_err(|_| "decode MCP SSE event failed")?;
                validate_message(&message)?;
                if message.get("method").is_some() {
                    if let Some(callbacks) = &self.callbacks {
                        callbacks.dispatch(&message, callback_reply.clone());
                        failure.check()?;
                        continue;
                    }
                    if let Some(reply) = client_reply(&message) {
                        self.send_callback_reply(&reply, session.as_deref()).await?;
                    }
                } else if message.get("id") == Some(id) {
                    check_raw_response(&message, id)?;
                    return Ok((message, session));
                }
            }
        }
        Err("MCP SSE stream ended before matching response".into())
    }
    async fn send_callback_reply(&self, reply: &Value, session: Option<&str>) -> Result<(), String> {
        let mut request = self.post(reply);
        if self.session.is_none() {
            if let Some(session) = session { request = request.header("Mcp-Session-Id", session); }
        }
        let response = request.send().await.map_err(|_| "reply to MCP server failed")?;
        if response.status() == StatusCode::UNAUTHORIZED {
            // This transport serializes exchanges; the local lease is exactly
            // the one attached above. Notify only its ID, never acquire/replay.
            self.reject_lease();
        }
        if response.status() != StatusCode::ACCEPTED { return Err("MCP server did not accept client reply".into()); }
        Ok(())
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
        decode_response(value, request.id).map_err(|_| "invalid MCP HTTP response".into())
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
    /// Bounded best-effort cleanup using only the still-valid local lease.
    /// Never calls the provider, refreshes or retries; remote deletion can fail.
    /// Releases the local lease even on failure, without revoking cached grants.
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
                let _ = self.attach(request).send().await;
            });
        }
        self.lease.borrow_mut().take();
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
                    .map_err(|_| "invalid SSE UTF-8")?;
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

/// Explicit authenticated one-shot counterpart. No global credential service.
pub fn http_request_with_auth(
    server: &HttpServer,
    request: &Request,
    binding: AuthBinding,
    provider: Arc<dyn CredentialProvider>,
    control: &Control,
) -> Result<Response, String> {
    HttpTransport::with_auth(server, binding, provider)?.request(request, control)
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

#[cfg(test)]
#[path = "../tests/support/callback_transport.rs"]
mod callback_transport_tests;
