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

/// TLS 需要一个 `ServerName`：IP 字面量必须走 `IpAddress`，否则合法地址会被
/// 当成非法 DNS 名拒掉。
fn tls_server_name(
    host: &str,
) -> Result<tokio_rustls::rustls::pki_types::ServerName<'static>, String> {
    use std::net::IpAddr;
    use tokio_rustls::rustls::pki_types::ServerName;
    if let Ok(address) = host.parse::<IpAddr>() {
        return Ok(ServerName::IpAddress(address.into()));
    }
    ServerName::try_from(host.to_string()).map_err(|_| format!("invalid TLS server name: {host}"))
}

/// Newest modification time among the existing TLS files.
fn newest_mtime(files: &crate::tls::TlsFiles) -> Option<std::time::SystemTime> {
    [
        Some(&files.cert),
        Some(&files.key),
        files.client_ca.as_ref(),
    ]
    .into_iter()
    .flatten()
    .filter_map(|path| std::fs::metadata(path).ok()?.modified().ok())
    .max()
}

/// 服务端/客户端共用的字节流：明文 TCP 或已完成 TLS 握手的流。
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

type Socket = WebSocketStream<Box<dyn Stream>>;

/// How long a TLS/WebSocket handshake may take before the socket is dropped.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// 每条 JSON 消息占一个 WebSocket 文本帧；协议与 TCP JSONL 一致。
pub struct WebSocketTransport {
    listener: Mutex<Option<TcpListener>>,
    /// 服务端 TLS；`Some` 时监听 `wss://`。握手中途可被证书轮换替换。
    acceptor: std::sync::Mutex<Option<TlsAcceptor>>,
    /// 客户端是否使用 `wss://`（与服务端的 TLS 状态无关）。
    secure: bool,
    /// 轮换来源：PEM 文件、检查间隔、上次成功加载的时间。
    rotation: Option<Rotation>,
}

/// Where rotated TLS material is read from.
struct Rotation {
    files: crate::tls::TlsFiles,
    interval: Duration,
    loaded_at: std::sync::Mutex<std::time::SystemTime>,
    last_check: std::sync::Mutex<Instant>,
}

impl WebSocketTransport {
    pub fn new() -> Self {
        Self {
            listener: Mutex::new(None),
            acceptor: std::sync::Mutex::new(None),
            secure: false,
            rotation: None,
        }
    }

    /// 服务端：先做 TLS 握手再升级 WebSocket。
    pub fn with_tls(acceptor: TlsAcceptor) -> Self {
        Self {
            listener: Mutex::new(None),
            acceptor: std::sync::Mutex::new(Some(acceptor)),
            secure: false,
            rotation: None,
        }
    }

    /// 服务端 TLS，并在这几个 PEM 文件变化后重新加载。
    ///
    /// `config` 必须是调用方已经用同一组文件加载并校验过的配置，这样启动错误
    /// 能在开启监听之前暴露；轮换失败时保留旧配置而不是让监听器失效。
    pub fn with_tls_rotation(
        files: crate::tls::TlsFiles,
        config: Arc<tokio_rustls::rustls::ServerConfig>,
        interval: Duration,
    ) -> Self {
        let loaded_at = newest_mtime(&files).unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        Self {
            listener: Mutex::new(None),
            acceptor: std::sync::Mutex::new(Some(TlsAcceptor::from(config))),
            secure: false,
            rotation: Some(Rotation {
                files,
                interval,
                loaded_at: std::sync::Mutex::new(loaded_at),
                last_check: std::sync::Mutex::new(Instant::now()),
            }),
        }
    }

    /// Reload rotated certificates when the files are newer than the last load.
    ///
    /// Called before each accept, throttled by the configured interval, so a bad
    /// rotation logs and keeps serving the previous material.
    fn reload_if_changed(&self) {
        let Some(rotation) = &self.rotation else {
            return;
        };
        {
            let mut last = rotation.last_check.lock().expect("tls reload clock");
            if last.elapsed() < rotation.interval {
                return;
            }
            *last = Instant::now();
        }
        let Some(newest) = newest_mtime(&rotation.files) else {
            return;
        };
        let loaded_at = *rotation.loaded_at.lock().expect("tls loaded clock");
        if newest <= loaded_at {
            return;
        }
        let files = &rotation.files;
        match crate::tls::load_server_config(&files.cert, &files.key, files.client_ca.as_deref()) {
            Ok(config) => {
                *self.acceptor.lock().expect("tls acceptor") = Some(TlsAcceptor::from(config));
                *rotation.loaded_at.lock().expect("tls loaded clock") = newest;
                eprintln!("[rpi-server] reloaded rotated TLS material");
            }
            Err(error) => {
                eprintln!("[rpi-server] TLS rotation rejected, keeping previous material: {error}")
            }
        }
    }

    /// Snapshot the current acceptor so a rotation cannot change it mid-handshake.
    fn current_acceptor(&self) -> Option<TlsAcceptor> {
        self.acceptor.lock().expect("tls acceptor").clone()
    }

    /// 客户端：连接 `wss://`，按系统根证书校验。
    pub fn secure() -> Self {
        Self {
            listener: Mutex::new(None),
            acceptor: std::sync::Mutex::new(None),
            secure: true,
            rotation: None,
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
        let server_name = tls_server_name(host)?;
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
        self.reload_if_changed();
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
        let stream: Box<dyn Stream> = match self.current_acceptor() {
            Some(acceptor) => Box::new(
                tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream))
                    .await
                    .map_err(|_| "tls handshake timed out".to_string())?
                    .map_err(|e| format!("tls handshake failed: {e}"))?,
            ),
            None => Box::new(stream),
        };
        let socket = tokio::time::timeout(HANDSHAKE_TIMEOUT, accept_async(stream))
            .await
            .map_err(|_| "websocket handshake timed out".to_string())?
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

    #[test]
    fn tls_server_names_accept_dns_and_ip_literals() {
        use tokio_rustls::rustls::pki_types::ServerName;
        assert!(matches!(
            tls_server_name("localhost"),
            Ok(ServerName::DnsName(_))
        ));
        assert!(matches!(
            tls_server_name("ide.example"),
            Ok(ServerName::DnsName(_))
        ));
        assert!(matches!(
            tls_server_name("127.0.0.1"),
            Ok(ServerName::IpAddress(_))
        ));
        assert!(matches!(
            tls_server_name("::1"),
            Ok(ServerName::IpAddress(_))
        ));
        assert!(tls_server_name("not a host").is_err());
    }

    /// Mint `(ca pem, leaf cert pem, leaf key pem)` for a fresh CA.
    fn mint_leaf() -> (String, String, String) {
        use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa};
        let ca_key = rcgen::KeyPair::generate().expect("ca key");
        let mut ca_params = CertificateParams::new(Vec::new()).expect("ca params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).expect("ca cert");

        let leaf_key = rcgen::KeyPair::generate().expect("leaf key");
        let mut leaf_params =
            CertificateParams::new(vec!["localhost".to_string()]).expect("leaf params");
        leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let leaf_cert = leaf_params
            .signed_by(&leaf_key, &ca_cert, &ca_key)
            .expect("leaf cert");
        (ca_cert.pem(), leaf_cert.pem(), leaf_key.serialize_pem())
    }

    fn trust(pem: &str) -> tokio_rustls::rustls::RootCertStore {
        use tokio_rustls::rustls::pki_types::CertificateDer;
        let mut roots = tokio_rustls::rustls::RootCertStore::empty();
        for certificate in rustls_pemfile::certs(&mut pem.as_bytes()) {
            roots
                .add(CertificateDer::from(certificate.expect("cert").to_vec()))
                .expect("trust anchor");
        }
        roots
    }

    #[tokio::test]
    async fn rotated_tls_material_is_used_by_the_next_handshake() {
        let first = mint_leaf();
        let second = mint_leaf();
        let dir = std::env::temp_dir().join(format!("rpi-server-rotate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");
        std::fs::write(&cert_path, &first.1).expect("cert");
        std::fs::write(&key_path, &first.2).expect("key");

        let files = crate::tls::TlsFiles::new(&cert_path, &key_path);
        let config = files.load().expect("tls config");
        let server = Arc::new(WebSocketTransport::with_tls_rotation(
            files,
            config,
            Duration::ZERO,
        ));
        let addr = free_addr();
        server.listen(&addr).await.expect("listen");

        // The first CA is trusted before rotation.
        let accepting = {
            let server = Arc::clone(&server);
            tokio::spawn(async move { server.accept().await })
        };
        let connector = tokio_tungstenite::Connector::Rustls(Arc::new(
            tokio_rustls::rustls::ClientConfig::builder()
                .with_root_certificates(trust(&first.0))
                .with_no_client_auth(),
        ));
        let _ = tokio_tungstenite::connect_async_tls_with_config(
            format!("wss://localhost:{}/", addr.rsplit(':').next().unwrap()),
            None,
            false,
            Some(connector),
        )
        .await
        .expect("first handshake");
        let _ = accepting.await;

        // Rotate the files, move their mtime forward, and let the next accept
        // reload them.
        std::fs::write(&cert_path, &second.1).expect("cert");
        std::fs::write(&key_path, &second.2).expect("key");
        let future = std::time::SystemTime::now() + Duration::from_secs(5);
        for path in [&cert_path, &key_path] {
            std::fs::File::options()
                .write(true)
                .open(path)
                .expect("open")
                .set_modified(future)
                .expect("set mtime");
        }

        let accepting = {
            let server = Arc::clone(&server);
            tokio::spawn(async move { server.accept().await })
        };
        let connector = tokio_tungstenite::Connector::Rustls(Arc::new(
            tokio_rustls::rustls::ClientConfig::builder()
                .with_root_certificates(trust(&second.0))
                .with_no_client_auth(),
        ));
        let rotated = tokio_tungstenite::connect_async_tls_with_config(
            format!("wss://localhost:{}/", addr.rsplit(':').next().unwrap()),
            None,
            false,
            Some(connector),
        )
        .await;
        assert!(
            rotated.is_ok(),
            "the rotated certificate must be served: {:?}",
            rotated.err()
        );
        let _ = accepting.await;
        let loaded_at = *server
            .rotation
            .as_ref()
            .expect("rotation")
            .loaded_at
            .lock()
            .expect("tls loaded clock");
        assert!(
            loaded_at >= future,
            "successful rotation must advance the loaded timestamp: {loaded_at:?} < {future:?}"
        );

        // The superseded CA no longer verifies the served certificate.
        let accepting = {
            let server = Arc::clone(&server);
            tokio::spawn(async move { server.accept().await })
        };
        let connector = tokio_tungstenite::Connector::Rustls(Arc::new(
            tokio_rustls::rustls::ClientConfig::builder()
                .with_root_certificates(trust(&first.0))
                .with_no_client_auth(),
        ));
        let stale = tokio_tungstenite::connect_async_tls_with_config(
            format!("wss://localhost:{}/", addr.rsplit(':').next().unwrap()),
            None,
            false,
            Some(connector),
        )
        .await;
        assert!(stale.is_err(), "the old CA must not verify the new leaf");
        let _ = accepting.await;

        let _ = std::fs::remove_dir_all(&dir);
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
