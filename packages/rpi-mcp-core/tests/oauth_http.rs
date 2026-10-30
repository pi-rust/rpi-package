//! HTTP loopback fixtures are explicit development transport tests, not production OAuth.
use rpi_mcp_core::{auth::parse_bearer_challenge, http::HttpTransport, *};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

struct Fixture {
    url: String,
    count: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<Value>>>,
    heads: Arc<Mutex<Vec<String>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Fixture {
    fn start(status: u16, token: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let b = bodies.clone();
        let heads = Arc::new(Mutex::new(Vec::new()));
        let h = heads.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s = stop.clone();
        let worker = thread::spawn(move || {
            let mut held = Vec::new();
            while !s.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut input = Vec::new();
                let mut buf = [0; 2048];
                let (head, value) = loop {
                    let n = stream.read(&mut buf).unwrap();
                    if n == 0 {
                        return;
                    }
                    input.extend_from_slice(&buf[..n]);
                    if let Some(end) = input.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&input[..end]).to_string();
                        let len = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if input.len() >= end + 4 + len {
                            break (
                                head,
                                serde_json::from_slice::<Value>(&input[end + 4..])
                                    .unwrap_or(Value::Null),
                            );
                        }
                    }
                };
                c.fetch_add(1, Ordering::SeqCst);
                b.lock().unwrap().push(value.clone());
                h.lock().unwrap().push(head.clone());
                if (status == 496 || status == 495)
                    && (value["method"] == "tools/call" || head.starts_with("DELETE "))
                {
                    // Keep the peer open without blocking acceptance of cleanup.
                    // Owned fixture sockets are dropped when the fixture stops.
                    held.push(stream);
                    continue;
                }
                let authorized = head
                    .to_ascii_lowercase()
                    .contains("authorization: bearer fixture-token");
                let mut code = if token || authorized && matches!(status, 401 | 495 | 496) {
                    200
                } else if matches!(status, 498 | 494 | 495 | 496 | 488) {
                    401
                } else {
                    status
                };
                if (489..=493).contains(&status) {
                    let (content_type, body, extra_length) = match status {
                        493 if value.get("method").is_none() => continue,
                        493 => (
                            "text/event-stream",
                            "data: {\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}\n\n",
                            0,
                        ),
                        492 => ("application/json", "{", 300),
                        491 => ("application/json", "{\"secret-json-canary\":invalid}", 0),
                        490 => ("secret-content-type-canary", "", 0),
                        _ => (
                            "text/event-stream",
                            "data: {\"secret-sse-canary\":invalid}\n\n",
                            0,
                        ),
                    };
                    let _ = write!(stream, "HTTP/1.1 200 Fixture\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len() + extra_length);
                    continue;
                }
                if status == 499 {
                    let _ = write!(stream, "HTTP/1.1 200 Fixture\r\nContent-Type: text/event-stream\r\nContent-Length: 300\r\nConnection: close\r\n\r\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/test\"}}\n\n");
                    continue;
                }
                if status == 497 {
                    continue;
                }
                if code == 200 && value.get("id").is_none() && !token {
                    code = 202;
                }
                let body = if token {
                    json!({"access_token":"fixture-token", "token_type":"Bearer"}).to_string()
                } else if code == 200 {
                    {
                        let result = match value["method"].as_str() {
                            Some("initialize") => {
                                json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
                            }
                            Some("tools/list") => {
                                json!({"tools":[{"name":"increment","inputSchema":{"type":"object"}}]})
                            }
                            _ => json!({"ok":true}),
                        };
                        json!({"jsonrpc":"2.0","id":value["id"],"result":result}).to_string()
                    }
                } else {
                    "secret-error-canary".into()
                };
                let challenge = if status == 494 {
                    "WWW-Authenticate: Bearer realm=\"unclosed\r\n"
                } else if code == 401 && status != 488 {
                    "WWW-Authenticate: Basic realm=\"other\", Bearer realm=\"a,b\", resource_metadata=\"https://issuer.example/prm\"\r\n"
                } else {
                    ""
                };
                let session = if value["method"] == "initialize" && code == 200 {
                    "Mcp-Session-Id: fixture-session\r\n"
                } else {
                    ""
                };
                let _ = write!(stream, "HTTP/1.1 {code} Fixture\r\n{challenge}{session}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        Self {
            url,
            count,
            bodies,
            heads,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
struct Provider {
    endpoint: String,
    cache: Mutex<HashMap<AuthBinding, CredentialLease>>,
    challenges: AtomicUsize,
    pending: bool,
    alive: AtomicUsize,
    calls: AtomicUsize,
    rejected: Mutex<Vec<String>>,
    lifetime: Mutex<Option<Duration>>,
}
struct Guard<'a>(&'a AtomicUsize);
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl CredentialProvider for Provider {
    fn rejected_lease(&self, binding: &AuthBinding, id: &str) {
        self.rejected.lock().unwrap().push(id.to_owned());
        let mut cache = self.cache.lock().unwrap();
        if cache.get(binding).is_some_and(|lease| lease.id() == id) {
            cache.remove(binding);
        }
    }
    fn credential<'a>(&'a self, request: AuthRequest<'a>) -> AuthFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            match request.reason {
                AuthReason::CachedOnly => Ok(self
                    .cache
                    .lock()
                    .unwrap()
                    .get(request.binding)
                    .cloned()
                    .map(AuthDecision::Credential)
                    .unwrap_or(AuthDecision::Unavailable)),
                AuthReason::Challenge { .. } => {
                    self.challenges.fetch_add(1, Ordering::SeqCst);
                    self.alive.fetch_add(1, Ordering::SeqCst);
                    let _guard = Guard(&self.alive);
                    if self.pending {
                        std::future::pending::<()>().await;
                    }
                    let response: Value = reqwest::Client::new()
                        .post(&self.endpoint)
                        .send()
                        .await
                        .map_err(|_| AuthError::Exchange)?
                        .json()
                        .await
                        .map_err(|_| AuthError::Exchange)?;
                    let lease = CredentialLease::new(
                        request.binding,
                        format!("lease-canary-{}", self.challenges.load(Ordering::SeqCst)),
                        "fixture-issuer".into(),
                        vec![],
                        response["access_token"]
                            .as_str()
                            .ok_or(AuthError::Exchange)?,
                        self.lifetime
                            .lock()
                            .unwrap()
                            .map(|duration| std::time::Instant::now() + duration),
                    )?;
                    self.cache
                        .lock()
                        .unwrap()
                        .insert(request.binding.clone(), lease.clone());
                    Ok(AuthDecision::Credential(lease))
                }
            }
        })
    }
}
fn provider(endpoint: String, pending: bool) -> Arc<Provider> {
    Arc::new(Provider {
        endpoint,
        cache: Mutex::new(HashMap::new()),
        challenges: AtomicUsize::new(0),
        pending,
        alive: AtomicUsize::new(0),
        calls: AtomicUsize::new(0),
        rejected: Mutex::new(Vec::new()),
        lifetime: Mutex::new(None),
    })
}
fn server(url: &str) -> HttpServer {
    HttpServer {
        url: url.into(),
        headers: Default::default(),
        enabled: true,
        timeout: 2,
        exposure: Default::default(),
    }
}
fn binding(url: &str, scope: &str) -> AuthBinding {
    AuthBinding::new(
        1,
        scope.into(),
        std::env::current_dir().unwrap(),
        url.into(),
        url.into(),
        1,
    )
    .unwrap()
}
fn request() -> Value {
    json!({"jsonrpc":"2.0","id":73,"method":"tools/call","params":{"name":"increment","arguments":{"x":1}}})
}
#[test]
fn token_exchange_exact_retry_cached_reuse_and_scope_isolation() {
    let rs = Fixture::start(401, false);
    let token = Fixture::start(200, true);
    let p = provider(token.url.clone(), false);
    let mut transport =
        HttpTransport::with_auth(&server(&rs.url), binding(&rs.url, "a"), p.clone()).unwrap();
    transport.raw(request(), &Control::default()).unwrap();
    transport.raw(request(), &Control::default()).unwrap();
    assert_eq!(rs.count.load(Ordering::SeqCst), 3);
    assert_eq!(token.count.load(Ordering::SeqCst), 1);
    assert!(rs.bodies.lock().unwrap().iter().all(|v| v == &request()));
    let mut sibling =
        HttpTransport::with_auth(&server(&rs.url), binding(&rs.url, "b"), p.clone()).unwrap();
    sibling.raw(request(), &Control::default()).unwrap();
    assert_eq!(token.count.load(Ordering::SeqCst), 2);
    assert!(!format!("{:?}", p.cache.lock().unwrap()).contains("fixture-token"));
}
#[test]
fn static_authorization_bypasses_provider_and_duplicates_fail() {
    let rs = Fixture::start(401, false);
    let p = provider(String::new(), true);
    let mut s = server(&rs.url);
    s.headers
        .insert("aUtHoRiZaTiOn".into(), "Bearer fixture-token".into());
    let mut t = HttpTransport::with_auth(&s, binding(&rs.url, "a"), p.clone()).unwrap();
    t.raw(request(), &Control::default()).unwrap();
    assert_eq!(p.challenges.load(Ordering::SeqCst), 0);
    s.headers.insert("Authorization".into(), "secret".into());
    assert!(HttpTransport::new(&s).is_err());
}
#[test]
fn forbidden_has_no_replay_or_error_body_leak() {
    let rs = Fixture::start(403, false);
    let p = provider(String::new(), false);
    let mut t =
        HttpTransport::with_auth(&server(&rs.url), binding(&rs.url, "a"), p.clone()).unwrap();
    let error = t.raw(request(), &Control::default()).unwrap_err();
    assert!(!error.contains("canary"));
    assert_eq!(rs.count.load(Ordering::SeqCst), 1);
    assert_eq!(p.challenges.load(Ordering::SeqCst), 0);
}
#[test]
fn cancelled_and_timed_out_provider_is_dropped_without_cancel_notification() {
    for cancel in [true, false] {
        let rs = Fixture::start(401, false);
        let p = provider(String::new(), true);
        let c = Control::default();
        let control = c.clone();
        let pp = p.clone();
        let url = rs.url.clone();
        let worker = thread::spawn(move || {
            let mut t = HttpTransport::with_auth(&server(&url), binding(&url, "a"), pp).unwrap();
            t.set_timeout(1);
            t.raw(request(), &control)
        });
        while p.alive.load(Ordering::SeqCst) == 0 {
            thread::sleep(Duration::from_millis(2));
        }
        if cancel {
            c.cancel();
        }
        assert!(worker.join().unwrap().is_err());
        assert_eq!(p.alive.load(Ordering::SeqCst), 0);
        assert_eq!(rs.count.load(Ordering::SeqCst), 1);
    }
}
#[test]
fn bounded_parser_handles_quotes_multischeme_duplicates_and_redaction() {
    let mut h = reqwest::header::HeaderMap::new();
    h.append("www-authenticate", "Basic realm=\"a,b\", bEaReR scope=\"read\\\"x,y\", RESOURCE_METADATA=\"https://example.test/prm\"".parse().unwrap());
    let challenge = parse_bearer_challenge(&h).unwrap().unwrap();
    assert_eq!(challenge.scope.as_deref(), Some("read\"x,y"));
    assert!(!format!("{challenge:?}").contains("example"));
    h.append(
        "www-authenticate",
        "Bearer realm=\"second\"".parse().unwrap(),
    );
    assert!(parse_bearer_challenge(&h).is_err());
    h.clear();
    h.insert(
        "www-authenticate",
        format!("Bearer realm=\"{}\"", "x".repeat(16384))
            .parse()
            .unwrap(),
    );
    assert!(parse_bearer_challenge(&h).is_err());
    h.clear();
    h.insert(
        "www-authenticate",
        "Bearer scope=\"a\", SCOPE=\"b\"".parse().unwrap(),
    );
    assert!(parse_bearer_challenge(&h).is_err());
}

#[test]
fn second_401_partial_sse_and_reset_never_loop() {
    for status in [498, 499, 497] {
        let rs = Fixture::start(status, false);
        let token = Fixture::start(200, true);
        let p = provider(token.url.clone(), false);
        let mut t =
            HttpTransport::with_auth(&server(&rs.url), binding(&rs.url, "a"), p.clone()).unwrap();
        assert!(t.raw(request(), &Control::default()).is_err());
        assert_eq!(
            rs.count.load(Ordering::SeqCst),
            if status == 498 { 2 } else { 1 }
        );
        assert_eq!(
            p.challenges.load(Ordering::SeqCst),
            usize::from(status == 498)
        );
    }
}
#[test]
fn binding_dimensions_and_revoked_lease_do_not_cross_reuse() {
    let rs = Fixture::start(401, false);
    let token = Fixture::start(200, true);
    let p = provider(token.url.clone(), false);
    let original = binding(&rs.url, "a");
    for dimension in 0..6 {
        let mut b = original.clone();
        match dimension {
            0 => {}
            1 => b.namespace += 1,
            2 => b.generation += 1,
            3 => b.target.push('x'),
            4 => b.cwd = std::fs::canonicalize("..").unwrap(),
            _ => {
                b.resource.push_str("/child");
            }
        }
        let mut config = server(&b.resource);
        config.timeout = 1;
        let mut t = HttpTransport::with_auth(&config, b, p.clone()).unwrap();
        t.raw(request(), &Control::default()).unwrap();
    }
    assert_eq!(token.count.load(Ordering::SeqCst), 6);
    // Simulate provider-side revocation: only this binding's grant is evicted.
    p.cache.lock().unwrap().remove(&original);
    let mut t = HttpTransport::with_auth(&server(&rs.url), original, p.clone()).unwrap();
    t.raw(request(), &Control::default()).unwrap();
    assert_eq!(token.count.load(Ordering::SeqCst), 7);
}
#[test]
fn runtime_close_drops_pending_provider_and_sibling_survives() {
    let rs = Fixture::start(401, false);
    let sibling_rs = Fixture::start(200, false);
    let p = provider(String::new(), true);
    let runtime = Arc::new(McpRuntime::with_auth(p.clone()));
    let context = RequestContext {
        scope: "pending".into(),
        cwd: std::env::current_dir().unwrap(),
    };
    let params = json!({"url":rs.url,"action":"list"});
    let r = runtime.clone();
    let c = context.clone();
    let q = params.clone();
    let worker = thread::spawn(move || r.list(&c, &q, &Control::default()));
    let end = std::time::Instant::now() + Duration::from_secs(3);
    while p.alive.load(Ordering::SeqCst) == 0 {
        assert!(std::time::Instant::now() < end);
        thread::sleep(Duration::from_millis(2));
    }
    let sibling = RequestContext {
        scope: "sibling".into(),
        cwd: context.cwd.clone(),
    };
    runtime
        .list(
            &sibling,
            &json!({"url":sibling_rs.url}),
            &Control::default(),
        )
        .unwrap();
    runtime
        .list(
            &context,
            &json!({"url":rs.url,"action":"close"}),
            &Control::default(),
        )
        .unwrap();
    assert!(worker.join().unwrap().is_err());
    assert_eq!(p.alive.load(Ordering::SeqCst), 0);
    assert_eq!(rs.count.load(Ordering::SeqCst), 1);
    runtime
        .list(
            &sibling,
            &json!({"url":sibling_rs.url}),
            &Control::default(),
        )
        .unwrap();
}

#[test]
fn rejected_ids_evict_cache_across_consecutive_operations() {
    let rs = Fixture::start(498, false);
    let token = Fixture::start(200, true);
    let p = provider(token.url.clone(), false);
    let b = binding(&rs.url, "rejections");
    let mut t = HttpTransport::with_auth(&server(&rs.url), b.clone(), p.clone()).unwrap();
    for _ in 0..2 {
        assert!(t.raw(request(), &Control::default()).is_err());
        assert!(!p.cache.lock().unwrap().contains_key(&b));
    }
    assert_eq!(rs.count.load(Ordering::SeqCst), 4);
    assert_eq!(p.challenges.load(Ordering::SeqCst), 2);
    assert_eq!(
        *p.rejected.lock().unwrap(),
        ["lease-canary-1", "lease-canary-2"]
    );
    assert!(rs
        .heads
        .lock()
        .unwrap()
        .iter()
        .step_by(2)
        .all(|h| !h.to_ascii_lowercase().contains("authorization:")));

    // An old rejection must not evict a newer grant for the same binding.
    let newer =
        CredentialLease::new(&b, "newer".into(), "issuer".into(), vec![], "token", None).unwrap();
    p.cache.lock().unwrap().insert(b.clone(), newer);
    p.rejected_lease(&b, "lease-canary-2");
    assert_eq!(p.cache.lock().unwrap()[&b].id(), "newer");
}

#[test]
fn malformed_initial_401_invalidates_cached_id_without_interaction() {
    let rs = Fixture::start(494, false);
    let p = provider(String::new(), true);
    let b = binding(&rs.url, "malformed");
    let lease =
        CredentialLease::new(&b, "initial".into(), "issuer".into(), vec![], "token", None).unwrap();
    p.cache.lock().unwrap().insert(b.clone(), lease);
    let mut t = HttpTransport::with_auth(&server(&rs.url), b.clone(), p.clone()).unwrap();
    for _ in 0..2 {
        assert!(t.raw(request(), &Control::default()).is_err());
    }
    assert_eq!(*p.rejected.lock().unwrap(), ["initial"]);
    assert!(p.cache.lock().unwrap().is_empty());
    assert_eq!(p.challenges.load(Ordering::SeqCst), 0);
    assert_eq!(rs.count.load(Ordering::SeqCst), 2);
}

#[test]
fn parser_utf8_tabs_token68_and_fail_closed_bounds() {
    fn parse(bytes: &[u8]) -> Result<Option<auth::BearerChallenge>, AuthError> {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            "www-authenticate",
            reqwest::header::HeaderValue::from_bytes(bytes).unwrap(),
        );
        parse_bearer_challenge(&h)
    }
    for opaque in ["YWJj/+", "YWJj/+==", "YWJj", "YWJj="] {
        let s = format!("Negotiate\t{opaque}\t, Basic\trealm\t=\t\"你好, café\", Bearer\tscope\t=\t\"读取\t文档\"");
        assert_eq!(
            parse(s.as_bytes()).unwrap().unwrap().scope.as_deref(),
            Some("读取\t文档")
        );
    }
    for s in [
        "Basic realm=\"unclosed, Bearer scope=read",
        "Basic realm=\"bad\\",
        "Basic realm=\"ok\"junk, Bearer scope=read",
        "Bearer scope=\"a\", SCOPE=\"b\"",
    ] {
        assert!(parse(s.as_bytes()).is_err(), "{s}");
    }
    assert!(parse(b"Basic realm=\"\xff\", Bearer scope=read").is_err());
    assert!(parse(b"Basic realm=\"\xc3\\xa9\", Bearer scope=read").is_err());
    let sixteen = format!("{}Bearer scope=read", "Negotiate YWJj, ".repeat(15));
    assert!(parse(sixteen.as_bytes()).unwrap().is_some());
    assert!(parse(format!("Negotiate YWJj, {sixteen}").as_bytes()).is_err());
    assert!(parse(format!("Bearer realm=\"{}\"", "é".repeat(8192)).as_bytes()).is_err());
}

#[test]
fn configuration_debug_redacts_headers_but_serialization_is_plain() {
    let mut s = server("https://example.test/mcp?secret-url-canary");
    s.headers
        .insert("Authorization".into(), "Bearer secret-header-canary".into());
    s.headers
        .insert("X-Private".into(), "secret-custom-canary".into());
    let entry = McpServerConfig::Http(s.clone());
    let config = McpConfig {
        servers: [("test".into(), entry.clone())].into(),
    };
    for debug in [
        format!("{s:?}"),
        format!("{entry:?}"),
        format!("{config:?}"),
    ] {
        assert!(!debug.contains("canary"));
        assert!(debug.contains("[redacted]"));
    }
    let value = serde_json::to_value(&config).unwrap();
    assert_eq!(
        value["mcpServers"]["test"]["headers"]["Authorization"],
        "Bearer secret-header-canary"
    );
    assert_eq!(serde_json::from_value::<McpConfig>(value).unwrap(), config);
}

#[test]
fn cleanup_wire_headers_expiry_no_provider_and_bounded_failure() {
    for expired in [false, true] {
        let rs = Fixture::start(496, false);
        let token = Fixture::start(200, true);
        let p = provider(token.url.clone(), false);
        if expired {
            *p.lifetime.lock().unwrap() = Some(Duration::from_millis(300));
        }
        let runtime = Arc::new(McpRuntime::with_auth(p.clone()));
        let context = RequestContext {
            scope: "cleanup".into(),
            cwd: std::env::current_dir().unwrap(),
        };
        runtime
            .list(&context, &json!({"url":rs.url}), &Control::default())
            .unwrap();
        let c = Control::default();
        let control = c.clone();
        let r = runtime.clone();
        let cx = context.clone();
        let url = rs.url.clone();
        let worker = thread::spawn(move || {
            r.call(
                &cx,
                &json!({"url":url,"tool":"increment","arguments":{}}),
                &control,
            )
        });
        let end = std::time::Instant::now() + Duration::from_secs(3);
        while !rs
            .bodies
            .lock()
            .unwrap()
            .iter()
            .any(|v| v["method"] == "tools/call")
        {
            assert!(std::time::Instant::now() < end);
            thread::sleep(Duration::from_millis(2));
        }
        let calls = p.calls.load(Ordering::SeqCst);
        if expired {
            thread::sleep(Duration::from_millis(350));
        }
        let start = std::time::Instant::now();
        c.cancel();
        assert!(worker.join().unwrap().is_err());
        runtime
            .list(
                &context,
                &json!({"url":rs.url,"action":"close"}),
                &Control::default(),
            )
            .unwrap();
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_eq!(p.calls.load(Ordering::SeqCst), calls);
        assert!(p.rejected.lock().unwrap().is_empty());
        let heads = rs.heads.lock().unwrap();
        let bodies = rs.bodies.lock().unwrap();
        let cancel = bodies
            .iter()
            .position(|v| v["method"] == "notifications/cancelled")
            .unwrap();
        let delete = heads.iter().position(|h| h.starts_with("DELETE ")).unwrap();
        for index in [cancel, delete] {
            let h = heads[index].to_ascii_lowercase();
            assert!(h.contains("mcp-session-id: fixture-session"));
            assert!(h.contains("mcp-protocol-version: 2025-06-18"));
            assert_eq!(h.contains("authorization: bearer fixture-token"), !expired);
        }
        assert_eq!(heads.iter().filter(|h| h.starts_with("DELETE ")).count(), 1);
    }
}

#[test]
fn all_http_sse_and_reply_failures_hide_url_and_error_canaries() {
    for status in [403, 497, 499, 493, 492, 491, 490, 489] {
        let rs = Fixture::start(status, false);
        let url = format!("{}?secret-url-canary", rs.url);
        let mut s = server(&url);
        s.headers
            .insert("X-Private".into(), "secret-header-canary".into());
        let mut t = HttpTransport::new(&s).unwrap();
        let error = t.raw(request(), &Control::default()).unwrap_err();
        assert!(!error.contains("canary"), "status {status}: {error}");
        assert!(!error.contains(&url), "status {status}: {error}");
        if status == 493 {
            assert_eq!(error, "reply to MCP server failed");
            assert_eq!(rs.count.load(Ordering::SeqCst), 2);
        }
    }
}

#[test]
fn missing_challenge_invalidates_and_cancelled_challenge_cannot_restore_rejected_lease() {
    for missing in [true, false] {
        let rs = Fixture::start(if missing { 488 } else { 498 }, false);
        let p = provider(String::new(), true);
        let b = binding(&rs.url, "cancel-rejected");
        let lease =
            CredentialLease::new(&b, "cached".into(), "issuer".into(), vec![], "token", None)
                .unwrap();
        p.cache.lock().unwrap().insert(b.clone(), lease);
        let c = Control::default();
        let control = c.clone();
        let pp = p.clone();
        let url = rs.url.clone();
        let worker = thread::spawn(move || {
            let mut t = HttpTransport::with_auth(&server(&url), b, pp).unwrap();
            t.raw(request(), &control)
        });
        if !missing {
            let end = std::time::Instant::now() + Duration::from_secs(3);
            while p.alive.load(Ordering::SeqCst) == 0 {
                assert!(std::time::Instant::now() < end);
                thread::sleep(Duration::from_millis(2));
            }
            assert!(p.cache.lock().unwrap().is_empty());
            c.cancel();
        }
        assert!(worker.join().unwrap().is_err());
        assert_eq!(*p.rejected.lock().unwrap(), ["cached"]);
        assert!(p.cache.lock().unwrap().is_empty());
        assert_eq!(p.alive.load(Ordering::SeqCst), 0);
        assert_eq!(rs.count.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn explicit_close_releases_local_lease_without_provider_revocation() {
    let rs = Fixture::start(200, false);
    let p = provider(String::new(), false);
    let b = binding(&rs.url, "close");
    let lease = CredentialLease::new(
        &b,
        "cached".into(),
        "issuer".into(),
        vec![],
        "fixture-token",
        None,
    )
    .unwrap();
    p.cache.lock().unwrap().insert(b.clone(), lease);
    let mut t = HttpTransport::with_auth(&server(&rs.url), b.clone(), p.clone()).unwrap();
    t.raw(
        json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
        &Control::default(),
    )
    .unwrap();
    let calls = p.calls.load(Ordering::SeqCst);
    t.close();
    t.close();
    assert_eq!(p.calls.load(Ordering::SeqCst), calls);
    assert!(p.rejected.lock().unwrap().is_empty());
    assert!(p.cache.lock().unwrap().contains_key(&b));
    assert_eq!(
        rs.heads
            .lock()
            .unwrap()
            .iter()
            .filter(|h| h.starts_with("DELETE "))
            .count(),
        1
    );
}
