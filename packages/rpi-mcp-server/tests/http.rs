//! Loopback tests for the Streamable HTTP and WebSocket transports.

use rpi_extcore::host::HostSession;
use rpi_mcp_server::http::{serve_listener, serve_listener_until, HttpOptions, SessionFactory};
use rpi_mcp_server::McpServer;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

const TOKEN: &str = "test-token";

/// Native commands the scripted host received, for lifecycle assertions.
type CommandLog = Arc<Mutex<Vec<Value>>>;

/// Factory that records what the server writes to the native child.
fn recording_factory(commands: CommandLog) -> SessionFactory {
    Arc::new(move || {
        let commands = Arc::clone(&commands);
        Box::pin(async move {
            let (host_stdin, host_commands) = tokio::io::duplex(64 * 1024);
            let (session, _events) = HostSession::test_connection_with_ready(
                host_stdin,
                64,
                json!({
                    "type": "ready",
                    "protocol": "rpi",
                    "supportedCommands": ["get_state", "get_messages", "compact"],
                }),
            );
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let mut lines = tokio::io::BufReader::new(host_commands).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(value) = serde_json::from_str::<Value>(&line) {
                        commands.lock().unwrap().push(value);
                    }
                }
            });
            Ok(McpServer::from_session(session))
        })
    })
}

/// Start a loopback listener backed by a scripted native host.
async fn start(token: Option<&str>) -> String {
    let factory = recording_factory(Arc::new(Mutex::new(Vec::new())));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let mut options = HttpOptions::new(address.to_string());
    options.token = token.map(str::to_string);
    tokio::spawn(async move {
        let _ = serve_listener(listener, options, factory).await;
    });
    format!("http://{address}")
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .expect("client")
}

fn initialize_body(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {"protocolVersion": "2025-06-18"},
    })
}

async fn open_session(base: &str) -> String {
    let response = client()
        .post(format!("{base}/mcp"))
        .json(&initialize_body(1))
        .send()
        .await
        .expect("initialize");
    assert_eq!(response.status(), 200);
    response
        .headers()
        .get("mcp-session-id")
        .expect("session id header")
        .to_str()
        .unwrap()
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_cap_and_graceful_shutdown() {
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let factory = recording_factory(Arc::new(Mutex::new(Vec::new())));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let mut options = HttpOptions::new(address.to_string());
    options.max_sessions = 1;
    let serving = tokio::spawn(async move {
        serve_listener_until(listener, options, factory, async move {
            let _ = shutdown_rx.await;
        })
        .await
    });
    let base = format!("http://{address}");

    open_session(&base).await;
    let status = client()
        .post(format!("{base}/mcp"))
        .json(&initialize_body(2))
        .send()
        .await
        .expect("second session")
        .status();
    assert_eq!(status, 429);

    shutdown.send(()).expect("signal shutdown");
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), serving)
        .await
        .expect("shutdown is bounded")
        .expect("listener task");
    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reinitializing_an_existing_session_is_rejected() {
    let base = start(None).await;
    let session = open_session(&base).await;
    let response = client()
        .post(format!("{base}/mcp"))
        .header("mcp-session-id", &session)
        .json(&initialize_body(2))
        .send()
        .await
        .expect("re-initialize");
    assert_eq!(response.status(), 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cors_preflight_only_allows_known_origins() {
    let base = start(None).await;
    let local = client()
        .request(reqwest::Method::OPTIONS, format!("{base}/mcp"))
        .header("origin", "http://localhost:5173")
        .header("access-control-request-method", "POST")
        .send()
        .await
        .expect("preflight");
    assert_eq!(local.status(), 204);
    assert_eq!(
        local
            .headers()
            .get("access-control-allow-origin")
            .unwrap()
            .to_str()
            .unwrap(),
        "http://localhost:5173"
    );
    assert!(local
        .headers()
        .get("access-control-expose-headers")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("mcp-session-id"));

    let foreign = client()
        .request(reqwest::Method::OPTIONS, format!("{base}/mcp"))
        .header("origin", "https://evil.example")
        .send()
        .await
        .expect("preflight");
    assert_eq!(foreign.status(), 403);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_reaches_the_native_host() {
    let commands: CommandLog = Arc::new(Mutex::new(Vec::new()));
    let factory = recording_factory(Arc::clone(&commands));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        let _ = serve_listener(listener, HttpOptions::new(address.to_string()), factory).await;
    });
    let base = format!("http://{address}");
    let session = open_session(&base).await;

    // The prompt never settles: this checks the cancellation path, not a run.
    let prompt = {
        let base = base.clone();
        let session = session.clone();
        tokio::spawn(async move {
            let _ = client()
                .post(format!("{base}/mcp"))
                .header("mcp-session-id", &session)
                .json(&json!({
                    "jsonrpc":"2.0",
                    "id":2,
                    "method":"tools/call",
                    "params":{"name":"rpi_prompt","arguments":{"message":"long run"}},
                }))
                .send()
                .await;
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let status = client()
        .post(format!("{base}/mcp"))
        .header("mcp-session-id", &session)
        .json(&json!({
            "jsonrpc":"2.0",
            "method":"notifications/cancelled",
            "params":{"requestId":2,"reason":"test"},
        }))
        .send()
        .await
        .expect("cancel")
        .status();
    assert_eq!(status, 202);

    let mut aborted = false;
    for _ in 0..40 {
        if commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| command["type"] == "abort")
        {
            aborted = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(aborted, "{commands:#?}");
    prompt.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creates_a_session_and_routes_requests() {
    let base = start(None).await;
    let session = open_session(&base).await;

    let body: Value = client()
        .post(format!("{base}/mcp"))
        .header("mcp-session-id", &session)
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .send()
        .await
        .expect("tools/list")
        .json()
        .await
        .expect("json body");
    assert_eq!(body["id"], 2);
    assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 8);

    // Without a session id there is no native session to route to.
    let status = client()
        .post(format!("{base}/mcp"))
        .json(&json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}))
        .send()
        .await
        .expect("missing session")
        .status();
    assert_eq!(status, 400);

    // Unknown sessions expire into 404, per the Streamable HTTP contract.
    let status = client()
        .post(format!("{base}/mcp"))
        .header("mcp-session-id", "does-not-exist")
        .json(&json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}))
        .send()
        .await
        .expect("unknown session")
        .status();
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notifications_are_accepted_without_a_body() {
    let base = start(None).await;
    let session = open_session(&base).await;
    let response = client()
        .post(format!("{base}/mcp"))
        .header("mcp-session-id", &session)
        .json(&json!({
            "jsonrpc":"2.0",
            "method":"notifications/cancelled",
            "params":{"requestId":99,"reason":"test"},
        }))
        .send()
        .await
        .expect("notification");
    assert_eq!(response.status(), 202);
    assert!(response.bytes().await.expect("body").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_retires_the_session_and_get_is_not_offered() {
    let base = start(None).await;
    let session = open_session(&base).await;

    let response = client()
        .get(format!("{base}/mcp"))
        .header("mcp-session-id", &session)
        .send()
        .await
        .expect("get");
    assert_eq!(response.status(), 405);
    assert_eq!(
        response.headers().get("allow").unwrap().to_str().unwrap(),
        "POST, DELETE"
    );

    let status = client()
        .delete(format!("{base}/mcp"))
        .header("mcp-session-id", &session)
        .send()
        .await
        .expect("delete")
        .status();
    assert_eq!(status, 204);

    let status = client()
        .post(format!("{base}/mcp"))
        .header("mcp-session-id", &session)
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .send()
        .await
        .expect("after delete")
        .status();
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enforces_token_origin_and_protocol_version() {
    let base = start(Some(TOKEN)).await;

    let status = client()
        .post(format!("{base}/mcp"))
        .json(&initialize_body(1))
        .send()
        .await
        .expect("no token")
        .status();
    assert_eq!(status, 401);

    let response = client()
        .post(format!("{base}/mcp"))
        .header("authorization", format!("Bearer {TOKEN}"))
        .json(&initialize_body(2))
        .send()
        .await
        .expect("token");
    assert_eq!(response.status(), 200);
    let session = response
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let status = client()
        .post(format!("{base}/mcp"))
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("origin", "https://evil.example")
        .json(&json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}))
        .send()
        .await
        .expect("bad origin")
        .status();
    assert_eq!(status, 403);

    let status = client()
        .post(format!("{base}/mcp"))
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("mcp-session-id", &session)
        .header("mcp-protocol-version", "1999-01-01")
        .json(&json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}))
        .send()
        .await
        .expect("bad version")
        .status();
    assert_eq!(status, 400);

    let body: Value = client()
        .post(format!("{base}/mcp"))
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("mcp-session-id", &session)
        .header("mcp-protocol-version", "2025-06-18")
        .header("origin", "http://localhost:5173")
        .json(&json!({"jsonrpc":"2.0","id":5,"method":"tools/list"}))
        .send()
        .await
        .expect("allowed")
        .json()
        .await
        .expect("json body");
    assert_eq!(body["id"], 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_calls_stream_over_sse_when_the_client_accepts_it() {
    let base = start(None).await;
    let session = open_session(&base).await;
    let response = client()
        .post(format!("{base}/mcp"))
        .header("mcp-session-id", &session)
        .header("accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"tools/call",
            "params":{"name":"rpi_prompt","arguments":{"message":"hi"}},
        }))
        .send()
        .await
        .expect("prompt");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "text/event-stream"
    );
    // The scripted host never settles this prompt, so the stream stays open:
    // receiving SSE headers already proves the streaming path is taken.
}

mod websocket {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    /// Build the socket URL from the HTTP base plus an explicit path/query.
    async fn connect(
        base: &str,
        suffix: &str,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let url = format!("ws://{}{suffix}", base.trim_start_matches("http://"));
        let (socket, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("websocket upgrade");
        socket
    }

    async fn next_json(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Value {
        loop {
            let message = tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
                .await
                .expect("frame timeout")
                .expect("socket closed")
                .expect("frame");
            match message {
                Message::Text(text) => {
                    return serde_json::from_str(&text).expect("json frame");
                }
                Message::Close(_) => panic!("closed before a frame arrived"),
                _ => continue,
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn serves_the_same_session_over_a_websocket() {
        let base = start(None).await;
        let mut socket = connect(&base, "/mcp/ws").await;

        // The first frame announces the private session handle.
        let hello = next_json(&mut socket).await;
        assert_eq!(hello["method"], "rpi/session");
        let session = hello["params"]["sessionId"].as_str().unwrap().to_string();

        socket
            .send(Message::Text(initialize_body(1).to_string().into()))
            .await
            .expect("initialize");
        let response = next_json(&mut socket).await;
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["protocolVersion"], "2025-06-18");

        socket
            .send(Message::Text(
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("tools/list");
        let response = next_json(&mut socket).await;
        assert_eq!(response["id"], 2);
        assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 8);

        // A second socket can reattach to the same native session.
        let mut second = connect(&base, &format!("/mcp/ws?session={session}")).await;
        let hello = next_json(&mut second).await;
        assert_eq!(hello["params"]["sessionId"], session);
        second
            .send(Message::Text(
                json!({"jsonrpc":"2.0","id":3,"method":"tools/list"})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("tools/list on the reattached socket");
        let response = next_json(&mut second).await;
        assert_eq!(response["id"], 3);

        let _ = socket.close(None).await;
        let _ = second.close(None).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejects_unknown_sessions_and_bad_tokens() {
        let base = start(Some(TOKEN)).await;
        let url = format!(
            "{}/mcp/ws?session=nope",
            base.replacen("http://", "ws://", 1)
        );
        // Unknown session: the upgrade is refused before any frame is served.
        assert!(tokio_tungstenite::connect_async(url).await.is_err());

        let base = start(None).await;
        let url = format!(
            "{}/mcp/ws?session=nope",
            base.replacen("http://", "ws://", 1)
        );
        assert!(tokio_tungstenite::connect_async(url).await.is_err());
    }
}
