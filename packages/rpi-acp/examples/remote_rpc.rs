//! Deterministic native peer for remote transport tests. Never calls models/tools.
//! Persists completed exchanges; interrupted turns are deliberately not invented.
use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn send(value: Value) {
    println!("{value}");
    std::io::stdout().flush().unwrap();
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let arg = |name| args.windows(2).find(|p| p[0] == name).map(|p| p[1].clone());
    let path = if let Some(source) = arg("--fork") {
        // Native --fork allocates its own identity (there is no --session-id)
        // and persists under --session-dir before the staging host is closed.
        let history = std::fs::read_to_string(source).unwrap();
        let mut rows: Vec<Value> = history
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let id = uuid::Uuid::new_v4().to_string();
        let parent = rows[0]["id"].clone();
        rows[0]["id"] = json!(id);
        rows[0]["parentSessionId"] = parent;
        let directory = std::path::PathBuf::from(arg("--session-dir").unwrap()).join("fixture");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("remote_{id}.jsonl"));
        let mut file = std::fs::File::create(&path).unwrap();
        for row in rows {
            writeln!(file, "{row}").unwrap();
        }
        file.sync_all().unwrap();
        path
    } else if let Some(id) = arg("--session-id") {
        let directory = std::path::PathBuf::from(arg("--session-dir").unwrap()).join("fixture");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("remote_{id}.jsonl"));
        let cwd = std::env::current_dir().unwrap();
        std::fs::write(
            &path,
            format!(
                "{}\n",
                json!({"kind":"header","version":4,"id":id,"createdAt":1,"cwd":cwd})
            ),
        )
        .unwrap();
        path
    } else {
        arg("--session").unwrap().into()
    };
    let history = std::fs::read_to_string(&path).unwrap();
    let mut entries = history
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap());
    let id = entries.next().unwrap()["id"].as_str().unwrap().to_owned();
    let mut messages: Vec<Value> = entries
        .map(|entry| {
            let mut message = entry["message"].clone();
            message["entryId"] = entry["id"].clone();
            message
        })
        .collect();
    if let Ok(path) = std::env::var("RPI_REMOTE_TEST_PIDS") {
        writeln!(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{}",
            std::process::id()
        )
        .unwrap();
    }
    let early_update = std::env::var("RPI_REMOTE_TEST_EARLY_UPDATE").as_deref() == Ok("1");
    if early_update {
        send(json!({"type":"ready","protocol":"rpi","supportedCommands":["get_commands","execute_command"]}));
    } else {
        send(json!({"type":"ready","protocol":"rpi"}));
    }
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let value: Value = serde_json::from_str(&line).unwrap();
        let kind = value["type"].as_str().unwrap_or("");
        let leaf = messages.last().map(|m| m["entryId"].clone());
        let mut data = json!({});
        match kind {
            "get_state" => data = json!({"sessionId":id,"leafId":leaf}),
            "get_available_models" => data = json!({"models":[]}),
            "get_available_thinking_levels" => data = json!({"levels":[]}),
            "get_modes" => data = json!({"modes":[]}),
            "get_commands" if early_update => data = json!({"commands":[{"name":"early","description":"Pre-result fixture command"}]}),
            "execute_command" if early_update => {
                assert_eq!(value["command"], "early");
                data = json!({"executed":true});
            }
            "get_messages" => {
                let end = match value["beforeId"].as_str() {
                    Some(before) => messages
                        .iter()
                        .position(|m| m["entryId"] == before)
                        .expect("unknown history cursor"),
                    None => messages.len(),
                };
                let limit = value["limit"].as_u64().expect("bounded history request") as usize;
                assert!((1..=128).contains(&limit));
                let start = end.saturating_sub(limit);
                let page = &messages[start..end];
                data = json!({
                    "messages": page, "leafId": leaf, "total": messages.len(),
                    "firstId": page.first().map(|m| &m["entryId"]),
                    "lastId": page.last().map(|m| &m["entryId"]), "hasMore": start > 0,
                    "beforeId": value.get("beforeId").cloned().unwrap_or(Value::Null)
                });
            }
            "prompt" => {
                send(
                    json!({"type":"response","id":value["id"],"command":kind,"success":true,"data":{"accepted":true}}),
                );
                send(json!({"type":"message_update","eventType":"text_delta","delta":"remote🌊"}));
                if value["message"] != "wait" {
                    for message in [
                        json!({"role":"user","content":value["message"]}),
                        json!({"role":"assistant","content":[{"type":"text","text":"remote🌊"}],"stopReason":"stop"}),
                    ] {
                        let entry_id = format!("entry-{}", messages.len());
                        let parent = messages.last().map(|m| m["entryId"].clone());
                        let entry = json!({"kind":"message","id":entry_id,"parentId":parent,"message":message});
                        let mut file = std::fs::OpenOptions::new()
                            .append(true)
                            .open(&path)
                            .unwrap();
                        writeln!(file, "{entry}").unwrap();
                        file.sync_all().unwrap();
                        let mut message = entry["message"].clone();
                        message["entryId"] = json!(entry_id);
                        messages.push(message);
                    }
                    send(json!({"type":"message_end","message":messages.last().unwrap()}));
                    send(json!({"type":"agent_settled","outcome":"completed"}));
                }
                continue;
            }
            "abort" => {
                send(json!({"type":"agent_settled","outcome":"aborted","aborted":true}));
                continue;
            }
            "shutdown" => break,
            _ => {}
        }
        send(json!({"type":"response","id":value["id"],"command":kind,"success":true,"data":data}));
    }
}
