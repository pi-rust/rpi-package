//! Loopback HTTP MCP + in-memory native RPC bridge (no model or installed native binary).
#![cfg(feature = "test-support")]
#![forbid(unsafe_code)]
use rpi_extcore::{
    external_tools::{HttpToolServer, StdioToolServer, ToolServer},
    host::{HostSession, TestEvents},
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::JoinHandle,
};
const WINDOW: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct Record {
    method: String,
    auth: String,
    session: String,
}
struct State {
    records: Mutex<Vec<Record>>,
    connections: AtomicUsize,
    sessions: AtomicUsize,
    release: Notify,
    block: &'static str,
    bad_list: bool,
}
struct HttpFixture {
    url: String,
    state: Arc<State>,
    task: JoinHandle<()>,
}
impl HttpFixture {
    async fn new(block: &'static str, bad_list: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let state = Arc::new(State {
            records: Mutex::new(vec![]),
            connections: AtomicUsize::new(0),
            sessions: AtomicUsize::new(0),
            release: Notify::new(),
            block,
            bad_list,
        });
        let shared = state.clone();
        let task = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.unwrap();
                        shared.connections.fetch_add(1, Ordering::SeqCst);
                        let shared = shared.clone();
                        tasks.spawn(async move { serve(socket, shared).await; });
                    }
                    Some(result) = tasks.join_next(), if !tasks.is_empty() => { result.unwrap(); }
                }
            }
        });
        Self { url, state, task }
    }
    fn server(&self, auth: &str) -> ToolServer {
        ToolServer::Http(HttpToolServer {
            name: "http-fixture".into(),
            url: self.url.clone(),
            headers: BTreeMap::from([("Authorization".into(), auth.into())]),
        })
    }
    fn count(&self, method: &str) -> usize {
        self.state
            .records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == method)
            .count()
    }
    async fn wait(&self, method: &str, count: usize) {
        tokio::time::timeout(WINDOW, async {
            while self.count(method) < count {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("HTTP fixture did not receive expected request");
    }
}
impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn raw_result() -> Value {
    json!({"content":[{"type":"text","text":"actual HTTP","textSignature":"signature","_meta":{"raw":true}},{"type":"image","data":"aGk=","mimeType":"image/png","annotations":{"priority":0.5}}],"structuredContent":{"nested":[1,true]},"isError":false,"_meta":{"preserved":true}})
}
async fn serve(socket: TcpStream, state: Arc<State>) {
    let mut reader = BufReader::new(socket);
    let mut first = String::new();
    if reader.read_line(&mut first).await.unwrap_or(0) == 0 {
        return;
    }
    let verb = first.split_whitespace().next().unwrap();
    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let length: usize = headers
        .get("content-length")
        .map(|n| n.parse().unwrap())
        .unwrap_or(0);
    assert!(length < 1024 * 1024);
    let mut bytes = vec![0; length];
    if reader.read_exact(&mut bytes).await.is_err() {
        return;
    }
    let request: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    let method = if verb == "DELETE" {
        "DELETE"
    } else {
        request["method"].as_str().unwrap()
    };
    state.records.lock().unwrap().push(Record {
        method: method.into(),
        auth: headers.get("authorization").cloned().unwrap_or_default(),
        session: headers.get("mcp-session-id").cloned().unwrap_or_default(),
    });
    if method == state.block {
        state.release.notified().await;
    }
    let mut extra = String::new();
    let (status, body) = if verb == "DELETE" || request.get("id").is_none() {
        ("202 Accepted", String::new())
    } else {
        let result = match method {
            "initialize" => {
                let n = state.sessions.fetch_add(1, Ordering::SeqCst);
                extra = format!("Mcp-Session-Id: fixture-{n}\r\n");
                json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"http-fixture","version":"1"}})
            }
            "tools/list" if state.bad_list => {
                json!({"tools":[{"name":"bad","inputSchema":{"type":"string"}}]})
            }
            "tools/list" => {
                json!({"tools":[{"name":"echo","description":"HTTP echo","inputSchema":{"type":"object"}}]})
            }
            "tools/call" => raw_result(),
            _ => json!({}),
        };
        (
            "200 OK",
            json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string(),
        )
    };
    let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = reader.get_mut().write_all(response.as_bytes()).await;
}

struct TestBridge {
    host: Arc<HostSession>,
    events: TestEvents,
    peer: BufReader<DuplexStream>,
}
impl TestBridge {
    fn new() -> Self {
        let (writer, peer) = tokio::io::duplex(65536);
        let (host, events) = HostSession::test_connection_with_ready(
            writer,
            2,
            json!({
                "type":"ready","protocol":"rpi",
                "supportedCommands":["configure_external_tools","external_tool_response"],
                "supportedEvents":["external_tool_request","external_tool_request_closed"]
            }),
        );
        Self {
            host,
            events,
            peer: BufReader::new(peer),
        }
    }
    async fn install(&mut self, servers: Vec<ToolServer>) -> Value {
        let configure = self.host.configure_tools(
            servers,
            std::env::current_dir().unwrap(),
            "same-scope".into(),
        );
        let native = async {
            let config = frame(&mut self.peer).await;
            assert_eq!(config["type"], "configure_external_tools");
            let names: Vec<Value> = config["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].clone())
                .collect();
            self.events.send(json!({"type":"response","id":config["id"],"success":true,"data":{"tools":names}})).ok();
            config
        };
        let (result, config) = tokio::join!(configure, native);
        result.unwrap();
        config
    }
    fn invoke(&self, name: &Value, id: &str) {
        self.events.send(json!({"type":"external_tool_request","requestId":id,"toolName":name,"arguments":{}})).ok();
    }
    fn close(&self, id: &str) {
        self.events
            .send(json!({"type":"external_tool_request_closed","requestId":id}))
            .ok();
    }
    async fn result(&mut self, id: &str) -> Value {
        let response = frame(&mut self.peer).await;
        assert_eq!(response["type"], "external_tool_response");
        assert_eq!(response["requestId"], id);
        self.events.send(json!({"type":"response","id":response["id"],"success":true,"data":{"accepted":true}})).ok();
        self.close(id);
        response
    }
}
async fn frame(peer: &mut BufReader<DuplexStream>) -> Value {
    let mut line = String::new();
    tokio::time::timeout(WINDOW, peer.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}
async fn no_frame(peer: &mut BufReader<DuplexStream>) {
    let mut line = String::new();
    match tokio::time::timeout(Duration::from_millis(200), peer.read_line(&mut line)).await {
        Err(_) | Ok(Ok(0)) => {}
        other => panic!("unexpected late native reply: {other:?} {line}"),
    }
}

#[tokio::test]
async fn every_native_capability_is_required_before_first_http_connection() {
    let fixture = HttpFixture::new("", false).await;
    for missing in [
        "configure_external_tools",
        "external_tool_response",
        "external_tool_request",
        "external_tool_request_closed",
    ] {
        let (writer, _peer) = tokio::io::duplex(4096);
        let commands: Vec<_> = ["configure_external_tools", "external_tool_response"]
            .into_iter()
            .filter(|s| *s != missing)
            .collect();
        let events: Vec<_> = ["external_tool_request", "external_tool_request_closed"]
            .into_iter()
            .filter(|s| *s != missing)
            .collect();
        let (host, _events) = HostSession::test_connection_with_ready(
            writer,
            2,
            json!({"type":"ready","protocol":"rpi","supportedCommands":commands,"supportedEvents":events}),
        );
        assert_eq!(
            host.configure_tools(
                vec![fixture.server("secret")],
                std::env::current_dir().unwrap(),
                "scope".into()
            )
            .await
            .unwrap_err(),
            "native host does not support external tools"
        );
        host.terminate().await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fixture.state.connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn http_routes_raw_native_results_and_canonical_configuration_without_requery() {
    let fixture = HttpFixture::new("", false).await;
    let mut bridge = TestBridge::new();
    let config = bridge
        .install(vec![fixture.server("Bearer secret-A")])
        .await;
    let encoded = config.to_string();
    assert!(!encoded.contains("secret-A"));
    assert!(!encoded.contains(&fixture.url));
    let mut canonical = fixture.server("Bearer secret-A");
    if let ToolServer::Http(server) = &mut canonical {
        server.headers = BTreeMap::from([("authorization".into(), "Bearer secret-A".into())]);
    }
    bridge
        .host
        .configure_tools(
            vec![canonical],
            std::env::current_dir().unwrap(),
            "same-scope".into(),
        )
        .await
        .unwrap();
    assert!(bridge
        .host
        .configure_tools(
            vec![fixture.server("changed-secret")],
            std::env::current_dir().unwrap(),
            "same-scope".into()
        )
        .await
        .unwrap_err()
        .contains("close and reopen"));
    for id in ["one", "two"] {
        bridge.invoke(&config["tools"][0]["name"], id);
        assert_eq!(bridge.result(id).await["result"], raw_result());
    }
    assert_eq!(fixture.count("initialize"), 1);
    assert_eq!(fixture.count("tools/list"), 1);
    assert_eq!(fixture.count("tools/call"), 2);
    bridge.host.terminate().await;
    fixture.wait("DELETE", 1).await;
}

#[tokio::test]
async fn siblings_isolate_authorization_sessions_and_shutdown() {
    let fixture = HttpFixture::new("", false).await;
    let mut a = TestBridge::new();
    let mut b = TestBridge::new();
    let ca = a.install(vec![fixture.server("secret-A")]).await;
    let cb = b.install(vec![fixture.server("secret-B")]).await;
    assert_eq!(ca["tools"], cb["tools"]);
    a.invoke(&ca["tools"][0]["name"], "a");
    b.invoke(&cb["tools"][0]["name"], "b");
    assert_eq!(a.result("a").await["result"], raw_result());
    assert_eq!(b.result("b").await["result"], raw_result());
    a.host.terminate().await;
    fixture.wait("DELETE", 1).await;
    b.invoke(&cb["tools"][0]["name"], "b-again");
    assert_eq!(b.result("b-again").await["result"], raw_result());
    b.host.terminate().await;
    fixture.wait("DELETE", 2).await;
    let records = fixture.state.records.lock().unwrap();
    for (auth, session) in [("secret-A", "fixture-0"), ("secret-B", "fixture-1")] {
        let requests: Vec<_> = records.iter().filter(|r| r.auth == auth).collect();
        assert!(requests.iter().any(|r| r.method == "tools/call"));
        assert!(requests.iter().any(|r| r.method == "DELETE"));
        assert!(requests.iter().all(|r| if r.method == "initialize" {
            r.session.is_empty()
        } else {
            r.session == session
        }));
    }
    drop(records);
    assert_eq!(fixture.count("initialize"), 2);
}

#[tokio::test]
async fn closed_http_call_cancels_without_late_reply_or_reconnect() {
    let fixture = HttpFixture::new("tools/call", false).await;
    let mut bridge = TestBridge::new();
    let config = bridge.install(vec![fixture.server("secret")]).await;
    bridge.invoke(&config["tools"][0]["name"], "closed");
    fixture.wait("tools/call", 1).await;
    bridge.close("closed");
    fixture.wait("DELETE", 1).await;
    fixture.state.release.notify_waiters();
    no_frame(&mut bridge.peer).await;
    bridge.invoke(&config["tools"][0]["name"], "next");
    assert_eq!(
        bridge.result("next").await["error"],
        "external tool call failed"
    );
    assert_eq!(fixture.count("initialize"), 1);
    assert_eq!(fixture.count("tools/list"), 1);
    assert_eq!(fixture.count("tools/call"), 1);
    bridge.host.terminate().await;
}

#[tokio::test]
async fn owner_drop_and_failed_discovery_delete_sessions() {
    let fixture = HttpFixture::new("tools/call", false).await;
    let mut bridge = TestBridge::new();
    let config = bridge.install(vec![fixture.server("secret")]).await;
    bridge.invoke(&config["tools"][0]["name"], "drop");
    fixture.wait("tools/call", 1).await;
    let weak = Arc::downgrade(&bridge.host);
    drop(bridge.events);
    drop(bridge.host);
    assert!(weak.upgrade().is_none());
    fixture.wait("DELETE", 1).await;
    fixture.state.release.notify_waiters();
    no_frame(&mut bridge.peer).await;

    let invalid = HttpFixture::new("", true).await;
    let mut bridge = TestBridge::new();
    assert_eq!(
        bridge
            .host
            .configure_tools(
                vec![invalid.server("secret")],
                std::env::current_dir().unwrap(),
                "scope".into()
            )
            .await
            .unwrap_err(),
        "external tool discovery failed"
    );
    invalid.wait("DELETE", 1).await;
    no_frame(&mut bridge.peer).await;
    bridge.host.terminate().await;
}

#[tokio::test]
async fn dropped_discovery_cancels_blocked_http_and_deletes_session() {
    let fixture = HttpFixture::new("tools/list", false).await;
    let mut bridge = TestBridge::new();
    let host = bridge.host.clone();
    let server = fixture.server("secret");
    let task = tokio::spawn(async move {
        host.configure_tools(
            vec![server],
            std::env::current_dir().unwrap(),
            "scope".into(),
        )
        .await
    });
    fixture.wait("tools/list", 1).await;
    task.abort();
    let _ = task.await;
    fixture.wait("DELETE", 1).await;
    fixture.state.release.notify_waiters();
    no_frame(&mut bridge.peer).await;
    bridge.host.terminate().await;
}

// A shell child is used only in the mixed-catalog test, with no nested Cargo or
// external MCP service. Its temporary script inherits the runner's isolated TEMP.
struct StdioFixture {
    dir: PathBuf,
}
impl StdioFixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("http-bridge-stdio-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        Self { dir }
    }
    fn server(&self) -> ToolServer {
        #[cfg(windows)]
        let (command, args) = {
            let path = self.dir.join("mcp.ps1");
            std::fs::write(&path, r#"
while ($null -ne ($line = [Console]::ReadLine())) {
    $r = $line | ConvertFrom-Json
    if ($null -eq $r.id) { continue }
    switch ($r.method) {
        'initialize' { $v = @{protocolVersion='2025-06-18';capabilities=@{tools=@{}};serverInfo=@{name='mixed';version='1'}} }
        'tools/list' { $v = @{tools=@(@{name='echo';inputSchema=@{type='object'}})} }
        'tools/call' { $v = @{content=@(@{type='text';text='actual stdio'})} }
        default { $v = @{} }
    }
    [Console]::WriteLine((@{jsonrpc='2.0';id=$r.id;result=$v} | ConvertTo-Json -Depth 20 -Compress))
}
"#).unwrap();
            (
                "powershell.exe".into(),
                vec![
                    "-NoLogo".into(),
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-ExecutionPolicy".into(),
                    "Bypass".into(),
                    "-File".into(),
                    path.to_str().unwrap().into(),
                ],
            )
        };
        #[cfg(unix)]
        let (command, args) = {
            let path = self.dir.join("mcp.sh");
            std::fs::write(&path, r#"
while IFS= read -r line; do
    id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([^,}]*\).*/\1/p')
    [ -n "$id" ] || continue
    method=$(printf '%s\n' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
    case "$method" in
        initialize) result='{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"mixed","version":"1"}}' ;;
        tools/list) result='{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}' ;;
        tools/call) result='{"content":[{"type":"text","text":"actual stdio"}]}' ;;
        *) result='{}' ;;
    esac
    printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$result"
done
"#).unwrap();
            ("sh".into(), vec![path.to_str().unwrap().into()])
        };
        ToolServer::Stdio(StdioToolServer {
            name: "stdio-fixture".into(),
            command,
            args,
            env: BTreeMap::new(),
        })
    }
}
impl Drop for StdioFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn http_call_budget_retires_only_target_and_never_reconnects() {
    let fixture = HttpFixture::new("tools/call", false).await;
    let mut bridge = TestBridge::new();
    let config = bridge.install(vec![fixture.server("secret")]).await;
    bridge.invoke(&config["tools"][0]["name"], "budget");
    fixture.wait("tools/call", 1).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(46)).await;
    tokio::time::resume();
    tokio::task::yield_now().await;
    let response = bridge.result("budget").await;
    assert_eq!(response["error"], "external tool call timed out");
    fixture.wait("DELETE", 1).await;
    fixture.state.release.notify_waiters();
    bridge.invoke(&config["tools"][0]["name"], "after-budget");
    assert_eq!(
        bridge.result("after-budget").await["error"],
        "external tool call failed"
    );
    assert_eq!(fixture.count("initialize"), 1);
    assert_eq!(fixture.count("tools/call"), 1);
    bridge.host.terminate().await;
}

#[tokio::test]
async fn mixed_discovery_failure_never_sends_partial_native_catalog_and_is_retryable() {
    let invalid = HttpFixture::new("", true).await;
    let stdio = StdioFixture::new();
    let mut bridge = TestBridge::new();
    assert_eq!(
        bridge
            .host
            .configure_tools(
                vec![stdio.server(), invalid.server("secret")],
                std::env::current_dir().unwrap(),
                "same-scope".into(),
            )
            .await
            .unwrap_err(),
        "external tool discovery failed"
    );
    invalid.wait("DELETE", 1).await;
    no_frame(&mut bridge.peer).await;
    let valid = HttpFixture::new("", false).await;
    let config = bridge
        .install(vec![stdio.server(), valid.server("secret")])
        .await;
    assert_eq!(config["tools"].as_array().unwrap().len(), 2);
    bridge.host.terminate().await;
    valid.wait("DELETE", 1).await;
}

#[tokio::test]
async fn native_registration_rejection_cleans_http_session_without_echoing_secret() {
    let fixture = HttpFixture::new("", false).await;
    let mut bridge = TestBridge::new();
    let configure = bridge.host.configure_tools(
        vec![fixture.server("secret")],
        std::env::current_dir().unwrap(),
        "same-scope".into(),
    );
    let reject = async {
        let config = frame(&mut bridge.peer).await;
        bridge.events.send(json!({"type":"response","id":config["id"],"success":false,"error":"secret validation details"})).ok();
    };
    let (result, _) = tokio::join!(configure, reject);
    assert_eq!(result.unwrap_err(), "native tool configuration rejected");
    fixture.wait("DELETE", 1).await;
    let config = bridge.install(vec![fixture.server("secret")]).await;
    bridge.invoke(&config["tools"][0]["name"], "retry");
    assert_eq!(bridge.result("retry").await["result"], raw_result());
    bridge.host.terminate().await;
    fixture.wait("DELETE", 2).await;
}

#[tokio::test]
async fn mixed_transports_install_one_atomic_native_catalog_and_route_both() {
    let http = HttpFixture::new("", false).await;
    let stdio = StdioFixture::new();
    let mut bridge = TestBridge::new();
    let config = bridge
        .install(vec![stdio.server(), http.server("secret")])
        .await;
    assert_eq!(config["tools"].as_array().unwrap().len(), 2);
    assert_ne!(config["tools"][0]["name"], config["tools"][1]["name"]);
    bridge.invoke(&config["tools"][0]["name"], "stdio");
    assert_eq!(
        bridge.result("stdio").await["result"]["content"][0]["text"],
        "actual stdio"
    );
    bridge.invoke(&config["tools"][1]["name"], "http");
    assert_eq!(bridge.result("http").await["result"], raw_result());
    no_frame(&mut bridge.peer).await;
    bridge.host.terminate().await;
    http.wait("DELETE", 1).await;
}
