//! Deterministic native RPC peer for the real-process ACP MCP regression.
//! Deliberately independent of fake_rpc.rs; persisted history is authoritative.
use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn send(value: Value) {
    println!("{value}");
    std::io::stdout().flush().unwrap();
}
fn persist(path: &std::path::Path, messages: &mut Vec<Value>, role: &str, content: Value) -> Value {
    let id = format!("native-message-{}", messages.len() + 1);
    let message = json!({"id":id,"role":role,"content":content});
    let entry = json!({"kind":"message","id":id,
        "parentId":messages.last().map(|m| &m["entryId"]),"message":message});
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{entry}").unwrap();
    let mut row = message.clone();
    row["entryId"] = json!(id);
    messages.push(row);
    message
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let arg = |key: &str| args.windows(2).find(|a| a[0] == key).map(|a| a[1].clone());
    let transcript = if let Some(session_id) = arg("--session-id") {
        let dir = std::path::PathBuf::from(arg("--session-dir").unwrap()).join("fixture");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("fixture_{session_id}.jsonl"));
        std::fs::write(&path, format!("{}\n", json!({"kind":"header","version":4,"id":session_id,"createdAt":1,"cwd":std::env::current_dir().unwrap()}))).unwrap();
        path
    } else {
        std::path::PathBuf::from(arg("--session").unwrap())
    };
    let text = std::fs::read_to_string(&transcript).unwrap();
    let mut entries = text.lines().map(|line| serde_json::from_str::<Value>(line).unwrap());
    let session_id = entries.next().unwrap()["id"].as_str().unwrap().to_owned();
    let mut messages: Vec<Value> = entries.filter(|entry| entry["kind"] == "message").map(|entry| {
        let mut message = entry["message"].clone();
        message["entryId"] = entry["id"].clone();
        message
    }).collect();
    send(json!({"type":"ready","protocol":"rpi","supportedCommands":["configure_external_tools","external_tool_response"]}));
    let mut tools = json!([]);
    let mut pending_tool = None;
    for line in std::io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let command = request["type"].as_str().unwrap();
        if let Ok(path) = std::env::var("RPI_TEST_MCP_NATIVE_LOG") {
            let mut log = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
            writeln!(log, "{request}").unwrap();
        }
        let data = match command {
            "get_state" => json!({"sessionId":session_id,"leafId":messages.last().map(|m| &m["entryId"])}),
            "get_messages" => {
                let limit = request["limit"].as_u64().unwrap() as usize;
                assert!((1..=128).contains(&limit));
                let end = match request["beforeId"].as_str() {
                    Some(id) => messages.iter().position(|m| m["entryId"] == id).expect("unknown history cursor"),
                    None => messages.len(),
                };
                let start = end.saturating_sub(limit);
                let page = &messages[start..end];
                json!({"messages":page,"total":messages.len(),
                    "leafId":messages.last().map(|m| &m["entryId"]),
                    "firstId":page.first().map(|m| &m["entryId"]),
                    "lastId":page.last().map(|m| &m["entryId"]),
                    "beforeId":request["beforeId"],"hasMore":start>0})
            }
            "get_available_models" => json!({"models":[]}),
            "get_modes" => json!({"modes":[]}),
            "get_available_thinking_levels" => json!({"levels":[]}),
            "configure_external_tools" => { tools = request["tools"].clone(); json!({"accepted":true}) },
            "shutdown" => break,
            _ => json!({"accepted":true}),
        };
        if request.get("id").is_some() {
            send(json!({"type":"response","id":request["id"],"command":command,"success":true,"data":data}));
        }
        match command {
            "prompt" => {
                assert_eq!(tools.as_array().unwrap().len(), 1, "MCP tool missing");
                let message = persist(&transcript, &mut messages, "user", json!([{"type":"text","text":request["message"]}]));
                send(json!({"type":"message_start","message":message}));
                send(json!({"type":"message_end","message":message}));
                // Simulate the native model's raw external-tool request, not an ACP tool call.
                pending_tool = request["message"].as_str().map(str::to_owned);
                send(json!({"type":"external_tool_request","requestId":request["message"],"toolName":tools[0]["name"],"arguments":{"mode":request["message"]}}));
            }
            "abort" => {
                if let Some(id) = pending_tool.take() {
                    send(json!({"type":"external_tool_request_closed","requestId":id}));
                }
                send(json!({"type":"agent_settled","runId":"mcp-fixture","outcome":"cancelled"}));
            }
            "external_tool_response" => {
                pending_tool = None;
                let mut message = persist(&transcript, &mut messages, "assistant", json!([{"type":"text","text":"MCP complete"}]));
                message["stopReason"] = json!("stop");
                send(json!({"type":"message_end","message":message}));
                send(json!({"type":"agent_settled","runId":"mcp-fixture","outcome":"completed"}));
            }
            _ => {}
        }
    }
}
