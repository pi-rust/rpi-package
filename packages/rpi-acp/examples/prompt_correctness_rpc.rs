//! Offline native peer for prompt_correctness.py; never runs a model or tool.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};

fn send(value: Value) {
    println!("{value}");
    std::io::stdout().flush().unwrap();
}
fn response(command: &Value, data: Value) {
    send(
        json!({"type":"response","id":command["id"],"command":command["type"],"success":true,"data":data}),
    );
}
fn client(id: &str, method: &str, params: Value) {
    send(json!({"type":"client_request","requestId":id,"method":method,"params":params}));
}
fn settle(outcome: &str) {
    send(
        json!({"type":"agent_settled","runId":"correctness-run","outcome":outcome,
        "aborted":outcome == "aborted", "error": if outcome == "failed" { json!("post-insertion failure") } else { Value::Null }}),
    );
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |key: &str| args.windows(2).find(|v| v[0] == key).map(|v| v[1].clone());
    let session = arg("--session-id").expect("fixture requires a new session");
    let directory = std::path::PathBuf::from(arg("--session-dir").unwrap()).join("fixture");
    std::fs::create_dir_all(&directory).unwrap();
    let transcript = directory.join(format!("correctness_{session}.jsonl"));
    std::fs::write(&transcript, format!("{}\n", json!({"kind":"header","version":4,"id":session,"createdAt":1,"cwd":std::env::current_dir().unwrap()}))).unwrap();
    send(
        json!({"type":"ready","protocol":"rpi", "supportedCommands":["prompt","abort","get_state","client_request","get_available_models","get_modes","get_available_thinking_levels"], "supportedEvents":["message_start","agent_settled","client_request"]}),
    );
    let mut scenario = String::new();
    let mut pending: HashMap<String, Value> = HashMap::new();
    let mut diagnostic_id = 0;
    let mut kill_ack = false;
    let mut release_ack = false;
    let mut settled = false;
    let mut aborted = false;
    let mut rejected = 0;
    let mut wait_cancelled = false;
    for line in std::io::stdin().lock().lines() {
        let command: Value = serde_json::from_str(&line.unwrap()).unwrap();
        if let Ok(path) = std::env::var("RPI_PROMPT_CORRECTNESS_LOG") {
            let mut log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(log, "{command}").unwrap();
        }
        match command["type"].as_str().unwrap_or("") {
            "get_state" => response(&command, json!({"sessionId":session,"leafId":null})),
            "get_available_models" => response(&command, json!({"models":[]})),
            "get_modes" => response(&command, json!({"modes":[]})),
            "get_available_thinking_levels" => response(&command, json!({"levels":[]})),
            "prompt" => {
                scenario = command["message"].as_str().unwrap().to_owned();
                if scenario == "reject" {
                    send(
                        json!({"type":"response","id":command["id"],"command":"prompt","success":false,"error":"pre-insertion rejection"}),
                    );
                    continue;
                }
                response(&command, json!({"accepted":true}));
                let message = json!({"role":"user","id":"native-persisted-user-001","content":[{"type":"text","text":scenario}]});
                let mut history = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&transcript)
                    .unwrap();
                writeln!(history, "{}", json!({"kind":"message","id":"native-persisted-user-001","parentId":null,"message":message})).unwrap();
                send(json!({"type":"message_start","message":message}));
                if scenario == "duplicate" {
                    send(json!({"type":"message_start","message":message}));
                }
                if scenario == "cancel" {
                    client(
                        "create",
                        "terminal/create",
                        json!({"sessionId":session,"command":"offline-fixture"}),
                    );
                }
                // No settlement until an explicit diagnostic signal or cleanup acks.
            }
            "client_request" => {
                if command["params"]["path"]
                    .as_str()
                    .is_some_and(|p| p.ends_with("settle"))
                {
                    response(&command, json!({"content":"settlement signalled"}));
                    assert!(!settled);
                    settled = true;
                    settle(if scenario == "post-failure" {
                        "failed"
                    } else {
                        "completed"
                    });
                    continue;
                }
                diagnostic_id += 1;
                let id = format!("diagnostic-{diagnostic_id}");
                assert!(
                    command["correlationId"].is_string(),
                    "diagnostics need a correlationId"
                );
                pending.insert(id, command);
                // Force both same-method calls to be pending before either event.
                // A method-only subscription will therefore forward duplicates.
                if pending.len() == 2 {
                    for (request_id, original) in &pending {
                        send(json!({"type":"client_request","requestId":request_id,
                            "correlationId":original["correlationId"],
                            "method":original["method"],"params":original["params"]}));
                    }
                }
            }
            "abort" => {
                assert_eq!(scenario, "cancel");
                if aborted {
                    continue; // cancellation may be sent by multiple session owners
                }
                aborted = true;
                client(
                    "forbidden-fs",
                    "fs/read_text_file",
                    json!({"sessionId":session,"path":std::env::current_dir().unwrap().join("forbidden.txt")}),
                );
                client(
                    "forbidden-create",
                    "terminal/create",
                    json!({"sessionId":session,"command":"must-not-run"}),
                );
                client(
                    "kill",
                    "terminal/kill",
                    json!({"sessionId":session,"terminalId":"fixture-terminal"}),
                );
                response(&command, json!({}));
            }
            "client_response" => {
                let id = command["requestId"].as_str().unwrap();
                if let Some(original) = pending.remove(id) {
                    response(&original, command["result"].clone());
                } else {
                    match id {
                        "create" => {
                            assert_eq!(command["result"]["terminalId"], "fixture-terminal");
                            client(
                                "wait",
                                "terminal/wait_for_exit",
                                json!({"sessionId":session,"terminalId":"fixture-terminal"}),
                            );
                        }
                        "wait" => {
                            assert!(command["cancelled"] == true || command.get("error").is_some());
                            wait_cancelled = true;
                        }
                        "forbidden-fs" | "forbidden-create" => {
                            assert!(command["cancelled"] == true || command.get("error").is_some());
                            rejected += 1;
                        }
                        "kill" => {
                            assert!(aborted && !kill_ack && command["result"].is_object());
                            kill_ack = true;
                            client(
                                "release",
                                "terminal/release",
                                json!({"sessionId":session,"terminalId":"fixture-terminal"}),
                            );
                        }
                        "release" => {
                            assert!(kill_ack && !release_ack && command["result"].is_object());
                            release_ack = true;
                        }
                        _ => panic!("unexpected client response: {command}"),
                    }
                    if release_ack && rejected == 2 && wait_cancelled && !settled {
                        settled = true;
                        settle("aborted");
                    }
                }
            }
            "shutdown" => break,
            _ => response(&command, json!({})),
        }
    }
}
