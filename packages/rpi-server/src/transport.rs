use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, RwLock};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, WebSocketStream};

/// 传输层 trait - 抽象底层协议
#[async_trait]
pub trait Transport: Send + Sync + 'static {
    type Connection: Connection;

    /// 启动监听（服务端用）
    async fn listen(&self, addr: &str) -> Result<(), String>;

    /// 接受新连接（服务端用）
    async fn accept(&self) -> Result<Self::Connection, String>;

    /// 连接到远端（客户端用）
    async fn connect(&self, addr: &str) -> Result<Self::Connection, String>;

    /// 停止监听
    async fn shutdown(&self) -> Result<(), String>;
}

/// 单个连接 trait - 抽象消息收发
#[async_trait]
pub trait Connection: Send + 'static {
    /// 发送 JSON 消息
    async fn send(&mut self, msg: Value) -> Result<(), String>;

    /// 接收 JSON 消息（返回 None 表示连接关闭）
    async fn recv(&mut self) -> Result<Option<Value>, String>;

    /// 连接 ID
    fn id(&self) -> &str;

    /// 关闭连接
    async fn close(&mut self) -> Result<(), String>;
}

/// 连接状态
#[derive(Debug, Clone)]
pub struct ConnectionStatus {
    pub connected: bool,
    pub reconnect_attempts: u32,
    pub last_heartbeat: Option<Instant>,
    pub latency_ms: u64,
    pub messages_sent: u64,
    pub messages_received: u64,
}

/// 连接管理器 - 封装重试、心跳、状态
pub struct ManagedConnection<T: Transport> {
    transport: Arc<T>,
    max_retries: u32,
    heartbeat_interval: Duration,
    retry_delay: Duration,
    status: Arc<RwLock<ConnectionStatus>>,
    connection: Arc<Mutex<Option<T::Connection>>>,
}

impl<T: Transport> ManagedConnection<T> {
    pub fn new(transport: Arc<T>) -> Self {
        Self {
            transport,
            max_retries: 10,
            heartbeat_interval: Duration::from_secs(30),
            retry_delay: Duration::from_secs(1),
            status: Arc::new(RwLock::new(ConnectionStatus {
                connected: false,
                reconnect_attempts: 0,
                last_heartbeat: None,
                latency_ms: 0,
                messages_sent: 0,
                messages_received: 0,
            })),
            connection: Arc::new(Mutex::new(None)),
        }
    }

    /// 带重试的连接（客户端用）
    pub async fn connect_with_retry(&self, addr: &str) -> Result<(), String> {
        for attempt in 1..=self.max_retries {
            {
                let mut status = self.status.write().await;
                status.reconnect_attempts = attempt;
            }

            match self.transport.connect(addr).await {
                Ok(conn) => {
                    *self.connection.lock().await = Some(conn);
                    let mut status = self.status.write().await;
                    status.connected = true;
                    status.reconnect_attempts = 0;
                    return Ok(());
                }
                Err(e) if attempt < self.max_retries => {
                    eprintln!(
                        "connect failed (attempt {}/{}): {}, retrying...",
                        attempt, self.max_retries, e
                    );
                    tokio::time::sleep(self.retry_delay * attempt).await;
                }
                Err(e) => {
                    let mut status = self.status.write().await;
                    status.connected = false;
                    return Err(format!(
                        "connect failed after {} attempts: {}",
                        self.max_retries, e
                    ));
                }
            }
        }
        unreachable!()
    }

    /// 发送消息
    pub async fn send(&self, msg: Value) -> Result<(), String> {
        let mut conn_guard = self.connection.lock().await;
        let conn = conn_guard
            .as_mut()
            .ok_or_else(|| "not connected".to_string())?;

        conn.send(msg).await?;

        let mut status = self.status.write().await;
        status.messages_sent += 1;

        Ok(())
    }

    /// 接收消息
    pub async fn recv(&self) -> Result<Option<Value>, String> {
        let mut conn_guard = self.connection.lock().await;
        let conn = conn_guard
            .as_mut()
            .ok_or_else(|| "not connected".to_string())?;

        let result = conn.recv().await?;

        if result.is_some() {
            let mut status = self.status.write().await;
            status.messages_received += 1;
        } else {
            // 连接关闭
            let mut status = self.status.write().await;
            status.connected = false;
        }

        Ok(result)
    }

    /// 启动心跳（在独立 task 中运行）
    pub async fn start_heartbeat(&self) -> Result<(), String> {
        let mut interval = tokio::time::interval(self.heartbeat_interval);

        loop {
            interval.tick().await;

            let ping = serde_json::json!({
                "type": "ping",
                "timestamp": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            });

            let start = Instant::now();
            self.send(ping).await?;

            // 等待 pong（带超时）
            match tokio::time::timeout(Duration::from_secs(5), self.recv()).await {
                Ok(Ok(Some(msg))) if msg["type"] == "pong" => {
                    let latency = start.elapsed().as_millis() as u64;
                    let mut status = self.status.write().await;
                    status.latency_ms = latency;
                    status.last_heartbeat = Some(Instant::now());
                }
                Ok(Ok(None)) => {
                    let mut status = self.status.write().await;
                    status.connected = false;
                    return Err("connection closed during heartbeat".into());
                }
                Ok(Err(e)) => {
                    let mut status = self.status.write().await;
                    status.connected = false;
                    return Err(format!("heartbeat error: {}", e));
                }
                Err(_) => {
                    let mut status = self.status.write().await;
                    status.connected = false;
                    return Err("heartbeat timeout".into());
                }
                _ => {}
            }
        }
    }

    /// 获取当前状态
    pub async fn status(&self) -> ConnectionStatus {
        self.status.read().await.clone()
    }

    /// 关闭连接
    pub async fn close(&self) -> Result<(), String> {
        let mut conn_guard = self.connection.lock().await;
        if let Some(mut conn) = conn_guard.take() {
            conn.close().await?;
        }
        let mut status = self.status.write().await;
        status.connected = false;
        Ok(())
    }
}

impl<T: Transport> Clone for ManagedConnection<T> {
    fn clone(&self) -> Self {
        Self {
            transport: Arc::clone(&self.transport),
            max_retries: self.max_retries,
            heartbeat_interval: self.heartbeat_interval,
            retry_delay: self.retry_delay,
            status: Arc::clone(&self.status),
            connection: Arc::clone(&self.connection),
        }
    }
}

// ── TCP 实现 ──────────────────────────────────────────────────────────────────

pub struct TcpTransport {
    listener: Mutex<Option<TcpListener>>,
}

impl TcpTransport {
    pub fn new() -> Self {
        Self {
            listener: Mutex::new(None),
        }
    }
}

impl Default for TcpTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Transport for TcpTransport {
    type Connection = TcpConnection;

    async fn listen(&self, addr: &str) -> Result<(), String> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| format!("bind failed: {e}"))?;
        *self.listener.lock().await = Some(listener);
        Ok(())
    }

    async fn accept(&self) -> Result<TcpConnection, String> {
        let (stream, addr) = {
            let listener = self.listener.lock().await;
            let listener = listener.as_ref().ok_or("not listening")?;
            listener
                .accept()
                .await
                .map_err(|e| format!("accept failed: {e}"))?
        };
        Ok(TcpConnection::new(stream, format!("tcp-{addr}")))
    }

    async fn connect(&self, addr: &str) -> Result<TcpConnection, String> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|e| format!("connect failed: {e}"))?;

        Ok(TcpConnection::new(stream, format!("tcp-client-{addr}")))
    }

    async fn shutdown(&self) -> Result<(), String> {
        *self.listener.lock().await = None;
        Ok(())
    }
}

pub struct TcpConnection {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
    id: String,
}

impl TcpConnection {
    pub fn new(stream: TcpStream, id: String) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            reader: BufReader::new(reader),
            writer,
            id,
        }
    }
}

#[async_trait]
impl Connection for TcpConnection {
    async fn send(&mut self, msg: Value) -> Result<(), String> {
        let mut line = serde_json::to_string(&msg).map_err(|e| format!("serialize failed: {e}"))?;
        line.push('\n');

        self.writer
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("write failed: {e}"))?;

        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<Value>, String> {
        let mut line = String::new();

        match self.reader.read_line(&mut line).await {
            Ok(0) => Ok(None), // EOF
            Ok(_) => {
                let msg =
                    serde_json::from_str(line.trim()).map_err(|e| format!("parse failed: {e}"))?;
                Ok(Some(msg))
            }
            Err(e) => Err(format!("read failed: {e}")),
        }
    }

    fn id(&self) -> &str {
        &self.id
    }

    async fn close(&mut self) -> Result<(), String> {
        self.writer
            .shutdown()
            .await
            .map_err(|e| format!("close failed: {e}"))
    }
}

// ── WebSocket 实现 ────────────────────────────────────────────────────────────

/// 服务端/客户端共用的字节流：明文 TCP 或已完成 TLS 握手的流。
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

type Socket = WebSocketStream<Box<dyn Stream>>;

/// 每条 JSON 消息占一个 WebSocket 文本帧；协议与 TCP JSONL 一致。
pub struct WebSocketTransport {
    listener: Mutex<Option<TcpListener>>,
    /// 服务端 TLS；`Some` 时监听 `wss://`。
    acceptor: Option<TlsAcceptor>,
    /// 客户端是否使用 `wss://`（与服务端的 TLS 状态无关）。
    secure: bool,
}

impl WebSocketTransport {
    pub fn new() -> Self {
        Self {
            listener: Mutex::new(None),
            acceptor: None,
            secure: false,
        }
    }

    /// 服务端：先做 TLS 握手再升级 WebSocket。
    pub fn with_tls(acceptor: TlsAcceptor) -> Self {
        Self {
            listener: Mutex::new(None),
            acceptor: Some(acceptor),
            secure: false,
        }
    }

    /// 客户端：连接 `wss://`，按系统根证书校验。
    pub fn secure() -> Self {
        Self {
            listener: Mutex::new(None),
            acceptor: None,
            secure: true,
        }
    }

    /// 客户端使用的完整 URL。`addr` 是 `host:port` 或完整 URL。
    fn url(&self, addr: &str) -> String {
        let scheme = if self.secure { "wss" } else { "ws" };
        if addr.starts_with("ws://") || addr.starts_with("wss://") {
            addr.to_string()
        } else {
            format!("{scheme}://{addr}/")
        }
    }

    /// 明文或 TLS 的客户端字节流。
    async fn connect_stream(
        &self,
        host: &str,
        port: u16,
        secure: bool,
    ) -> Result<Box<dyn Stream>, String> {
        let tcp = TcpStream::connect((host, port))
            .await
            .map_err(|e| format!("connect failed: {e}"))?;
        if !secure {
            return Ok(Box::new(tcp));
        }
        let mut roots = tokio_rustls::rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = tokio_rustls::rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let server_name = tokio_rustls::rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|_| format!("invalid TLS server name: {host}"))?;
        let stream = connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| format!("tls handshake failed: {e}"))?;
        Ok(Box::new(stream))
    }
}

impl Default for WebSocketTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Transport for WebSocketTransport {
    type Connection = WebSocketConnection;

    async fn listen(&self, addr: &str) -> Result<(), String> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| format!("bind failed: {e}"))?;
        *self.listener.lock().await = Some(listener);
        Ok(())
    }

    async fn accept(&self) -> Result<WebSocketConnection, String> {
        // Take the connection, then release the listener: a peer that connects
        // and never finishes the TLS/WebSocket handshake must not block every
        // other accept.
        let (stream, addr) = {
            let listener = self.listener.lock().await;
            let listener = listener.as_ref().ok_or("not listening")?;
            listener
                .accept()
                .await
                .map_err(|e| format!("accept failed: {e}"))?
        };
        // TLS first when configured, then the WebSocket handshake on the same
        // boxed stream type the client side uses.
        let stream: Box<dyn Stream> = match &self.acceptor {
            Some(acceptor) => Box::new(
                acceptor
                    .accept(stream)
                    .await
                    .map_err(|e| format!("tls handshake failed: {e}"))?,
            ),
            None => Box::new(stream),
        };
        let socket = accept_async(stream)
            .await
            .map_err(|e| format!("websocket handshake failed: {e}"))?;
        Ok(WebSocketConnection::new(socket, format!("ws-{addr}")))
    }

    async fn connect(&self, addr: &str) -> Result<WebSocketConnection, String> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let url = self.url(addr);
        let request = url
            .clone()
            .into_client_request()
            .map_err(|e| format!("invalid websocket url {url}: {e}"))?;
        let host = request
            .uri()
            .host()
            .ok_or_else(|| format!("websocket url {url} has no host"))?
            .to_string();
        // An explicit `wss://` address is TLS regardless of how the transport
        // was constructed, so a caller cannot silently downgrade to plaintext.
        let secure = self.secure || url.starts_with("wss://");
        let port = request
            .uri()
            .port_u16()
            .unwrap_or(if secure { 443 } else { 80 });
        let stream = self.connect_stream(&host, port, secure).await?;
        let (socket, _) = tokio_tungstenite::client_async(request, stream)
            .await
            .map_err(|e| format!("websocket connect failed: {e}"))?;
        Ok(WebSocketConnection::new(
            socket,
            format!("ws-client-{addr}"),
        ))
    }

    async fn shutdown(&self) -> Result<(), String> {
        *self.listener.lock().await = None;
        Ok(())
    }
}

pub struct WebSocketConnection {
    socket: Socket,
    id: String,
}

impl WebSocketConnection {
    pub fn new(socket: Socket, id: String) -> Self {
        Self { socket, id }
    }
}

#[async_trait]
impl Connection for WebSocketConnection {
    async fn send(&mut self, msg: Value) -> Result<(), String> {
        let text = serde_json::to_string(&msg).map_err(|e| format!("serialize failed: {e}"))?;
        self.socket
            .send(Message::Text(text.into()))
            .await
            .map_err(|e| format!("write failed: {e}"))?;
        self.socket
            .flush()
            .await
            .map_err(|e| format!("flush failed: {e}"))
    }

    async fn recv(&mut self) -> Result<Option<Value>, String> {
        loop {
            let message = match self.socket.next().await {
                Some(Ok(message)) => message,
                Some(Err(error)) => return Err(format!("read failed: {error}")),
                None => return Ok(None),
            };
            match message {
                Message::Text(text) => {
                    return serde_json::from_str(text.as_str())
                        .map(Some)
                        .map_err(|e| format!("parse failed: {e}"));
                }
                Message::Binary(bytes) => {
                    let text = String::from_utf8(bytes.to_vec())
                        .map_err(|_| "binary frame is not UTF-8".to_string())?;
                    return serde_json::from_str(&text)
                        .map(Some)
                        .map_err(|e| format!("parse failed: {e}"));
                }
                Message::Ping(payload) => {
                    // tungstenite answers pings on read, but reply explicitly so a
                    // peer that only sends pings sees a pong before the next message.
                    let _ = self.socket.send(Message::Pong(payload)).await;
                }
                Message::Pong(_) | Message::Frame(_) => {}
                Message::Close(_) => return Ok(None),
            }
        }
    }

    fn id(&self) -> &str {
        &self.id
    }

    async fn close(&mut self) -> Result<(), String> {
        self.socket
            .close(None)
            .await
            .map_err(|e| format!("close failed: {e}"))
    }
}

#[cfg(test)]
mod websocket_tests {
    use super::*;

    fn free_addr() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
        listener.local_addr().expect("address").to_string()
    }

    #[tokio::test]
    async fn round_trips_json_frames_in_both_directions() {
        let addr = free_addr();
        let server = Arc::new(WebSocketTransport::new());
        server.listen(&addr).await.expect("listen");

        let accepting = {
            let server = Arc::clone(&server);
            tokio::spawn(async move { server.accept().await })
        };
        let mut client = WebSocketTransport::new()
            .connect(&addr)
            .await
            .expect("connect");
        let mut peer = accepting.await.expect("accept task").expect("peer");

        client
            .send(serde_json::json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
            .await
            .expect("client send");
        let received = peer.recv().await.expect("peer recv").expect("frame");
        assert_eq!(received["method"], "ping");

        peer.send(serde_json::json!({"jsonrpc":"2.0","id":1,"result":{"pong":true}}))
            .await
            .expect("peer send");
        let reply = client.recv().await.expect("client recv").expect("frame");
        assert_eq!(reply["result"]["pong"], true);

        client.close().await.expect("client close");
        assert_eq!(peer.recv().await.expect("peer sees close"), None);
        server.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn connects_accept_ws_urls_and_bare_addresses() {
        let plain = WebSocketTransport::new();
        assert_eq!(plain.url("127.0.0.1:9800"), "ws://127.0.0.1:9800/");
        assert_eq!(plain.url("ws://example.test/rpi"), "ws://example.test/rpi");
        let secure = WebSocketTransport::secure();
        assert_eq!(secure.url("127.0.0.1:9800"), "wss://127.0.0.1:9800/");
        assert_eq!(
            secure.url("wss://example.test/rpi"),
            "wss://example.test/rpi"
        );
    }

    #[tokio::test]
    async fn a_stalled_handshake_does_not_block_other_connections() {
        let addr = free_addr();
        let server = Arc::new(WebSocketTransport::new());
        server.listen(&addr).await.expect("listen");

        // A peer that connects and never sends the WebSocket handshake. The
        // first accept parks in that handshake for much longer than the second
        // accept is allowed to take, so holding the listener would be visible.
        let _stalled = TcpStream::connect(&addr).await.expect("stalled peer");
        let first = {
            let server = Arc::clone(&server);
            tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(5), server.accept()).await
            })
        };
        tokio::time::sleep(Duration::from_millis(150)).await;

        let second = {
            let server = Arc::clone(&server);
            tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(1), server.accept()).await
            })
        };
        let connecting =
            tokio::spawn(async move { WebSocketTransport::new().connect(&addr).await });
        // Judge the accept first: a listener held across the stalled handshake
        // fails here within a second instead of hanging the client below.
        let mut peer = second
            .await
            .expect("accept task")
            .expect("accept was blocked by the stalled handshake")
            .expect("peer");
        let mut client = connecting
            .await
            .expect("connect task")
            .expect("second connection");

        client
            .send(serde_json::json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
            .await
            .expect("client send");
        let frame = peer.recv().await.expect("peer recv").expect("frame");
        assert_eq!(frame["method"], "ping");

        first.abort();
        let _ = client.close().await;
    }

    #[tokio::test]
    async fn accepting_without_listening_fails_closed() {
        let error = WebSocketTransport::new()
            .accept()
            .await
            .err()
            .expect("must fail");
        assert!(error.contains("not listening"), "{error}");
    }
}
