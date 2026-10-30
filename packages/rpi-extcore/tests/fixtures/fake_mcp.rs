//! Real stdio peer used only with the test-support feature.
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
fn main() {
    for line in io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let Some(id) = request.get("id") else {
            continue;
        };
        let result = match request["method"].as_str().unwrap_or("") {
            "initialize" => {
                let capabilities = if std::env::var_os("EXTCORE_CATALOG").is_some() {
                    json!({"resources":{},"prompts":{}})
                } else {
                    json!({"tools":{}})
                };
                json!({"protocolVersion":"2025-06-18","capabilities":capabilities,"serverInfo":{"name":"extcore-fixture","version":"1"}})
            }
            "tools/list" => {
                json!({"tools":[{"name":"remote.name/with punctuation","description":"fixture","inputSchema":{"type":"object"}}]})
            }
            "tools/call" => {
                if request["params"]["arguments"]["hang"] == true {
                    std::thread::sleep(std::time::Duration::from_secs(120));
                }
                json!({"isError":request["params"]["arguments"]["fail"] == true,"content":[{"type":"text","text":request["params"]["name"]},{"type":"image","data":"aGk=","mimeType":"image/png"}],"structuredContent":{"pid":std::process::id(),"arguments":request["params"]["arguments"]}})
            }
            "resources/list" => {
                json!({"resources":[{"uri":"fixture://text","name":"text"}],"nextCursor":"page2","_meta":{"cursor":request["params"]["cursor"]}})
            }
            "resources/templates/list" => {
                json!({"resourceTemplates":[{"uriTemplate":"fixture://{name}","name":"template"}]})
            }
            "resources/read" => {
                json!({"contents":[{"uri":request["params"]["uri"],"text":"resource-value"},{"uri":"fixture://image","mimeType":"image/png","blob":"aGk="}]})
            }
            "prompts/list" => {
                json!({"prompts":[{"name":"fixture-prompt","arguments":[{"name":"topic"}]}]})
            }
            "prompts/get" => {
                json!({"description":"untrusted","messages":[{"role":"user","content":{"type":"text","text":"prompt-value"}},{"role":"assistant","content":{"type":"image","data":"aGk=","mimeType":"image/png"}}],"_meta":{"arguments":request["params"]["arguments"]}})
            }
            _ => json!({}),
        };
        println!("{}", json!({"jsonrpc":"2.0","id":id,"result":result}));
        io::stdout().flush().unwrap();
    }
}
