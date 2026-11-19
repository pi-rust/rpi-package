//! MCP Streamable HTTP transport.
//!
//! One HTTP session maps to one native session. Each session reuses the stdio
//! dispatcher: inbound frames arrive over a channel and every frame the
//! dispatcher writes is broadcast to the HTTP requests waiting on that session.

use crate::protocol::serve as serve_stdio;
use crate::protocol::MAX_FRAME;
use crate::{McpServer, ServerOptions};
use axum::{
    body::{Body, Bytes},
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, RawQuery, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use futures_util::{SinkExt, StreamExt};
use rpi_plugin_sdk::stdio::InputLine;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::AsyncWrite,
    net::TcpListener,
    sync::{broadcast, mpsc, watch},
};

/// Endpoint that carries both client requests and server streams.
pub const ENDPOINT: &str = "/mcp";
/// Private bidirectional transport sharing the same HTTP session.
pub const WS_ENDPOINT: &str = "/mcp/ws";
/// Bound for one HTTP JSON-RPC body.
pub const MAX_BODY: usize = MAX_FRAME;
/// Sessions a single listener serves at once.
pub const DEFAULT_MAX_SESSIONS: usize = 8;
/// Idle session lifetime before it is retired.
const SESSION_IDLE: Duration = Duration::from_secs(15 * 60);
/// How long one POST waits for its own responses.
const REQUEST_DEADLINE: Duration = Duration::from_secs(600);
/// Bounded wait when stopping a session.
const STOP_DEADLINE: Duration = Duration::from_secs(5);

/// Small access/parse failure, converted to a response at the handler boundary.
#[derive(Debug)]
struct HttpError(StatusCode, String);

impl HttpError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self(status, message.into())
    }

    fn into_response(self) -> Response {
        (self.0, self.1).into_response()
    }
}

/// Creates the native session backing one HTTP session.
pub type SessionFactory =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<McpServer, String>> + Send>> + Send + Sync>;

#[derive(Clone, Debug)]
pub struct HttpOptions {
    /// `host:port` to bind. Non-loopback hosts require a token.
    pub bind: String,
    /// Bearer token required from every request when set.
    pub token: Option<String>,
    /// Extra allowed `Origin`/`Host` values, for example `https://ide.example`.
    pub allow_origins: Vec<String>,
    pub max_sessions: usize,
}

impl HttpOptions {
    pub fn new(bind: impl Into<String>) -> Self {
        Self {
            bind: bind.into(),
            token: None,
            allow_origins: Vec::new(),
            max_sessions: DEFAULT_MAX_SESSIONS,
        }
    }
}

/// One HTTP session: its native server, inbound queue and outbound broadcast.
struct HttpSession {
    id: String,
    inbound: mpsc::Sender<String>,
    outbound: broadcast::Sender<Value>,
    stop: watch::Sender<bool>,
    last_used: AtomicU64,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl HttpSession {
    async fn spawn(server: McpServer) -> Arc<Self> {
        let server = Arc::new(server);
        let (inbound, mut inbound_rx) = mpsc::channel::<String>(64);
        let (outbound, _) = broadcast::channel::<Value>(256);
        let (stop, stop_rx) = watch::channel(false);
        let writer = BroadcastWriter {
            sender: outbound.clone(),
            buffer: Vec::new(),
        };
        let task = tokio::spawn(async move {
            let poll = move || {
                Ok(match inbound_rx.try_recv() {
                    Ok(line) => InputLine::Line(line),
                    Err(mpsc::error::TryRecvError::Empty) => InputLine::Pending,
                    Err(mpsc::error::TryRecvError::Disconnected) => InputLine::Eof,
                })
            };
            let _ = serve_stdio(server, poll, None, stop_rx, writer).await;
        });
        Arc::new(Self {
            id: uuid::Uuid::new_v4().to_string(),
            inbound,
            outbound,
            stop,
            last_used: AtomicU64::new(now_ms()),
            task: Mutex::new(Some(task)),
        })
    }

    fn touch(&self) {
        self.last_used.store(now_ms(), Ordering::Relaxed);
    }

    fn idle_for(&self) -> Duration {
        Duration::from_millis(now_ms().saturating_sub(self.last_used.load(Ordering::Relaxed)))
    }

    async fn shutdown(&self) {
        let _ = self.stop.send(true);
        let task = self.task.lock().expect("session task").take();
        if let Some(task) = task {
            let _ = tokio::time::timeout(STOP_DEADLINE, task).await;
        }
    }
}

/// Splits the dispatcher's byte stream into JSON frames on the session bus.
struct BroadcastWriter {
    sender: broadcast::Sender<Value>,
    buffer: Vec<u8>,
}

impl AsyncWrite for BroadcastWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.buffer.extend_from_slice(buf);
        while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=end).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]).to_string();
            if let Ok(value) = serde_json::from_str::<Value>(text.trim()) {
                let _ = self.sender.send(value);
            }
        }
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[derive(Clone)]
pub struct AppState {
    sessions: Arc<Mutex<HashMap<String, Arc<HttpSession>>>>,
    factory: SessionFactory,
    token: Option<String>,
    allow_origins: Arc<Vec<String>>,
    max_sessions: usize,
}

impl AppState {
    fn session(&self, id: &str) -> Option<Arc<HttpSession>> {
        self.sessions.lock().expect("sessions").get(id).cloned()
    }

    fn count(&self) -> usize {
        self.sessions.lock().expect("sessions").len()
    }

    async fn create(&self) -> Result<Arc<HttpSession>, HttpError> {
        if self.count() >= self.max_sessions {
            return Err(HttpError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too many MCP sessions; close one with DELETE",
            ));
        }
        let server = (self.factory)().await.map_err(|error| {
            eprintln!("[rpi-mcp] failed to start native session: {error}");
            HttpError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "could not start a native session",
            )
        })?;
        let session = HttpSession::spawn(server).await;
        self.sessions
            .lock()
            .expect("sessions")
            .insert(session.id.clone(), Arc::clone(&session));
        Ok(session)
    }

    async fn remove(&self, id: &str) -> bool {
        let session = self.sessions.lock().expect("sessions").remove(id);
        match session {
            Some(session) => {
                session.shutdown().await;
                true
            }
            None => false,
        }
    }

    async fn shutdown_all(&self) {
        let sessions: Vec<Arc<HttpSession>> = self
            .sessions
            .lock()
            .expect("sessions")
            .drain()
            .map(|(_, session)| session)
            .collect();
        for session in sessions {
            session.shutdown().await;
        }
    }

    /// Reject cross-origin browser access unless the origin is explicitly allowed.
    fn check_access(&self, headers: &HeaderMap) -> Result<(), HttpError> {
        if let Some(origin) = header(headers, "origin") {
            if !self.origin_allowed(origin) {
                return Err(HttpError::new(StatusCode::FORBIDDEN, "origin not allowed"));
            }
        }
        if let Some(host) = header(headers, "host") {
            if !self.host_allowed(host) {
                return Err(HttpError::new(StatusCode::FORBIDDEN, "host not allowed"));
            }
        }
        if let Some(token) = &self.token {
            let presented = header(headers, "authorization")
                .and_then(|value| value.strip_prefix("Bearer "))
                .map(str::trim)
                .unwrap_or_default();
            if presented != token {
                return Err(HttpError::new(
                    StatusCode::UNAUTHORIZED,
                    "invalid bearer token",
                ));
            }
        }
        Ok(())
    }

    fn origin_allowed(&self, origin: &str) -> bool {
        if origin.trim() == "null" {
            return false;
        }
        if self.allow_origins.iter().any(|value| value == origin) {
            return true;
        }
        authority_host(origin).is_some_and(is_loopback_host)
    }

    fn host_allowed(&self, host: &str) -> bool {
        // `Host` carries the port, so compare host names, not raw values.
        let Some(name) = authority_host(host) else {
            return false;
        };
        if is_loopback_host(name) {
            return true;
        }
        self.allow_origins
            .iter()
            .any(|allowed| authority_host(allowed) == Some(name))
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// Strip scheme and path from an authority or URL and return its host.
fn authority_host(value: &str) -> Option<&str> {
    let value = value.trim();
    let after_scheme = match value.split_once("://") {
        Some((_, rest)) => rest,
        None => value,
    };
    let authority = after_scheme.split('/').next()?;
    if authority.is_empty() {
        return None;
    }
    // Drop userinfo and port, then unwrap IPv6 brackets.
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next()?
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    Some(host)
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1")
        || host
            .parse::<std::net::IpAddr>()
            .map(|address| address.is_loopback())
            .unwrap_or(false)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

fn id_key(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        other => other.to_string(),
    }
}

/// Parse one POST body into JSON-RPC frames.
fn parse_frames(body: &[u8]) -> Result<Vec<Value>, HttpError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| HttpError::new(StatusCode::BAD_REQUEST, "body is not valid JSON"))?;
    match value {
        Value::Array(frames) => {
            if frames.is_empty() || frames.iter().any(|frame| !frame.is_object()) {
                return Err(HttpError::new(
                    StatusCode::BAD_REQUEST,
                    "empty or malformed batch",
                ));
            }
            Ok(frames)
        }
        frame if frame.is_object() => Ok(vec![frame]),
        _ => Err(HttpError::new(
            StatusCode::BAD_REQUEST,
            "expected a JSON-RPC object",
        )),
    }
}

fn is_request(frame: &Value) -> bool {
    frame.get("method").is_some() && frame.get("id").is_some()
}

fn wants_stream(frames: &[Value], headers: &HeaderMap) -> bool {
    let accepts_sse = header(headers, "accept")
        .map(|value| value.contains("text/event-stream"))
        .unwrap_or(false);
    accepts_sse
        && frames.iter().any(|frame| {
            frame.get("method").and_then(Value::as_str) == Some("tools/call")
                && frame.pointer("/params/name").and_then(Value::as_str) == Some("rpi_prompt")
        })
}

async fn handle_post(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(error) = state.check_access(&headers) {
        return error.into_response();
    }
    let frames = match parse_frames(&body) {
        Ok(frames) => frames,
        Err(error) => return error.into_response(),
    };
    if let Some(version) = header(&headers, "mcp-protocol-version") {
        if !crate::protocol::supports_protocol(version) {
            return (
                StatusCode::BAD_REQUEST,
                format!("unsupported MCP-Protocol-Version: {version}"),
            )
                .into_response();
        }
    }

    let initializing = frames
        .iter()
        .any(|frame| frame.get("method").and_then(Value::as_str) == Some("initialize"));
    let session_header = header(&headers, "mcp-session-id").map(str::to_string);
    let session = match session_header {
        // A known session is reused; re-initializing an existing session would
        // silently reset native state, so it starts a fresh one instead.
        Some(id) if !initializing => match state.session(&id) {
            Some(session) => session,
            None => {
                return (StatusCode::NOT_FOUND, "unknown or expired MCP session").into_response()
            }
        },
        Some(_) => match state.create().await {
            Ok(session) => session,
            Err(error) => return error.into_response(),
        },
        None if initializing => match state.create().await {
            Ok(session) => session,
            Err(error) => return error.into_response(),
        },
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "initialize must create the session first",
            )
                .into_response()
        }
    };
    session.touch();

    let ids: HashSet<String> = frames
        .iter()
        .filter(|frame| is_request(frame))
        .map(|frame| id_key(&frame["id"]))
        .collect();
    let mut outbound = session.outbound.subscribe();
    for frame in &frames {
        if session.inbound.send(frame.to_string()).await.is_err() {
            return (StatusCode::SERVICE_UNAVAILABLE, "session is shutting down").into_response();
        }
    }
    if ids.is_empty() {
        // Notifications and responses only: 202 with no body.
        return StatusCode::ACCEPTED.into_response();
    }

    if wants_stream(&frames, &headers) {
        return stream_responses(outbound, ids);
    }
    let responses = gather(&mut outbound, ids).await;
    let body = match responses.len() {
        0 => Value::Null,
        1 => responses.into_iter().next().unwrap_or(Value::Null),
        _ => Value::Array(responses),
    };
    let mut response = axum::Json(body).into_response();
    response.headers_mut().insert(
        header::HeaderName::from_static("mcp-session-id"),
        header::HeaderValue::from_str(&session.id)
            .unwrap_or_else(|_| header::HeaderValue::from_static("invalid")),
    );
    response
}

/// Wait for one response per requested id, ignoring unrelated frames.
async fn gather(outbound: &mut broadcast::Receiver<Value>, ids: HashSet<String>) -> Vec<Value> {
    let mut pending = ids;
    let mut responses = Vec::new();
    let deadline = tokio::time::Instant::now() + REQUEST_DEADLINE;
    while !pending.is_empty() {
        let frame = match tokio::time::timeout_at(deadline, outbound.recv()).await {
            Ok(Ok(frame)) => frame,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) => break,
            Err(_) => break,
        };
        if frame.get("method").is_some() {
            continue;
        }
        let Some(id) = frame.get("id") else {
            continue;
        };
        let key = id_key(id);
        if pending.remove(&key) {
            responses.push(frame);
        }
    }
    responses
}

fn stream_responses(outbound: broadcast::Receiver<Value>, ids: HashSet<String>) -> Response {
    let (sender, receiver) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        let mut outbound = outbound;
        let mut pending = ids;
        let deadline = tokio::time::Instant::now() + REQUEST_DEADLINE;
        while !pending.is_empty() {
            let frame = match tokio::time::timeout_at(deadline, outbound.recv()).await {
                Ok(Ok(frame)) => frame,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => break,
                Err(_) => break,
            };
            if frame.get("method").is_some() {
                // Progress and other server messages stream as they arrive.
                let _ = sender.send(frame);
                continue;
            }
            let Some(id) = frame.get("id") else {
                continue;
            };
            if pending.remove(&id_key(id)) {
                let _ = sender.send(frame);
            }
        }
    });
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|value| {
            (
                Ok::<Bytes, Infallible>(Bytes::from(format!("event: message\ndata: {value}\n\n"))),
                receiver,
            )
        })
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn handle_delete(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = state.check_access(&headers) {
        return error.into_response();
    }
    let Some(id) = header(&headers, "mcp-session-id").map(str::to_string) else {
        return (StatusCode::BAD_REQUEST, "mcp-session-id is required").into_response();
    };
    if state.remove(&id).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (StatusCode::NOT_FOUND, "unknown or expired MCP session").into_response()
    }
}

async fn handle_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = state.check_access(&headers) {
        return error.into_response();
    }
    // No standalone server stream: updates are delivered inside POST streams.
    let mut response = (
        StatusCode::METHOD_NOT_ALLOWED,
        "this server does not offer a standalone GET stream",
    )
        .into_response();
    response.headers_mut().insert(
        header::ALLOW,
        header::HeaderValue::from_static("POST, DELETE"),
    );
    response
}

async fn reap_idle(state: AppState) {
    let mut ticker = tokio::time::interval(Duration::from_secs(60));
    loop {
        ticker.tick().await;
        let expired: Vec<String> = state
            .sessions
            .lock()
            .expect("sessions")
            .iter()
            .filter(|(_, session)| session.idle_for() >= SESSION_IDLE)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            eprintln!("[rpi-mcp] retiring idle MCP session");
            state.remove(&id).await;
        }
    }
}

fn router(state: AppState) -> Router {
    Router::new()
        .route(
            ENDPOINT,
            post(handle_post).get(handle_get).delete(handle_delete),
        )
        // Private extension: MCP clients use POST, but our own clients can keep
        // one bidirectional socket on the same native session.
        .route(WS_ENDPOINT, get(handle_websocket))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(state)
}

/// Upgrade path for the private WebSocket transport (not an MCP transport).
async fn handle_websocket(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    upgrade: WebSocketUpgrade,
) -> Response {
    if let Err(error) = state.check_access(&headers) {
        return error.into_response();
    }
    let requested = query
        .as_deref()
        .and_then(|query| query_param(query, "session"));
    let session = match requested {
        Some(id) => match state.session(&id) {
            Some(session) => session,
            None => {
                return (StatusCode::NOT_FOUND, "unknown or expired MCP session").into_response()
            }
        },
        None => match state.create().await {
            Ok(session) => session,
            Err(error) => return error.into_response(),
        },
    };
    session.touch();
    let outbound = session.outbound.subscribe();
    let inbound = session.inbound.clone();
    let session_id = session.id.clone();
    upgrade
        .max_message_size(MAX_BODY)
        .on_upgrade(move |socket| pump(socket, session_id, inbound, outbound, session))
}

/// Bridge one socket onto the session's dispatcher queues.
async fn pump(
    socket: WebSocket,
    session_id: String,
    inbound: mpsc::Sender<String>,
    mut outbound: broadcast::Receiver<Value>,
    session: Arc<HttpSession>,
) {
    let (mut sink, mut stream) = socket.split();
    // Announce the handle first so a client can reattach after a reconnect.
    let hello = json!({
        "jsonrpc": "2.0",
        "method": "rpi/session",
        "params": {"sessionId": session_id},
    });
    if sink
        .send(Message::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    session.touch();
                    if inbound.send(text.to_string()).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Binary(bytes))) => {
                    session.touch();
                    if let Ok(text) = String::from_utf8(bytes.to_vec()) {
                        let _ = inbound.send(text).await;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => continue,
            },
            outgoing = outbound.recv() => match outgoing {
                Ok(value) => {
                    if sink.send(Message::Text(value.to_string().into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
    let _ = sink.close().await;
}

fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name && !value.is_empty()).then(|| value.to_string())
    })
}

/// Serve Streamable HTTP on a bound listener until the task is dropped.
pub async fn serve_listener(
    listener: TcpListener,
    options: HttpOptions,
    factory: SessionFactory,
) -> Result<(), String> {
    let state = AppState {
        sessions: Arc::new(Mutex::new(HashMap::new())),
        factory,
        token: options.token.clone(),
        allow_origins: Arc::new(options.allow_origins.clone()),
        max_sessions: options.max_sessions.max(1),
    };
    let reaper = tokio::spawn(reap_idle(state.clone()));
    let result = axum::serve(listener, router(state.clone()))
        .await
        .map_err(|error| error.to_string());
    reaper.abort();
    state.shutdown_all().await;
    result
}

/// Bind `options.bind` and serve until the task is dropped.
pub async fn serve(options: HttpOptions, server: ServerOptions) -> Result<(), String> {
    let factory: SessionFactory = Arc::new(move || {
        let server = server.clone();
        Box::pin(async move { McpServer::open(server).await })
    });
    serve_with_factory(options, factory).await
}

pub async fn serve_with_factory(
    options: HttpOptions,
    factory: SessionFactory,
) -> Result<(), String> {
    let address: SocketAddr = options.bind.parse().map_err(|_| {
        format!(
            "invalid --http address `{}`; expected host:port",
            options.bind
        )
    })?;
    if !address.ip().is_loopback() && options.token.is_none() {
        return Err(format!(
            "refusing to bind {address} without --token (MCP HTTP has no transport encryption)"
        ));
    }
    let listener = TcpListener::bind(address)
        .await
        .map_err(|error| format!("cannot bind {address}: {error}"))?;
    let local = listener
        .local_addr()
        .map_err(|error| format!("cannot read the bound address: {error}"))?;
    eprintln!("[rpi-mcp] MCP HTTP listening on http://{local}{ENDPOINT}");
    serve_listener(listener, options, factory).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                header::HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    fn state(token: Option<&str>, origins: &[&str]) -> AppState {
        AppState {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            factory: Arc::new(|| Box::pin(async { Err("unused".to_string()) })),
            token: token.map(str::to_string),
            allow_origins: Arc::new(origins.iter().map(|value| value.to_string()).collect()),
            max_sessions: 2,
        }
    }

    #[test]
    fn loopback_hosts_and_origins_are_allowed() {
        let state = state(None, &[]);
        assert!(state.host_allowed("127.0.0.1:3030"));
        assert!(state.host_allowed("localhost:3030"));
        assert!(state.host_allowed("[::1]:3030"));
        assert!(!state.host_allowed("evil.example"));
        assert!(state.origin_allowed("http://localhost:5173"));
        assert!(state.origin_allowed("http://127.0.0.1:3030"));
        assert!(!state.origin_allowed("https://evil.example"));
        assert!(!state.origin_allowed("null"));
    }

    #[test]
    fn explicit_origins_extend_the_loopback_policy() {
        let state = state(None, &["https://ide.example"]);
        assert!(state.origin_allowed("https://ide.example"));
        assert!(state.host_allowed("ide.example"));
        assert!(!state.origin_allowed("https://other.example"));
    }

    #[test]
    fn bearer_token_is_required_when_configured() {
        let state = state(Some("secret"), &[]);
        assert!(state
            .check_access(&headers(&[("host", "127.0.0.1:1")]))
            .is_err());
        assert!(state
            .check_access(&headers(&[
                ("host", "127.0.0.1:1"),
                ("authorization", "Bearer secret")
            ]))
            .is_ok());
        assert!(state
            .check_access(&headers(&[
                ("host", "127.0.0.1:1"),
                ("authorization", "Bearer wrong")
            ]))
            .is_err());
    }

    #[test]
    fn cross_origin_requests_are_rejected() {
        let state = state(None, &[]);
        let error = state
            .check_access(&headers(&[
                ("host", "127.0.0.1:1"),
                ("origin", "https://evil.example"),
            ]))
            .unwrap_err();
        assert_eq!(error.0, StatusCode::FORBIDDEN);
    }

    #[test]
    fn classifies_bodies_and_streaming_requests() {
        let frames = parse_frames(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
        assert!(is_request(&frames[0]));
        assert!(parse_frames(br#"[]"#).is_err());
        assert!(parse_frames(b"not json").is_err());

        let sse = headers(&[("accept", "application/json, text/event-stream")]);
        let prompt = parse_frames(
            br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"rpi_prompt"}}"#,
        )
        .unwrap();
        assert!(wants_stream(&prompt, &sse));
        assert!(!wants_stream(
            &prompt,
            &headers(&[("accept", "application/json")])
        ));

        let list = parse_frames(br#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#).unwrap();
        assert!(!wants_stream(&list, &sse));

        let notification = parse_frames(
            br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#,
        )
        .unwrap();
        assert!(!is_request(&notification[0]));
    }

    #[test]
    fn authority_parsing_handles_ports_userinfo_and_ipv6() {
        assert_eq!(authority_host("http://localhost:5173"), Some("localhost"));
        assert_eq!(
            authority_host("https://user:pw@ide.example:443/x"),
            Some("ide.example")
        );
        assert_eq!(authority_host("[::1]:3030"), Some("::1"));
        assert_eq!(authority_host("127.0.0.1:8080"), Some("127.0.0.1"));
    }
}
