//! Pinned v1 Streamable HTTP RFD profile. No event IDs or live resume.
//! Identity is bound to the originating HTTP/2 TCP peer, not just a bearer ID.
use super::*;
use axum::{body::to_bytes, extract::{ConnectInfo, FromRequestParts, Request}, http::{Method, Version}, response::sse::{Event, KeepAlive, Sse}, routing::any, Json};
use std::{collections::{HashMap, HashSet}, convert::Infallible};
use tokio::sync::{Mutex, oneshot};
use std::{pin::Pin, task::{Context, Poll}};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

// A generation belongs to the accepted socket, not its reusable peer address.
#[derive(Clone, Debug)]
struct Network { generation: uuid::Uuid, closed: watch::Receiver<bool> }
struct Socket { io: tokio::net::TcpStream, closed: watch::Sender<bool> }
impl Drop for Socket { fn drop(&mut self) { let _ = self.closed.send(true); } }
impl AsyncRead for Socket {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.io).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Err(_))) || matches!(result, Poll::Ready(Ok(()))) && before == buf.filled().len() { let _ = self.closed.send(true); }
        result
    }
}
impl AsyncWrite for Socket {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> { Pin::new(&mut self.io).poll_write(cx, buf) }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> { Pin::new(&mut self.io).poll_flush(cx) }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> { Pin::new(&mut self.io).poll_shutdown(cx) }
}
struct Listener(TcpListener);
impl axum::serve::Listener for Listener {
    type Io = Socket;
    type Addr = Network;
    async fn accept(&mut self) -> (Socket, Network) {
        let (io, _) = axum::serve::Listener::accept(&mut self.0).await;
        let (closed, rx) = watch::channel(false);
        (Socket { io, closed }, Network { generation: uuid::Uuid::new_v4(), closed: rx })
    }
    fn local_addr(&self) -> std::io::Result<Network> {
        let (_, closed) = watch::channel(false);
        Ok(Network { generation: uuid::Uuid::nil(), closed })
    }
}
impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, Listener>> for Network {
    fn connect_info(stream: axum::serve::IncomingStream<'_, Listener>) -> Self { stream.remote_addr().clone() }
}

const ACTIVE: usize = 32;
const HEADERS: usize = 16 * 1024;
type Reply = oneshot::Sender<Result<Response, StatusCode>>;
struct Command { method: Method, headers: HeaderMap, value: Option<Value>, reply: Reply }
#[derive(Clone)]
struct Link { generation: uuid::Uuid, tx: mpsc::Sender<Command> }
#[derive(Clone)]
struct HttpServer { security: Server, links: Arc<Mutex<HashMap<String, Link>>>, claims: Arc<Mutex<HashMap<String, String>>>, requests: Arc<Semaphore> }

/// Explicit opt-in listener. All dispatchers are joined before this returns.
pub async fn serve(listener: TcpListener, config: Config, stop: watch::Receiver<bool>) -> std::io::Result<()> {
    let security = Server { config, address: listener.local_addr()?, slots: Arc::new(Semaphore::new(MAX_CONNECTIONS)), stop: stop.clone() };
    let slots = security.slots.clone();
    let state = HttpServer { security, links: Arc::new(Mutex::new(HashMap::new())), claims: Arc::new(Mutex::new(HashMap::new())), requests: Arc::new(Semaphore::new(64)) };
    let mut shutdown = stop;
    axum::serve(Listener(listener), Router::new().route("/acp", any(handle)).with_state(state).into_make_service_with_connect_info::<Network>())
        .with_graceful_shutdown(async move { if !*shutdown.borrow() { let _ = shutdown.changed().await; } }).await?;
    let _all = slots.acquire_many(MAX_CONNECTIONS as u32).await;
    Ok(())
}
fn error(status: StatusCode) -> Response {
    let mut response = status.into_response();
    if status == StatusCode::UNAUTHORIZED { response.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer")); }
    response
}
async fn handle(State(server): State<HttpServer>, ConnectInfo(network): ConnectInfo<Network>, request: Request) -> Response {
    if request.headers().contains_key(header::UPGRADE) {
        let (mut parts, _) = request.into_parts();
        if parts.uri.query().is_some() { return error(StatusCode::BAD_REQUEST); }
        if let Err(status) = authorize(&server.security, &parts.headers) { return error(status); }
        return match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
            Ok(ws) => super::upgrade(State(server.security), parts.headers, ws).await,
            Err(rejection) => rejection.into_response(),
        };
    }
    match checked(server, network, request).await { Ok(response) => response, Err(status) => error(status) }
}
async fn checked(server: HttpServer, network: Network, request: Request) -> Result<Response, StatusCode> {
    let _permit = server.requests.clone().try_acquire_owned().map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    let (parts, body) = request.into_parts();
    let mut headers = parts.headers;
    // HTTP/2 uses :authority; Host, when supplied, must agree with it.
    if let Some(authority) = parts.uri.authority() {
        if headers.contains_key("host") && single_header(&headers, "host") != Some(authority.as_str()) { return Err(StatusCode::FORBIDDEN); }
        if !headers.contains_key("host") { headers.insert(header::HOST, HeaderValue::from_str(authority.as_str()).map_err(|_| StatusCode::FORBIDDEN)?); }
    }
    if headers.iter().map(|(k,v)| k.as_str().len()+v.len()).sum::<usize>() > HEADERS { return Err(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE); }
    authorize(&server.security, &headers)?;
    if *server.security.stop.borrow() { return Err(StatusCode::SERVICE_UNAVAILABLE); }
    if parts.uri.query().is_some() { return Err(StatusCode::BAD_REQUEST); }
    if parts.version != Version::HTTP_2 { return Err(StatusCode::HTTP_VERSION_NOT_SUPPORTED); }
    // RFD explicitly defers these wire contracts: never pretend to resume.
    if headers.contains_key("last-event-id") || headers.contains_key("acp-protocol-version") { return Err(StatusCode::NOT_IMPLEMENTED); }
    for name in ["acp-connection-id", "acp-session-id", "content-type", "accept"] {
        if headers.contains_key(name) && single_header(&headers,name).is_none() { return Err(StatusCode::BAD_REQUEST); }
    }
    let value = if parts.method == Method::POST {
        if single_header(&headers,"content-type").is_none_or(|v| v.split(';').next().unwrap_or("").trim() != "application/json") { return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE); }
        let bytes = tokio::time::timeout(WRITE_TIMEOUT, to_bytes(body, MAX_MESSAGE)).await.map_err(|_| StatusCode::REQUEST_TIMEOUT)?.map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| StatusCode::BAD_REQUEST)?;
        if value.is_array() { return Err(StatusCode::NOT_IMPLEMENTED); }
        if !value.is_object() || value["jsonrpc"] != "2.0" { return Err(StatusCode::BAD_REQUEST); }
        Some(value)
    } else { None };
    if !matches!(parts.method, Method::GET | Method::POST | Method::DELETE) { return Err(StatusCode::METHOD_NOT_ALLOWED); }
    if parts.method == Method::GET && !single_header(&headers,"accept").is_some_and(|v| v.split(',').any(|v| v.trim() == "text/event-stream")) { return Err(StatusCode::NOT_ACCEPTABLE); }
    let initialize = value.as_ref().is_some_and(|v| v["method"] == "initialize");
    let (reply, receive) = oneshot::channel();
    let command = Command { method: parts.method, headers: headers.clone(), value, reply };
    if initialize {
        if headers.contains_key("acp-connection-id") || headers.contains_key("acp-session-id") { return Err(StatusCode::BAD_REQUEST); }
        if command.value.as_ref().and_then(key).is_none() { return Err(StatusCode::BAD_REQUEST); }
        if command.value.as_ref().is_some_and(|v| v["params"]["protocolVersion"].as_u64().is_some_and(|v| v >= 2)) {
            return Ok(Json(serde_json::json!({"jsonrpc":"2.0", "id":command.value.as_ref().unwrap()["id"], "error":{"code":-32602,"message":"Streamable HTTP supports ACP v1 only; v2 durability is not implemented"}})).into_response());
        }
        let permit = server.security.slots.clone().try_acquire_owned().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = mpsc::channel(32);
        server.links.lock().await.insert(id.clone(), Link { generation: network.generation, tx });
        tokio::spawn(async move { let _permit = permit; actor(server.clone(), id.clone(), network.closed, command, rx).await; server.links.lock().await.remove(&id); });
    } else {
        let id = single_header(&headers,"acp-connection-id").ok_or(StatusCode::BAD_REQUEST)?;
        let link = server.links.lock().await.get(id).cloned().ok_or(StatusCode::NOT_FOUND)?;
        if link.generation != network.generation { return Err(StatusCode::NOT_FOUND); }
        link.tx.try_send(command).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    }
    tokio::time::timeout(WRITE_TIMEOUT, receive).await.map_err(|_| StatusCode::GATEWAY_TIMEOUT)?.map_err(|_| StatusCode::GONE)?
}

struct StreamSlot { tx: mpsc::Sender<String>, rx: Option<mpsc::Receiver<String>> }
impl StreamSlot {
    fn new() -> Self { let (tx,rx) = mpsc::channel(8); Self { tx, rx: Some(rx) } }
    fn open(&mut self) -> Result<Response, StatusCode> {
        let rx = self.rx.take().ok_or(StatusCode::CONFLICT)?;
        let stream = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|text| (Ok::<_,Infallible>(Event::default().data(text)), rx)) });
        Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(5))).into_response())
    }
    fn send(&self, text: String) -> bool { self.tx.try_send(text).is_ok() }
}
fn key(v: &Value) -> Option<String> { let id = v.get("id")?; if id.is_string() || id.is_i64() || id.is_u64() { Some(id.to_string()) } else { None } }

async fn actor(server: HttpServer, id: String, mut network_closed: watch::Receiver<bool>, first: Command, mut commands: mpsc::Receiver<Command>) {
    let (input, rx) = mpsc::channel(32);
    let (output, reader) = tokio::io::duplex(64 * 1024);
    let mut reader = BufReader::new(reader).take((MAX_MESSAGE+1) as u64);
    let (stop, stop_rx) = watch::channel(false);
    let mut dispatcher = tokio::spawn(crate::protocol::run_with_io(stop_rx, rx, None, output));
    let mut shutdown = server.security.stop.clone();
    let mut connection = StreamSlot::new();
    let mut sessions: HashMap<String, StreamSlot> = HashMap::new();
    // Client IDs are connection-global. Correlation, never completion order,
    // determines the response stream. Server IDs have their own namespace.
    let mut pending: HashMap<String, (Option<String>, bool, Option<String>)> = HashMap::new();
    let mut attached = HashSet::new();
    // Unknown roots are permitted only while new/fork results are outstanding.
    // Their bounded streams reserve global claims, but never grant admission.
    let mut opening_unknown = HashSet::new();
    let mut provisional = HashSet::new();
    let mut calls: HashMap<String, Option<String>> = HashMap::new();
    let mut used = HashSet::new();
    let mut init_reply = Some(first.reply);
    let init = first.value.unwrap_or(Value::Null);
    let init_key = key(&init);
    let mut count = 1;
    let mut opens = 0;
    let mut line = String::new();
    let lifetime = tokio::time::sleep(server.security.config.timeouts.lifetime);
    tokio::pin!(lifetime);
    let idle = tokio::time::sleep(server.security.config.timeouts.initial);
    tokio::pin!(idle);
    if let Some(k) = &init_key { used.insert(k.clone()); }
    if init_key.is_none() || input.try_send(init.to_string()).is_err() { let _ = stop.send(true); }
    else {
    loop {
        if *network_closed.borrow() || *shutdown.borrow() || connection.tx.is_closed() || sessions.values().any(|s| s.tx.is_closed()) { break; }
        tokio::select! {
            _ = network_closed.changed() => break,
            _ = shutdown.changed() => break,
            _ = connection.tx.closed() => break,
            _ = &mut lifetime => break,
            _ = &mut idle => break,
            command = commands.recv(), if init_reply.is_none() => {
                let Some(command) = command else { break; };
                let Command { method, headers, value, reply } = command;
                let scope = single_header(&headers,"acp-session-id").map(str::to_owned);
                if method == Method::DELETE {
                    if scope.is_some() { let _ = reply.send(Err(StatusCode::BAD_REQUEST)); continue; }
                    let _ = reply.send(Ok(StatusCode::ACCEPTED.into_response())); break;
                }
                if method == Method::GET {
                    let result = match scope {
                        Some(s) => {
                            if provisional.contains(&s) { let _ = reply.send(Err(StatusCode::NOT_FOUND)); continue; }
                            if !sessions.contains_key(&s) {
                                let mut claims = server.claims.lock().await;
                                if claims.get(&s).is_some_and(|owner| owner != &id) { let _ = reply.send(Err(StatusCode::CONFLICT)); continue; }
                                if crate::history::saved_session(&s).is_err() { let _ = reply.send(Err(StatusCode::NOT_FOUND)); continue; }
                                claims.insert(s.clone(), id.clone());
                                sessions.insert(s.clone(), StreamSlot::new());
                            }
                            sessions.get_mut(&s).unwrap().open()
                        },
                        None => connection.open()
                    };
                    let _ = reply.send(result);
                    idle.as_mut().reset(tokio::time::Instant::now()+server.security.config.timeouts.idle);
                    continue;
                }
                count += 1;
                if count > MAX_MESSAGES { let _ = reply.send(Err(StatusCode::TOO_MANY_REQUESTS)); break; }
                let value = value.unwrap_or(Value::Null);
                let method = value["method"].as_str();
                let opening = matches!(method, Some("session/new" | "session/load" | "session/resume" | "session/fork"));
                let body_scope = value["params"]["sessionId"].as_str();
                let mut invalid = false;
                let mut attach = None;
                if opening {
                    if key(&value).is_none() { invalid = true; }
                    opens += 1;
                    if opens > MAX_OPENS { let _ = reply.send(Err(StatusCode::TOO_MANY_REQUESTS)); break; }
                    // Explicit load, not GET or an arbitrary session ID, may
                    // attach persisted history to this fresh dispatcher.
                    if matches!(method, Some("session/load" | "session/resume")) {
                        if scope.as_deref() != body_scope || body_scope.is_none() || key(&value).is_none() { invalid = true; }
                        else { attach = scope.clone(); }
                    } else if method == Some("session/fork") {
                        if scope.as_deref() != body_scope || scope.as_ref().is_none_or(|s| !attached.contains(s)) { invalid = true; }
                    } else if scope.is_some() { invalid = true; }
                } else if let Some(s) = &scope {
                    if !attached.contains(s) || body_scope.is_some_and(|b| b != s) { invalid = true; }
                } else if body_scope.is_some() || method.is_some_and(|m| m.starts_with("session/") && m != "session/list") { invalid = true; }
                if method.is_none() {
                    match key(&value).and_then(|k| calls.get(&k).cloned()) { Some(expected) if expected == scope => {}, _ => invalid = true }
                    if value.get("result").is_some() == value.get("error").is_some() { invalid = true; }
                } else if let Some(k) = key(&value) {
                    if used.contains(&k) || pending.len() >= ACTIVE { invalid = true; }
                } else if value.get("id").is_some() { invalid = true; }
                if invalid { let _ = reply.send(Err(StatusCode::BAD_REQUEST)); continue; }
                let mut rollback = None;
                if let Some(s) = attach {
                    let mut claims = server.claims.lock().await;
                    if claims.get(&s).is_some_and(|owner| owner != &id) { let _ = reply.send(Err(StatusCode::CONFLICT)); continue; }
                    if !attached.contains(&s) {
                        if crate::history::saved_session(&s).is_err() { let _ = reply.send(Err(StatusCode::NOT_FOUND)); continue; }
                        if pending.values().any(|(_, _, loading)| loading.as_ref() == Some(&s)) { let _ = reply.send(Err(StatusCode::CONFLICT)); continue; }
                        claims.insert(s.clone(), id.clone());
                        sessions.entry(s.clone()).or_insert_with(StreamSlot::new);
                        rollback = Some(s);
                    }
                }
                if let Some(k) = key(&value) {
                    if method.is_none() { calls.remove(&k); }
                    else {
                        if matches!(method, Some("session/new" | "session/fork")) { opening_unknown.insert(k.clone()); }
                        used.insert(k.clone()); pending.insert(k, (if opening { None } else { scope.clone() }, opening, rollback));
                    }
                }
                if input.try_send(value.to_string()).is_err() { let _ = reply.send(Err(StatusCode::SERVICE_UNAVAILABLE)); break; }
                let _ = reply.send(Ok(StatusCode::ACCEPTED.into_response()));
                idle.as_mut().reset(tokio::time::Instant::now()+server.security.config.timeouts.idle);
            }
            result = reader.read_line(&mut line) => {
                if !matches!(result, Ok(n) if n > 0) || line.len() > MAX_MESSAGE || !line.ends_with('\n') { break; }
                let Ok(mut value) = serde_json::from_str::<Value>(&line) else { break; };
                if let Some(reply) = init_reply.take() {
                    if key(&value) != init_key { let _ = reply.send(Err(StatusCode::BAD_GATEWAY)); break; }
                    if value.get("error").is_some() { let _ = reply.send(Ok(Json(value).into_response())); break; }
                    if value.get("result").is_none() { let _ = reply.send(Err(StatusCode::BAD_GATEWAY)); break; }
                    value["result"]["connectionId"] = Value::String(id.clone());
                    let mut response = Json(value).into_response();
                    response.headers_mut().insert("acp-connection-id", HeaderValue::from_str(&id).unwrap());
                    if reply.send(Ok(response)).is_err() { break; }
                    idle.as_mut().reset(tokio::time::Instant::now()+server.security.config.timeouts.initial);
                } else {
                    let scope = if value.get("method").is_some() {
                        let scope = value["params"]["sessionId"].as_str().map(str::to_owned);
                        if let Some(s) = &scope {
                            if provisional.contains(s) && key(&value).is_some() { break; }
                            if !sessions.contains_key(s) {
                                // This is not child-session discovery: only notifications
                                // for a pending root allocation may reserve an unknown ID.
                                if key(&value).is_some() || opening_unknown.is_empty() || provisional.len() >= 8 { break; }
                                let mut claims = server.claims.lock().await;
                                if claims.get(s).is_some_and(|owner| owner != &id) { break; }
                                claims.insert(s.clone(), id.clone());
                                sessions.insert(s.clone(), StreamSlot::new());
                                provisional.insert(s.clone());
                            }
                        }
                        if let Some(k) = key(&value) { if calls.len() >= ACTIVE { break; } calls.insert(k,scope.clone()); }
                        scope
                    } else {
                        let Some(response_key) = key(&value) else { break; };
                        let Some((scope,opening,rollback)) = pending.remove(&response_key) else { break; };
                        opening_unknown.remove(&response_key);
                        if value.get("error").is_some() { if let Some(s) = &rollback {
                            sessions.remove(s);
                            let mut claims = server.claims.lock().await;
                            if claims.get(s) == Some(&id) { claims.remove(s); }
                        } }
                        // Load/resume results need not repeat sessionId. The
                        // validated pending attachment is authoritative on success.
                        let opened = value.get("result").and_then(|result| result["sessionId"].as_str().or(rollback.as_deref()));
                        if opening && value.get("error").is_none() { if let Some(s) = opened {
                            let mut claims = server.claims.lock().await;
                            if claims.get(s).is_some_and(|owner| owner != &id) { break; }
                            claims.insert(s.to_owned(),id.clone());
                            sessions.entry(s.to_owned()).or_insert_with(StreamSlot::new);
                            attached.insert(s.to_owned());
                            provisional.remove(s);
                        } }
                        // Other outstanding openings may still own an unidentified
                        // stream. Once the last settles, only committed IDs survive.
                        if opening_unknown.is_empty() && !provisional.is_empty() {
                            let mut claims = server.claims.lock().await;
                            for s in provisional.drain() {
                                sessions.remove(&s);
                                if claims.get(&s) == Some(&id) { claims.remove(&s); }
                            }
                        }
                        scope
                    };
                    // The official RFD includes sessionId in session responses;
                    // dispatcher responses are neutral JSON-RPC and omit it.
                    if let Some(s) = &scope { if value.get("result").is_some_and(Value::is_object) { value["result"]["sessionId"] = Value::String(s.clone()); } }
                    let text = value.to_string();
                    let sent = match scope { Some(s) => sessions.get(&s).is_some_and(|slot| slot.send(text)), None => connection.send(text) };
                    if !sent { break; }
                }
                line.clear(); reader.set_limit((MAX_MESSAGE+1) as u64);
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => { if init_reply.as_ref().is_some_and(|r| r.is_closed()) { break; } }
        }
    }
    }
    // Single owner handles every EOF, overflow, deadline, DELETE and shutdown.
    let _ = stop.send(true);
    drop(input); drop(reader); drop(connection); drop(sessions);
    if tokio::time::timeout(CLEANUP_TIMEOUT, &mut dispatcher).await.is_err() { dispatcher.abort(); let _ = dispatcher.await; crate::diagln!("[rpi-acp] HTTP dispatcher cleanup timed out"); }
    server.claims.lock().await.retain(|_, owner| owner != &id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provisional_stream_is_bounded_and_preserves_order() {
        let mut slot = StreamSlot::new();
        for n in 0..8 { assert!(slot.send(n.to_string())); }
        assert!(!slot.send("overflow".into()), "actor must close rather than drop on overflow");
        let mut rx = slot.rx.take().unwrap();
        for n in 0..8 { assert_eq!(rx.recv().await.unwrap(), n.to_string()); }
        assert!(rx.try_recv().is_err(), "overflow must not replace an earlier event");
    }
}
