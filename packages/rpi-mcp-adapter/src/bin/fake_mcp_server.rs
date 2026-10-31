#![forbid(unsafe_code)]

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

// Ownership-test instrumentation is opt-in. Append-only records synchronize
// tests with real requests (including blocked background reads), not sleeps.
fn ownership_trace(modes: &[String], event: &str, details: Value) {
    if let Some(path) = modes
        .iter()
        .find_map(|arg| arg.strip_prefix("--ownership-log="))
    {
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        // Serialize before appending: formatting Value directly into the file
        // performs many writes that can interleave with the descendant's log.
        let record = format!(
            "{}\n",
            json!({"pid":std::process::id(),"event":event,"details":details})
        );
        log.write_all(record.as_bytes()).unwrap();
    }
}

fn main() {
    let modes: Vec<String> = std::env::args().collect();
    let mode = |name: &str| modes.iter().any(|arg| arg == name);
    ownership_trace(&modes, "start", json!({"leaf":mode("--no-read")}));
    if mode("--no-read") {
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    }
    let mut descendant = None;
    if mode("--spawn-child") || mode("--healthy-child") {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.arg("--no-read").stdin(std::process::Stdio::null());
        for arg in &modes {
            if arg.starts_with("--ownership-log=") {
                command.arg(arg);
            }
        }
        let child = command.spawn().unwrap();
        ownership_trace(&modes, "descendant", json!({"pid":child.id()}));
        eprintln!("descendant-pid={}", child.id());
        descendant = Some(child);
    }
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    let mut calls = 0;
    let mut delivery_value = "current content".to_string();
    let mut subscribed = std::collections::BTreeSet::new();
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
        ownership_trace(&modes, "request", json!({"method":method}));
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
                json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{},"resources":{}},"serverInfo":{"name":"rpi-fake-mcp","version":"1"}})
            }
            "resources/list" => {
                if let Some(pages) = modes.iter().find_map(|arg| {
                    arg.strip_prefix("--resource-pages=")
                        .and_then(|count| count.parse::<usize>().ok())
                }) {
                    let page = request["params"]["cursor"]
                        .as_str()
                        .unwrap_or("0")
                        .parse::<usize>()
                        .unwrap();
                    let mut result = json!({"resources":[{"uri":format!("mem://page-{page}"),"name":format!("page-{page}")}]});
                    if page + 1 < pages {
                        result["nextCursor"] = json!((page + 1).to_string());
                    }
                    result
                } else {
                    json!({"resources":[{"uri":"mem://alpha","name":"alpha"}]})
                }
            }
            "resources/read" => {
                if mode("--drop-first-resource-read") {
                    let marker = std::env::var("RPI_FAKE_RESOURCE_MARKER").unwrap();
                    if !std::path::Path::new(&marker).exists() {
                        std::fs::write(marker, "dropped").unwrap();
                        break;
                    }
                }
                if mode("--hang-read") {
                    eprintln!("fake server deliberately blocked in resources/read");
                    std::thread::sleep(std::time::Duration::from_secs(60));
                }
                let uri = request["params"]["uri"].as_str().unwrap_or("mem://alpha");
                json!({"contents":[{"uri":uri,"text":if mode("--delivery-values") { delivery_value.clone() } else { format!("value-of-{uri}") }}]})
            }
            "resources/subscribe" => {
                if mode("--reject-duplicate-subscribe")
                    && subscribed
                        .contains(request["params"]["uri"].as_str().unwrap_or("mem://alpha"))
                {
                    writeln!(stdout, "{}", json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"already subscribed"}})).unwrap();
                    stdout.flush().unwrap();
                    continue;
                }
                if mode("--reject-restored-subscribe") {
                    let marker = std::env::var("RPI_FAKE_RESOURCE_MARKER").unwrap();
                    if std::path::Path::new(&marker).exists() {
                        writeln!(stdout, "{}", json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"resource no longer exists"}})).unwrap();
                        stdout.flush().unwrap();
                        continue;
                    }
                    std::fs::write(marker, "subscribed").unwrap();
                }
                let uri = request["params"]["uri"].as_str().unwrap_or("mem://alpha");
                subscribed.insert(uri.to_string());
                // Push the first update *before* the subscribe response, which is
                // the ordering that used to lose it (the local subscription mark
                // was only written after the reply arrived).
                if mode("--notify-early") {
                    let uri = request["params"]["uri"].as_str().unwrap_or("mem://alpha");
                    writeln!(
                        stdout,
                        "{}",
                        json!({"jsonrpc":"2.0","method":"notifications/resources/updated","params":{"uri":uri}})
                    )
                    .unwrap();
                    stdout.flush().unwrap();
                }
                json!({})
            }
            "resources/unsubscribe" => {
                if mode("--reject-unsubscribe") {
                    writeln!(stdout, "{}", json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":"unsubscribe temporarily failed"}})).unwrap();
                    stdout.flush().unwrap();
                    continue;
                }
                if let Some(uri) = request["params"]["uri"].as_str() {
                    subscribed.remove(uri);
                }
                json!({})
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
                if mode("--delivery-values") {
                    delivery_value = request["params"]["arguments"]["value"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                }
                if mode("--stateful-resources") && request["params"]["arguments"]["quiet"] != true {
                    if request["params"]["arguments"]["value"] == "disconnect" {
                        break;
                    }
                    for uri in &subscribed {
                        writeln!(stdout, "{}", json!({"jsonrpc":"2.0","method":"notifications/resources/updated","params":{"uri":uri}})).unwrap();
                    }
                    stdout.flush().unwrap();
                }
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
        let result = if mode("--process-info") && method == "tools/call" {
            let mut result = result;
            result["structuredContent"] = json!({
                "pid":std::process::id(),
                "cwd":std::env::current_dir().unwrap(),
                "profile":std::env::var("RPI_OWNERSHIP_PROFILE").ok()
            });
            result
        } else {
            result
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
        // Deliberately bypass normal descendant cleanup only for this mode.
        // The leaf retains inherited stdout/stderr for at most 60 seconds;
        // runtime teardown must kill the tree even after its leader has exited.
        if mode("--exit-leader")
            && method == "tools/call"
            && request["params"]["arguments"]["value"] == "exit-leader"
        {
            ownership_trace(&modes, "leader-exit", json!({}));
            std::process::exit(0);
        }
    }
    if let Some(mut child) = descendant {
        let _ = child.kill();
        let _ = child.wait();
    }
}
