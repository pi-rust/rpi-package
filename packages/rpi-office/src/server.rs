//! Loopback WebSocket bridge server.
//!
//! Ported from the upstream `bridge-server.ts`: one hub task owns the attached
//! pane registry, the pending tool-call correlation map and the `/health`
//! surface, and every other thread talks to it through a command channel. The
//! hub never calls back into the host, so a slow runtime action cannot stall
//! the socket state machine.
#![forbid(unsafe_code)]

use crate::bridge::{self, Bridge, HealthMeta, Pane};
use crate::catalog::{catalog, Host};
use crate::protocol::{self, ClientMessage};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::TcpListener as StdTcpListener;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// How long a new connection may take to send `hello`.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Heartbeat cadence (ping idle panes, reap dead ones).
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// A pane that has not been heard from for this long is considered dead.
pub const PANE_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Close frame code used for a shutting-down bridge.
const CLOSE_GOING_AWAY: u16 = 1001;
/// Close frame code used for protocol violations.
const CLOSE_POLICY: u16 = 1008;
/// Largest HTTP request head accepted on the `/health` surface.
const MAX_HTTP_HEAD: usize = 8 * 1024;

/// Result of one proxied Office.js operation.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutput {
    pub text: String,
    pub details: Option<Value>,
}

/// Host callbacks the hub fires. Implementations must not block: they run on
/// the hub task, and the plugin routes them to a worker thread.
pub trait Hooks: Send + Sync {
    fn on_user_message(&self, text: &str, pane: &Pane);
    fn on_abort_request(&self, prompt_id: Option<&str>, pane: &Pane);
    fn on_panes_changed(&self, panes: &[Pane]);
    fn on_log(&self, _message: &str) {}
}

/// Commands any thread may send to the hub.
pub enum ServerCommand {
    Call {
        id: String,
        host: Host,
        op: String,
        args: Value,
        reply: Sender<Result<ToolOutput, String>>,
    },
    CancelCall {
        id: String,
    },
    PushToPane {
        pane_id: String,
        message: Value,
    },
    Stop(Sender<()>),
}

enum HubEvent {
    Command(ServerCommand),
    Open {
        conn: u64,
        tx: UnboundedSender<Message>,
    },
    Frame {
        conn: u64,
        raw: String,
    },
    Closed {
        conn: u64,
    },
}

/// Panes and metadata shared with readers that must not wait on the hub.
#[derive(Default)]
pub struct SharedState {
    pub panes: Vec<Pane>,
    pub actual_port: Option<u16>,
}

struct Conn {
    opened_at: u64,
    last_ping: u64,
    tx: UnboundedSender<Message>,
}

struct PendingCall {
    reply: Sender<Result<ToolOutput, String>>,
    conn: u64,
}

/// Handle to a running bridge server.
pub struct Server {
    pub port: u16,
    pub shared: Arc<Mutex<SharedState>>,
    commands: UnboundedSender<HubEvent>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Server {
    /// Start serving on an already-bound loopback listener.
    ///
    /// The caller binds synchronously so port conflicts surface as a normal
    /// command error instead of a background thread failure.
    pub fn start(
        listener: StdTcpListener,
        port: u16,
        meta: HealthMeta,
        hooks: Arc<dyn Hooks>,
    ) -> Result<Self, String> {
        let shared = Arc::new(Mutex::new(SharedState {
            panes: Vec::new(),
            actual_port: Some(port),
        }));
        let (commands, command_rx) = unbounded_channel();
        let thread_shared = Arc::clone(&shared);
        let thread_commands = commands.clone();
        let join = std::thread::Builder::new()
            .name("rpi-office-bridge".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(2)
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        hooks.on_log(&format!("bridge runtime failed: {error}"));
                        return;
                    }
                };
                runtime.block_on(run(
                    listener,
                    port,
                    meta,
                    hooks,
                    thread_shared,
                    thread_commands,
                    command_rx,
                ));
            })
            .map_err(|error| format!("could not start the bridge thread: {error}"))?;
        Ok(Self {
            port,
            shared,
            commands,
            join: Mutex::new(Some(join)),
        })
    }

    /// Proxy one operation to the pane that owns the requested document.
    pub fn call(
        &self,
        id: String,
        host: Host,
        op: String,
        args: Value,
        reply: Sender<Result<ToolOutput, String>>,
    ) -> Result<(), String> {
        self.send(ServerCommand::Call {
            id,
            host,
            op,
            args,
            reply,
        })
    }

    pub fn cancel_call(&self, id: &str) -> Result<(), String> {
        self.send(ServerCommand::CancelCall { id: id.to_string() })
    }

    pub fn push_to_pane(&self, pane_id: &str, message: Value) -> Result<(), String> {
        self.send(ServerCommand::PushToPane {
            pane_id: pane_id.to_string(),
            message,
        })
    }

    fn send(&self, command: ServerCommand) -> Result<(), String> {
        self.commands
            .send(HubEvent::Command(command))
            .map_err(|_| "office-bridge: server is not running".to_string())
    }

    /// Current pane snapshot (cheap clone).
    pub fn panes(&self) -> Vec<Pane> {
        self.shared
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .panes
            .clone()
    }

    /// Stop the server and wait for the thread to exit.
    pub fn stop(&self) -> Result<(), String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = self
            .commands
            .send(HubEvent::Command(ServerCommand::Stop(tx)));
        let _ = rx.recv_timeout(Duration::from_secs(5));
        let handle = self
            .join
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = self
            .commands
            .send(HubEvent::Command(ServerCommand::Stop(tx)));
        let _ = rx.recv_timeout(Duration::from_secs(3));
        let handle = self
            .join
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

/// Bind a loopback listener, mapping the OS error to an actionable sentence.
pub fn bind(port: u16) -> Result<StdTcpListener, String> {
    match StdTcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            let _ = listener.set_nonblocking(true);
            Ok(listener)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => Err(format!(
            "Office bridge could not start: port {port} is already in use. \
Another rpi session or program owns it. Run /office status there and close it, \
or connect this session on another port with /office connect <port>."
        )),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Err(format!(
            "Office bridge could not start on :{port}: permission denied ({error})."
        )),
        Err(error) => Err(format!("Office bridge could not start on :{port}: {error}")),
    }
}

/* ── hub ───────────────────────────────────────────────────────────── */

struct Hub {
    bridge: Bridge,
    conns: HashMap<u64, Conn>,
    pending: HashMap<String, PendingCall>,
    shared: Arc<Mutex<SharedState>>,
    hooks: Arc<dyn Hooks>,
    meta: HealthMeta,
}

impl Hub {
    fn publish(&self) {
        let panes = self.bridge.attached();
        let mut shared = self
            .shared
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        shared.panes = panes;
    }

    fn notify_panes_changed(&self) {
        let panes = self.bridge.attached();
        self.hooks.on_panes_changed(&panes);
    }

    fn send_frame(&self, conn: u64, frame: Value) {
        if let Some(handle) = self.conns.get(&conn) {
            let _ = handle.tx.send(Message::text(frame.to_string()));
        }
    }

    fn close_conn(&mut self, conn: u64, code: u16, reason: &str) {
        if let Some(handle) = self.conns.remove(&conn) {
            let _ = handle.tx.send(Message::Close(Some(
                tokio_tungstenite::tungstenite::protocol::CloseFrame {
                    code: code.into(),
                    reason: reason.to_string().into(),
                },
            )));
        }
        self.detach(conn);
    }

    fn detach(&mut self, conn: u64) {
        let had_pane = self.bridge.detach(conn);
        // Reject every pending call the departed pane never answered.
        let doomed: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, call)| call.conn == conn)
            .map(|(id, _)| id.clone())
            .collect();
        for id in doomed {
            if let Some(call) = self.pending.remove(&id) {
                let _ = call.reply.send(Err(
                    "office-bridge: pane disconnected while the tool was running".into(),
                ));
            }
        }
        if had_pane {
            self.publish();
            self.notify_panes_changed();
        }
    }

    fn handle_frame(&mut self, conn: u64, raw: &str) {
        let message = match protocol::parse_client_message(raw) {
            Ok(message) => message,
            Err(error) => {
                self.send_frame(conn, protocol::error_frame("bad_message", &error));
                return;
            }
        };
        match message {
            ClientMessage::Hello(hello) => {
                let pane = self.bridge.attach(conn, &hello).clone();
                self.send_frame(
                    conn,
                    protocol::welcome(
                        &self.meta.service,
                        &self.meta.server_version,
                        self.meta.pi_version.as_deref(),
                    ),
                );
                let _ = pane;
                self.publish();
                self.notify_panes_changed();
            }
            ClientMessage::Ping { ts } => {
                self.send_frame(conn, protocol::pong(ts));
            }
            ClientMessage::ToolResult {
                id,
                ok,
                text,
                details,
                error,
            } => {
                self.bridge.touch(conn);
                let Some(call) = self.pending.get(&id) else {
                    return;
                };
                // Only the pane the call was routed to may answer it.
                if call.conn != conn {
                    return;
                }
                let call = self.pending.remove(&id).expect("checked above");
                if !ok {
                    let _ = call.reply.send(Err(format!(
                        "office-bridge: {}",
                        error.unwrap_or_else(|| "office tool failed".into())
                    )));
                    return;
                }
                let _ = call.reply.send(Ok(ToolOutput {
                    text: protocol::truncate_text(&text),
                    details: protocol::bound_details(details),
                }));
            }
            ClientMessage::UserMessage { text } => {
                self.bridge.touch(conn);
                self.bridge.mark_focused(conn, None);
                self.publish();
                let trimmed = text.trim();
                if let (false, Some(pane)) = (trimmed.is_empty(), self.bridge.pane(conn)) {
                    let pane = pane.clone();
                    self.hooks.on_user_message(trimmed, &pane);
                }
            }
            ClientMessage::PaneState {
                focused,
                visible,
                active_at: _,
            } => {
                self.bridge.touch(conn);
                if let Some(visible) = visible {
                    self.bridge.set_visible(conn, visible);
                }
                if focused == Some(true) {
                    self.bridge.mark_focused(conn, visible);
                } else if focused == Some(false) {
                    self.bridge.set_focused(conn, false);
                }
                self.publish();
                self.notify_panes_changed();
            }
            ClientMessage::Status { model, provider } => {
                self.bridge
                    .set_status(conn, model.as_deref(), provider.as_deref());
                self.publish();
            }
            ClientMessage::Abort { prompt_id } => {
                self.bridge.touch(conn);
                if let Some(pane) = self.bridge.pane(conn) {
                    let pane = pane.clone();
                    self.hooks.on_abort_request(prompt_id.as_deref(), &pane);
                }
            }
        }
    }

    fn handle_command(&mut self, command: ServerCommand) -> bool {
        match command {
            ServerCommand::Call {
                id,
                host,
                op,
                args,
                reply,
            } => {
                let (target, rest) = bridge::split_target(&args);
                let op_id = format!("{}.{op}", host.as_str());
                let pane = match self.bridge.resolve(host, target.as_deref()) {
                    Ok(pane) => pane,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return true;
                    }
                };
                // Capability gate: a pane may only execute ops it advertised
                // (or, when it advertised nothing, only the v1 op set).
                if let Some(ops) = &pane.ops {
                    if !ops.iter().any(|advertised| advertised == &op_id) {
                        let _ = reply.send(Err(format!(
                            "office-bridge: the attached {host} pane does not advertise \"{op_id}\" \
(it supports {count} ops). Reload the pi-for-office add-in to enable this tool.",
                            host = host.as_str(),
                            count = ops.len()
                        )));
                        return true;
                    }
                } else if !catalog().legacy_allows(&op_id) {
                    let _ = reply.send(Err(format!(
                        "office-bridge: \"{op_id}\" requires a newer add-in. The attached pane is a \
legacy client (no capability list); update pi-for-office and reload the document to enable this tool."
                    )));
                    return true;
                }
                let (conn, document_id) = (pane.conn, pane.document_id.clone());
                let frame = protocol::tool_call(&id, &op_id, &rest, document_id.as_deref());
                self.pending.insert(id.clone(), PendingCall { reply, conn });
                let sent = self
                    .conns
                    .get(&conn)
                    .map(|handle| handle.tx.send(Message::text(frame.to_string())).is_ok())
                    .unwrap_or(false);
                if !sent {
                    if let Some(call) = self.pending.remove(&id) {
                        let _ = call.reply.send(Err(
                            "office-bridge: pane disconnected before the tool call was sent".into(),
                        ));
                    }
                }
            }
            ServerCommand::CancelCall { id } => {
                if let Some(call) = self.pending.remove(&id) {
                    let _ = call
                        .reply
                        .send(Err("office-bridge: tool call aborted by the agent".into()));
                }
            }
            ServerCommand::PushToPane { pane_id, message } => {
                if let Some(pane) = self.bridge.pane_by_id(&pane_id) {
                    let conn = pane.conn;
                    self.send_frame(conn, message);
                }
            }
            ServerCommand::Stop(reply) => {
                let ids: Vec<u64> = self.conns.keys().copied().collect();
                for conn in ids {
                    self.close_conn(conn, CLOSE_GOING_AWAY, "bridge shutting down");
                }
                let _ = reply.send(());
                return false;
            }
        }
        true
    }

    fn heartbeat(&mut self) {
        let now = protocol::now_ms();
        let stale: Vec<u64> = self
            .conns
            .iter()
            .filter_map(|(conn, handle)| {
                if self.bridge.pane(*conn).is_none() {
                    if now.saturating_sub(handle.opened_at) > HELLO_TIMEOUT.as_millis() as u64 {
                        return Some(*conn);
                    }
                    return None;
                }
                let last_seen = self
                    .bridge
                    .pane(*conn)
                    .map(|pane| pane.last_seen)
                    .unwrap_or(handle.opened_at);
                if now.saturating_sub(last_seen) > PANE_IDLE_TIMEOUT.as_millis() as u64 {
                    return Some(*conn);
                }
                None
            })
            .collect();
        for conn in stale {
            self.close_conn(conn, CLOSE_POLICY, "bridge: connection timed out");
        }
        for handle in self.conns.values_mut() {
            if now.saturating_sub(handle.last_ping) > 30_000 {
                let _ = handle.tx.send(Message::Ping(Vec::new().into()));
                handle.last_ping = now;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    listener: StdTcpListener,
    port: u16,
    meta: HealthMeta,
    hooks: Arc<dyn Hooks>,
    shared: Arc<Mutex<SharedState>>,
    commands: UnboundedSender<HubEvent>,
    mut command_rx: UnboundedReceiver<HubEvent>,
) {
    let listener = match TcpListener::from_std(listener) {
        Ok(listener) => listener,
        Err(error) => {
            hooks.on_log(&format!("bridge listener failed: {error}"));
            return;
        }
    };
    let hub_shared = Arc::clone(&shared);
    let hub_meta = meta.clone();
    let hub_commands = commands.clone();
    let accept = tokio::spawn(async move {
        let mut next_conn: u64 = 1;
        loop {
            match listener.accept().await {
                Ok((stream, _peer)) => {
                    let conn = next_conn;
                    next_conn += 1;
                    let events = hub_commands.clone();
                    let shared = Arc::clone(&hub_shared);
                    let meta = hub_meta.clone();
                    tokio::spawn(async move {
                        serve_connection(stream, conn, events, shared, meta).await;
                    });
                }
                Err(error) => {
                    // A transient accept error must not kill the bridge.
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        continue;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    });

    let mut hub = Hub {
        bridge: Bridge::new(),
        conns: HashMap::new(),
        pending: HashMap::new(),
        shared: Arc::clone(&shared),
        hooks: Arc::clone(&hooks),
        meta: meta.clone(),
    };
    // New connections are announced by `serve_connection` through ConnOpen.
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    loop {
        tokio::select! {
            event = command_rx.recv() => {
                let Some(event) = event else { break };
                match event {
                    HubEvent::Command(command) => {
                        if !hub.handle_command(command) {
                            break;
                        }
                    }
                    HubEvent::Open { conn, tx } => {
                        let now = protocol::now_ms();
                        hub.conns.insert(
                            conn,
                            Conn {
                                opened_at: now,
                                last_ping: now,
                                tx,
                            },
                        );
                    }
                    HubEvent::Frame { conn, raw } => hub.handle_frame(conn, &raw),
                    HubEvent::Closed { conn } => {
                        hub.conns.remove(&conn);
                        hub.detach(conn);
                    }
                }
            }
            _ = heartbeat.tick() => hub.heartbeat(),
        }
    }
    accept.abort();
    // Publish the final (empty) pane set so readers never see ghosts.
    {
        let mut shared = hub
            .shared
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        shared.panes.clear();
        shared.actual_port = None;
    }
    hub.hooks.on_panes_changed(&[]);
    hooks.on_log(&format!("bridge on :{port} stopped"));
}

/// Serve one accepted connection: WebSocket upgrade, or the `/health` HTTP
/// surface when the request is plain HTTP.
async fn serve_connection(
    mut stream: TcpStream,
    conn: u64,
    events: UnboundedSender<HubEvent>,
    shared: Arc<Mutex<SharedState>>,
    meta: HealthMeta,
) {
    let mut head = [0u8; 4];
    let peeked = match stream.peek(&mut head).await {
        Ok(count) => count,
        Err(_) => return,
    };
    if peeked == 0 {
        return;
    }
    // Both surfaces are plain HTTP GETs; the Upgrade header decides below.
    if peeked < 4 || !head.eq_ignore_ascii_case(b"GET ") {
        return;
    }
    // Read the request head without consuming it twice: `peek` does not advance
    // the stream, so read the full head here and decide from its content.
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) => return,
            Ok(count) => {
                buffer.extend_from_slice(&chunk[..count]);
                if buffer.windows(4).any(|window| window == b"\r\n\r\n")
                    || buffer.len() > MAX_HTTP_HEAD
                {
                    break;
                }
            }
            Err(_) => return,
        }
    }
    let request = String::from_utf8_lossy(&buffer).to_string();
    let is_upgrade = request.to_ascii_lowercase().contains("upgrade: websocket");
    if !is_upgrade {
        respond_http(&mut stream, &request, &shared, &meta).await;
        return;
    }
    // The WebSocket handshake must read the request itself. Serve it from a
    // fresh stream copy is impossible, so hand the buffered bytes back via a
    // rewind: tungstenite only needs the framed bytes after the head, and the
    // handshake callback validates the head we already parsed.
    let stream = RewindStream::new(buffer, stream);
    let websocket = match tokio_tungstenite::accept_async(stream).await {
        Ok(websocket) => websocket,
        Err(_) => return,
    };
    run_websocket(websocket, conn, events).await;
}

/// A stream that replays already-read bytes before continuing on the socket.
struct RewindStream {
    prefix: Vec<u8>,
    offset: usize,
    inner: TcpStream,
}

impl RewindStream {
    fn new(prefix: Vec<u8>, inner: TcpStream) -> Self {
        Self {
            prefix,
            offset: 0,
            inner,
        }
    }
}

impl tokio::io::AsyncRead for RewindStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.offset < self.prefix.len() {
            let remaining = self.prefix.len() - self.offset;
            let take = remaining.min(buf.remaining());
            let start = self.offset;
            buf.put_slice(&self.prefix[start..start + take]);
            self.offset += take;
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for RewindStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

async fn run_websocket(
    websocket: WebSocketStream<RewindStream>,
    conn: u64,
    events: UnboundedSender<HubEvent>,
) {
    let (mut sink, mut stream) = websocket.split();
    let (tx, mut rx) = unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let closing = matches!(message, Message::Close(_));
            if sink.send(message).await.is_err() {
                break;
            }
            if closing {
                break;
            }
        }
        let _ = sink.close().await;
    });

    // Register the writer channel so the hub can address this connection.
    if events
        .send(HubEvent::Open {
            conn,
            tx: tx.clone(),
        })
        .is_err()
    {
        writer.abort();
        return;
    }
    while let Some(next) = stream.next().await {
        match next {
            Ok(Message::Text(text)) => {
                if events
                    .send(HubEvent::Frame {
                        conn,
                        raw: text.to_string(),
                    })
                    .is_err()
                {
                    break;
                }
            }
            Ok(Message::Binary(_)) | Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => continue,
            Ok(Message::Close(_)) | Ok(Message::Frame(_)) => break,
            Err(_) => break,
        }
    }
    let _ = events.send(HubEvent::Closed { conn });
    // Drop our sender so the writer task drains and exits.
    drop(tx);
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
}

/// Minimal loopback HTTP surface: `GET /health`, `OPTIONS` preflight, else 404.
async fn respond_http(
    stream: &mut TcpStream,
    request: &str,
    shared: &Arc<Mutex<SharedState>>,
    meta: &HealthMeta,
) {
    let mut lines = request.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut origin: Option<String> = None;
    for line in lines {
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("origin") {
                let value = value.trim().to_string();
                if !value.is_empty() {
                    origin = Some(value);
                }
            }
        }
    }
    let allow_origin = match &origin {
        None => Some("*".to_string()),
        Some(origin) => {
            let mut allowed: Vec<String> = crate::config::DEFAULT_ALLOWED_ORIGINS
                .iter()
                .map(|value| value.to_string())
                .collect();
            allowed.extend(extra_origins());
            if allowed.iter().any(|candidate| candidate == origin) {
                Some(origin.clone())
            } else {
                None
            }
        }
    };
    let (status, body) = if method == "OPTIONS" {
        (204, None)
    } else if method == "GET" && path == "/health" {
        let panes = shared
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .panes
            .clone();
        let panes = {
            let mut panes = panes;
            panes.sort_by_key(|pane| std::cmp::Reverse(pane.connected_at));
            panes
        };
        (200, Some(bridge::health_json(&panes, meta)))
    } else {
        (404, Some(json!({"ok": false, "error": "not_found"})))
    };
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\n\
Cache-Control: no-store\r\nAccess-Control-Allow-Methods: GET, OPTIONS\r\n\
Access-Control-Allow-Headers: *\r\nAccess-Control-Allow-Private-Network: true\r\n\
Content-Length: {length}\r\nConnection: close\r\n",
        reason = if status == 204 { "No Content" } else { "OK" },
        length = body.len()
    );
    if let Some(origin) = allow_origin {
        head.push_str(&format!(
            "Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\n"
        ));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes()).await;
    if status != 204 {
        let _ = stream.write_all(body.as_bytes()).await;
    }
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

fn extra_origins() -> Vec<String> {
    crate::config::extra_allowed_origins(&|name| std::env::var(name).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_reports_actionable_port_conflicts() {
        let first = bind(0).expect("ephemeral bind");
        let port = first.local_addr().unwrap().port();
        let error = bind(port).unwrap_err();
        assert!(error.contains(&format!("port {port} is already in use")));
        assert!(error.contains("/office connect <port>"));
    }

    #[test]
    fn server_starts_serves_health_and_stops() {
        let listener = bind(0).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let meta = HealthMeta {
            service: "rpi-office".into(),
            server_version: "0.1.0".into(),
            pi_version: None,
            port,
            started_at: protocol::now_ms(),
        };
        let server = Server::start(listener, port, meta, Arc::new(NoHooks)).expect("start");
        let body = fetch_health(port);
        assert!(body.contains("\"ok\":true"), "health body: {body}");
        assert!(body.contains("\"service\":\"rpi-office\""));
        assert!(body.contains("\"panes\":[]"));
        server.stop().expect("stop");
        assert!(server.panes().is_empty());
    }

    struct NoHooks;
    impl Hooks for NoHooks {
        fn on_user_message(&self, _text: &str, _pane: &Pane) {}
        fn on_abort_request(&self, _prompt_id: Option<&str>, _pane: &Pane) {}
        fn on_panes_changed(&self, _panes: &[Pane]) {}
    }

    /// Blocking HTTP GET against the loopback health surface.
    pub(crate) fn fetch_health(port: u16) -> String {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .expect("write");
        let mut raw = String::new();
        let _ = stream.read_to_string(&mut raw);
        raw
    }
}
