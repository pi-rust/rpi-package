use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn main() {
    let capture = std::env::args().nth(1).unwrap();
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(capture)
        .unwrap();
    let mut pending: Option<Value> = None;
    let mut advertised = Value::Null;
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        writeln!(log, "{line}").unwrap();
        log.flush().unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        let Some(id) = request.get("id") else {
            continue;
        };
        if request.get("method").is_none() {
            if request["id"] == "ping-probe" { continue; }
            let parent = pending.take().unwrap();
            println!("{}", json!({"jsonrpc":"2.0","id":parent,"result":{"content":[{"type":"text","text":json!({"callback":request,"advertised":advertised}).to_string()}]}}));
            std::io::stdout().flush().unwrap();
            continue;
        }
        let params = &request["params"];
        let result = match request["method"].as_str().unwrap() {
            "initialize" => {
                advertised = params["capabilities"].clone();
                json!({"protocolVersion":"2025-06-18", "capabilities":{"prompts":{},"resources":{}},"serverInfo":{"name":"fixture","version":"1"}})
            }
            "tools/list" => json!({"tools":(["success","context","malformed","cancel"].map(|name|json!({"name":name,"inputSchema":{"type":"object"}})))}),
            "tools/call" => {
                pending = Some(id.clone());
                let scenario = params["name"].as_str().unwrap();
                let mut sample = json!({"messages":[{"role":"user","content":{"type":"text","text":"PRIVATE-REMOTE-PAYLOAD"}}],"maxTokens":20});
                if scenario == "context" { sample["includeContext"] = json!("allServers"); }
                let callback_id = if scenario == "malformed" { json!({"bad":"id"}) } else { json!("sample-01") };
                println!("{}", json!({"jsonrpc":"2.0","id":callback_id,"method":"sampling/createMessage","params":sample}));
                println!("{}", json!({"jsonrpc":"2.0","id":"ping-probe","method":"ping"}));
                if scenario == "cancel" {
                    println!("{}", json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"sample-01"}}));
                }
                std::io::stdout().flush().unwrap();
                continue;
            }
            "prompts/list" => match params["cursor"].as_str() {
                Some("cycle") => json!({"prompts":[],"nextCursor":"cycle"}),
                Some("loop-a") => json!({"prompts":[],"nextCursor":"loop-b"}),
                Some("loop-b") => json!({"prompts":[],"nextCursor":"loop-a"}),
                Some("end") => json!({"prompts":[{"name":"second"}],"_meta":{"page":2}}),
                Some("many") => json!({"prompts": vec![json!({"name":"p"}); 16_385]}),
                Some(cursor) if cursor.starts_with("page-") => {
                    let page: u64 = cursor[5..].parse().unwrap();
                    json!({"prompts":[],"nextCursor":format!("page-{}",page+1)})
                }
                _ => {
                    json!({"prompts":[{"name":"first","title":"Title","arguments":[{"name":"topic","required":true,"custom":9}],"_meta":{"x":1}}],"nextCursor":"end","extension":42})
                }
            },
            "resources/templates/list" => {
                json!({"resourceTemplates":[{"uriTemplate":"mem://{name}","name":"memory","mimeType":"text/plain","annotations":{"priority":0.5},"_meta":{"hint":true}}]})
            }
            "prompts/get" => {
                match params["name"].as_str().unwrap() {
                    "eof" => return,
                    "wait" => {
                        std::thread::sleep(std::time::Duration::from_secs(60));
                    }
                    _ => {}
                }
                json!({"description":"structured", "messages":[
                    {"role":"user","content":{"type":"image","data":"QUJD","mimeType":"image/png","annotations":{"audience":["user"]}}},
                    {"role":"assistant","content":{"type":"resource","resource":{"uri":"mem://x","blob":"QUJD","custom":7}},"custom":1},
                    {"role":"user","content":{"type":"future","opaque":[1,2]}}
                ],"_meta":{"modelHints":[{"name":"model"}]},"echo":params,"pid":std::process::id(),"cwd":std::env::current_dir().unwrap()})
            }
            _ => panic!("unexpected protocol request: {request}"),
        };
        println!("{}", json!({"jsonrpc":"2.0","id":id,"result":result}));
        std::io::stdout().flush().unwrap();
    }
}
