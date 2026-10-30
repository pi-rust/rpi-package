//! Focused identity admission/retirement tests; loopback is not an OAuth implementation.
use super::*;
use crate::{
    AuthBinding, AuthDecision, AuthFuture, AuthRequest, CredentialLease, CredentialProvider,
};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Barrier;

#[derive(Default)]
struct Provider {
    bindings: Mutex<Vec<AuthBinding>>,
    pending: AtomicBool,
    active: AtomicUsize,
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl CredentialProvider for Provider {
    fn credential<'a>(&'a self, request: AuthRequest<'a>) -> AuthFuture<'a> {
        Box::pin(async move {
            self.bindings.lock().unwrap().push(request.binding.clone());
            let pending = self.pending.load(Ordering::SeqCst);
            self.active.fetch_add(1, Ordering::SeqCst);
            let _active = Active(&self.active);
            if pending {
                std::future::pending::<()>().await;
            }
            Ok(AuthDecision::Credential(CredentialLease::new(
                request.binding,
                "lifecycle-lease".into(),
                "fixture".into(),
                vec![],
                "lifecycle-token",
                None,
            )?))
        })
    }
}

struct Fixture {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Fixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0; 4096];
                let request = loop {
                    let n = stream.read(&mut buf).unwrap();
                    assert_ne!(n, 0);
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let len = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|s| s.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + len {
                            break serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + len])
                                .unwrap();
                        }
                    }
                };
                let (code, body) = if request.get("id").is_none() {
                    (202, String::new())
                } else {
                    let result = if request["method"] == "initialize" {
                        json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"lifecycle","version":"1"}})
                    } else {
                        json!({"tools":[]})
                    };
                    (
                        200,
                        json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string(),
                    )
                };
                write!(stream, "HTTP/1.1 {code} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        Self {
            url,
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
fn context(scope: &str) -> RequestContext {
    RequestContext {
        scope: scope.into(),
        cwd: std::env::current_dir().unwrap(),
    }
}
fn assert_drained(runtime: &McpRuntime) {
    assert!(runtime.auth_targets.lock().unwrap().is_empty());
    assert!(runtime.sessions.lock().unwrap().is_empty());
    assert!(runtime.subscriptions.lock().unwrap().is_empty());
    assert!(runtime.pending.lock().unwrap().is_empty());
}
fn generation(key: &str) -> u64 {
    serde_json::from_str::<Value>(key).unwrap()[5]
        .as_u64()
        .unwrap()
}

#[test]
fn unknown_closes_and_more_than_128_open_close_cycles_reclaim_identity_capacity() {
    let fixture = Fixture::start();
    let provider = Arc::new(Provider::default());
    let runtime = McpRuntime::with_auth(provider.clone());
    for i in 0..140 {
        runtime
            .list(
                &context(&format!("unknown-{i}")),
                &json!({"url":fixture.url,"action":"close"}),
                &Control::default(),
            )
            .unwrap();
    }
    assert_drained(&runtime);
    assert!(provider.bindings.lock().unwrap().is_empty());
    let mut previous = 0;
    for i in 0..140 {
        let context = context(&format!("cycle-{i}"));
        runtime
            .list(&context, &json!({"url":fixture.url}), &Control::default())
            .unwrap();
        let key = runtime
            .sessions
            .lock()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        assert!(generation(&key) > previous);
        previous = generation(&key);
        let entry = runtime.sessions.lock().unwrap()[&key].clone();
        runtime
            .list(
                &context,
                &json!({"url":fixture.url,"action":"close"}),
                &Control::default(),
            )
            .unwrap();
        assert!(entry.dispatcher.lock().unwrap().is_none());
        let state = entry.session.lock().unwrap();
        assert!(state.session.is_none() && state.config.is_none());
        assert_drained(&runtime);
    }
}

#[test]
fn shutdown_revokes_unadmitted_targets_and_reopening_uses_fresh_bindings() {
    let fixture = Fixture::start();
    let provider = Arc::new(Provider::default());
    let runtime = McpRuntime::with_auth(provider.clone());
    let params = json!({"url":fixture.url});
    let ctx = context("shutdown");
    runtime.list(&ctx, &params, &Control::default()).unwrap();
    let stale = runtime.target(&ctx, &params).unwrap();
    let old = provider.bindings.lock().unwrap()[0].clone();
    runtime.shutdown_connections();
    assert_drained(&runtime);
    assert_stale(&runtime, stale);
    for i in 0..140 {
        let ctx = context(&format!("after-shutdown-{i}"));
        runtime.list(&ctx, &params, &Control::default()).unwrap();
        runtime.shutdown_connections();
        assert_drained(&runtime);
    }
    runtime.list(&ctx, &params, &Control::default()).unwrap();
    let reopened = provider.bindings.lock().unwrap().last().unwrap().clone();
    assert_ne!(reopened, old);
    let sibling_runtime = McpRuntime::with_auth(provider.clone());
    sibling_runtime
        .list(&ctx, &params, &Control::default())
        .unwrap();
    let bindings = provider.bindings.lock().unwrap();
    assert_ne!(bindings.last().unwrap(), &reopened);
}

fn assert_stale(runtime: &McpRuntime, target: (String, String, McpServerConfig, u64)) {
    let (key, _, config, timeout) = target;
    let result = runtime.with_session(
        key,
        config,
        timeout,
        &Control::default(),
        None,
        |_, _, _| -> Result<(), String> {
            panic!("retired generation reached the session callback")
        },
    );
    assert_eq!(result.unwrap_err(), "MCP authentication identity retired");
}

#[test]
fn barrier_stale_generation_cannot_reacquire_after_config_retirement() {
    let fixture = Fixture::start();
    let provider = Arc::new(Provider::default());
    let runtime = Arc::new(McpRuntime::with_auth(provider.clone()));
    let dir = std::env::temp_dir().join(format!(
        "mcp-lifecycle-{}-{}",
        std::process::id(),
        AUTH_IDS.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mcp.json");
    let write_config = |timeout| {
        std::fs::write(
            &path,
            json!({"mcpServers":{"fixture":{"url":fixture.url,"timeout":timeout}}}).to_string(),
        )
        .unwrap()
    };
    write_config(2);
    let params = json!({"configPath":path,"server":"fixture"});
    let ctx = context("config-race");
    // Establish G1, including an actual credential binding and owned worker.
    runtime.list(&ctx, &params, &Control::default()).unwrap();
    let first = runtime.target(&ctx, &params).unwrap();
    let old_entry = runtime.sessions.lock().unwrap()[&first.0].clone();
    runtime.add_subscription(&first.0, "mem://retired");
    let selected = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let worker = {
        let runtime = runtime.clone();
        let params = params.clone();
        let ctx = ctx.clone();
        let selected = selected.clone();
        let resume = resume.clone();
        std::thread::spawn(move || {
            // Private test interleave: exactly between target and with_session.
            let stale = runtime.target(&ctx, &params).unwrap();
            selected.wait();
            resume.wait();
            assert_stale(&runtime, stale);
        })
    };
    selected.wait();
    write_config(3);
    runtime.list(&ctx, &params, &Control::default()).unwrap();
    let second = runtime.target(&ctx, &params).unwrap();
    assert!(generation(&second.0) > generation(&first.0));
    assert!(old_entry.dispatcher.lock().unwrap().is_none());
    assert!(old_entry.session.lock().unwrap().config.is_none());
    let calls_before_resume = provider.bindings.lock().unwrap().len();
    resume.wait();
    worker.join().unwrap();
    let bindings = provider.bindings.lock().unwrap();
    assert_eq!(
        bindings.len(),
        calls_before_resume,
        "stale admission must not consult provider"
    );
    assert_ne!(bindings.first().unwrap(), bindings.last().unwrap());
    drop(bindings);
    runtime
        .list(
            &ctx,
            &json!({"configPath":path,"server":"fixture","action":"close"}),
            &Control::default(),
        )
        .unwrap();
    assert_drained(&runtime);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn close_cancels_provider_without_blocking_sibling_and_retires_reservations() {
    let fixture = Fixture::start();
    let provider = Arc::new(Provider::default());
    let runtime = Arc::new(McpRuntime::with_auth(provider.clone()));
    let params = json!({"url":fixture.url});
    runtime
        .list(&context("sibling"), &params, &Control::default())
        .unwrap();
    provider.pending.store(true, Ordering::SeqCst);
    let worker = {
        let runtime = runtime.clone();
        let params = params.clone();
        std::thread::spawn(move || runtime.list(&context("pending"), &params, &Control::default()))
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while provider.active.load(Ordering::SeqCst) == 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    provider.pending.store(false, Ordering::SeqCst);
    // This would deadlock if admission held the identity registry over provider I/O.
    runtime
        .list(&context("sibling"), &params, &Control::default())
        .unwrap();
    let stale = runtime.target(&context("pending"), &params).unwrap();
    runtime
        .list(
            &context("pending"),
            &json!({"url":fixture.url,"action":"close"}),
            &Control::default(),
        )
        .unwrap();
    assert!(worker.join().unwrap().is_err());
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    assert_stale(&runtime, stale);
    runtime
        .list(&context("sibling"), &params, &Control::default())
        .unwrap();
    assert_eq!(runtime.sessions.lock().unwrap().len(), 1);
    runtime.shutdown_connections();
    assert_drained(&runtime);
}
