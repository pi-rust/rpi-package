use std::io::{self, BufRead, Write};

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(request) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let result = match method {
            "initialize" => serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "rpi-fake-mcp", "version": "1"}
            }),
            "tools/list" => serde_json::json!({
                "tools": [{
                    "name": "echo",
                    "description": "Echo one value",
                    "inputSchema": {"type":"object", "properties":{"value":{"type":"string"}}}
                }]
            }),
            "tools/call" => {
                let value = request
                    .get("params")
                    .and_then(|params| params.get("arguments"))
                    .and_then(|args| args.get("value"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                serde_json::json!({"content":[{"type":"text","text":format!("echo:{value}")}]})
            }
            _ => serde_json::json!({"ok": true}),
        };
        let response = serde_json::json!({"jsonrpc":"2.0", "id":id, "result":result});
        if writeln!(stdout, "{}", response).is_err() {
            break;
        }
        if stdout.flush().is_err() {
            break;
        }
    }
}
