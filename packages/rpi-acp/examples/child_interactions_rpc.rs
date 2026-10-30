//! Dedicated child-interaction native peer. No model, auth, MCP or tool execution.
use serde_json::{json, Value};
use std::{collections::HashSet, io::{BufRead, Write}};
fn send(value: Value) { println!("{value}"); std::io::stdout().flush().unwrap(); }
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| args.windows(2).find(|a| a[0] == name).map(|a| a[1].clone()).unwrap();
    let root = arg("--session-id");
    let mode = std::env::var("RPI_TEST_INTERACTIONS").unwrap_or_else(|_| "normal".into());
    let dir = std::path::PathBuf::from(arg("--session-dir"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("fixture.jsonl"), format!("{}\n", json!({"kind":"header","version":4,"id":root,"createdAt":1,"cwd":std::env::current_dir().unwrap()}))).unwrap();
    send(json!({"type":"ready","protocol":"rpi","childEventsVersion":1}));
    let mut pending = HashSet::<String>::new();
    for line in std::io::stdin().lock().lines() {
        let message: Value = serde_json::from_str(&line.unwrap()).unwrap();
        if let Ok(path) = std::env::var("RPI_TEST_INTERACTION_LOG") {
            let mut file = std::fs::OpenOptions::new().append(true).create(true).open(path).unwrap();
            writeln!(file, "{}", json!({"root":root,"message":message})).unwrap();
        }
        match message["type"].as_str().unwrap_or("") {
            "get_state" => send(json!({"type":"response","id":message["id"],"command":"get_state","success":true,"data":{"sessionId":root,"leafId":null}})),
            "get_available_models" | "get_modes" | "get_available_thinking_levels" => send(json!({"type":"response","id":message["id"],"command":message["type"],"success":true,"data":{"models":[],"modes":[],"levels":[]}})),
            "prompt" => {
                send(json!({"type":"response","id":message["id"],"command":"prompt","success":true,"data":{"accepted":true}}));
                send(json!({"type":"message_end","message":{"role":"user","messageId":"fixture-user","content":[{"type":"text","text":message["message"]}]}}));
                if mode != "unannounced" && mode != "unnegotiated" {
                    send(json!({"type":"child_start","parentSessionId":root,"sessionId":"worker"}));
                }
                let path = std::env::current_dir().unwrap().join("fixture.txt");
                let mut fs = json!({"type":"client_request","requestId":"child-fs","sessionId":"worker","method":"fs/read_text_file","params":{"sessionId":"worker","path":path}});
                if mode == "forged" { fs["params"]["sessionId"] = json!(root); }
                pending.insert("child-fs".into()); send(fs);
                if matches!(mode.as_str(), "unannounced" | "unnegotiated" | "forged") { continue; }
                let events = [
                    json!({"type":"client_request","requestId":"root-fs","method":"fs/read_text_file","params":{"sessionId":root,"path":path}}),
                    json!({"type":"client_request","requestId":"child-terminal","sessionId":"worker","method":"terminal/create","params":{"command":"fixture"}}),
                    json!({"type":"ui_dialog_request","requestId":"child-permission","sessionId":"worker","toolCallId":"tool","ui":{"purpose":"permission","kind":"selector","question":"Allow?","options":[{"title":"Yes","permission":"allow_once"},{"title":"No","permission":"reject_once"}]}}),
                    json!({"type":"ui_dialog_request","requestId":"child-form","sessionId":"worker","ui":{"purpose":"question","kind":"selector","question":"Choose","options":[{"title":"Rust"},{"title":"Python"}]}}),
                    json!({"type":"ui_dialog_request","requestId":"child-url","sessionId":"worker","ui":{"purpose":"question","kind":"url","question":"Sign in","url":"https://example.test/login","elicitationId":"fixture-url"}}),
                ];
                for event in events { pending.insert(event["requestId"].as_str().unwrap().into()); send(event); }
                if mode == "close" {
                    pending.remove("child-form");
                    send(json!({"type":"ui_dialog_closed","requestId":"child-form"}));
                }
            }
            "client_response" | "ui_dialog_response" => {
                assert!(pending.remove(message["requestId"].as_str().unwrap()), "unexpected or late native response: {message}");
                if pending.is_empty() {
                    send(json!({"type":"agent_settled","runId":"fixture","outcome":"completed"}));
                }
            }
            "abort" => {
                pending.clear();
                for (id, method) in [("cleanup-kill", "terminal/kill"), ("cleanup-release", "terminal/release")] {
                    pending.insert(id.into());
                    send(json!({"type":"client_request","requestId":id,"sessionId":"worker","method":method,"params":{"sessionId":"worker","terminalId":"term"}}));
                }
                // Interactive cancellation replies are allowed during teardown.
                pending.extend(["child-fs","root-fs","child-terminal","child-permission","child-form","child-url"].map(str::to_owned));
                send(json!({"type":"agent_settled","runId":"fixture","outcome":"aborted","aborted":true}));
            }
            _ => send(json!({"type":"response","id":message["id"],"command":message["type"],"success":false,"error":"unsupported fixture command"})),
        }
    }
}
