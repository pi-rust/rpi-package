#![cfg(feature = "test-support")]
use rpi_extcore::{
    host::HostSession,
    mcp::{self, McpConfig, McpServerConfig, StdioServer},
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::mpsc,
};

fn mock(
    supported: bool,
) -> (
    Arc<HostSession>,
    rpi_extcore::host::TestEvents,
    mpsc::UnboundedReceiver<Value>,
) {
    let (input, output) = tokio::io::duplex(1024 * 1024);
    let commands = if supported {
        json!(["configure_external_tools", "external_tool_response"])
    } else {
        json!([])
    };
    let (host, events) = HostSession::test_connection_with_ready(
        input,
        128,
        json!({"type":"ready","protocol":"rpi","supportedCommands":commands}),
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let replies = events.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(output).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let request: Value = serde_json::from_str(&line).unwrap();
            tx.send(request.clone()).unwrap();
            let _ = replies.send(json!({"type":"response","id":request["id"],"success":true,"data":{"accepted":true}}));
        }
    });
    (host, events, rx)
}
fn config() -> McpConfig {
    McpConfig {
        scope: "fixture-session".into(),
        cwd: std::env::current_dir().unwrap(),
        discovery_timeout: Duration::from_secs(5),
        servers: BTreeMap::from([(
            "fixture".into(),
            McpServerConfig::Stdio(StdioServer {
                command: env!("CARGO_BIN_EXE_rpi-extcore-mcp-fixture").into(),
                args: vec![],
                env: BTreeMap::new(),
                cwd: None,
                enabled: true,
                timeout: 60,
                exposure: mcp::Exposure::Direct,
            }),
        )]),
    }
}
async fn next(rx: &mut mpsc::UnboundedReceiver<Value>) -> Value {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap()
}
#[tokio::test]
async fn config_call_cancel_isolation_and_cleanup() {
    let (host, events, mut rx) = mock(true);
    let mut binding = mcp::attach(host.clone(), config()).await.unwrap();
    let configured = next(&mut rx).await;
    assert_eq!(configured["type"], "configure_external_tools");
    let name = configured["tools"][0]["name"].as_str().unwrap();
    assert_eq!(name.len(), 64);
    assert_eq!(
        binding.descriptors()[0].remote_name,
        "remote.name/with punctuation"
    );
    assert_eq!(
        configured["tools"][0]["inputSchema"],
        json!({"type":"object"})
    );
    events.send(json!({"type":"external_tool_request","requestId":"one","toolName":name,"arguments":{"fail":true}})).unwrap();
    let response = next(&mut rx).await;
    assert_eq!(response["type"], "external_tool_response");
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["content"][0]["text"],
        "remote.name/with punctuation"
    );
    assert_eq!(response["result"]["content"][1]["type"], "image");
    let pid = response["result"]["structuredContent"]["pid"].clone();
    events.send(json!({"type":"external_tool_request","requestId":"one","toolName":name,"arguments":{}})).unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(100), rx.recv())
        .await
        .is_err());
    let (other_host, other_events, mut other_rx) = mock(true);
    let mut other = mcp::attach(other_host.clone(), config()).await.unwrap();
    let other_config = next(&mut other_rx).await;
    assert_eq!(other_config["tools"][0]["name"], name);
    other_events.send(json!({"type":"external_tool_request","requestId":"one","toolName":name,"arguments":{}})).unwrap();
    let other_response = next(&mut other_rx).await;
    assert_ne!(other_response["result"]["structuredContent"]["pid"], pid);
    events.send(json!({"type":"external_tool_request","requestId":"hang","toolName":name,"arguments":{"hang":true}})).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    events
        .send(json!({"type":"external_tool_request_closed","requestId":"hang"}))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), binding.shutdown())
        .await
        .unwrap()
        .unwrap();
    let clear = next(&mut rx).await;
    assert_eq!(clear["tools"], json!([]));
    assert!(binding.is_finished());
    other.shutdown().await.unwrap();
    host.terminate().await;
    other_host.terminate().await;
}
#[tokio::test]
async fn empty_noop_and_capability_gate() {
    let (host, _events, mut rx) = mock(false);
    assert!(mcp::attach(host.clone(), config())
        .await
        .err()
        .unwrap()
        .contains("configure_external_tools"));
    let mut empty = config();
    empty.servers.clear();
    let mut binding = mcp::attach(host.clone(), empty).await.unwrap();
    assert!(!binding.is_active());
    binding.shutdown().await.unwrap();
    assert!(rx.try_recv().is_err());
    host.terminate().await;
}

#[tokio::test]
async fn host_close_and_drop_remove_secret_metadata() {
    for drop_owner in [false, true] {
        let marker = uuid::Uuid::new_v4().to_string();
        let mut cfg = config();
        if let McpServerConfig::Stdio(server) = cfg.servers.get_mut("fixture").unwrap() {
            server
                .env
                .insert("EXTCORE_SECRET_TEST_MARKER".into(), marker.clone());
        }
        let (host, events, mut rx) = mock(true);
        let mut binding = mcp::attach(host.clone(), cfg).await.unwrap();
        next(&mut rx).await;
        let path = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(Result::ok)
            .find_map(|e| {
                let path = e.path();
                if !e.file_name().to_string_lossy().starts_with("rpi-mcp-") {
                    return None;
                }
                std::fs::read_to_string(&path)
                    .ok()
                    .filter(|s| s.contains(&marker))
                    .map(|_| path)
            })
            .expect("live session private metadata exists");
        if drop_owner {
            drop(binding);
        } else {
            events.send(json!({"type":"host_closed"})).unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !binding.is_finished() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            binding.shutdown().await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while path.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        host.terminate().await;
    }
}

#[tokio::test]
async fn optional_provider_is_scoped_and_used_on_actual_http_discovery() {
    use rpi_mcp_core::{AuthDecision, AuthFuture, AuthRequest, CredentialLease, CredentialProvider};
    use std::io::{Read, Write};
    struct Provider(std::sync::Mutex<Vec<rpi_mcp_core::AuthBinding>>);
    impl CredentialProvider for Provider {
        fn credential<'a>(&'a self, request: AuthRequest<'a>) -> AuthFuture<'a> {
            Box::pin(async move {
                self.0.lock().unwrap().push(request.binding.clone());
                Ok(AuthDecision::Credential(CredentialLease::new(request.binding,
                    "lease".into(), "https://issuer.example/".into(), vec!["tools".into()],
                    "fixture-token", None)?))
            })
        }
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopping = stop.clone();
    let worker = std::thread::spawn(move || {
        while !stopping.load(std::sync::atomic::Ordering::SeqCst) {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(2)); continue;
            };
            // Windows accepted sockets inherit the listener's nonblocking mode.
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut input = Vec::new();
            let mut buf = [0u8; 2048];
            let (header, value) = loop {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 { break (String::new(), Value::Null); }
                input.extend_from_slice(&buf[..n]);
                if let Some(end) = input.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&input[..end]).to_string();
                    let len = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|n| n.trim().parse::<usize>().ok())).unwrap_or(0);
                    if input.len() >= end + 4 + len {
                        break (head, serde_json::from_slice::<Value>(&input[end+4..end+4+len]).unwrap_or(Value::Null));
                    }
                }
            };
            if header.is_empty() { continue; }
            assert!(header.to_ascii_lowercase().contains("authorization: bearer fixture-token"));
            let result = match value["method"].as_str() {
                Some("initialize") => json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}),
                Some("tools/list") => json!({"tools":[{"name":"scoped","inputSchema":{"type":"object"}}]}),
                _ => json!({}),
            };
            let body = json!({"jsonrpc":"2.0","id":value["id"],"result":result}).to_string();
            let code = if value.get("id").is_none() { 202 } else { 200 };
            write!(stream, "HTTP/1.1 {code} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let provider = Arc::new(Provider(Default::default()));
    let (host, _events, mut rx) = mock(true);
    let mut cfg = config();
    cfg.servers = BTreeMap::from([("http".into(), McpServerConfig::Http(mcp::HttpServer {
        url: url.clone(), headers: BTreeMap::new(), enabled: true, timeout: 5, exposure: mcp::Exposure::Direct,
    }))]);
    let mut binding = mcp::attach_with_options(host.clone(), cfg.clone(), mcp::McpOptions {
        credential_provider: Some(provider.clone()),
        ..Default::default()
    }).await.unwrap();
    assert_eq!(next(&mut rx).await["tools"][0]["name"], binding.descriptors()[0].native_name);
    assert_eq!(binding.descriptors()[0].remote_name, "scoped");
    for observed in provider.0.lock().unwrap().iter() {
        assert_eq!(observed.scope, cfg.scope);
        assert_eq!(observed.cwd, std::fs::canonicalize(&cfg.cwd).unwrap());
        assert_eq!(observed.resource, url);
    }
    assert!(!provider.0.lock().unwrap().is_empty());
    binding.shutdown().await.unwrap();
    host.terminate().await;
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    worker.join().unwrap();
}

#[tokio::test]
async fn overflow_fails_explicitly() {
    let (host, events, mut rx) = mock(true);
    let mut binding = mcp::attach(host.clone(), config()).await.unwrap();
    let configured = next(&mut rx).await;
    // No await: the receiver must observe a lagged queue, never replay tools.
    for i in 0..512 {
        let _ = events.send(json!({"type":"external_tool_request","requestId":format!("overflow-{i}"),"toolName":configured["tools"][0]["name"],"arguments":{}}));
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while !binding.is_finished() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(binding
        .shutdown()
        .await
        .unwrap_err()
        .contains("event stream lost"));
    host.terminate().await;
}
