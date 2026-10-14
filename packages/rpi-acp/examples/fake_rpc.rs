//! Offline RPC peer for cross-language ACP/Pipe tests. No model or tool executes.
use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn send(message: Value) {
    println!("{message}");
    std::io::stdout().flush().unwrap();
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
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
            directory.join("session.jsonl"),
            format!("{}\n", json!({"kind":"header","version":4,"id":id})),
        )
        .unwrap();
    }
    let mut prompt_id = Value::Null;
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
                json!({"type":"response", "id":message["id"], "status":"ok", "result":{"leafId":null,"sessionId":session_id}}),
            ),
            "prompt" => {
                prompt_id = message["id"].clone();
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
                    json!({"type":"response","id":prompt_id,"status":"ok","result":{"outcome":"completed","finalText":"完成🌊"}}),
                );
            }
            "abort" => send(
                json!({"type":"response","id":prompt_id,"status":"ok","result":{"outcome":"aborted"}}),
            ),
            _ => {}
        }
    }
}
