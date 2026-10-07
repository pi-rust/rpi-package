use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
fn main() {
    let modes: Vec<String> = std::env::args().collect();
    let mode = |name: &str| modes.iter().any(|arg| arg == name);
    if mode("--no-read") {
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    }
    let mut descendant = None;
    if mode("--spawn-child") {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--no-read")
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap();
        eprintln!("descendant-pid={}", child.id());
        descendant = Some(child);
    }
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    let mut calls = 0;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            continue;
        };
        if mode("--hang")
            || mode("--spawn-child")
            || (mode("--hang-call") && method == "tools/call")
        {
            eprintln!("fake server deliberately blocked in {method}");
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
        if mode("--oversize") {
            stdout.write_all(&vec![b'x'; 16 * 1024 * 1024 + 1]).unwrap();
            stdout.flush().unwrap();
            continue;
        }
        if mode("--notify") {
            writeln!(
                stdout,
                "{}",
                json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"})
            )
            .unwrap();
            writeln!(stdout, "{}", json!({"jsonrpc":"2.0","id":999,"result":{}})).unwrap();
        }
        if mode("--ping") {
            writeln!(
                stdout,
                "{}",
                json!({"jsonrpc":"2.0","id":"server-ping","method":"ping"})
            )
            .unwrap();
            stdout.flush().unwrap();
            let mut reply = String::new();
            input.read_line(&mut reply).unwrap();
            let reply: Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(reply["id"], "server-ping");
            assert_eq!(reply["result"], json!({}));
        }
        let result = match method {
            "initialize" => {
                json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"rpi-fake-mcp","version":"1"}})
            }
            "tools/list" => {
                let echo = json!({"name":"echo","description":"Echo one value","inputSchema":{"type":"object","properties":{"value":{"type":"string"}}}});
                if mode("--paged") {
                    if request["params"]["cursor"] == "page2" {
                        json!({"tools":[{"name":"counter","inputSchema":{"type":"object"}},{"name":"fail","inputSchema":{"type":"object"}},{"name":"image","inputSchema":{"type":"object"}}]})
                    } else {
                        json!({"tools":[echo],"nextCursor":"page2"})
                    }
                } else if mode("--bad-cursor") {
                    json!({"tools":[echo],"nextCursor":"same"})
                } else {
                    json!({"tools":[echo]})
                }
            }
            "tools/call" => {
                calls += 1;
                match request["params"]["name"].as_str().unwrap_or("") {
                    "counter" => {
                        json!({"content":[{"type":"text","text":format!("{}:{calls}", std::process::id())}]})
                    }
                    "fail" => {
                        json!({"isError":true,"content":[{"type":"text","text":"intentional failure"}]})
                    }
                    "image" => {
                        json!({"content":[{"type":"image","data":"aGVsbG8=","mimeType":"image/png"}],"structuredContent":{"ok":true}})
                    }
                    _ => {
                        json!({"content":[{"type":"text","text":format!("echo:{}",request["params"]["arguments"]["value"].as_str().unwrap_or(""))}]})
                    }
                }
            }
            _ => json!({}),
        };
        if writeln!(
            stdout,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"result":result})
        )
        .and_then(|_| stdout.flush())
        .is_err()
        {
            break;
        }
    }
    if let Some(mut child) = descendant {
        let _ = child.kill();
        let _ = child.wait();
    }
}
