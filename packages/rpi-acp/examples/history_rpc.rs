//! Bounded-history native RPC fixture for the real ACP bridge subprocess tests.
use serde_json::{json, Value};
use std::io::{BufRead, Write};
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |key: &str| args.windows(2).find(|v| v[0] == key).map(|v| v[1].clone());
    let id = if let Some(id) = arg("--session-id") {
        let directory = std::path::PathBuf::from(arg("--session-dir").unwrap()).join("fixture");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(format!("1_{id}.jsonl")), format!("{}\n", json!({"kind":"header","version":4,"id":id,"createdAt":1,"cwd":std::env::current_dir().unwrap()}))).unwrap();
        id
    } else {
        let header = std::fs::read_to_string(arg("--session").unwrap()).unwrap();
        serde_json::from_str::<Value>(header.lines().next().unwrap()).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let send = |value: Value| {
        println!("{value}");
        std::io::stdout().flush().unwrap();
    };
    if let Ok(path) = std::env::var("RPI_HISTORY_PID_LOG") {
        let mut log = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        writeln!(log, "{}", std::process::id()).unwrap();
    }
    let ready = if std::env::var_os("RPI_HISTORY_DISCOVERY_ONLY").is_some() {
        // Command discovery without execution must not produce selectable ACP commands.
        json!({"type":"ready","protocol":"rpi","supportedCommands":["get_commands"]})
    } else if std::env::var_os("RPI_HISTORY_COMPACT").is_some() {
        // Discovery-only test capability: ordering tests must never execute it.
        json!({"type":"ready","protocol":"rpi","supportedCommands":["compact"]})
    } else {
        json!({"type":"ready","protocol":"rpi"})
    };
    send(ready);
    let mut pages = 0;
    for line in std::io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        if let Ok(path) = std::env::var("RPI_HISTORY_LOG") {
            let mut log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(log, "{request}").unwrap();
        }
        let data = match request["type"].as_str().unwrap() {
            "get_state" => json!({"leafId":"512","sessionId":id}),
            "prompt" => {
                send(json!({"type":"response","id":request["id"],"command":"prompt",
                    "success":true,"data":{"accepted":true}}));
                send(json!({"type":"agent_start"}));
                send(json!({"type":"message_start","message":{"kind":"user","role":"user",
                    "messageId":"held-user","content":[{"type":"text","text":request["message"]}]}}));
                continue;
            }
            "abort" => {
                send(json!({"type":"agent_settled","outcome":"aborted"}));
                json!({})
            }
            "get_messages" => {
                pages += 1;
                let limit = request["limit"].as_u64().unwrap() as usize;
                assert!((1..=128).contains(&limit));
                if let Ok(delay) = std::env::var("RPI_HISTORY_DELAY") {
                    std::thread::sleep(std::time::Duration::from_millis(delay.parse().unwrap()));
                }
                let end = request["beforeId"]
                    .as_str()
                    .map(|id| id.parse::<usize>().unwrap())
                    .unwrap_or(513);
                let start = end.saturating_sub(limit);
                let mut data = json!({"messages":(start..end).map(|n| json!({"entryId":n.to_string(),"role":"user","id":format!("user-{n}"),"content":format!("history-{n}")})).collect::<Vec<_>>(),"total":513,"leafId":"512","firstId":(start<end).then(||start.to_string()),"lastId":(start<end).then(||(end-1).to_string()),"beforeId":request["beforeId"],"hasMore":start>0});
                if std::env::var_os("RPI_HISTORY_BAD_PAGE").is_some() {
                    data["firstId"] = json!("wrong");
                }
                if std::env::var_os("RPI_HISTORY_BRANCH_CHANGE").is_some() && pages > 1 {
                    data["leafId"] = json!("changed");
                }
                data
            }
            "get_available_models" => json!({"models":[]}),
            "get_available_thinking_levels" => json!({"levels":[]}),
            "get_modes" => json!({"modes":[]}),
            "shutdown" => break,
            _ => {
                send(
                    json!({"type":"response","id":request["id"],"success":false,"error":"unsupported fixture command"}),
                );
                continue;
            }
        };
        send(json!({"type":"response","id":request["id"],"success":true,"data":data}));
    }
}
