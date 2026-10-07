//! Real loopback HTTP fixtures exercise headers, initialization, SSE and ABI results.
use crate::{mcp_call, mcp_list, Control};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

struct Server {
    url: String,
    seen: Arc<Mutex<Vec<(String, String, Value)>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Server {
    fn start(
        handler: impl Fn(&str, &str, &Value) -> (u16, String, Vec<u8>) + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = std::thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // The listener polls nonblocking; accepted sockets must
                        // block while the request headers arrive (Windows can
                        // inherit the listener mode on the accepted socket).
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        let mut head = String::new();
                        if reader.read_line(&mut head).unwrap_or(0) == 0 {
                            continue;
                        }
                        let mut headers = String::new();
                        let mut length = 0;
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                                break;
                            }
                            if let Some(value) =
                                line.to_ascii_lowercase().strip_prefix("content-length:")
                            {
                                length = value.trim().parse().unwrap();
                            }
                            headers.push_str(&line.to_ascii_lowercase());
                        }
                        let mut bytes = vec![0; length];
                        if reader.read_exact(&mut bytes).is_err() {
                            continue;
                        }
                        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                        record
                            .lock()
                            .unwrap()
                            .push((head.clone(), headers.clone(), value.clone()));
                        let (status, extra, body) = handler(&head, &headers, &value);
                        let header = format!("HTTP/1.1 {status} fixture\r\nConnection: close\r\nContent-Length: {}\r\n{extra}\r\n", body.len());
                        let _ = stream.write_all(header.as_bytes());
                        for chunk in body.chunks(7) {
                            if stream.write_all(chunk).is_err() {
                                break;
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            url,
            seen,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
fn managed_fixture(sse: bool) -> Server {
    let counter = Mutex::new(0);
    Server::start(move |head, headers, request| {
        if head.starts_with("DELETE") {
            assert!(headers.contains("mcp-session-id: fixture-session"));
            return (200, String::new(), vec![]);
        }
        assert!(headers.contains("accept: application/json, text/event-stream"));
        let method = request["method"].as_str().unwrap_or("");
        if method != "initialize" {
            assert!(
                headers.contains("mcp-session-id: fixture-session"),
                "{headers}"
            );
            assert!(headers.contains("mcp-protocol-version: 2025-06-18"));
        }
        if method == "notifications/initialized" || method == "notifications/cancelled" {
            return (202, String::new(), vec![]);
        }
        if method.is_empty() {
            // Reply to a server-initiated ping.
            assert_eq!(request["id"], "server-ping");
            assert_eq!(request["result"], json!({}));
            return (202, String::new(), vec![]);
        }
        let result = match method {
            "initialize" => {
                json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
            }
            "tools/list" => {
                json!({"tools":[{"name":"image","inputSchema":{"type":"object"}},{"name":"fail","inputSchema":{"type":"object"}}]})
            }
            "tools/call" => {
                let mut counter = counter.lock().unwrap();
                *counter += 1;
                if request["params"]["name"] == "fail" {
                    json!({"isError":true,"content":[{"type":"text","text":"failed intentionally"}]})
                } else {
                    json!({"content":[{"type":"image","data":"aGVsbG8=","mimeType":"image/png"}],"structuredContent":{"count":*counter}})
                }
            }
            _ => panic!("unexpected method {method}"),
        };
        let response = json!({"jsonrpc":"2.0","id":request["id"],"result":result});
        let session = if method == "initialize" {
            "Mcp-Session-Id: fixture-session\r\n"
        } else {
            ""
        };
        if sse {
            let ping = if method == "tools/list" {
                "data: {\"jsonrpc\":\"2.0\",\"id\":\"server-ping\",\"method\":\"ping\"}\r\n\r\n"
            } else {
                ""
            };
            (200, format!("{session}Content-Type: text/event-stream\r\n"),
                format!(": keepalive\r\n\r\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{{}}}}\r\n\r\n{ping}data: {response}\r\n\r\n").into_bytes())
        } else {
            (
                200,
                format!("{session}Content-Type: application/json\r\n"),
                response.to_string().into_bytes(),
            )
        }
    })
}
fn managed_flow(sse: bool) {
    let server = managed_fixture(sse);
    let params = json!({"url":server.url,"tool":"image","arguments":{}});
    let first = mcp_call(&params, &Control::default()).unwrap();
    assert_eq!(first["content"][0]["type"], "image");
    assert_eq!(first["content"][0]["data"], "aGVsbG8=");
    assert_eq!(
        first["details"]["mcpResult"]["structuredContent"]["count"],
        1
    );
    let second = mcp_call(&params, &Control::default()).unwrap();
    assert_eq!(
        second["details"]["mcpResult"]["structuredContent"]["count"],
        2
    );
    let error = mcp_call(
        &json!({"url":server.url,"tool":"fail"}),
        &Control::default(),
    )
    .unwrap_err();
    assert!(
        error.contains("failed intentionally"),
        "{error}; seen={:?}",
        server.seen.lock().unwrap()
    );
    let discovery = mcp_list(&json!({"url":server.url}), &Control::default())
        .unwrap_or_else(|e| panic!("{e}; seen={:?}", server.seen.lock().unwrap()));
    let discovery: Value =
        serde_json::from_str(discovery["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        discovery["tools"][0]["inputSchema"],
        json!({"type":"object"})
    );
    mcp_list(
        &json!({"url":server.url,"action":"close"}),
        &Control::default(),
    )
    .unwrap();
    let seen = server.seen.lock().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|(_, _, v)| v["method"] == "initialize")
            .count(),
        1
    );
    assert_eq!(
        seen.iter()
            .filter(|(_, _, v)| v["method"] == "notifications/initialized")
            .count(),
        1
    );
    assert!(seen.iter().any(|(head, _, _)| head.starts_with("DELETE")));
}
#[test]
fn managed_json_lifecycle_and_native_images() {
    managed_flow(false);
}
#[test]
fn managed_sse_lifecycle_ping_and_native_images() {
    managed_flow(true);
}

#[test]
fn http_cancellation_interrupts_waiting_for_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let control = Control::default();
    let cancel = control.clone();
    let worker = std::thread::spawn(move || {
        crate::mcp_request(
            &json!({"url":url,"method":"tools/list","timeoutSeconds":60}),
            &control,
        )
    });
    let (_stream, _) = listener.accept().unwrap();
    // No response headers; the TCP connection stays open.
    let start = Instant::now();
    cancel.cancel();
    assert!(worker.join().unwrap().unwrap_err().contains("cancelled"));
    assert!(start.elapsed() < Duration::from_secs(1));
}
#[test]
fn mismatched_json_response_id_is_rejected() {
    let server = Server::start(|_, _, _| {
        (
            200,
            "Content-Type: application/json\r\n".into(),
            br#"{"jsonrpc":"2.0","id":999,"result":{}}"#.to_vec(),
        )
    });
    assert!(crate::mcp_request(
        &json!({"url":server.url,"method":"test"}),
        &Control::default()
    )
    .unwrap_err()
    .contains("ID"));
}
#[test]
fn expired_session_is_reinitialized_without_replaying_tool() {
    let initializes = Mutex::new(0);
    let calls = Mutex::new(0);
    let server = Server::start(move |head, _, request| {
        if head.starts_with("DELETE") || request["method"] == "notifications/initialized" {
            return (202, String::new(), vec![]);
        }
        let method = request["method"].as_str().unwrap();
        let result = match method {
            "initialize" => {
                *initializes.lock().unwrap() += 1;
                json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
            }
            "tools/list" => json!({"tools":[{"name":"tool","inputSchema":{"type":"object"}}]}),
            "tools/call" => {
                let mut calls = calls.lock().unwrap();
                *calls += 1;
                if *calls == 1 {
                    return (404, String::new(), vec![]);
                }
                json!({"content":[{"type":"text","text":"ok"}]})
            }
            _ => panic!("{method}"),
        };
        let session = if method == "initialize" {
            "Mcp-Session-Id: expiring\r\n"
        } else {
            ""
        };
        (
            200,
            format!("{session}Content-Type: application/json\r\n"),
            json!({"jsonrpc":"2.0","id":request["id"],"result":result})
                .to_string()
                .into_bytes(),
        )
    });
    let params = json!({"url":server.url,"tool":"tool"});
    assert!(mcp_call(&params, &Control::default())
        .unwrap_err()
        .contains("expired"));
    assert!(mcp_call(&params, &Control::default()).is_ok());
    mcp_list(
        &json!({"url":server.url,"action":"close"}),
        &Control::default(),
    )
    .unwrap();
    let seen = server.seen.lock().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|(_, _, v)| v["method"] == "tools/call")
            .count(),
        2
    );
    assert_eq!(
        seen.iter()
            .filter(|(_, _, v)| v["method"] == "initialize")
            .count(),
        2
    );
}
