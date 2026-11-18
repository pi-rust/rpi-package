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
    /// Native events to inject after a delay, in milliseconds.
    injections: Vec<(u64, Value)>,
}

impl Harness {
    fn new() -> Self {
        // The host's stdin is the server's downstream command stream.
        let (host_stdin, downstream) = tokio::io::duplex(256 * 1024);
        // Advertise the commands the adapter gates on, like a real host would.
        let (session, events) = HostSession::test_connection_with_ready(
            host_stdin,
            64,
            json!({
                "type": "ready",
                "protocol": "rpi",
                "supportedCommands": [
                    "get_state",
                    "get_messages",
                    "compact",
                    "get_available_models",
                    "get_available_thinking_levels",
                    "set_model",
                    "set_thinking_level",
                    "abort",
                ],
            }),
        );
        Self {
            server: Arc::new(McpServer::from_session(session)),
            events,
            stdin: VecDeque::new(),
            downstream,
            injections: Vec::new(),
        }
    }

    fn push(&mut self, value: Value) {
        self.stdin.push_back(value.to_string());
    }

    /// Inject a native event once the dispatcher has started.
    fn inject(&mut self, delay_ms: u64, event: Value) {
        self.injections.push((delay_ms, event));
    }

    /// Run the dispatcher, feed queued frames, then stop it and return every
    /// frame the server wrote. `reply` answers native commands.
    async fn run<S>(mut self, reply: S) -> (Vec<Value>, Vec<Value>)
    where
        S: Fn(&Value) -> Option<Value> + Send + 'static,
    {
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
        let commands_seen = Arc::new(Mutex::new(Vec::new()));
        let commands_handle = Arc::clone(&commands_seen);
        // Native command pump: answer like `rpi --mode rpc` would.
        let responder = tokio::spawn(async move {
            let mut commands = BufReader::new(self.downstream).lines();
            while let Ok(Some(line)) = commands.next_line().await {
                let Ok(command) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                commands_handle
                    .lock()
                    .unwrap()
                    .push(command.clone() as Value);
                if let Some(response) = reply(&command) {
                    let _ = events.send(response);
                }
            }
        });
        for (delay, event) in std::mem::take(&mut self.injections) {
            let events = self.events.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                events.send_projected(event).await;
            });
        }
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
        let commands = commands_seen.lock().unwrap().clone();
        (frames, commands)
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
    let (frames, _) = harness.run(|_| None).await;

    assert_eq!(frames.len(), 4, "{frames:#?}");
    assert_eq!(frames[0]["id"], 1);
    assert_eq!(frames[0]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(frames[0]["result"]["capabilities"]["tools"], json!({}));
    assert_eq!(
        frames[0]["result"]["capabilities"]["resources"]["subscribe"],
        true
    );
    assert_eq!(frames[1]["id"], 2);
    assert_eq!(frames[1]["result"]["tools"].as_array().unwrap().len(), 8);
    assert_eq!(frames[2]["error"]["code"], -32601);
    assert_eq!(frames[3]["error"]["code"], -32602);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_tool_calls_before_initialize() {
    let mut harness = Harness::new();
    harness.push(request(1, "tools/call", json!({"name":"rpi_state"})));
    let (frames, _) = harness.run(|_| None).await;
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
    let (frames, _) = harness
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lists_and_reads_transcript_and_state_resources() {
    let mut harness = Harness::new();
    harness.push(request(
        1,
        "initialize",
        json!({"protocolVersion":"2025-06-18"}),
    ));
    harness.push(request(2, "resources/list", json!({})));
    harness.push(request(
        3,
        "resources/read",
        json!({"uri":"rpi://session/transcript"}),
    ));
    harness.push(request(
        4,
        "resources/read",
        json!({"uri":"rpi://session/state"}),
    ));
    harness.push(request(
        5,
        "resources/read",
        json!({"uri":"rpi://session/unknown"}),
    ));
    let (frames, _) = harness
        .run(|command| match command["type"].as_str() {
            Some("get_messages") => Some(json!({
                "type":"response",
                "id": command["id"],
                "success": true,
                "data": {"messages": [
                    {"role":"user","content":"hello there"},
                    {"role":"assistant","content":[{"type":"text","text":"hi"}]},
                ]},
            })),
            Some("get_state") => Some(json!({
                "type":"response",
                "id": command["id"],
                "success": true,
                "data": {"model":"fixture/model"},
            })),
            _ => None,
        })
        .await;

    assert_eq!(frames.len(), 5, "{frames:#?}");
    let resources = frames[1]["result"]["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 2);
    assert_eq!(resources[0]["uri"], "rpi://session/transcript");
    assert_eq!(resources[0]["mimeType"], "text/markdown");
    assert_eq!(resources[1]["uri"], "rpi://session/state");

    let transcript = frames[2]["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(transcript.contains("hello there"), "{transcript}");
    assert!(transcript.contains("## assistant"), "{transcript}");

    assert_eq!(
        frames[3]["result"]["contents"][0]["mimeType"],
        "application/json"
    );
    assert!(frames[3]["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("fixture/model"));
    assert_eq!(frames[4]["error"]["code"], -32602);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notifies_subscribed_resources_when_the_run_settles() {
    let mut harness = Harness::new();
    harness.push(request(
        1,
        "initialize",
        json!({"protocolVersion":"2025-06-18"}),
    ));
    harness.push(request(
        2,
        "resources/subscribe",
        json!({"uri":"rpi://session/transcript"}),
    ));
    // The watcher is spawned by subscribe; settle a few times so the test does
    // not depend on that spawn racing the first injected event.
    for step in 1..=5 {
        harness.inject(
            step * 150,
            json!({"type":"agent_settled","outcome":"completed"}),
        );
    }
    let (frames, _) = harness.run(|_| None).await;

    assert_eq!(frames[0]["id"], 1);
    assert_eq!(frames[1]["id"], 2);
    assert_eq!(frames[1]["result"], json!({}));
    let updates: Vec<&Value> = frames
        .iter()
        .filter(|frame| frame["method"] == "notifications/resources/updated")
        .collect();
    assert!(!updates.is_empty(), "{frames:#?}");
    assert_eq!(updates[0]["params"]["uri"], "rpi://session/transcript");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_returns_settled_text_and_reports_progress() {
    let mut harness = Harness::new();
    harness.push(request(
        1,
        "initialize",
        json!({"protocolVersion":"2025-06-18"}),
    ));
    harness.push(request(
        2,
        "tools/call",
        json!({
            "name":"rpi_prompt",
            "arguments":{"message":"say pong"},
            "_meta":{"progressToken":"p1"},
        }),
    ));
    // Acceptance is correlated by request id; the settlement then ends the run.
    harness.inject(
        300,
        json!({"type":"agent_settled","outcome":"completed","finalText":"pong"}),
    );
    let (frames, _) = harness
        .run(|command| {
            (command["type"] == "prompt").then(|| {
                json!({
                    "type":"response",
                    "id": command["id"],
                    "success": true,
                    "data": {"accepted": true},
                })
            })
        })
        .await;

    let result = frames
        .iter()
        .find(|frame| frame["id"] == 2)
        .expect("prompt response")
        .clone();
    assert_eq!(result["result"]["isError"], false, "{result:#?}");
    assert_eq!(
        result["result"]["structuredContent"]["outcome"],
        "completed"
    );
    assert!(result["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("pong"));
    // Progress notifications only appear when the client supplies a token.
    let progress: Vec<&Value> = frames
        .iter()
        .filter(|frame| frame["method"] == "notifications/progress")
        .collect();
    for frame in progress {
        assert_eq!(frame["params"]["progressToken"], "p1");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn denies_permission_dialogs_instead_of_hanging() {
    let mut harness = Harness::new();
    harness.push(request(
        1,
        "initialize",
        json!({"protocolVersion":"2025-06-18"}),
    ));
    harness.push(request(
        2,
        "tools/call",
        json!({"name":"rpi_prompt","arguments":{"message":"run a command"}}),
    ));
    harness.inject(
        200,
        json!({
            "type":"ui_dialog_request",
            "requestId":"d1",
            "title":"Allow bash?",
            "options":["Allow","Deny"],
        }),
    );
    harness.inject(
        500,
        json!({"type":"agent_settled","outcome":"completed","finalText":"done"}),
    );
    let (frames, commands) = harness
        .run(|command| {
            (command["type"] == "prompt").then(|| {
                json!({
                    "type":"response",
                    "id": command["id"],
                    "success": true,
                    "data": {"accepted": true},
                })
            })
        })
        .await;

    let denial = commands
        .iter()
        .find(|command| command["type"] == "ui_dialog_response")
        .expect("dispatcher answers the native dialog");
    assert_eq!(denial["requestId"], "d1");
    assert_eq!(denial["cancelled"], true);

    let result = frames
        .iter()
        .find(|frame| frame["id"] == 2)
        .expect("prompt response")
        .clone();
    assert_eq!(result["result"]["isError"], false, "{result:#?}");
    assert_eq!(
        result["result"]["structuredContent"]["deniedApprovals"][0],
        "Allow bash?"
    );
    assert!(result["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("denied 1 approval request"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compacts_context_and_returns_the_summary() {
    let mut harness = Harness::new();
    harness.push(request(
        1,
        "initialize",
        json!({"protocolVersion":"2025-06-18"}),
    ));
    harness.push(request(
        2,
        "tools/call",
        json!({"name":"rpi_compact","arguments":{}}),
    ));
    harness.inject(
        300,
        json!({
            "type":"agent_settled",
            "outcome":"compacted",
            "summary":"Earlier work summarized.",
        }),
    );
    let (frames, commands) = harness
        .run(|command| {
            (command["type"] == "compact").then(|| {
                json!({
                    "type":"response",
                    "id": command["id"],
                    "success": true,
                    "data": {"accepted": true},
                })
            })
        })
        .await;

    assert!(
        commands.iter().any(|command| command["type"] == "compact"),
        "{commands:#?}"
    );
    let result = frames
        .iter()
        .find(|frame| frame["id"] == 2)
        .expect("compact response")
        .clone();
    assert_eq!(result["result"]["isError"], false, "{result:#?}");
    assert_eq!(
        result["result"]["structuredContent"]["outcome"],
        "completed"
    );
    assert!(result["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Earlier work summarized."));
}
