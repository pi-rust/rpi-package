//! Unit-level transport fixtures: fake sampling only, no native model execution.
use super::*;
use crate::auth::{AuthError, AuthFuture};
use crate::sampling::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{mpsc, Mutex};
use std::time::Instant;

struct Fake;
impl SamplingAuthorizer for Fake {
    fn authorize<'a>(&'a self, c: &'a SamplingContext, _: &'a SamplingRequest, _: &'a Control) -> AuthorizationFuture<'a> {
        Box::pin(async move { Ok(SamplingPolicy { context: c.clone(), max_messages: 128, max_tokens: 100, max_duration: Duration::from_secs(10) }) })
    }
}
impl SamplingExecutor for Fake {
    fn execute<'a>(&'a self, _: &'a SamplingContext, _: &'a SamplingRequest, _: &'a Control) -> SamplingFuture<'a> {
        Box::pin(async { SamplingResult::parse(json!({"role":"assistant","content":{"type":"text","text":"done"},"model":"fake","stopReason":"endTurn"})) })
    }
}
fn callbacks() -> Arc<crate::callback::Callbacks> {
    Arc::new(crate::callback::Callbacks::new(SamplingOptions { authorizer: Arc::new(Fake), executor: Arc::new(Fake) }).unwrap())
}
fn accept(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let end = Instant::now() + Duration::from_secs(4);
    loop {
        match listener.accept() {
            Ok((s, _)) => { s.set_read_timeout(Some(Duration::from_secs(3))).unwrap(); return s; }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => panic!("accept: {e}"),
        }
    }
}
fn read(s: &mut TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let mut b = [0];
    while !bytes.ends_with(b"\r\n\r\n") { s.read_exact(&mut b).unwrap(); bytes.push(b[0]); }
    let headers = String::from_utf8(bytes).unwrap();
    let len = headers.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|s| s.trim().parse::<usize>().unwrap())).unwrap_or(0);
    let mut body = vec![0; len]; s.read_exact(&mut body).unwrap();
    (headers, if body.is_empty() { Value::Null } else { serde_json::from_slice(&body).unwrap() })
}
fn respond(s: &mut TcpStream, status: &str, body: &str) {
    write!(s, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    s.flush().unwrap();
}
fn server(listener: &TcpListener) -> HttpServer {
    serde_json::from_value(json!({"url":format!("http://{}/mcp", listener.local_addr().unwrap()), "timeout":10})).unwrap()
}
#[test]
fn http_one_chunk_nine_reverse_replies_fail_before_stalled_parent_deadline() {
    for method in ["ping", "sampling/createMessage"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut transport = HttpTransport::new(&server(&listener)).unwrap();
        transport.set_callbacks(Some(callbacks()));
        let (done, hold) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut s = accept(&listener); read(&mut s);
            let body = (0..9).map(|id| format!("data: {}\n\n", json!({"jsonrpc":"2.0","id":id,"method":method}))).collect::<String>();
            write!(s, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n", body.len(), body).unwrap(); s.flush().unwrap();
            // Keep the parent stream open: EOF must not be the failure signal.
            hold.recv_timeout(Duration::from_secs(3)).unwrap();
        });
        let start = Instant::now();
        let error = transport.raw(json!({"jsonrpc":"2.0","id":100,"method":"tools/call"}), &Control::default()).unwrap_err();
        done.send(()).unwrap(); worker.join().unwrap();
        assert!(error.contains("callback reply transport failed"), "{error}");
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

#[derive(Default)]
struct Ledger { rejected: Mutex<Vec<String>>, current: Mutex<String>, lookups: Mutex<usize> }
impl CredentialProvider for Ledger {
    fn credential<'a>(&'a self, r: AuthRequest<'a>) -> AuthFuture<'a> {
        Box::pin(async move {
            assert!(matches!(r.reason, AuthReason::CachedOnly));
            *self.lookups.lock().unwrap() += 1;
            Ok(AuthDecision::Credential(CredentialLease::new(r.binding, "old-lease".into(), "issuer".into(), vec![], "old-token", None).map_err(|_| AuthError::Provider)?))
        })
    }
    fn rejected_lease(&self, _: &AuthBinding, id: &str) {
        self.rejected.lock().unwrap().push(id.into());
        let mut current = self.current.lock().unwrap();
        if current.as_str() == id { current.clear(); }
    }
}
#[test]
fn callback_post_401_rejects_exact_lease_once_without_replay_or_cleanup_reuse() {
    for enabled in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let config = server(&listener);
        let binding = AuthBinding::new(1, "scope".into(), std::env::current_dir().unwrap(), "target".into(), config.url.clone(), 1).unwrap();
        let provider = Arc::new(Ledger::default());
        *provider.current.lock().unwrap() = "newer-lease".into();
        let mut transport = HttpTransport::with_auth(&config, binding, provider.clone()).unwrap();
        transport.session = Some("session".into());
        if enabled { transport.set_callbacks(Some(callbacks())); }
        let worker = std::thread::spawn(move || {
            let mut parent = accept(&listener); let (headers, _) = read(&mut parent);
            assert!(headers.contains("Bearer old-token"));
            write!(parent, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {}\n\n", json!({"jsonrpc":"2.0","id":"ping","method":"ping"})).unwrap(); parent.flush().unwrap();
            let mut reply = accept(&listener); let (headers, body) = read(&mut reply);
            assert!(headers.contains("Bearer old-token")); assert_eq!(body["id"], "ping");
            respond(&mut reply, "401 Unauthorized", "");
            let mut cleanup = accept(&listener); let (headers, _) = read(&mut cleanup);
            assert!(headers.starts_with("DELETE ")); assert!(!headers.to_ascii_lowercase().contains("authorization:"));
            respond(&mut cleanup, "204 No Content", "");
        });
        let error = transport.raw(json!({"jsonrpc":"2.0","id":100,"method":"tools/call"}), &Control::default()).unwrap_err();
        assert_eq!(error, "MCP server did not accept client reply");
        assert!(transport.lease.borrow().is_none());
        transport.close(); worker.join().unwrap();
        assert_eq!(*provider.rejected.lock().unwrap(), vec!["old-lease"]);
        assert_eq!(*provider.current.lock().unwrap(), "newer-lease");
        assert_eq!(*provider.lookups.lock().unwrap(), 1);
    }
}

#[cfg(windows)]
#[test]
fn stdio_callback_writer_overflow_fails_with_peer_not_reading() {
    use crate::transport::StdioTransport;
    let script = "$null=[Console]::ReadLine(); $id='x'*1000; for($i=0;$i -lt 2000;$i++){[Console]::WriteLine('{\"jsonrpc\":\"2.0\",\"method\":\"ping\",\"id\":\"'+$id+$i+'\"}')}; Start-Sleep -Seconds 30";
    let mut transport = StdioTransport::start("powershell.exe", &["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), script.into()], &Default::default(), None, None).unwrap();
    transport.set_callbacks(Some(callbacks())); transport.set_timeout(10);
    let start = Instant::now();
    let error = transport.request(&crate::jsonrpc::Request { jsonrpc: "2.0".into(), id: 1, method: "tools/call".into(), params: Some(json!({})) }).unwrap_err();
    assert!(error.contains("callback reply transport failed"), "{error}");
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn successful_async_sample_overflow_wakes_stalled_owner() {
    struct Gated(Control);
    impl SamplingExecutor for Gated {
        fn execute<'a>(&'a self, _: &'a SamplingContext, _: &'a SamplingRequest, _: &'a Control) -> SamplingFuture<'a> {
            Box::pin(async move {
                self.0.cancelled().await;
                SamplingResult::parse(json!({"role":"assistant","content":{"type":"text","text":"done"},"model":"fake","stopReason":"endTurn"}))
            })
        }
    }
    let release = Control::default();
    let callbacks = crate::callback::Callbacks::new(SamplingOptions { authorizer: Arc::new(Fake), executor: Arc::new(Gated(release.clone())) }).unwrap();
    callbacks.activate(SamplingContext { namespace:"test".into(), scope:"scope".into(), server:"server".into(), cwd:std::env::current_dir().unwrap(), target:"target".into(), generation:1, operation:"tools/call:1".into(), deadline:Instant::now()+Duration::from_secs(5) }, Control::default());
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    for _ in 0..8 { tx.try_send(Value::Null).unwrap(); }
    let failure = crate::callback::ReplyFailure::default();
    let sink_failure = failure.clone();
    let success = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sink_success = success.clone();
    callbacks.dispatch(&json!({"jsonrpc":"2.0","id":"sample","method":"sampling/createMessage","params":{"messages":[{"role":"user","content":{"type":"text","text":"test"}}],"maxTokens":1}}), Arc::new(move |v| {
        sink_success.store(v.get("result").is_some(), std::sync::atomic::Ordering::Release);
        if tx.try_send(v).is_err() { sink_failure.fail(); }
    }));
    release.cancel();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        // No input events: a completed successful producer must wake this owner.
        tokio::time::timeout(Duration::from_secs(1), failure.failed()).await.unwrap();
    });
    assert!(failure.check().unwrap_err().contains("callback reply transport failed"));
    assert!(success.load(std::sync::atomic::Ordering::Acquire));
    callbacks.shutdown();
}
