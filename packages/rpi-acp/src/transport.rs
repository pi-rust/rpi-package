//! Experimental WebSocket-only profile of the ACP remote transport RFD.
//!
//! `/acp` upgrades return `Acp-Connection-Id`; each text message is one JSON-RPC
//! object (not newline framing). HTTP/2 Streamable HTTP/SSE is NOT implemented.
//! Connections own dispatchers and live children. Disconnect settles/cancels and
//! tears them down; native persisted history can be loaded on a new connection.
//! No live replay or automatic retry is promised. Put non-loopback listeners
//! behind a trusted TLS proxy; bearer credentials confer full agent privileges.

#![deny(clippy::all)]

pub mod http;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    net::TcpListener,
    sync::{mpsc, watch, Semaphore},
};

const MAX_MESSAGE: usize = 1024 * 1024;
const MAX_CONNECTIONS: usize = 16;
const MAX_MESSAGES: usize = 256;
const MAX_OPENS: usize = 8;
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);

/// Deliberately not Debug: never format credentials into logs.
#[derive(Clone)]
pub struct Config {
    token_hash: [u8; 32],
    allowed_origin: Option<String>,
    timeouts: Timeouts,
}

#[derive(Clone, Copy)]
struct Timeouts {
    initial: Duration,
    idle: Duration,
    lifetime: Duration,
}

impl Config {
    /// Authentication is mandatory, including on loopback. Origins are denied
    /// unless exactly configured; no wildcard or credential-in-query support.
    pub fn new(token: &str, allowed_origin: Option<String>) -> Result<Self, &'static str> {
        if token.len() < 32 || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("remote bearer secret must contain at least 32 printable ASCII bytes");
        }
        if let Some(origin) = &allowed_origin {
            let uri: axum::http::Uri = origin.parse().map_err(|_| "invalid allowed Origin")?;
            if !matches!(uri.scheme_str(), Some("http" | "https"))
                || uri.authority().is_none()
                || uri.path_and_query().is_some_and(|p| p.as_str() != "/")
                || uri.authority().is_some_and(|a| a.as_str().contains('@'))
                || origin.ends_with('/')
            {
                return Err(
                    "allowed Origin must be an exact http(s) origin without trailing slash",
                );
            }
        }
        Ok(Self {
            token_hash: Sha256::digest(token.as_bytes()).into(),
            allowed_origin,
            timeouts: Timeouts {
                initial: WRITE_TIMEOUT,
                idle: Duration::from_secs(120),
                lifetime: Duration::from_secs(3600),
            },
        })
    }
}

#[derive(Clone)]
struct Server {
    config: Config,
    address: SocketAddr,
    slots: Arc<Semaphore>,
    stop: watch::Receiver<bool>,
}

/// The caller owns listener binding and the shutdown sender. This future waits
/// for all upgraded connections to finish cleanup before returning.
pub async fn serve(
    listener: TcpListener,
    config: Config,
    stop: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let server = Server {
        config,
        address: listener.local_addr()?,
        slots: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        stop: stop.clone(),
    };
    let slots = server.slots.clone();
    let mut shutdown = stop;
    axum::serve(
        listener,
        Router::new().route("/acp", get(upgrade)).with_state(server),
    )
    .with_graceful_shutdown(async move {
        if !*shutdown.borrow() {
            let _ = shutdown.changed().await;
        }
    })
    .await?;
    // Axum does not await upgraded sockets; their permits cover dispatcher cleanup.
    let _all = slots.acquire_many(MAX_CONNECTIONS as u32).await;
    Ok(())
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    Some(value)
}

fn authorize(server: &Server, headers: &HeaderMap) -> Result<(), StatusCode> {
    let authorization =
        single_header(headers, "authorization").and_then(|v| v.strip_prefix("Bearer "));
    let hash: [u8; 32] = Sha256::digest(authorization.unwrap_or_default().as_bytes()).into();
    if authorization.is_none() || !bool::from(hash.ct_eq(&server.config.token_hash)) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let host = single_header(headers, "host").ok_or(StatusCode::FORBIDDEN)?;
    let direct = server.address.to_string();
    let localhost = format!("localhost:{}", server.address.port());
    if host != direct && !(server.address.ip().is_loopback() && host == localhost) {
        return Err(StatusCode::FORBIDDEN);
    }
    if headers.contains_key("origin") {
        let origin = single_header(headers, "origin").ok_or(StatusCode::FORBIDDEN)?;
        if server.config.allowed_origin.as_deref() != Some(origin) {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    Ok(())
}

async fn upgrade(
    State(server): State<Server>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if let Err(status) = authorize(&server, &headers) {
        let mut response = status.into_response();
        if status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        return response;
    }
    if *server.stop.borrow() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Ok(permit) = server.slots.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let id = uuid::Uuid::new_v4().to_string();
    let mut response = ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            connection(socket, server.stop, server.config.timeouts).await;
        })
        .into_response();
    response.headers_mut().insert(
        "acp-connection-id",
        HeaderValue::from_str(&id).expect("UUID is a header"),
    );
    response
}

async fn connection(
    mut socket: WebSocket,
    mut shutdown: watch::Receiver<bool>,
    timeouts: Timeouts,
) {
    let (input_tx, input_rx) = mpsc::channel(32);
    // Byte-bounded output prevents a slow network peer retaining unbounded events.
    let (output, reader) = tokio::io::duplex(64 * 1024);
    let mut reader = BufReader::new(reader).take((MAX_MESSAGE + 1) as u64);
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut dispatcher = tokio::spawn(crate::protocol::run_with_io(
        stop_rx, input_rx, None, output,
    ));
    let mut initialized = false;
    let mut count = 0;
    let mut opens = 0;
    let lifetime = tokio::time::sleep(timeouts.lifetime);
    tokio::pin!(lifetime);
    let idle = tokio::time::sleep(timeouts.initial);
    tokio::pin!(idle);
    // Keep read_line's buffer across select cancellation, but cap it with Take.
    let mut line = String::new();
    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => break,
            _ = &mut lifetime => break,
            _ = &mut idle => break,
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        count += 1;
                        if count > MAX_MESSAGES { break; }
                        let Ok(value) = serde_json::from_str::<Value>(&text) else { break; };
                        if !value.is_object() || value["jsonrpc"] != "2.0" { break; }
                        let method = value["method"].as_str();
                        if !initialized {
                            if method != Some("initialize") || value.get("id").is_none_or(Value::is_null) { break; }
                            initialized = true;
                        } else if method == Some("initialize") { break; }
                        if matches!(method, Some("session/new" | "session/load" | "session/resume")) {
                            opens += 1;
                            if opens > MAX_OPENS { break; }
                        }
                        // Never wait for queue space while output needs draining.
                        if input_tx.try_send(text.to_string()).is_err() { break; }
                        idle.as_mut().reset(tokio::time::Instant::now() + timeouts.idle);
                    }
                    Some(Ok(Message::Binary(_))) => {}, // RFD: binary frames ignored
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {},
                    _ => break,
                }
            }
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line.len() > MAX_MESSAGE || !line.ends_with('\n') => break,
                    Ok(_) => {
                        let text = line.trim_end_matches('\n').to_owned();
                        if !matches!(tokio::time::timeout(WRITE_TIMEOUT, socket.send(Message::Text(text.into()))).await, Ok(Ok(()))) { break; }
                        line.clear();
                        reader.set_limit((MAX_MESSAGE + 1) as u64);
                    }
                }
            }
        }
    }
    // Do not cancel the dispatcher future on socket EOF: its normal stop path
    // resolves pending dialogs, cancels tasks, and shuts down core-owned children.
    let _ = stop_tx.send(true);
    drop(input_tx);
    drop(reader); // unblock any output writer before waiting for task cleanup
    if tokio::time::timeout(CLEANUP_TIMEOUT, &mut dispatcher)
        .await
        .is_err()
    {
        dispatcher.abort();
        let _ = dispatcher.await;
        crate::diagln!("[rpi-acp] experimental remote dispatcher cleanup timed out");
    }
    let _ = tokio::time::timeout(WRITE_TIMEOUT, socket.send(Message::Close(None))).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{client::IntoClientRequest, Message as ClientMessage},
    };

    // Real sockets and real dispatcher, short private deadlines; production
    // limits cannot be relaxed through flags or public configuration.
    #[tokio::test]
    async fn initial_idle_and_absolute_lifetime_close_and_release_slots() {
        for mode in 0..3 {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let mut config = Config::new("test-secret-01234567890123456789012345", None).unwrap();
            let short = Duration::from_millis(150);
            let long = Duration::from_secs(10);
            config.timeouts = Timeouts {
                initial: if mode == 0 { short } else { long },
                idle: if mode == 1 { short } else { long },
                lifetime: if mode == 2 { short } else { long },
            };
            let (stop, rx) = watch::channel(false);
            let task = tokio::spawn(serve(listener, config, rx));
            let mut request = format!("ws://{address}/acp").into_client_request().unwrap();
            request.headers_mut().insert(
                "authorization",
                "Bearer test-secret-01234567890123456789012345"
                    .parse()
                    .unwrap(),
            );
            let (mut socket, _) = connect_async(request).await.unwrap();
            if mode != 0 {
                socket.send(ClientMessage::Text(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}"#.into())).await.unwrap();
                assert!(matches!(
                    socket.next().await,
                    Some(Ok(ClientMessage::Text(_)))
                ));
            }
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if mode == 2 {
                        // Inbound activity cannot extend the absolute lifetime.
                        let _ = socket
                            .send(ClientMessage::Text(
                                r#"{"jsonrpc":"2.0","id":2,"method":"unknown"}"#.into(),
                            ))
                            .await;
                    }
                    match socket.next().await {
                        None | Some(Err(_)) | Some(Ok(ClientMessage::Close(_))) => break,
                        _ => tokio::time::sleep(Duration::from_millis(20)).await,
                    }
                }
            })
            .await
            .unwrap();
            stop.send(true).unwrap();
            tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
    }
}
