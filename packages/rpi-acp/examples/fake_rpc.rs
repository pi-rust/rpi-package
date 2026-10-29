//! Offline RPC peer for cross-language ACP/Pipe tests. No model or tool executes.
use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn send(message: Value) {
    println!("{message}");
    std::io::stdout().flush().unwrap();
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args
        .windows(2)
        .any(|args| args[0] == "auth" && args[1] == "logout")
    {
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
    let session_id = argument("--session-id").unwrap_or_else(|| {
        let history = std::fs::read_to_string(argument("--session").unwrap()).unwrap();
        serde_json::from_str::<Value>(history.lines().next().unwrap()).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    });
    if let Some(id) = argument("--session-id") {
        let directory =
            std::path::PathBuf::from(argument("--session-dir").unwrap()).join("fixture");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join(format!("fixture_{id}.jsonl")),
            format!("{}\n", json!({"kind":"header","version":4,"id":id})),
        )
        .unwrap();
    }
    send(json!({"type":"ready","protocol":"rpi"}));
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
            "get_state" => send(
                json!({"type":"response", "id":message["id"], "command":"get_state", "success":true, "data":{"leafId":null,"sessionId":session_id}}),
            ),
            "get_available_models" => send(
                json!({"type":"response","id":message["id"],"command":"get_available_models","success":true,"data":{"models":[]}}),
            ),
            "get_available_thinking_levels" => send(
                json!({"type":"response","id":message["id"],"command":"get_available_thinking_levels","success":true,"data":{"levels":[]}}),
            ),
            "prompt" => {
                assert!(message["message"].is_string());
                send(
                    json!({"type":"response","id":message["id"],"command":"prompt","success":true,"data":{"accepted":true}}),
                );
                send(
                    json!({"type":"ui_dialog_request", "requestId":"question", "toolCallId":"ask", "ui":{
                    "purpose":"question","kind":"selector","question":"语言？",
                    "options":[{"title":"Rust"},{"title":"Python"}],"allowMultiple":true,"allowComment":true}}),
                );
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
