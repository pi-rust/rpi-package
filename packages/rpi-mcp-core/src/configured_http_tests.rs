//! Real loopback coverage for the configured-only HTTP authority/runtime path.
use crate::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const SECRET: &str = "HTTP-SECRET-CANARY";
#[derive(Clone)]
enum Mode {
    Json,
    Sse,
    Status(u16),
    Malformed,
    Mismatch,
    Disconnect,
    RpcError,
    StallHeaders,
    StallBody,
    StallCleanup,
    Pages,
    RepeatedCursor,
    Redirect(String),
    BadCatalog,
    BadVersion,
    BadNotification,
    InitStallBody,
    InitMalformed,
    InitMismatch,
}
#[derive(Clone, Debug)]
struct WireRequest {
    verb: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
}
struct Fixture {
    url: String,
    mode: Arc<Mutex<Mode>>,
    requests: Arc<Mutex<Vec<WireRequest>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    diagnostics: Arc<Mutex<Vec<String>>>,
}
impl Fixture {
    fn new(mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!(
            "http://{}/mcp?opaque=%2f%2B&token={SECRET}",
            listener.local_addr().unwrap()
        );
        let mode = Arc::new(Mutex::new(mode));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let (m, r, s, d) = (
            mode.clone(),
            requests.clone(),
            stop.clone(),
            diagnostics.clone(),
        );
        let worker = thread::spawn(move || {
            let mut workers = Vec::new();
            while !s.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // Windows accept inherits the listener's nonblocking mode.
                        // Request workers use blocking reads with explicit timeouts.
                        stream.set_nonblocking(false).unwrap();
                        let (m, r, s, d) = (m.clone(), r.clone(), s.clone(), d.clone());
                        d.lock().unwrap().push("accepted".into());
                        workers.push(thread::spawn(move || serve(stream, m, r, s, d)));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("loopback accept failed: {e}"),
                }
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            url,
            mode,
            requests,
            stop,
            worker: Some(worker),
            diagnostics,
        }
    }
    fn target(&self) -> ConfiguredHttp {
        let config: HttpServer = serde_json::from_value(json!({
            "url":self.url, "headers":{"Authorization":SECRET,"X-Empty":""}, "timeout":5
        }))
        .unwrap();
        ConfiguredHttp::new(&context(), "server", config)
            .ok()
            .unwrap()
    }
    fn discover(&self, runtime: &McpRuntime, target: &ConfiguredHttp) -> DiscoveredServer {
        runtime
            .discover_http(target, limits(), &Control::default())
            .unwrap_or_else(|e| {
                panic!(
                    "{e}; fixture diagnostics: {:?}; requests: {:?}",
                    self.diagnostics.lock().unwrap(),
                    self.records()
                )
            })
    }
    fn set(&self, mode: Mode) {
        *self.mode.lock().unwrap() = mode;
    }
    fn records(&self) -> Vec<WireRequest> {
        self.requests.lock().unwrap().clone()
    }
    fn count(&self, method: &str) -> usize {
        self.records()
            .iter()
            .filter(|r| r.body["method"] == method)
            .count()
    }
    fn wait_for(&self, method: &str) {
        let until = Instant::now() + Duration::from_secs(3);
        while self.count(method) == 0 {
            assert!(
                Instant::now() < until,
                "request {method} did not reach fixture"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn context() -> CoreRequestContext {
    CoreRequestContext {
        scope: "http-tests".into(),
        cwd: std::env::current_dir().unwrap(),
    }
}
fn limits() -> DiscoveryLimits {
    DiscoveryLimits {
        max_tools: 8,
        max_catalog_bytes: 4096,
    }
}
fn raw_result() -> Value {
    json!({"content":[{"type":"text","text":"你好"},{"type":"image","data":"QUJD","mimeType":"image/png"}],
        "isError":true,"structuredContent":{"retained":true},"_meta":{"unknown":"retained"}})
}
fn read_request(stream: &mut TcpStream) -> Result<WireRequest, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut byte = [0];
    while !bytes.ends_with(b"\r\n\r\n") {
        if stream
            .read(&mut byte)
            .map_err(|e| format!("header read: {e:?}; bytes={}", bytes.len()))?
            == 0
        {
            return Err(format!("header EOF; bytes={}", bytes.len()));
        }
        bytes.push(byte[0]);
        assert!(bytes.len() < 32 * 1024);
    }
    let text = String::from_utf8(bytes).unwrap();
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap().split_whitespace();
    let verb = first.next().unwrap().to_string();
    let path = first.next().unwrap().to_string();
    let headers: BTreeMap<_, _> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let len = headers
        .get("content-length")
        .map(|n| n.parse::<usize>().unwrap())
        .unwrap_or(0);
    assert!(len < 1024 * 1024);
    let mut body = vec![0; len];
    stream
        .read_exact(&mut body)
        .map_err(|e| format!("body read: {e:?}; expected={len}"))?;
    Ok(WireRequest {
        verb,
        path,
        headers,
        body: if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap()
        },
    })
}
fn reply(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    extra: &str,
    body: &[u8],
    fragmented: bool,
) {
    let header = format!("HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",body.len());
    if stream.write_all(header.as_bytes()).is_err() {
        return;
    }
    if fragmented {
        for chunk in body.chunks(3) {
            if stream.write_all(chunk).is_err() {
                return;
            }
            let _ = stream.flush();
            thread::sleep(Duration::from_millis(1));
        }
    } else {
        let _ = stream.write_all(body);
    }
}
fn serve(
    mut stream: TcpStream,
    mode: Arc<Mutex<Mode>>,
    records: Arc<Mutex<Vec<WireRequest>>>,
    stop: Arc<AtomicBool>,
    diagnostics: Arc<Mutex<Vec<String>>>,
) {
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(error) => {
            diagnostics.lock().unwrap().push(error);
            return;
        }
    };
    records.lock().unwrap().push(request.clone());
    let mode = mode.lock().unwrap().clone();
    let method = request.body["method"].as_str().unwrap_or("");
    if request.verb == "DELETE"
        || method.starts_with("notifications/")
        || request.body.get("result").is_some()
    {
        if matches!(mode, Mode::StallCleanup)
            && (request.verb == "DELETE" || method == "notifications/cancelled")
        {
            while !stop.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(5));
            }
            return;
        }
        let status =
            if method == "notifications/initialized" && matches!(mode, Mode::BadNotification) {
                200
            } else {
                202
            };
        reply(&mut stream, status, "application/json", "", b"", false);
        return;
    }
    let id = request.body["id"].clone();
    let result = match method {
        "initialize" => {
            json!({"protocolVersion":if matches!(mode,Mode::BadVersion) { SECRET } else { "2025-03-26" },
            "capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
        }
        "tools/list" => {
            if matches!(mode, Mode::BadCatalog) {
                json!({"tools":[{"name":SECRET,"inputSchema":{}}]})
            } else if matches!(mode, Mode::RepeatedCursor) {
                json!({"tools":[],"nextCursor":SECRET})
            } else if matches!(mode, Mode::Pages) {
                if request.body["params"].get("cursor").is_some() {
                    assert_eq!(request.body["params"]["cursor"], SECRET);
                    json!({"tools":[{"name":"second","inputSchema":{"type":"object"}}]})
                } else {
                    json!({"tools":[{"name":"echo/中文","inputSchema":{"type":"object"}}],"nextCursor":SECRET})
                }
            } else {
                json!({"tools":[{"name":"echo/中文","inputSchema":{"type":"object"}}]})
            }
        }
        "tools/call" => raw_result(),
        _ => panic!("unexpected request method"),
    };
    let mut body = json!({"jsonrpc":"2.0","id":id,"result":result});
    let mut status = 200;
    let mut extra = if method == "initialize" {
        "Mcp-Session-Id: fixture-session\r\n".to_string()
    } else {
        String::new()
    };
    let mut content_type = "application/json";
    let mut bytes = serde_json::to_vec(&body).unwrap();
    let mut fragmented = false;
    if method == "initialize" {
        match mode {
            Mode::InitStallBody => {
                // A ping reply proves the client consumed these session headers
                // before cancellation, rather than merely racing header delivery.
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 10000\r\nMcp-Session-Id: fixture-session\r\nConnection: close\r\n\r\ndata: {\"jsonrpc\":\"2.0\",\"id\":\"server-ping\",\"method\":\"ping\"}\r\n\r\n");
                let _ = stream.flush();
                while !stop.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(5));
                }
                return;
            }
            Mode::InitMalformed => bytes = format!("{{{SECRET}").into_bytes(),
            Mode::InitMismatch => {
                body["id"] = json!(9999);
                bytes = serde_json::to_vec(&body).unwrap();
            }
            _ => {}
        }
    }
    if method == "tools/call" {
        match mode {
            Mode::Status(code) => { status = code; bytes = SECRET.as_bytes().to_vec(); }
            Mode::Redirect(ref url) => { status = 307; extra = format!("Location: {url}\r\n"); }
            Mode::Malformed => bytes = format!("{{{SECRET}").into_bytes(),
            Mode::Mismatch => { body["id"] = json!(9999); bytes = serde_json::to_vec(&body).unwrap(); }
            Mode::RpcError => bytes = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"error":{"code":-1,"message":SECRET,"data":SECRET}})).unwrap(),
            Mode::Disconnect => return,
            Mode::StallHeaders | Mode::StallBody | Mode::StallCleanup => {
                if matches!(mode, Mode::StallBody | Mode::StallCleanup) {
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 10000\r\nConnection: close\r\n\r\n{");
                }
                while !stop.load(Ordering::Acquire) { thread::sleep(Duration::from_millis(5)); }
                return;
            }
            _ => {}
        }
    }
    if matches!(mode, Mode::Sse) {
        // Include a server request during initialization, before version negotiation.
        // The client reply must use the provisional session and receive HTTP 202.
        let prelude = if method == "initialize" {
            "data: {\"jsonrpc\":\"2.0\",\"id\":\"server-ping\",\"method\":\"ping\"}\r\n\r\n"
        } else {
            ""
        };
        content_type = "text/event-stream";
        bytes = format!("{prelude}data: {body}\r\n\r\n").into_bytes();
        fragmented = true;
    }
    reply(
        &mut stream,
        status,
        content_type,
        &extra,
        &bytes,
        fragmented,
    );
}
fn safe(error: &str) {
    for forbidden in [SECRET, "127.0.0.1", "opaque=", "fixture-session", "http://"] {
        assert!(
            !error.contains(forbidden),
            "typed error leaked a value: {error}"
        );
    }
}

#[test]
fn constructor_is_strict_value_free_and_preserves_opaque_authority() {
    let fixture = Fixture::new(Mode::Json);
    let target = fixture.target();
    assert_eq!(target.config.url, fixture.url);
    assert_eq!(target.config.headers.get("authorization").unwrap(), SECRET);
    assert_eq!(target.config.headers.get("x-empty").unwrap(), "");
    assert_eq!(
        serde_json::from_str::<Value>(&target.key).unwrap(),
        json!(["http-tests", ["configured-http"], "server"])
    );
    assert!(!target.key.contains(SECRET));
    assert!(Arc::ptr_eq(&target.generation, &target.clone().generation));
    assert!(!Arc::ptr_eq(
        &target.generation,
        &fixture.target().generation
    ));
    for url in [
        "https://user:HTTP-SECRET-CANARY@example.org/",
        "https://@example.org/",
        "https:///@example.org/",
        "https://\\@example.org/",
        "https://example.org/#",
        "https://example.org/\\HTTP-SECRET-CANARY",
        "https://example.org\\@evil.invalid/",
        "https://example.org/\tHTTP-SECRET-CANARY",
        "https://example.org/\u{85}HTTP-SECRET-CANARY",
        "https://example.org/#HTTP-SECRET-CANARY",
        "https://example.org/\nHTTP-SECRET-CANARY",
        " https://example.org/",
        "https://example.org/ ",
        "file:///HTTP-SECRET-CANARY",
        "http://",
        "http:///",
        "HTTP-SECRET-CANARY",
    ] {
        let mut cfg = target.config.clone();
        cfg.url = url.into();
        safe(
            &ConfiguredHttp::new(&context(), "server", cfg)
                .err()
                .unwrap(),
        );
    }
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
        "not a token",
        "bad:token",
        "bad\0token",
        "",
    ] {
        let mut cfg = target.config.clone();
        cfg.headers.insert(name.into(), SECRET.into());
        safe(
            &ConfiguredHttp::new(&context(), "server", cfg)
                .err()
                .unwrap(),
        );
    }
    for value in ["\t", "\r\nHTTP-SECRET-CANARY", "\0", "\u{7f}", "\u{85}"] {
        let mut cfg = target.config.clone();
        cfg.headers.insert("x-value".into(), value.into());
        safe(
            &ConfiguredHttp::new(&context(), "server", cfg)
                .err()
                .unwrap(),
        );
    }
    let mut cfg = target.config.clone();
    cfg.headers.insert("Authorization".into(), SECRET.into());
    safe(
        &ConfiguredHttp::new(&context(), "server", cfg)
            .err()
            .unwrap(),
    );
    for bad in ["", " ", "HTTP-SECRET-CANARY\0"] {
        let mut ctx = context();
        ctx.scope = bad.into();
        safe(
            &ConfiguredHttp::new(&ctx, "server", target.config.clone())
                .err()
                .unwrap(),
        );
        safe(
            &ConfiguredHttp::new(&context(), bad, target.config.clone())
                .err()
                .unwrap(),
        );
    }
    for cwd in ["relative", "", "C:/HTTP-SECRET-CANARY\0"] {
        let mut ctx = context();
        ctx.cwd = cwd.into();
        safe(
            &ConfiguredHttp::new(&ctx, "server", target.config.clone())
                .err()
                .unwrap(),
        );
    }
    for timeout in [0, 301, u64::MAX] {
        let mut cfg = target.config.clone();
        cfg.timeout = timeout;
        safe(
            &ConfiguredHttp::new(&context(), "server", cfg)
                .err()
                .unwrap(),
        );
    }
    for timeout in [1, 300] {
        let mut cfg = target.config.clone();
        cfg.timeout = timeout;
        assert!(ConfiguredHttp::new(&context(), "server", cfg).is_ok());
    }
    let mut cfg = target.config.clone();
    cfg.enabled = false;
    safe(
        &ConfiguredHttp::new(&context(), "server", cfg)
            .err()
            .unwrap(),
    );
    let transport = crate::http::HttpTransport::configured(&target.config, "core-test").unwrap();
    safe(&format!("{transport:?}"));
}

#[test]
fn discovery_json_and_fragmented_post_sse_preserve_raw_results_and_headers() {
    for mode in [Mode::Json, Mode::Sse] {
        let fixture = Fixture::new(mode);
        let target = fixture.target();
        let runtime = McpRuntime::new();
        let control = Control::default();
        assert!(runtime
            .call_http(&target, "echo/中文", json!({}), &control)
            .unwrap_err()
            .contains("requires discovery"));
        assert!(fixture.records().is_empty());
        let found = fixture.discover(&runtime, &target);
        assert_eq!(found.tools[0].name, "echo/中文");
        assert_eq!(found.info.protocol_version, "2025-03-26");
        assert_eq!(
            runtime
                .call_http(
                    &target.clone(),
                    "echo/中文",
                    json!({"secret":SECRET}),
                    &control
                )
                .unwrap(),
            raw_result()
        );
        // `isError` is a raw successful result and must not retire admission.
        assert_eq!(
            runtime
                .call_http(&target, "echo/中文", json!({}), &control)
                .unwrap(),
            raw_result()
        );
        assert_eq!(fixture.count("initialize"), 1);
        assert_eq!(fixture.count("tools/list"), 1);
        assert_eq!(fixture.count("tools/call"), 2);
        runtime.shutdown_connections();
        let records = fixture.records();
        assert_eq!(records.iter().filter(|r| r.verb == "DELETE").count(), 1);
        for request in records {
            assert_eq!(request.path, format!("/mcp?opaque=%2f%2B&token={SECRET}"));
            assert_eq!(request.headers["authorization"], SECRET);
            if request.body["method"] == "initialize" {
                assert!(!request.headers.contains_key("mcp-session-id"));
                assert!(!request.headers.contains_key("mcp-protocol-version"));
            } else {
                assert_eq!(request.headers["mcp-session-id"], "fixture-session");
                if request.body["id"] == "server-ping" {
                    assert!(!request.headers.contains_key("mcp-protocol-version"));
                } else {
                    assert_eq!(request.headers["mcp-protocol-version"], "2025-03-26");
                }
            }
            if request.verb == "POST" {
                assert_eq!(
                    request.headers["accept"],
                    "application/json, text/event-stream"
                );
            }
        }
    }
}

#[test]
fn transport_failures_retire_generation_and_never_reconnect_or_replay() {
    for mode in [
        Mode::Status(401),
        Mode::Status(403),
        Mode::Status(404),
        Mode::Mismatch,
        Mode::Malformed,
        Mode::Disconnect,
        Mode::RpcError,
    ] {
        let fixture = Fixture::new(Mode::Json);
        let target = fixture.target();
        let runtime = McpRuntime::new();
        let control = Control::default();
        fixture.discover(&runtime, &target);
        fixture.set(mode);
        safe(
            &runtime
                .call_http(&target, "echo", json!({}), &control)
                .unwrap_err(),
        );
        let before = fixture.records().len();
        for _ in 0..2 {
            safe(
                &runtime
                    .call_http(&target.clone(), "echo", json!({}), &control)
                    .unwrap_err(),
            );
        }
        assert_eq!(fixture.records().len(), before);
        assert_eq!(fixture.count("initialize"), 1);
        assert_eq!(fixture.count("tools/call"), 1);
        fixture.set(Mode::Json);
        fixture.discover(&runtime, &target);
        assert_eq!(
            runtime
                .call_http(&target, "echo", json!({}), &control)
                .unwrap(),
            raw_result()
        );
        assert_eq!(fixture.count("initialize"), 2);
    }
}

#[test]
fn actual_paginated_catalog_enforces_total_count_and_serialized_byte_limits() {
    let catalog = json!([
        {"name":"echo/中文","inputSchema":{"type":"object"}},
        {"name":"second","inputSchema":{"type":"object"}}
    ]);
    let exact = serde_json::to_vec(&catalog).unwrap().len();
    for (max_tools, max_catalog_bytes, succeeds) in
        [(2, exact, true), (1, exact, false), (2, exact - 1, false)]
    {
        let fixture = Fixture::new(Mode::Pages);
        let target = fixture.target();
        let runtime = McpRuntime::new();
        let result = runtime.discover_http(
            &target,
            DiscoveryLimits {
                max_tools,
                max_catalog_bytes,
            },
            &Control::default(),
        );
        if succeeds {
            assert_eq!(result.unwrap().tools.len(), 2);
        } else {
            safe(&result.unwrap_err());
            let before = fixture.records().len();
            assert!(runtime
                .call_http(&target, "echo", json!({}), &Control::default())
                .is_err());
            assert_eq!(fixture.records().len(), before);
        }
        assert_eq!(fixture.count("tools/list"), 2);
    }
    let fixture = Fixture::new(Mode::RepeatedCursor);
    let target = fixture.target();
    safe(
        &McpRuntime::new()
            .discover_http(&target, limits(), &Control::default())
            .unwrap_err(),
    );
    assert_eq!(fixture.count("tools/list"), 2);
}

#[test]
fn discovery_errors_clear_cloned_admission_and_never_echo_remote_values() {
    for mode in [Mode::BadVersion, Mode::BadNotification, Mode::BadCatalog] {
        let fixture = Fixture::new(mode);
        let target = fixture.target();
        let runtime = McpRuntime::new();
        safe(
            &runtime
                .discover_http(&target, limits(), &Control::default())
                .unwrap_err(),
        );
        let before = fixture.records().len();
        safe(
            &runtime
                .call_http(&target, "echo", json!({}), &Control::default())
                .unwrap_err(),
        );
        assert_eq!(fixture.records().len(), before);
    }
    let fixture = Fixture::new(Mode::Json);
    let target = fixture.target();
    let clone = target.clone();
    let runtime = McpRuntime::new();
    fixture.discover(&runtime, &target);
    let before = fixture.records().len();
    assert!(runtime
        .discover_http(
            &target,
            DiscoveryLimits {
                max_tools: 1,
                max_catalog_bytes: 0
            },
            &Control::default()
        )
        .is_err());
    assert!(clone.generation.lock().unwrap().is_none());
    assert!(runtime
        .call_http(&clone, "echo", json!({}), &Control::default())
        .is_err());
    assert_eq!(fixture.records().len(), before);
    fixture.discover(&runtime, &target);
    fixture.set(Mode::RpcError);
    safe(
        &runtime
            .call_http(&target, "echo", json!({}), &Control::default())
            .unwrap_err(),
    );
}

#[test]
fn runtime_independent_targets_replacement_shutdown_and_owner_drop_are_isolated() {
    let fixture = Fixture::new(Mode::Json);
    let target = fixture.target();
    let a = McpRuntime::new();
    let b = McpRuntime::new();
    let c = Control::default();
    a.discover_http(&target, limits(), &c).unwrap();
    let old_generation = target.generation.lock().unwrap().clone().unwrap();
    let before = fixture.records().len();
    assert!(b
        .call_http(&target, "echo", json!({}), &c)
        .unwrap_err()
        .contains("retired"));
    assert!(a
        .call_http(&fixture.target(), "echo", json!({}), &c)
        .unwrap_err()
        .contains("requires discovery"));
    assert_eq!(fixture.records().len(), before);
    let replacement = fixture.target();
    let mut cfg = replacement.config.clone();
    cfg.headers.insert("x-changed".into(), "yes".into());
    let replacement = ConfiguredHttp::new(&context(), "server", cfg).ok().unwrap();
    a.discover_http(&replacement, limits(), &c).unwrap();
    assert!(!Arc::ptr_eq(
        &old_generation,
        replacement.generation.lock().unwrap().as_ref().unwrap()
    ));
    let before = fixture.records().len();
    assert!(a
        .call_http(&target, "echo", json!({}), &c)
        .unwrap_err()
        .contains("stale"));
    assert_eq!(fixture.records().len(), before);
    b.discover_http(&target, limits(), &c).unwrap();
    a.shutdown_connections();
    assert!(a.call_http(&replacement, "echo", json!({}), &c).is_err());
    assert_eq!(
        b.call_http(&target, "echo", json!({}), &c).unwrap(),
        raw_result()
    );
    let deletes = fixture
        .records()
        .iter()
        .filter(|r| r.verb == "DELETE")
        .count();
    drop(b);
    assert_eq!(
        fixture
            .records()
            .iter()
            .filter(|r| r.verb == "DELETE")
            .count(),
        deletes + 1
    );
}

#[test]
fn redirects_are_not_followed_and_do_not_contact_sink() {
    let sink = Fixture::new(Mode::Json);
    let fixture = Fixture::new(Mode::Json);
    let target = fixture.target();
    let runtime = McpRuntime::new();
    fixture.discover(&runtime, &target);
    fixture.set(Mode::Redirect(sink.url.clone()));
    safe(
        &runtime
            .call_http(&target, "echo", json!({}), &Control::default())
            .unwrap_err(),
    );
    assert!(runtime
        .call_http(&target, "echo", json!({}), &Control::default())
        .is_err());
    assert!(sink.records().is_empty());
    assert_eq!(fixture.count("tools/call"), 1);
}

#[test]
fn shutdown_cancels_active_http_and_clone_rediscovery_wait_is_cancellable() {
    let fixture = Fixture::new(Mode::Json);
    let mut config = fixture.target().config;
    config.timeout = 30;
    let target = ConfiguredHttp::new(&context(), "server", config)
        .ok()
        .unwrap();
    let runtime = Arc::new(McpRuntime::new());
    fixture.discover(&runtime, &target);
    fixture.set(Mode::StallBody);
    let (r, t) = (runtime.clone(), target.clone());
    let call = thread::spawn(move || r.call_http(&t, "echo", json!({}), &Control::default()));
    fixture.wait_for("tools/call");
    let wait_control = Control::default();
    let (r, t, c) = (runtime.clone(), target.clone(), wait_control.clone());
    let rediscover = thread::spawn(move || r.discover_http(&t, limits(), &c));
    thread::sleep(Duration::from_millis(20));
    wait_control.cancel();
    assert!(rediscover
        .join()
        .unwrap()
        .unwrap_err()
        .contains("cancelled"));
    let start = Instant::now();
    runtime.shutdown_connections();
    safe(&call.join().unwrap().unwrap_err());
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(fixture.count("initialize"), 1);
    assert_eq!(fixture.count("tools/list"), 1);
    assert_eq!(fixture.count("tools/call"), 1);
    let before = fixture.records().len();
    assert!(runtime
        .call_http(&target, "echo", json!({}), &Control::default())
        .is_err());
    assert_eq!(fixture.records().len(), before);
}

#[test]
fn failed_initialize_body_keeps_provisional_cleanup_authority_without_replay() {
    for mode in [
        Mode::InitMalformed,
        Mode::InitMismatch,
        Mode::BadNotification,
    ] {
        let fixture = Fixture::new(mode.clone());
        let target = fixture.target();
        let runtime = McpRuntime::new();
        safe(
            &runtime
                .discover_http(&target, limits(), &Control::default())
                .unwrap_err(),
        );
        assert_failed_initialize_cleanup(&fixture, matches!(mode, Mode::BadNotification), false);
        assert_failed_initialize_admission(&fixture, &runtime, &target);
    }
}

#[test]
fn initialize_headers_survive_synchronized_body_cancellation_and_timeout() {
    for cancel in [true, false] {
        let fixture = Fixture::new(Mode::InitStallBody);
        let mut config = fixture.target().config;
        config.timeout = 2;
        let target = ConfiguredHttp::new(&context(), "server", config)
            .ok()
            .unwrap();
        let runtime = Arc::new(McpRuntime::new());
        let control = Control::default();
        let (r, t, c) = (runtime.clone(), target.clone(), control.clone());
        let start = Instant::now();
        let worker = thread::spawn(move || r.discover_http(&t, limits(), &c));
        let until = Instant::now() + Duration::from_secs(3);
        while !fixture
            .records()
            .iter()
            .any(|r| r.body["id"] == "server-ping")
        {
            assert!(
                Instant::now() < until,
                "initialize headers were not consumed: {:?}",
                fixture.diagnostics.lock().unwrap()
            );
            thread::sleep(Duration::from_millis(5));
        }
        if cancel {
            control.cancel();
        }
        safe(&worker.join().unwrap().unwrap_err());
        // A per-connection test guard, not a global runtime shutdown guarantee.
        assert!(start.elapsed() < Duration::from_secs(6));
        assert_failed_initialize_cleanup(&fixture, false, true);
        assert_failed_initialize_admission(&fixture, &runtime, &target);
    }
}

fn assert_failed_initialize_cleanup(fixture: &Fixture, negotiated: bool, cancelled: bool) {
    let records = fixture.records();
    let deletes: Vec<_> = records.iter().filter(|r| r.verb == "DELETE").collect();
    assert_eq!(
        deletes.len(),
        1,
        "missing provisional session cleanup: {records:?}"
    );
    let cancellations: Vec<_> = records
        .iter()
        .filter(|r| r.body["method"] == "notifications/cancelled")
        .collect();
    assert_eq!(cancellations.len(), usize::from(cancelled));
    for request in deletes.into_iter().chain(cancellations) {
        assert_eq!(request.headers["authorization"], SECRET);
        assert_eq!(request.headers["mcp-session-id"], "fixture-session");
        assert_eq!(request.path, format!("/mcp?opaque=%2f%2B&token={SECRET}"));
        assert_eq!(
            request
                .headers
                .get("mcp-protocol-version")
                .map(String::as_str),
            negotiated.then_some("2025-03-26")
        );
        if request.verb != "DELETE" {
            assert_eq!(request.body["params"]["requestId"], 1);
        }
    }
}

fn assert_failed_initialize_admission(
    fixture: &Fixture,
    runtime: &McpRuntime,
    target: &ConfiguredHttp,
) {
    assert_eq!(fixture.count("initialize"), 1);
    assert_eq!(fixture.count("tools/list"), 0);
    let before = fixture.records().len();
    safe(
        &runtime
            .call_http(&target.clone(), "echo", json!({}), &Control::default())
            .unwrap_err(),
    );
    assert_eq!(fixture.records().len(), before);
    fixture.set(Mode::Json);
    fixture.discover(runtime, target);
    assert_eq!(
        runtime
            .call_http(target, "echo", json!({}), &Control::default())
            .unwrap(),
        raw_result()
    );
    assert_eq!(fixture.count("initialize"), 2);
}

#[test]
fn legacy_runtime_rpc_errors_retain_session_and_transport_errors_reconnect() {
    let fixture = Fixture::new(Mode::Json);
    let runtime = McpRuntime::new();
    let control = Control::default();
    let params = json!({"url":fixture.url,"tool":"echo/中文","arguments":{}});
    runtime
        .list(&context(), &json!({"url":fixture.url}), &control)
        .unwrap();
    fixture.set(Mode::RpcError);
    for _ in 0..2 {
        let error = runtime.call(&context(), &params, &control).unwrap_err();
        assert_eq!(error, format!("MCP JSON-RPC error -1: {SECRET}"));
    }
    assert_eq!(fixture.count("initialize"), 1);
    assert_eq!(fixture.count("tools/call"), 2);
    assert!(!fixture.records().iter().any(|r| r.verb == "DELETE"));
    fixture.set(Mode::Json);
    assert_eq!(
        runtime.call(&context(), &params, &control).unwrap(),
        raw_result()
    );
    fixture.set(Mode::Status(404));
    assert!(runtime.call(&context(), &params, &control).is_err());
    fixture.set(Mode::Json);
    assert_eq!(
        runtime.call(&context(), &params, &control).unwrap(),
        raw_result()
    );
    assert_eq!(fixture.count("initialize"), 2);
}

#[test]
fn legacy_constructor_keeps_failed_initialize_body_cleanup_behavior() {
    for mode in [Mode::InitMalformed, Mode::InitMismatch] {
        let fixture = Fixture::new(mode);
        let mut transport =
            crate::http::HttpTransport::with_user_agent(&fixture.target().config, "legacy-test")
                .unwrap();
        assert!(transport
            .request(
                &crate::connection::initialize_request(1, "legacy-test", "1"),
                &Control::default()
            )
            .is_err());
        drop(transport);
        assert_eq!(fixture.count("initialize"), 1);
        assert!(!fixture.records().iter().any(|r| r.verb == "DELETE"));
    }
}

#[test]
fn cancellation_and_timeout_abort_headers_or_stalled_body_and_retire_without_replay() {
    for mode in [Mode::StallHeaders, Mode::StallBody, Mode::StallCleanup] {
        for cancel in [true, false] {
            let fixture = Fixture::new(Mode::Json);
            // Leave scheduling headroom for initialize on busy Windows CI while
            // retaining a short real transport timeout for the stalled call.
            let mut config = fixture.target().config;
            config.timeout = 2;
            let target = ConfiguredHttp::new(&context(), "server", config)
                .ok()
                .unwrap();
            let runtime = Arc::new(McpRuntime::new());
            fixture.discover(&runtime, &target);
            fixture.set(mode.clone());
            let control = Control::default();
            let (r, t, c) = (runtime.clone(), target.clone(), control.clone());
            let start = Instant::now();
            let worker = thread::spawn(move || r.call_http(&t, "echo", json!({}), &c));
            fixture.wait_for("tools/call");
            if cancel {
                control.cancel();
            }
            safe(&worker.join().unwrap().unwrap_err());
            assert!(start.elapsed() < Duration::from_secs(6));
            assert_eq!(
                fixture.count("notifications/cancelled"),
                1,
                "{:?}",
                fixture.diagnostics.lock().unwrap()
            );
            assert_eq!(
                fixture
                    .records()
                    .iter()
                    .filter(|r| r.verb == "DELETE")
                    .count(),
                1
            );
            let before = fixture.records().len();
            assert!(runtime
                .call_http(&target, "echo", json!({}), &Control::default())
                .is_err());
            assert_eq!(fixture.records().len(), before);
            assert_eq!(fixture.count("tools/call"), 1);
            assert_eq!(fixture.count("initialize"), 1);
        }
    }
}
