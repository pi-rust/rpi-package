//! Offline RPC peer for cross-language ACP/Pipe tests. No model or tool executes.
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::time::Duration;

fn test_delay(name: &str) {
    if let Ok(value) = std::env::var(name) {
        if let Ok(milliseconds) = value.parse::<u64>() {
            std::thread::sleep(Duration::from_millis(milliseconds));
        }
    }
}

fn log_pid() {
    if let Ok(path) = std::env::var("RPI_TEST_RPC_PID_LOG") {
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(log, "{}", std::process::id()).unwrap();
    }
}

fn send(message: Value) {
    println!("{message}");
    std::io::stdout().flush().unwrap();
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    log_pid();
    if args
        .windows(2)
        .any(|args| args[0] == "auth" && args[1] == "logout")
    {
        test_delay("RPI_TEST_LOGOUT_HANG_MS");
        // Deliberately try to pollute stdout and consume stdin. The ACP auth
        // adapter must isolate both descriptors from its JSON-RPC stream.
        println!("fixture logout stdout pollution");
        std::io::stdout().flush().unwrap();
        let _ = std::io::stdin().read_line(&mut String::new());
        std::process::exit(0);
    }
    let argument = |name| {
        args.windows(2)
            .find(|args| args[0] == name)
            .map(|args| args[1].clone())
    };
    let fork_id = argument("--fork").map(|_| uuid::Uuid::new_v4().to_string());
    let session_id = fork_id.clone().or_else(|| argument("--session-id")).unwrap_or_else(|| {
        let history = std::fs::read_to_string(argument("--session").unwrap()).unwrap();
        serde_json::from_str::<Value>(history.lines().next().unwrap()).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    });
    if let Some(id) = fork_id.or_else(|| argument("--session-id")) {
        let directory =
            std::path::PathBuf::from(argument("--session-dir").unwrap()).join("fixture");
        std::fs::create_dir_all(&directory).unwrap();
        let cwd = std::env::current_dir().unwrap();
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        std::fs::write(
            directory.join(format!("fixture_{id}.jsonl")),
            format!(
                "{}\n",
                json!({"kind":"header","version":4,"id":id,"createdAt":created_at,"cwd":cwd})
            ),
        )
        .unwrap();
    }
    let child_fixture = std::env::var("RPI_TEST_CHILD_MODE").ok();
    let child_enabled = std::env::var("RPI_CHILD_EVENTS").ok();
    if let Ok(path) = std::env::var("RPI_TEST_CHILD_LOG") {
        let mut log = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        writeln!(log, "{}", json!({"sessionId":session_id,"childEvents":child_enabled,"fork":argument("--fork").is_some(),"reload":argument("--session").is_some()})).unwrap();
    }
    test_delay("RPI_TEST_RPC_READY_DELAY_MS");
    let mut ready = json!({"type":"ready","protocol":"rpi"});
    if child_fixture.is_some() {
        ready["childEventsVersion"] = json!(1);
    }
    send(ready);
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let message: Value = serde_json::from_str(&line).unwrap();
        if let Ok(path) = std::env::var("RPI_TEST_RPC_LOG") {
            let mut log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(log, "{message}").unwrap();
        }
        match message["type"].as_str().unwrap_or("") {
            "get_state" => {
                test_delay("RPI_TEST_RPC_GET_STATE_DELAY_MS");
                send(
                    json!({"type":"response", "id":message["id"], "command":"get_state", "success":true, "data":{"leafId":null,"sessionId":session_id}}),
                )
            }
            "get_available_models" => send(
                json!({"type":"response","id":message["id"],"command":"get_available_models","success":true,"data":{"models":[]}}),
            ),
            "get_modes" => send(
                json!({"type":"response","id":message["id"],"command":"get_modes","success":true,"data":{"modes":[]}}),
            ),
            "get_available_thinking_levels" => send(
                json!({"type":"response","id":message["id"],"command":"get_available_thinking_levels","success":true,"data":{"levels":[]}}),
            ),
            "prompt" => {
                assert!(message["message"].is_string());
                send(
                    json!({"type":"response","id":message["id"],"command":"prompt","success":true,"data":{"accepted":true}}),
                );
                if let Some(mode) = child_fixture.as_deref() {
                    send(json!({"type":"message_end","message":{"role":"user","messageId":"fixture-user","content":[{"type":"text","text":message["message"]}]}}));
                    if child_enabled.as_deref() == Some("1") {
                        send(json!({"type":"child_start","parentSessionId":session_id,"sessionId":"child","title":"Fixture worker"}));
                        let native = match mode {
                            "unknown" => json!({"type":"future_child_request"}),
                            "malformed" => json!({"type":"message_update","eventType":"text_delta","delta":42}),
                            "oversize" => json!({"type":"message_update","eventType":"text_delta","delta":"x".repeat(256 * 1024)}),
                            _ => json!({"type":"message_update","eventType":"text_delta","delta":"child hello"}),
                        };
                        send(json!({"type":"child_event","parentSessionId":session_id,"sessionId":"child","event":native}));
                        if mode == "normal" {
                            send(json!({"type":"child_event","parentSessionId":session_id,"sessionId":"child","event":{"type":"message_end","message":{"role":"assistant","id":"child-message","content":[{"type":"text","text":"child hello world"}]}}}));
                            send(json!({"type":"child_end","parentSessionId":session_id,"sessionId":"child","result":{"status":"completed","text":"child hello world"}}));
                        } else if mode == "missing_id" {
                            send(json!({"type":"child_end","parentSessionId":session_id,"sessionId":"child","result":{"status":"completed"}}));
                        }
                    }
                    // Keep invalid streams running so host termination, not an
                    // unrelated root settlement, determines the prompt failure.
                    if mode != "normal" { continue; }
                    send(json!({"type":"message_end","message":{"role":"assistant","messageId":"root-message","content":[{"type":"text","text":"root result"}],"stopReason":"stop"}}));
                    send(json!({"type":"agent_settled","runId":"fixture-run","outcome":"completed"}));
                    continue;
                }
                if let Ok(count) = std::env::var("RPI_TEST_RPC_BURST")
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok())
                    .ok_or(())
                {
                    for _ in 0..count {
                        send(
                            json!({"type":"message_update","eventType":"text_delta","delta":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}),
                        );
                    }
                }
                if let Ok(method) = std::env::var("RPI_TEST_RPC_CLIENT_REQUEST") {
                    let params = match method.as_str() {
                        "fs/read_text_file" => json!({
                            "sessionId": session_id,
                            "path": std::env::current_dir().unwrap().join("fixture.txt")
                        }),
                        "terminal/create" => json!({
                            "sessionId": session_id,
                            "command": "printf",
                            "args": ["client-terminal"]
                        }),
                        _ => panic!("unsupported client request fixture: {method}"),
                    };
                    send(
                        json!({"type":"client_request","requestId":"client-fixture","method":method,"params":params}),
                    );
                    continue;
                }
                send(
                    json!({"type":"ui_dialog_request", "requestId":"question", "toolCallId":"ask", "ui":{
                    "purpose":"question","kind":"selector","question":"语言？",
                    "options":[{"title":"Rust"},{"title":"Python"}],"allowMultiple":true,"allowComment":true}}),
                );
            }
            "client_response" if message["requestId"] == "client-fixture" => {
                assert!(message.get("result").is_some() || message["cancelled"] == true);
                send(
                    json!({"type":"message_update","eventType":"text_delta","delta":"client request complete"}),
                );
                send(
                    json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"client request complete"}],"stopReason":"stop"}}),
                );
                send(json!({"type":"agent_settled","runId":"fixture-run","outcome":"completed"}));
            }
            "ui_dialog_response" if message["requestId"] == "question" => {
                send(json!({"type":"ui_dialog_closed","requestId":"question"}));
                send(
                    json!({"type":"ui_dialog_request","requestId":"permission","toolCallId":"bash", "ui":{
                    "purpose":"permission","kind":"selector","question":"Allow command?",
                    "options":[{"title":"允许一次","permission":"allow_once"},{"title":"拒绝","permission":"reject_once"}]}}),
                );
            }
            "ui_dialog_response" if message["requestId"] == "permission" => {
                send(json!({"type":"ui_dialog_closed","requestId":"permission"}));
                send(json!({"type":"message_update","eventType":"text_delta","delta":"完成🌊"}));
                send(
                    json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"完成🌊"}],"stopReason":"stop"}}),
                );
                send(json!({"type":"agent_settled","runId":"fixture-run","outcome":"completed"}));
            }
            "abort" => send(
                json!({"type":"agent_settled","runId":"fixture-run","outcome":"aborted","aborted":true}),
            ),
            _ => {}
        }
    }
}
