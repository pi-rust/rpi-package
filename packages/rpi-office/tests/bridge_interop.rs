//! Interop tests: a real WebSocket client against the real bridge server.
//!
//! These drive the same wire path the pi-for-office task pane uses (hello →
//! welcome → tool_call → tool_result, pane prompts, focus routing, `/health`),
//! so a protocol regression fails here instead of in Excel.

use futures_util::SinkExt;
use rpi_office::bridge::{HealthMeta, Pane};
use rpi_office::catalog::Host;
use rpi_office::protocol::{self, BRIDGE_PROTOCOL_VERSION, OFFICE_TARGET_PARAM};
use rpi_office::server::{self, Server, ToolOutput};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const RECV_TIMEOUT: Duration = Duration::from_secs(5);

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Records the host-facing callbacks the hub fires.
#[derive(Default)]
struct Recording {
    user_messages: Mutex<Vec<(String, Pane)>>,
    aborts: Mutex<Vec<(Option<String>, Pane)>>,
    pane_changes: AtomicU64,
}

impl server::Hooks for Recording {
    fn on_user_message(&self, text: &str, pane: &Pane) {
        self.user_messages
            .lock()
            .unwrap()
            .push((text.to_string(), pane.clone()));
    }

    fn on_abort_request(&self, prompt_id: Option<&str>, pane: &Pane) {
        self.aborts
            .lock()
            .unwrap()
            .push((prompt_id.map(str::to_owned), pane.clone()));
    }

    fn on_panes_changed(&self, _panes: &[Pane]) {
        self.pane_changes.fetch_add(1, Ordering::SeqCst);
    }
}

struct Harness {
    port: u16,
    server: Arc<Server>,
    recording: Arc<Recording>,
    _hooks: Arc<dyn server::Hooks>,
}

impl Harness {
    fn start() -> Self {
        let listener = server::bind(0).expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let recording = Arc::new(Recording::default());
        let hooks: Arc<dyn server::Hooks> = recording.clone();
        let meta = HealthMeta {
            service: "pi-office-bridge".into(),
            server_version: "0.1.0-test".into(),
            pi_version: Some("test".into()),
            port,
            started_at: protocol::now_ms(),
        };
        let server = Arc::new(
            Server::start(listener, port, meta, Arc::clone(&hooks)).expect("start server"),
        );
        Self {
            port,
            server,
            recording,
            _hooks: hooks,
        }
    }

    fn call(&self, host: Host, op: &str, args: Value) -> Receiver<Result<ToolOutput, String>> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.server
            .call(
                protocol::next_call_id("tool"),
                host,
                op.to_string(),
                args,
                tx,
            )
            .expect("send call");
        rx
    }

    async fn connect(&self) -> Ws {
        let (ws, _response) = connect_async(format!("ws://127.0.0.1:{}", self.port))
            .await
            .expect("websocket connect");
        ws
    }

    fn health(&self) -> String {
        http_get(self.port, "/health", None)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.server.stop();
    }
}

async fn send(ws: &mut Ws, value: Value) {
    ws.send(Message::text(value.to_string()))
        .await
        .expect("send frame");
}

async fn recv(ws: &mut Ws) -> Value {
    use futures_util::StreamExt;
    let next = tokio::time::timeout(RECV_TIMEOUT, ws.next())
        .await
        .expect("timed out waiting for a server frame")
        .expect("stream ended")
        .expect("websocket error");
    match next {
        Message::Text(text) => serde_json::from_str(&text).expect("server frame is JSON"),
        other => panic!("unexpected frame: {other:?}"),
    }
}

fn hello(
    host: &str,
    pane_id: &str,
    name: Option<&str>,
    id: Option<&str>,
    ops: Option<Vec<String>>,
) -> Value {
    json!({
        "type": "hello",
        "protocolVersion": BRIDGE_PROTOCOL_VERSION,
        "host": host,
        "clientName": "pi-for-office-test",
        "paneId": pane_id,
        "document": {
            "id": id,
            "name": name,
            "source": if id.is_some() { "document.url" } else { "unknown" },
        },
        "ops": ops,
        "catalogVersion": 3,
    })
}

fn excel_ops() -> Vec<String> {
    vec!["excel.read_range".into(), "excel.get_overview".into()]
}

fn http_get(port: u16, path: &str, origin: Option<&str>) -> String {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let origin = origin
        .map(|origin| format!("Origin: {origin}\r\n"))
        .unwrap_or_default();
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{origin}\r\n");
    stream.write_all(request.as_bytes()).expect("write");
    let mut raw = String::new();
    let _ = stream.read_to_string(&mut raw);
    raw
}

fn body(raw: &str) -> Value {
    let (_, payload) = raw.split_once("\r\n\r\n").expect("http response body");
    serde_json::from_str(payload).expect("json body")
}

#[tokio::test]
async fn hello_then_tool_proxy_round_trip() {
    let harness = Harness::start();
    let mut ws = harness.connect().await;
    send(
        &mut ws,
        hello(
            "excel",
            "pane-1",
            Some("Book1.xlsx"),
            Some("url_sha256:abc"),
            Some(excel_ops()),
        ),
    )
    .await;

    let welcome = recv(&mut ws).await;
    assert_eq!(welcome["type"], json!("welcome"));
    assert_eq!(welcome["protocolVersion"], json!(BRIDGE_PROTOCOL_VERSION));
    assert_eq!(welcome["serverName"], json!("pi-office-bridge"));
    assert_eq!(welcome["serverVersion"], json!("0.1.0-test"));
    assert_eq!(welcome["piVersion"], json!("test"));
    let capabilities = welcome["capabilities"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(capabilities.iter().any(|value| value == "http-health"));
    assert!(capabilities.iter().any(|value| value == "agent-deltas"));
    assert!(
        !capabilities.iter().any(|value| value == "client-abort"),
        "this ABI has no run-abort action, so the capability must not be advertised"
    );

    let reply = harness.call(
        Host::Excel,
        "read_range",
        json!({"range": "A1:B2", OFFICE_TARGET_PARAM: "url_sha256:abc"}),
    );
    let call = recv(&mut ws).await;
    assert_eq!(call["type"], json!("tool_call"));
    assert_eq!(call["tool"], json!("excel.read_range"));
    assert_eq!(call["documentId"], json!("url_sha256:abc"));
    assert!(
        call["args"].get(OFFICE_TARGET_PARAM).is_none(),
        "the document selector must never reach the pane"
    );
    assert_eq!(call["args"]["range"], json!("A1:B2"));

    let call_id = call["id"].as_str().expect("call id").to_string();
    send(
        &mut ws,
        json!({"type": "tool_result", "id": call_id, "ok": true, "text": "A1=1", "details": {"cells": 4}}),
    )
    .await;

    let output = reply
        .recv_timeout(RECV_TIMEOUT)
        .expect("tool result")
        .expect("ok");
    assert_eq!(output.text, "A1=1");
    assert_eq!(output.details, Some(json!({"cells": 4})));
}

#[tokio::test]
async fn a_failed_tool_result_surfaces_the_pane_error() {
    let harness = Harness::start();
    let mut ws = harness.connect().await;
    send(
        &mut ws,
        hello(
            "word",
            "pane-1",
            Some("Doc.docx"),
            None,
            Some(vec!["word.insert_text".into()]),
        ),
    )
    .await;
    let _ = recv(&mut ws).await;

    let reply = harness.call(Host::Word, "insert_text", json!({"text": "hi"}));
    let call = recv(&mut ws).await;
    assert_eq!(call["tool"], json!("word.insert_text"));
    let call_id = call["id"].as_str().unwrap().to_string();
    send(
        &mut ws,
        json!({"type": "tool_result", "id": call_id, "ok": false, "text": "", "error": "WordApi 1.3 required"}),
    )
    .await;

    let error = reply
        .recv_timeout(RECV_TIMEOUT)
        .expect("tool result")
        .expect_err("error");
    assert!(error.contains("WordApi 1.3 required"));
}

#[tokio::test]
async fn capability_gates_reject_ops_the_pane_cannot_run() {
    let harness = Harness::start();
    let mut ws = harness.connect().await;
    // Advertise only get_overview.
    send(
        &mut ws,
        hello(
            "excel",
            "pane-1",
            Some("Book1.xlsx"),
            None,
            Some(vec!["excel.get_overview".into()]),
        ),
    )
    .await;
    let _ = recv(&mut ws).await;

    let error = harness
        .call(Host::Excel, "read_range", json!({}))
        .recv_timeout(RECV_TIMEOUT)
        .expect("result")
        .expect_err("not advertised");
    assert!(error.contains("does not advertise"));
    assert!(error.contains("excel.read_range"));

    // A legacy pane (no ops list) may run only the v1 op set.
    let mut legacy = harness.connect().await;
    send(
        &mut legacy,
        hello("excel", "pane-2", Some("Old.xlsx"), None, None),
    )
    .await;
    let _ = recv(&mut legacy).await;
    // Focus it so the two open Excel documents resolve unambiguously.
    send(&mut legacy, json!({"type": "pane_state", "focused": true})).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let error = harness
        .call(Host::Excel, "get_capabilities", json!({}))
        .recv_timeout(RECV_TIMEOUT)
        .expect("result")
        .expect_err("legacy gate");
    assert!(error.contains("requires a newer add-in"));

    // ... and the v1 op is allowed and reaches the legacy pane.
    let reply = harness.call(Host::Excel, "read_range", json!({"range": "A1"}));
    let call = recv(&mut legacy).await;
    assert_eq!(call["tool"], json!("excel.read_range"));
    let call_id = call["id"].as_str().unwrap().to_string();
    send(
        &mut legacy,
        json!({"type": "tool_result", "id": call_id, "ok": true, "text": "ok"}),
    )
    .await;
    assert_eq!(
        reply
            .recv_timeout(RECV_TIMEOUT)
            .expect("result")
            .unwrap()
            .text,
        "ok"
    );
}

#[tokio::test]
async fn documents_route_by_focus_and_ambiguity_fails_loudly() {
    let harness = Harness::start();
    let mut first = harness.connect().await;
    send(
        &mut first,
        hello(
            "excel",
            "pane-1",
            Some("Budget"),
            Some("id-1"),
            Some(excel_ops()),
        ),
    )
    .await;
    let _ = recv(&mut first).await;
    let mut second = harness.connect().await;
    send(
        &mut second,
        hello(
            "excel",
            "pane-2",
            Some("Forecast"),
            Some("id-2"),
            Some(excel_ops()),
        ),
    )
    .await;
    let _ = recv(&mut second).await;

    // Several open, none focused → actionable error naming both documents.
    let error = harness
        .call(Host::Excel, "read_range", json!({"range": "A1"}))
        .recv_timeout(RECV_TIMEOUT)
        .expect("result")
        .expect_err("ambiguous");
    assert!(error.contains("none is focused"));
    assert!(error.contains("Budget"));
    assert!(error.contains("Forecast"));

    // Focusing the second pane routes "active" there.
    send(&mut second, json!({"type": "pane_state", "focused": true})).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let reply = harness.call(Host::Excel, "read_range", json!({"range": "A1"}));
    let call = recv(&mut second).await;
    assert_eq!(call["documentId"], json!("id-2"));
    let call_id = call["id"].as_str().unwrap().to_string();
    send(
        &mut second,
        json!({"type": "tool_result", "id": call_id, "ok": true, "text": "second"}),
    )
    .await;
    assert_eq!(
        reply
            .recv_timeout(RECV_TIMEOUT)
            .expect("result")
            .unwrap()
            .text,
        "second"
    );

    // An exact name still targets the other document.
    let reply = harness.call(
        Host::Excel,
        "read_range",
        json!({"range": "A1", OFFICE_TARGET_PARAM: "budget"}),
    );
    let call = recv(&mut first).await;
    assert_eq!(call["documentId"], json!("id-1"));
    let call_id = call["id"].as_str().unwrap().to_string();
    send(
        &mut first,
        json!({"type": "tool_result", "id": call_id, "ok": true, "text": "first"}),
    )
    .await;
    assert_eq!(
        reply
            .recv_timeout(RECV_TIMEOUT)
            .expect("result")
            .unwrap()
            .text,
        "first"
    );
}

#[tokio::test]
async fn pane_prompts_focus_and_aborts_reach_the_host() {
    let harness = Harness::start();
    let mut ws = harness.connect().await;
    send(
        &mut ws,
        hello(
            "excel",
            "pane-1",
            Some("Book1.xlsx"),
            Some("id-1"),
            Some(excel_ops()),
        ),
    )
    .await;
    let _ = recv(&mut ws).await;

    send(
        &mut ws,
        json!({"type": "user_message", "text": "  sum A1:A3  "}),
    )
    .await;
    let mut attempts = 0;
    loop {
        let messages = harness.recording.user_messages.lock().unwrap().clone();
        if !messages.is_empty() {
            assert_eq!(messages[0].0, "sum A1:A3");
            assert_eq!(messages[0].1.pane_id, "pane-1");
            break;
        }
        attempts += 1;
        assert!(attempts < 100, "the pane prompt never reached the host");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Typing counts as focus, so "active" now resolves without a pane_state.
    let panes = harness.server.panes();
    assert!(panes
        .iter()
        .any(|pane| pane.pane_id == "pane-1" && pane.focused));

    send(&mut ws, json!({"type": "abort", "promptId": "prompt-1"})).await;
    let mut attempts = 0;
    loop {
        let aborts = harness.recording.aborts.lock().unwrap().clone();
        if !aborts.is_empty() {
            assert_eq!(aborts[0].0.as_deref(), Some("prompt-1"));
            break;
        }
        attempts += 1;
        assert!(attempts < 100, "the abort never reached the host");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn pending_calls_fail_when_the_pane_disconnects_or_is_cancelled() {
    let harness = Harness::start();
    let mut ws = harness.connect().await;
    send(
        &mut ws,
        hello(
            "excel",
            "pane-1",
            Some("Book1.xlsx"),
            None,
            Some(excel_ops()),
        ),
    )
    .await;
    let _ = recv(&mut ws).await;

    let reply = harness.call(Host::Excel, "read_range", json!({"range": "A1"}));
    let _ = recv(&mut ws).await;
    drop(ws);
    let error = reply
        .recv_timeout(RECV_TIMEOUT)
        .expect("result")
        .expect_err("disconnect");
    assert!(error.contains("pane disconnected"));

    let mut ws = harness.connect().await;
    send(
        &mut ws,
        hello(
            "excel",
            "pane-2",
            Some("Book2.xlsx"),
            None,
            Some(excel_ops()),
        ),
    )
    .await;
    let _ = recv(&mut ws).await;
    let (tx, rx) = std::sync::mpsc::channel();
    let id = protocol::next_call_id("tool");
    harness
        .server
        .call(
            id.clone(),
            Host::Excel,
            "read_range".into(),
            json!({"range": "A1"}),
            tx,
        )
        .expect("call");
    let _ = recv(&mut ws).await;
    harness.server.cancel_call(&id).expect("cancel");
    let error = rx
        .recv_timeout(RECV_TIMEOUT)
        .expect("result")
        .expect_err("cancelled");
    assert!(error.contains("aborted by the agent"));
}

#[tokio::test]
async fn agent_frames_are_pushed_to_the_right_pane_only() {
    let harness = Harness::start();
    let mut first = harness.connect().await;
    send(
        &mut first,
        hello("excel", "pane-1", Some("A.xlsx"), None, Some(excel_ops())),
    )
    .await;
    let _ = recv(&mut first).await;
    let mut second = harness.connect().await;
    send(
        &mut second,
        hello(
            "word",
            "pane-2",
            Some("B.docx"),
            None,
            Some(vec!["word.insert_text".into()]),
        ),
    )
    .await;
    let _ = recv(&mut second).await;

    harness
        .server
        .push_to_pane(
            "pane-2",
            protocol::agent_delta("部分", Some("prompt-1"), None),
        )
        .expect("push delta");
    harness
        .server
        .push_to_pane(
            "pane-2",
            protocol::agent_final("完成", Some("prompt-1"), Some("msg-1")),
        )
        .expect("push final");

    let delta = recv(&mut second).await;
    assert_eq!(delta["type"], json!("agent_message"));
    assert_eq!(delta["kind"], json!("delta"));
    assert_eq!(delta["text"], json!("部分"));
    assert_eq!(delta["promptId"], json!("prompt-1"));
    let final_message = recv(&mut second).await;
    assert_eq!(final_message["kind"], json!("final"));
    assert_eq!(final_message["messageId"], json!("msg-1"));

    // The Excel pane only sees its own traffic: ask it for a call and confirm
    // the first frame it receives is that call, not the Word reply.
    let _reply = harness.call(Host::Excel, "read_range", json!({"range": "A1"}));
    let call = recv(&mut first).await;
    assert_eq!(call["type"], json!("tool_call"));
}

#[tokio::test]
async fn health_endpoint_reports_panes_and_honours_origin_rules() {
    let harness = Harness::start();
    let mut ws = harness.connect().await;
    send(
        &mut ws,
        hello(
            "excel",
            "pane-1",
            Some("Book1.xlsx"),
            Some("id-1"),
            Some(excel_ops()),
        ),
    )
    .await;
    let _ = recv(&mut ws).await;

    // curl / non-browser clients get a permissive response.
    let raw = harness.health();
    assert!(raw.starts_with("HTTP/1.1 200"));
    assert!(raw.contains("Access-Control-Allow-Origin: *"));
    let payload = body(&raw);
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["service"], json!("pi-office-bridge"));
    assert_eq!(payload["protocolVersion"], json!(3));
    assert_eq!(payload["catalogVersion"], json!(3));
    assert_eq!(payload["panes"].as_array().unwrap().len(), 1);
    assert_eq!(payload["panes"][0]["host"], json!("excel"));
    assert_eq!(payload["panes"][0]["document"]["name"], json!("Book1.xlsx"));
    assert_eq!(payload["panes"][0]["windows"], json!(1));
    assert_eq!(payload["owner"]["pid"], json!(std::process::id()));

    // A known add-in origin is echoed back.
    let raw = http_get(
        harness.port,
        "/health",
        Some("https://dieuluucanh.github.io"),
    );
    assert!(raw.contains("Access-Control-Allow-Origin: https://dieuluucanh.github.io"));
    // An unknown browser origin gets no CORS grant.
    let raw = http_get(harness.port, "/health", Some("https://evil.example"));
    assert!(!raw.contains("Access-Control-Allow-Origin"));
    // Unknown paths are 404 JSON.
    let raw = http_get(harness.port, "/nope", None);
    assert!(raw.starts_with("HTTP/1.1 404"));
    assert_eq!(body(&raw)["error"], json!("not_found"));
}

#[tokio::test]
async fn bad_frames_and_protocol_mismatches_are_reported() {
    let harness = Harness::start();
    let mut ws = harness.connect().await;

    send(
        &mut ws,
        json!({
            "type": "hello",
            "protocolVersion": 1,
            "host": "excel",
            "clientName": "old",
            "paneId": "pane-1",
            "document": {"id": null, "name": "Old.xlsx", "source": "unknown"},
        }),
    )
    .await;
    let error = recv(&mut ws).await;
    assert_eq!(error["type"], json!("error"));
    assert_eq!(error["code"], json!("bad_message"));
    assert!(error["message"]
        .as_str()
        .unwrap()
        .contains("client protocol v1 is not supported"));

    // A non-JSON frame yields a bad_message error, not a dropped connection.
    ws.send(Message::text("not json".to_string()))
        .await
        .expect("send");
    let error = recv(&mut ws).await;
    assert_eq!(error["code"], json!("bad_message"));
    assert!(error["message"]
        .as_str()
        .unwrap()
        .contains("not valid JSON"));

    // The connection still works afterwards.
    send(&mut ws, json!({"type": "ping", "ts": 7})).await;
    let pong = recv(&mut ws).await;
    assert_eq!(pong["type"], json!("pong"));
    assert_eq!(pong["ts"], json!(7.0));
}
