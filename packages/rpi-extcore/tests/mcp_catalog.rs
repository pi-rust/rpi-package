#![cfg(feature = "test-support")]
use rpi_extcore::{
    host::HostSession,
    mcp::{self, McpConfig, McpServerConfig, StdioServer},
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::mpsc,
};

fn mock(
    supported: bool,
) -> (
    Arc<HostSession>,
    rpi_extcore::host::TestEvents,
    mpsc::UnboundedReceiver<Value>,
) {
    let (input, output) = tokio::io::duplex(1024 * 1024);
    let commands = if supported {
        json!(["configure_external_tools", "external_tool_response"])
    } else {
        json!([])
    };
    let (host, events) = HostSession::test_connection_with_ready(
        input,
        128,
        json!({"type":"ready","protocol":"rpi","supportedCommands":commands}),
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let replies = events.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(output).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let request: Value = serde_json::from_str(&line).unwrap();
            tx.send(request.clone()).unwrap();
            let _ = replies.send(json!({"type":"response","id":request["id"],"success":true,"data":{"accepted":true}}));
        }
    });
    (host, events, rx)
}
fn config() -> McpConfig {
    McpConfig {
        scope: "fixture-session".into(),
        cwd: std::env::current_dir().unwrap(),
        discovery_timeout: Duration::from_secs(5),
        servers: BTreeMap::from([(
            "fixture".into(),
            McpServerConfig::Stdio(StdioServer {
                command: env!("CARGO_BIN_EXE_rpi-extcore-mcp-fixture").into(),
                args: vec![],
                env: BTreeMap::new(),
                cwd: None,
                enabled: true,
                timeout: 60,
                exposure: mcp::Exposure::Direct,
            }),
        )]),
    }
}
async fn next(rx: &mut mpsc::UnboundedReceiver<Value>) -> Value {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn exercise_catalog(cfg: McpConfig) {
    let (host, events, mut rx) = mock(true);
    let mut binding = mcp::attach_with_options(
        host.clone(),
        cfg,
        mcp::McpOptions {
            catalog: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let configured = next(&mut rx).await;
    assert_eq!(configured["tools"].as_array().unwrap().len(), 5);
    for (index, (operation, arguments, field)) in [
        (
            "resources_list",
            json!({"cursor":"input-cursor"}),
            "resources",
        ),
        ("resource_templates_list", json!({}), "resourceTemplates"),
        (
            "resources_read",
            json!({"uri":"fixture://text"}),
            "contents",
        ),
        ("prompts_list", json!({}), "prompts"),
        (
            "prompts_get",
            json!({"name":"fixture-prompt","arguments":{"topic":"from-model"}}),
            "messages",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let descriptor = binding
            .descriptors()
            .iter()
            .find(|d| d.remote_name == operation)
            .unwrap();
        events.send(json!({"type":"external_tool_request","requestId":format!("catalog-{index}"),"toolName":descriptor.native_name,"arguments":arguments})).unwrap();
        let response = next(&mut rx).await;
        assert_eq!(response["type"], "external_tool_response");
        assert!(response.get("error").is_none(), "{response}");
        let value: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert!(value[field].is_array(), "{value}");
        match operation {
            "resources_list" => {
                assert_eq!(value["nextCursor"], "page2");
                assert_eq!(value["_meta"]["cursor"], "input-cursor");
            }
            "resources_read" => {
                assert_eq!(value["contents"][0]["text"], "resource-value");
                assert_eq!(value["contents"][1]["blob"], "aGk=");
            }
            "prompts_get" => {
                assert_eq!(value["messages"][0]["role"], "user");
                assert_eq!(value["messages"][0]["content"]["text"], "prompt-value");
                assert_eq!(value["messages"][1]["role"], "assistant");
                assert_eq!(value["messages"][1]["content"]["data"], "aGk=");
                assert_eq!(value["_meta"]["arguments"]["topic"], "from-model");
            }
            _ => {}
        }
    }
    let descriptor = binding
        .descriptors()
        .iter()
        .find(|d| d.remote_name == "prompts_get")
        .unwrap();
    events.send(json!({"type":"external_tool_request","requestId":"bad-target","toolName":descriptor.native_name,"arguments":{"name":"p","url":"http://evil.invalid"}})).unwrap();
    assert!(next(&mut rx).await["error"]["message"]
        .as_str()
        .unwrap()
        .contains("target is locked"));
    binding.shutdown().await.unwrap();
    assert_eq!(next(&mut rx).await["tools"], json!([]));
    host.terminate().await;
}

#[tokio::test]
async fn native_external_requests_reach_real_stdio_catalog() {
    let mut cfg = config();
    if let McpServerConfig::Stdio(s) = cfg.servers.get_mut("fixture").unwrap() {
        s.env.insert("EXTCORE_CATALOG".into(), "1".into());
    }
    exercise_catalog(cfg).await;
}

#[tokio::test]
async fn opt_in_does_not_advertise_unsupported_catalog() {
    let (host, _events, mut rx) = mock(true);
    let mut binding = mcp::attach_with_options(
        host.clone(),
        config(),
        mcp::McpOptions {
            catalog: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(next(&mut rx).await["tools"].as_array().unwrap().len(), 1);
    assert_eq!(
        binding.descriptors()[0].remote_name,
        "remote.name/with punctuation"
    );
    binding.shutdown().await.unwrap();
    host.terminate().await;
}

#[tokio::test]
async fn native_external_requests_reach_real_http_catalog() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopping = stop.clone();
    let methods = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = methods.clone();
    let worker = std::thread::spawn(move || {
        while !stopping.load(std::sync::atomic::Ordering::SeqCst) {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            };
            // Windows accepted sockets inherit the listener's nonblocking mode.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut input = Vec::new();
            let mut buf = [0u8; 2048];
            let value = loop {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break Value::Null;
                }
                input.extend_from_slice(&buf[..n]);
                if let Some(end) = input.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&input[..end]).to_string();
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|n| n.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if input.len() >= end + 4 + len {
                        break serde_json::from_slice::<Value>(&input[end + 4..end + 4 + len])
                            .unwrap_or(Value::Null);
                    }
                }
            };
            if value.is_null() {
                continue;
            }
            let method = value["method"].as_str().unwrap_or("");
            recorded.lock().unwrap().push(method.to_string());
            let result = match method {
                "initialize" => {
                    json!({"protocolVersion":"2025-06-18","capabilities":{"resources":{},"prompts":{}},"serverInfo":{"name":"fixture","version":"1"}})
                }
                "resources/list" => {
                    json!({"resources":[{"uri":"fixture://text","name":"text"}],"nextCursor":"page2","_meta":{"cursor":value["params"]["cursor"]}})
                }
                "resources/templates/list" => {
                    json!({"resourceTemplates":[{"uriTemplate":"fixture://{name}","name":"template"}]})
                }
                "resources/read" => {
                    json!({"contents":[{"uri":value["params"]["uri"],"text":"resource-value"},{"uri":"fixture://image","mimeType":"image/png","blob":"aGk="}]})
                }
                "prompts/list" => json!({"prompts":[{"name":"fixture-prompt"}]}),
                "prompts/get" => {
                    json!({"messages":[{"role":"user","content":{"type":"text","text":"prompt-value"}},{"role":"assistant","content":{"type":"image","data":"aGk=","mimeType":"image/png"}}],"_meta":{"arguments":value["params"]["arguments"]}})
                }
                "tools/list" => panic!("catalog-only server must not receive tools/list"),
                _ => json!({}),
            };
            let body = json!({"jsonrpc":"2.0","id":value["id"],"result":result}).to_string();
            let code = if value.get("id").is_none() { 202 } else { 200 };
            write!(stream, "HTTP/1.1 {code} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let mut cfg = config();
    cfg.servers = BTreeMap::from([(
        "fixture".into(),
        McpServerConfig::Http(mcp::HttpServer {
            url,
            headers: BTreeMap::new(),
            enabled: true,
            timeout: 5,
            exposure: mcp::Exposure::Direct,
        }),
    )]);
    exercise_catalog(cfg).await;
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    worker.join().unwrap();
    let methods = methods.lock().unwrap();
    assert_eq!(methods.iter().filter(|m| *m == "resources/list").count(), 1);
    assert_eq!(methods.iter().filter(|m| *m == "prompts/get").count(), 1);
}
