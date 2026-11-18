//! Dispatcher tests against a scripted native host (no real `rpi` process).
//!
//! These cover framing, method routing, tool validation and native request
//! correlation. They are not a substitute for the provenance-gated smoke run
//! against a real binary.

use rpi_extcore::host::{HostSession, TestEvents};
use rpi_mcp_server::{serve, McpServer};
use rpi_plugin_sdk::stdio::InputLine;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader, DuplexStream};
use tokio::sync::watch;

struct Harness {
    server: Arc<McpServer>,
    events: TestEvents,
    stdin: VecDeque<String>,
    downstream: DuplexStream,
}

impl Harness {
    fn new() -> Self {
        // The host's stdin is the server's downstream command stream.
        let (host_stdin, downstream) = tokio::io::duplex(256 * 1024);
        let (session, events) = HostSession::test_connection(host_stdin, 64);
        Self {
            server: Arc::new(McpServer::from_session(session)),
            events,
            stdin: VecDeque::new(),
            downstream,
        }
    }

    fn push(&mut self, value: Value) {
        self.stdin.push_back(value.to_string());
    }

    /// Run the dispatcher, feed queued frames, then stop it and return every
    /// frame the server wrote. `reply` answers native commands.
    async fn run(mut self, reply: impl Fn(&Value) -> Option<Value> + Send + 'static) -> Vec<Value> {
        let input = Arc::new(Mutex::new(std::mem::take(&mut self.stdin)));
        let poll = {
            let input = Arc::clone(&input);
            move || {
                Ok(match input.lock().unwrap().pop_front() {
                    Some(line) => InputLine::Line(line),
                    None => InputLine::Pending,
                })
            }
        };
        let (out, out_reader) = tokio::io::duplex(256 * 1024);
        let (stop_tx, stop_rx) = watch::channel(false);
        let events = self.events.clone();
        // Native command pump: answer like `rpi --mode rpc` would.
        let responder = tokio::spawn(async move {
            let mut commands = BufReader::new(self.downstream).lines();
            while let Ok(Some(line)) = commands.next_line().await {
                let Ok(command) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(response) = reply(&command) {
                    let _ = events.send(response);
                }
            }
        });
        let serving = tokio::spawn(serve(self.server, poll, None, stop_rx, out));
        let mut frames = Vec::new();
        let mut reader = BufReader::new(out_reader).lines();
        while let Ok(Ok(Some(line))) =
            tokio::time::timeout(Duration::from_secs(2), async { reader.next_line().await }).await
        {
            frames.push(serde_json::from_str(&line).expect("server wrote valid JSON"));
        }
        let _ = stop_tx.send(true);
        serving.await.expect("dispatcher task");
        drop(reader);
        responder.abort();
        frames
    }
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serves_initialize_catalog_and_method_errors_without_native_calls() {
    let mut harness = Harness::new();
    harness.push(request(
        1,
        "initialize",
        json!({"protocolVersion":"2025-03-26"}),
    ));
    harness.push(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    harness.push(request(2, "tools/list", json!({})));
    harness.push(request(3, "does/not/exist", json!({})));
    harness.push(request(4, "tools/call", json!({"name":"nope"})));
    let frames = harness.run(|_| None).await;

    assert_eq!(frames.len(), 4, "{frames:#?}");
    assert_eq!(frames[0]["id"], 1);
    assert_eq!(frames[0]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(frames[0]["result"]["capabilities"], json!({"tools":{}}));
    assert_eq!(frames[1]["id"], 2);
    assert_eq!(frames[1]["result"]["tools"].as_array().unwrap().len(), 7);
    assert_eq!(frames[2]["error"]["code"], -32601);
    assert_eq!(frames[3]["error"]["code"], -32602);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_tool_calls_before_initialize() {
    let mut harness = Harness::new();
    harness.push(request(1, "tools/call", json!({"name":"rpi_state"})));
    let frames = harness.run(|_| None).await;
    assert_eq!(frames.len(), 1, "{frames:#?}");
    assert_eq!(frames[0]["error"]["code"], -32600);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn correlates_native_state_response_with_the_mcp_request() {
    let mut harness = Harness::new();
    harness.push(request(
        1,
        "initialize",
        json!({"protocolVersion":"2025-06-18"}),
    ));
    harness.push(request(
        2,
        "tools/call",
        json!({"name":"rpi_state","arguments":{}}),
    ));
    let frames = harness
        .run(|command| {
            (command["type"] == "get_state").then(|| {
                json!({
                    "type":"response",
                    "id": command["id"],
                    "success": true,
                    "data": {"model":"fixture/model","thinkingLevel":"high"},
                })
            })
        })
        .await;

    assert_eq!(frames.len(), 2, "{frames:#?}");
    let result = &frames[1]["result"];
    assert_eq!(frames[1]["id"], 2);
    assert_eq!(result["isError"], false);
    assert_eq!(result["structuredContent"]["model"], "fixture/model");
    assert!(result["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("fixture/model"));
}
