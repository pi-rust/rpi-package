use futures_util::{SinkExt, StreamExt};
use rpi_acp::transport::{self, Config};
use serde_json::{json, Value};
use std::{net::SocketAddr, time::Duration};
use tokio::{net::TcpListener, sync::watch};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
};

const TOKEN: &str = "remote-test-secret-0123456789abcdef";
type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn start(
    origin: Option<String>,
) -> (
    SocketAddr,
    watch::Sender<bool>,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(transport::serve(
        listener,
        Config::new(TOKEN, origin).unwrap(),
        rx,
    ));
    (address, stop, task)
}

fn request(
    address: SocketAddr,
    token: Option<&str>,
    origin: Option<&str>,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    let mut request = format!("ws://{address}/acp").into_client_request().unwrap();
    if let Some(token) = token {
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
    }
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
    }
    request
}

async fn receive(socket: &mut Socket) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            Message::Text(text) => {
                assert!(
                    !text.ends_with('\n'),
                    "WebSocket framing must not expose stdio newline"
                );
                return serde_json::from_str(&text).unwrap();
            }
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

async fn send(socket: &mut Socket, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}
async fn initialize(socket: &mut Socket) {
    send(socket, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}})).await;
    let reply = receive(socket).await;
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["result"]["protocolVersion"], 1, "{reply}");
}
async fn closed(socket: &mut Socket) {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .unwrap()
        {
            None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
            _ => {}
        }
    }
}

#[tokio::test]
async fn auth_origin_host_and_connection_identity() {
    assert!(Config::new("short", None).is_err());
    assert!(Config::new(TOKEN, Some("*".into())).is_err());
    let (address, stop, task) = start(Some("https://editor.example".into())).await;
    for (token, origin, status) in [
        (None, None, 401),
        (Some("wrong"), None, 401),
        (Some(TOKEN), Some("null"), 403),
        (Some(TOKEN), Some("https://evil.example"), 403),
    ] {
        let error = connect_async(request(address, token, origin))
            .await
            .unwrap_err();
        match error {
            tokio_tungstenite::tungstenite::Error::Http(response) => {
                assert_eq!(response.status().as_u16(), status)
            }
            other => panic!("{other}"),
        }
    }
    let mut bad_host = request(address, Some(TOKEN), None);
    bad_host
        .headers_mut()
        .insert("host", "evil.example".parse().unwrap());
    assert!(
        matches!(connect_async(bad_host).await, Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == 403)
    );
    let mut duplicate = request(address, Some(TOKEN), None);
    duplicate
        .headers_mut()
        .append("authorization", format!("Bearer {TOKEN}").parse().unwrap());
    assert!(
        matches!(connect_async(duplicate).await, Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == 401)
    );
    let (mut socket, response) = connect_async(request(
        address,
        Some(TOKEN),
        Some("https://editor.example"),
    ))
    .await
    .unwrap();
    uuid::Uuid::parse_str(response.headers()["acp-connection-id"].to_str().unwrap()).unwrap();
    socket
        .send(Message::Binary(vec![0, 1, 2].into()))
        .await
        .unwrap();
    initialize(&mut socket).await;
    stop.send(true).unwrap();
    closed(&mut socket).await;
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn framing_initialize_first_limits_and_eof() {
    let (address, stop, task) = start(None).await;
    // Browser Origins denied by default, even with a correct secret.
    assert!(
        matches!(connect_async(request(address, Some(TOKEN), Some("http://localhost"))).await, Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == 403)
    );
    for text in [
        "[]",
        "{",
        "{\"jsonrpc\":\"2.0\",\"method\":\"session/new\",\"id\":2}",
    ] {
        let (mut socket, _) = connect_async(request(address, Some(TOKEN), None))
            .await
            .unwrap();
        socket.send(Message::Text(text.into())).await.unwrap();
        closed(&mut socket).await;
    }
    let (mut socket, _) = connect_async(request(address, Some(TOKEN), None))
        .await
        .unwrap();
    initialize(&mut socket).await;
    socket
        .send(Message::Text("x".repeat(1024 * 1024 + 1).into()))
        .await
        .unwrap();
    closed(&mut socket).await;
    // Drop without WebSocket close exercises raw socket EOF cleanup.
    let (mut socket, _) = connect_async(request(address, Some(TOKEN), None))
        .await
        .unwrap();
    initialize(&mut socket).await;
    drop(socket);
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn connection_and_message_budgets_are_bounded() {
    let (address, stop, task) = start(None).await;
    let mut sockets = Vec::new();
    for _ in 0..16 {
        sockets.push(
            connect_async(request(address, Some(TOKEN), None))
                .await
                .unwrap()
                .0,
        );
    }
    assert!(
        matches!(connect_async(request(address, Some(TOKEN), None)).await, Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == 503)
    );
    for socket in &mut sockets {
        initialize(socket).await;
    }
    // Sequential writes avoid testing queue overload instead of the total budget.
    for id in 2..=256 {
        send(
            &mut sockets[0],
            json!({"jsonrpc":"2.0","id":id,"method":"unknown","params":{}}),
        )
        .await;
        assert_eq!(receive(&mut sockets[0]).await["id"], id);
    }
    send(
        &mut sockets[0],
        json!({"jsonrpc":"2.0","id":257,"method":"unknown"}),
    )
    .await;
    closed(&mut sockets[0]).await;
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// Run in an isolated bridge process, so fake native and history environment do
/// not race any other package test. Build with `cargo build -p rpi-acp --examples`.
#[tokio::test]
async fn fake_native_stream_cancel_disconnect_and_reload() {
    let bridge = std::path::PathBuf::from(env!("CARGO_BIN_EXE_rpi-acp-bridge"));
    let fake = bridge
        .parent()
        .unwrap()
        .join("examples")
        .join(format!("remote_rpc{}", std::env::consts::EXE_SUFFIX));
    assert!(
        fake.exists(),
        "first build: cargo build -p rpi-acp --example remote_rpc"
    );
    let directory = std::env::temp_dir().join(format!("rpi-remote-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let pids = directory.join("pids");
    let mut child = tokio::process::Command::new(bridge)
        .args(["--experimental-remote", "--bind", &address.to_string()])
        .env("RPI_ACP_REMOTE_TOKEN", TOKEN)
        .env("RPI_BIN", fake)
        .env("RPI_ACP_SESSION_DIR", directory.join("history"))
        .env("RPI_REMOTE_TEST_PIDS", &pids)
        .env("RPI_CODING_AGENT_DIR", &directory)
        .env("RPI_ACP_NO_AUTO_REGISTER_ZED", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut socket = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok((socket, _)) = connect_async(request(address, Some(TOKEN), None)).await {
                break socket;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    initialize(&mut socket).await;
    send(&mut socket, json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":directory,"mcpServers":[]}})).await;
    let reply = receive(&mut socket).await;
    let session = reply["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{reply}"))
        .to_owned();
    for (id, text, reason) in [(3, "complete", "end_turn"), (4, "wait", "cancelled")] {
        send(&mut socket, json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":{"sessionId":session,"prompt":[{"type":"text","text":text}]}})).await;
        let mut streamed = false;
        loop {
            let reply = receive(&mut socket).await;
            if reply["method"] == "session/update"
                && reply["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
            {
                assert_eq!(reply["params"]["update"]["content"]["text"], "remote🌊");
                streamed = true;
                if text == "wait" {
                    send(&mut socket, json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}})).await;
                }
            }
            if reply["id"] == id {
                assert!(streamed);
                assert_eq!(reply["result"]["stopReason"], reason, "{reply}");
                break;
            }
        }
    }
    send(&mut socket, json!({"jsonrpc":"2.0","id":5,"method":"session/prompt","params":{"sessionId":session,"prompt":[{"type":"text","text":"wait"}]}})).await;
    loop {
        if receive(&mut socket).await["method"] == "session/update" {
            break;
        }
    }
    drop(socket); // active prompt EOF must terminate native child, not just idle EOF
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let ids = std::fs::read_to_string(&pids).unwrap();
            if ids.lines().all(|id| !process_alive(id)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("native child leaked on EOF");
    let (mut socket, _) = connect_async(request(address, Some(TOKEN), None))
        .await
        .unwrap();
    initialize(&mut socket).await;
    send(&mut socket, json!({"jsonrpc":"2.0","id":6,"method":"session/load","params":{"sessionId":session,"cwd":directory,"mcpServers":[]}})).await;
    let mut replay = Vec::new();
    loop {
        let reply = receive(&mut socket).await;
        if reply["method"] == "session/update" {
            let update = &reply["params"]["update"];
            if matches!(
                update["sessionUpdate"].as_str(),
                Some("user_message_chunk" | "agent_message_chunk")
            ) {
                replay.push((
                    update["sessionUpdate"].as_str().unwrap().to_owned(),
                    update["content"]["text"].as_str().unwrap().to_owned(),
                ));
            }
        }
        if reply["id"] == 6 {
            assert!(reply.get("result").is_some(), "{reply}");
            break;
        }
    }
    assert_eq!(
        replay,
        vec![
            ("user_message_chunk".into(), "complete".into()),
            ("agent_message_chunk".into(), "remote🌊".into())
        ],
        "reload must replay actual persisted completion, not invent interrupted turns"
    );
    // The replacement native process must also be cleaned up on idle EOF.
    drop(socket);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let ids = std::fs::read_to_string(&pids).unwrap();
            if ids.lines().all(|id| !process_alive(id)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("reloaded native child leaked on EOF");
    child.kill().await.unwrap();
    let _ = std::fs::remove_dir_all(directory);
}

fn process_alive(id: &str) -> bool {
    #[cfg(windows)]
    {
        String::from_utf8_lossy(
            &std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {id}"), "/FO", "CSV", "/NH"])
                .output()
                .unwrap()
                .stdout,
        )
        .contains(&format!("\"{id}\""))
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("kill")
            .args(["-0", id])
            .status()
            .unwrap()
            .success()
    }
}

#[tokio::test]
async fn queue_overload_open_budget_and_duplicate_initialize_cleanup() {
    let (address, stop, task) = start(None).await;
    let (mut socket, first) = connect_async(request(address, Some(TOKEN), None))
        .await
        .unwrap();
    initialize(&mut socket).await;
    // A burst exceeds the 32-item dispatcher queue. It must disconnect rather
    // than block the socket reader waiting for input capacity.
    for id in 2..=100 {
        if socket
            .feed(Message::Text(
                json!({"jsonrpc":"2.0","id":id,"method":"unknown"})
                    .to_string()
                    .into(),
            ))
            .await
            .is_err()
        {
            break;
        }
    }
    let _ = socket.flush().await;
    closed(&mut socket).await;
    let (mut socket, second) = connect_async(request(address, Some(TOKEN), None))
        .await
        .unwrap();
    assert_ne!(
        first.headers()["acp-connection-id"],
        second.headers()["acp-connection-id"]
    );
    initialize(&mut socket).await;
    for id in 2..=9 {
        send(
            &mut socket,
            json!({"jsonrpc":"2.0","id":id,"method":"session/load","params":{}}),
        )
        .await;
        assert_eq!(receive(&mut socket).await["id"], id);
    }
    send(
        &mut socket,
        json!({"jsonrpc":"2.0","id":10,"method":"session/load","params":{}}),
    )
    .await;
    closed(&mut socket).await;
    let (mut socket, _) = connect_async(request(address, Some(TOKEN), None))
        .await
        .unwrap();
    initialize(&mut socket).await;
    send(
        &mut socket,
        json!({"jsonrpc":"2.0","id":2,"method":"initialize"}),
    )
    .await;
    closed(&mut socket).await;
    for header in ["host", "origin"] {
        let mut duplicate = request(address, Some(TOKEN), Some("https://editor.example"));
        let value = duplicate.headers()[header].clone();
        duplicate.headers_mut().append(header, value);
        assert!(
            matches!(connect_async(duplicate).await, Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == 403)
        );
    }
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
