//! Opt-in Windows real-native tests. No deterministic RPC peer or installed config.
//! Build: cargo test --locked -p rpi-acp --test remote_native --no-run
//! Run with RPI_ACP_TEST_NATIVE=<native.exe>, optionally RPI_ACP_TEST_BRIDGE=<frozen bridge>,
//! and --ignored --test-threads=1. All artifacts remain under the C: cargo target.
#![cfg(windows)]
use axum::{
    extract::State,
    response::sse::{Event, Sse},
    routing::post,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    process::{Child, Command},
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
};

const TARGET: &str = "C:/Users/bigfish/.cargo/target";
const TOKEN: &str = "real-native-remote-test-0123456789abcdef";
const COMPLETE: &str = "complete 用户🌊";
const WAIT: &str = "cancel 等待🌊";
const EOF: &str = "disconnect 等待🌊";
const TEXT: &str = "native🌊你好";
const TAIL: &str = " — done✓";
const DEADLINE: Duration = Duration::from_secs(20);
type Captures = Arc<Mutex<Vec<Value>>>;
type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn provider(
    State(captures): State<Captures>,
    Json(body): Json<Value>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
    assert_eq!(body["model"], "fixture");
    assert_eq!(body["stream"], true);
    let messages = body["messages"].as_array().expect("OpenAI messages");
    assert!(messages
        .iter()
        .any(|m| m["role"] == "system" || m["role"] == "developer"));
    let user = messages
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .expect("user")
        .to_string();
    let delayed = user.contains("等待");
    captures.lock().unwrap().push(body);
    let stream = futures_util::stream::unfold(0, move |step| async move {
        let data = match step {
            0 => {
                // Native coalesces tiny deltas. Age its flush interval before
                // the first frame so cancellation observes text, not buffering.
                tokio::time::sleep(Duration::from_millis(300)).await;
                json!({"id":"fixture-response","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":TEXT},"finish_reason":null}]}).to_string()
            },
            1 => {
                tokio::time::sleep(if delayed {Duration::from_secs(60)} else {Duration::from_millis(150)}).await;
                json!({"choices":[{"index":0,"delta":{"content":TAIL},"finish_reason":null}]}).to_string()
            }
            2 => json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}).to_string(),
            3 => "[DONE]".into(),
            _ => return None,
        };
        Some((Ok(Event::default().data(data)), step + 1))
    });
    Sse::new(stream)
}

struct Fixture {
    root: PathBuf,
    workspace: PathBuf,
    address: SocketAddr,
    bridge: Child,
    provider: tokio::task::JoinHandle<()>,
    captures: Captures,
}
impl Fixture {
    async fn start(http: bool) -> Self {
        let native = PathBuf::from(
            std::env::var_os("RPI_ACP_TEST_NATIVE")
                .expect("explicit RPI_ACP_TEST_NATIVE required; run --ignored"),
        );
        assert!(native.is_absolute() && native.is_file());
        let root =
            PathBuf::from(TARGET).join(format!("acp-remote-native-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        let agent = root.join("agent");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(agent.join("extensions")).unwrap();
        let captures: Captures = Default::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider_address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/chat/completions", post(provider))
            .with_state(captures.clone());
        let provider = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        std::fs::write(agent.join("models.json"), json!({"providers":{"remote-local":{
            "api":"openai-completions","baseUrl":format!("http://{provider_address}/v1"),"apiKey":"local-only",
            "models":[{"id":"fixture","name":"fixture","reasoning":false,"input":["text"],"contextWindow":1000000,"maxTokens":1024}]
        }}}).to_string()).unwrap();
        std::fs::write(agent.join("settings.json"), json!({"defaultProvider":"remote-local","defaultModel":"fixture","defaultThinkingLevel":"off"}).to_string()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let executable = std::env::var_os("RPI_ACP_TEST_BRIDGE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_rpi-acp-bridge")));
        let log = std::fs::File::create(root.join("bridge.log")).unwrap();
        let bridge = Command::new(executable)
            .args([
                if http {
                    "--experimental-http"
                } else {
                    "--experimental-remote"
                },
                "--bind",
                &address.to_string(),
            ])
            .env("RPI_ACP_REMOTE_TOKEN", TOKEN)
            .env("RPI_BIN", native)
            .env("RPI_ACP_SESSION_DIR", root.join("history"))
            .env("RPI_CODING_AGENT_DIR", &agent)
            // Explicit isolated discovery also exercises pre-result command updates.
            .env("RPI_EXTENSIONS_DIR", agent.join("extensions"))
            .env("RPI_OFFLINE", "1")
            .env("HOME", &root)
            .env("USERPROFILE", &root)
            .env("TEMP", format!("{TARGET}/acp-tmp"))
            .env("TMP", format!("{TARGET}/acp-tmp"))
            .env("RPI_ACP_NO_AUTO_REGISTER_ZED", "1")
            .current_dir(&workspace)
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        tokio::time::timeout(DEADLINE, async {
            loop {
                if tokio::net::TcpStream::connect(address).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("bridge startup");
        eprintln!("real-native artifacts: {}", root.display());
        Self {
            root,
            workspace,
            address,
            bridge,
            provider,
            captures,
        }
    }
    fn setup(&self, session: Option<&str>) -> Value {
        let mut p = json!({"cwd":self.workspace,"mcpServers":[]});
        if let Some(s) = session {
            p["sessionId"] = json!(s);
        }
        p
    }
    async fn native(&self) -> Process {
        // Discover the *actual* direct native child, then hold a synchronization
        // handle across disconnect. Never infer exit from a task or PID log.
        let parent = self.bridge.id().unwrap();
        tokio::time::timeout(DEADLINE, async {loop {
            let output = Command::new("powershell.exe").args(["-NoProfile", "-Command", &format!("Get-CimInstance Win32_Process -Filter 'ParentProcessId = {parent}' | Select-Object -ExpandProperty ProcessId")]).output().await.unwrap();
            let ids: Vec<u32> = String::from_utf8_lossy(&output.stdout).lines().filter_map(|s| s.trim().parse().ok()).collect();
            assert!(ids.len() <= 1, "unexpected multiple direct children: {ids:?}");
            if let Some(pid) = ids.first() { return Process::open(*pid); }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }}).await.expect("real native child must exist")
    }
    fn transcript(&self, session: &str) -> Vec<Value> {
        assert_eq!(uuid::Uuid::parse_str(session).unwrap().to_string(), session);
        let file = rpi_extcore::history::find_saved_session(
            &self.root.join("history").join(session),
            session,
        )
        .unwrap();
        let rows: Vec<Value> = std::fs::read_to_string(file)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows[0]["id"], session, "native canonical ID");
        rows
    }
    fn check_captures(&self) {
        let calls = self.captures.lock().unwrap();
        assert_eq!(
            calls.len(),
            3,
            "one actual model call per prompt; no retries or replay calls"
        );
        for (call, marker) in calls.iter().zip([COMPLETE, WAIT, EOF]) {
            let user = call["messages"]
                .as_array()
                .unwrap()
                .iter()
                .rev()
                .find(|m| m["role"] == "user")
                .unwrap();
            assert!(user.to_string().contains(marker), "{user}");
        }
        std::fs::write(
            self.root.join("provider-requests.json"),
            serde_json::to_vec_pretty(&*calls).unwrap(),
        )
        .unwrap();
    }
    async fn stop(&mut self) {
        self.bridge.kill().await.unwrap();
        self.bridge.wait().await.unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.provider.abort();
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn WaitForSingleObject(handle: *mut std::ffi::c_void, millis: u32) -> u32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}
struct Process(*mut std::ffi::c_void);
impl Process {
    fn open(pid: u32) -> Self {
        let handle = unsafe { OpenProcess(0x00100000, 0, pid) };
        assert!(!handle.is_null(), "OpenProcess({pid}) failed");
        Self(handle)
    }
    fn alive(&self) {
        assert_eq!(
            unsafe { WaitForSingleObject(self.0, 0) },
            258,
            "child already exited"
        );
    }
    async fn exited(&self) {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                match unsafe { WaitForSingleObject(self.0, 0) } {
                    0 => break,
                    258 => {}
                    n => panic!("wait failed: {n}"),
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("native leaked after socket EOF");
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn rpc(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn prompt(id: u64, s: &str, text: &str) -> Value {
    rpc(
        id,
        "session/prompt",
        json!({"sessionId":s,"prompt":[{"type":"text","text":text}]}),
    )
}
fn cancel(s: &str) -> Value {
    json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":s}})
}
fn init() -> Value {
    rpc(
        1,
        "initialize",
        json!({"protocolVersion":1,"clientCapabilities":{}}),
    )
}
fn text(v: &Value) -> Option<(String, String)> {
    let u = &v["params"]["update"];
    let kind = u["sessionUpdate"].as_str()?;
    if !matches!(kind, "user_message_chunk" | "agent_message_chunk") {
        return None;
    }
    Some((kind.into(), u["content"]["text"].as_str().unwrap().into()))
}
fn active_messages(rows: &[Value]) -> Vec<&Value> {
    // Operation intents and assistant_frame records are not branch entries.
    let entries: std::collections::HashMap<_, _> = rows.iter()
        .filter(|r| r["kind"] == "entry")
        .map(|r| (r["id"].as_str().expect("native entry ID"), r)).collect();
    let mut leaf = rows.iter().rev().find(|r| r["kind"] == "entry" && r["lane"] == "main");
    let mut branch = Vec::new();
    let mut visited = std::collections::HashSet::new();
    while let Some(entry) = leaf {
        assert!(visited.insert(entry["id"].as_str().unwrap()), "native branch cycle");
        if entry["type"] == "message" {
            branch.push(entry);
        }
        leaf = entry["parentId"].as_str().map(|id| *entries.get(id).expect("native branch parent"));
    }
    branch.reverse();
    branch
}
fn persisted_replay(rows: &[Value]) -> Vec<Value> {
    active_messages(rows).into_iter().flat_map(|r| {
        let m = &r["message"];
        let kind = match m["role"].as_str() {
            Some("user") => "user_message_chunk",
            Some("assistant") => "agent_message_chunk",
            _ => panic!("unexpected native message: {m}"),
        };
        let blocks = if let Some(s) = m["content"].as_str() {
            vec![json!({"type":"text","text":s})]
        } else {
            m["content"].as_array().expect("native message content").clone()
        };
        blocks.into_iter().map(move |block| {
            assert_eq!(block["type"], "text", "unexpected fixture content");
            assert!(block["text"].is_string());
            let mut update = json!({"sessionUpdate":kind,"content":block});
            let id = if m["role"] == "assistant" {
                m["messageId"].as_str().filter(|id| !id.is_empty()).map(str::to_owned)
            } else {
                m["id"].as_str().map(str::to_owned)
                    .or_else(|| r["id"].as_str().map(|id| format!("history:{id}")))
            };
            if let Some(id) = id {
                update["messageId"] = json!(id);
            }
            update
        }).collect::<Vec<_>>()
    }).collect()
}
fn assert_replay(replay: &[Value], rows: &[Value]) {
    let expected = persisted_replay(rows);
    assert_eq!(replay.len(), expected.len(), "all replay data, including aborted partials");
    let actual: Vec<_> = replay.iter().zip(&expected).map(|(actual, expected)| {
        let mut actual = actual.clone();
        if expected.get("messageId").is_none() {
            actual.as_object_mut().unwrap().remove("messageId");
        }
        actual
    }).collect();
    assert_eq!(actual, expected, "exact native replay content and canonical IDs");
}
fn validate_persisted(rows: &[Value]) {
    let entries = active_messages(rows);
    let messages: Vec<_> = entries.iter().map(|r| &r["message"]).collect();
    let replay = persisted_replay(rows);
    let content: Vec<_> = replay.iter().map(|u| (
        u["sessionUpdate"].as_str().unwrap(),
        u["content"]["text"].as_str().unwrap().to_owned(),
    )).collect();
    assert_eq!(content, vec![
        ("user_message_chunk", COMPLETE.into()),
        ("agent_message_chunk", format!("{TEXT}{TAIL}")),
        ("user_message_chunk", WAIT.into()),
        ("agent_message_chunk", TEXT.into()),
        ("user_message_chunk", EOF.into()),
        ("agent_message_chunk", TEXT.into()),
    ], "native must retain both interrupted exchanges without a delayed tail");
    for message in [&messages[3], &messages[5]] {
        assert_eq!(message["stopReason"], "aborted", "persisted interrupted assistant");
    }
    assert!(messages
        .iter()
        .any(|m| m["role"] == "assistant" && m.to_string().contains(TAIL)));
    // Aborted requests may have native bookkeeping, but never a fabricated
    // completed assistant with the delayed second frame.
    assert_eq!(
        messages
            .iter()
            .filter(|m| m["role"] == "assistant" && m.to_string().contains(TAIL))
            .count(),
        1
    );
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .http2_prior_knowledge()
        .timeout(DEADLINE)
        .build()
        .unwrap()
}
fn post_rpc(
    c: &reqwest::Client,
    f: &Fixture,
    id: Option<&str>,
    s: Option<&str>,
    v: Value,
) -> reqwest::RequestBuilder {
    let mut r = c
        .post(format!("http://{}/acp", f.address))
        .bearer_auth(TOKEN)
        .json(&v);
    if let Some(id) = id {
        r = r.header("acp-connection-id", id);
    }
    if let Some(s) = s {
        r = r.header("acp-session-id", s);
    }
    r
}
async fn http_init(c: &reqwest::Client, f: &Fixture) -> String {
    let r = post_rpc(c, f, None, None, init()).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.version(), reqwest::Version::HTTP_2);
    let id = r.headers()["acp-connection-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["result"]["protocolVersion"], 1);
    assert_eq!(v["result"]["connectionId"], id);
    id
}
async fn accepted(c: &reqwest::Client, f: &Fixture, id: &str, s: Option<&str>, v: Value) {
    assert_eq!(
        post_rpc(c, f, Some(id), s, v)
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
}
struct Events {
    response: reqwest::Response,
    bytes: Vec<u8>,
}
async fn events(c: &reqwest::Client, f: &Fixture, id: &str, s: Option<&str>) -> Events {
    let mut r = c
        .get(format!("http://{}/acp", f.address))
        .bearer_auth(TOKEN)
        .header("accept", "text/event-stream")
        .header("acp-connection-id", id);
    if let Some(s) = s {
        r = r.header("acp-session-id", s);
    }
    let response = r.send().await.unwrap();
    assert_eq!(response.status(), 200);
    Events {
        response,
        bytes: Vec::new(),
    }
}
impl Events {
    async fn next(&mut self) -> Value {
        tokio::time::timeout(DEADLINE, async {
            loop {
                if let Some(end) = self.bytes.windows(2).position(|w| w == b"\n\n") {
                    let frame = String::from_utf8(self.bytes[..end].to_vec()).unwrap();
                    self.bytes.drain(..end + 2);
                    if let Some(data) = frame.lines().find_map(|l| l.strip_prefix("data: ")) {
                        return serde_json::from_str(data).unwrap();
                    }
                } else {
                    self.bytes
                        .extend_from_slice(&self.response.chunk().await.unwrap().expect("SSE EOF"));
                }
            }
        })
        .await
        .expect("SSE timeout")
    }
    async fn reply(&mut self, id: u64) -> Value {
        loop {
            let v = self.next().await;
            if v["id"] == id {
                return v;
            }
        }
    }
}

#[tokio::test]
#[ignore = "requires explicit RPI_ACP_TEST_NATIVE; real native Windows lifecycle"]
async fn http2_real_native_stream_cancel_eof_and_reconnect() {
    let mut f = Fixture::start(true).await;
    let c = client();
    let id = http_init(&c, &f).await;
    let mut conn = events(&c, &f, &id, None).await;
    accepted(&c, &f, &id, None, rpc(2, "session/new", f.setup(None))).await;
    let v = conn.reply(2).await;
    let s = v["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{v}"))
        .to_owned();
    let process = f.native().await;
    process.alive();
    let mut stream = events(&c, &f, &id, Some(&s)).await;
    for (rid, marker, reason) in [(3, COMPLETE, "end_turn"), (4, WAIT, "cancelled")] {
        accepted(&c, &f, &id, Some(&s), prompt(rid, &s, marker)).await;
        let mut output = String::new();
        let mut cancelled = false;
        loop {
            let v = stream.next().await;
            if let Some((kind, part)) = text(&v) {
                if kind == "agent_message_chunk" {
                    output.push_str(&part);
                    if rid == 4 && !cancelled {
                        accepted(&c, &f, &id, Some(&s), cancel(&s)).await;
                        cancelled = true;
                    }
                }
            }
            if v["id"] == rid {
                assert_eq!(v["result"]["stopReason"], reason, "{v}");
                break;
            }
        }
        assert_eq!(
            output,
            if rid == 3 {
                format!("{TEXT}{TAIL}")
            } else {
                TEXT.into()
            }
        );
        process.alive();
    }
    accepted(&c, &f, &id, Some(&s), prompt(5, &s, EOF)).await;
    loop {
        if text(&stream.next().await).is_some_and(|(k, _)| k == "agent_message_chunk") {
            break;
        }
    }
    drop(stream);
    drop(conn);
    drop(c);
    process.exited().await;
    let rows = f.transcript(&s);
    validate_persisted(&rows);
    let c = client();
    let id2 = http_init(&c, &f).await;
    assert_ne!(id, id2);
    assert_eq!(
        post_rpc(&c, &f, Some(&id), None, rpc(30, "unknown", json!({})))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let mut conn = events(&c, &f, &id2, None).await;
    let mut stream = events(&c, &f, &id2, Some(&s)).await; // reserve before load
    assert_eq!(
        post_rpc(&c, &f, Some(&id2), Some(&s), prompt(31, &s, "not attached"))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        f.captures.lock().unwrap().len(),
        3,
        "GET must not call provider"
    );
    let other = client();
    let other_id = http_init(&other, &f).await;
    assert_ne!(other_id, id2);
    assert_eq!(
        post_rpc(&other, &f, Some(&id2), Some(&s), prompt(32, &s, "stolen"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        post_rpc(
            &other,
            &f,
            Some(&other_id),
            Some(&s),
            prompt(33, &s, "unattached")
        )
        .send()
        .await
        .unwrap()
        .status(),
        400
    );
    accepted(
        &c,
        &f,
        &id2,
        Some(&s),
        rpc(6, "session/load", f.setup(Some(&s))),
    )
    .await;
    let v = conn.reply(6).await;
    assert!(v.get("result").is_some(), "{v}");
    let loaded = f.native().await;
    // A later response on the same session queue is a replay barrier, avoiding
    // sleeps/negative-timeout assertions for extra invented assistant frames.
    accepted(
        &c,
        &f,
        &id2,
        Some(&s),
        rpc(
            7,
            "session/set_mode",
            json!({"sessionId":s,"modeId":"nonexistent"}),
        ),
    )
    .await;
    let mut replay = Vec::new();
    loop {
        let v = stream.next().await;
        if text(&v).is_some() {
            replay.push(v["params"]["update"].clone());
        }
        if v["id"] == 7 {
            break;
        }
    }
    assert_replay(&replay, &rows);
    drop(stream);
    drop(conn);
    drop(c);
    drop(other);
    loaded.exited().await;
    // No GET ever exists on this connection. Observe actual child creation and
    // require network EOF to terminate it, rather than an SSE drop/deadline.
    let c = client();
    let id = http_init(&c, &f).await;
    accepted(&c, &f, &id, None, rpc(2, "session/new", f.setup(None))).await;
    let no_get = f.native().await;
    no_get.alive();
    drop(c);
    no_get.exited().await;
    f.check_captures();
    f.stop().await;
}

async fn ws(f: &Fixture) -> (Socket, String) {
    let mut request = format!("ws://{}/acp", f.address)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
    let (mut socket, r) = connect_async(request).await.unwrap();
    let id = r.headers()["acp-connection-id"]
        .to_str()
        .unwrap()
        .to_owned();
    send(&mut socket, init()).await;
    let v = receive(&mut socket).await;
    assert_eq!(v["result"]["protocolVersion"], 1, "{v}");
    (socket, id)
}
async fn send(s: &mut Socket, v: Value) {
    s.send(Message::Text(v.to_string().into())).await.unwrap();
}
async fn receive(s: &mut Socket) -> Value {
    loop {
        match tokio::time::timeout(DEADLINE, s.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            Message::Text(t) => return serde_json::from_str(&t).unwrap(),
            Message::Ping(_) | Message::Pong(_) => {}
            v => panic!("unexpected {v:?}"),
        }
    }
}
async fn reply(s: &mut Socket, id: u64) -> Value {
    loop {
        let v = receive(s).await;
        if v["id"] == id {
            return v;
        }
    }
}

#[tokio::test]
#[ignore = "requires explicit RPI_ACP_TEST_NATIVE; real native Windows lifecycle"]
async fn websocket_real_native_stream_cancel_eof_and_reconnect() {
    let mut f = Fixture::start(false).await;
    let (mut socket, id) = ws(&f).await;
    send(&mut socket, rpc(2, "session/new", f.setup(None))).await;
    let v = reply(&mut socket, 2).await;
    let s = v["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{v}"))
        .to_owned();
    let process = f.native().await;
    for (rid, marker, reason) in [(3, COMPLETE, "end_turn"), (4, WAIT, "cancelled")] {
        send(&mut socket, prompt(rid, &s, marker)).await;
        let mut output = String::new();
        let mut cancelled = false;
        loop {
            let v = receive(&mut socket).await;
            if let Some((kind, part)) = text(&v) {
                if kind == "agent_message_chunk" {
                    output.push_str(&part);
                    if rid == 4 && !cancelled {
                        send(&mut socket, cancel(&s)).await;
                        cancelled = true;
                    }
                }
            }
            if v["id"] == rid {
                assert_eq!(v["result"]["stopReason"], reason, "{v}");
                break;
            }
        }
        assert_eq!(
            output,
            if rid == 3 {
                format!("{TEXT}{TAIL}")
            } else {
                TEXT.into()
            }
        );
        process.alive();
    }
    send(&mut socket, prompt(5, &s, EOF)).await;
    loop {
        if text(&receive(&mut socket).await).is_some_and(|(k, _)| k == "agent_message_chunk") {
            break;
        }
    }
    drop(socket);
    process.exited().await;
    let rows = f.transcript(&s);
    validate_persisted(&rows);
    let (mut socket, id2) = ws(&f).await;
    assert_ne!(id, id2);
    // A new connection cannot operate on a persisted ID without loading it.
    send(&mut socket, prompt(2, &s, "unattached")).await;
    assert!(reply(&mut socket, 2).await.get("error").is_some());
    send(&mut socket, rpc(6, "session/load", f.setup(Some(&s)))).await;
    let mut replay = Vec::new();
    loop {
        let v = receive(&mut socket).await;
        if text(&v).is_some() {
            replay.push(v["params"]["update"].clone());
        }
        if v["id"] == 6 {
            assert!(v.get("result").is_some(), "{v}");
            break;
        }
    }
    assert_replay(&replay, &rows);
    let loaded = f.native().await;
    drop(socket);
    loaded.exited().await;
    f.check_captures();
    f.stop().await;
}
