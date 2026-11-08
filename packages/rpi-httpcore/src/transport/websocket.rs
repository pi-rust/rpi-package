use super::{ConnectionCloseReason, ConnectionError, ConnectionPermit, ConnectionRegistry};
use crate::access::AuthLease;
use axum::{
    extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use futures_util::StreamExt;
use serde_json::json;
use std::future::Future;

pub const CLOSE_LEASE_INVALIDATED: u16 = 1008;
pub const CLOSE_SHUTDOWN: u16 = 1001;
pub const CLOSE_MESSAGE_TOO_LARGE: u16 = 1009;

#[derive(Debug)]
pub enum WebSocketSessionError {
    Closed,
    MessageTooLarge,
    Socket(axum::Error),
}

/// A protocol-neutral WebSocket session. Business code owns frame decoding and
/// operation execution; this wrapper owns lifecycle checks and close reasons.
pub struct WebSocketSession {
    socket: WebSocket,
    permit: ConnectionPermit,
}

impl WebSocketSession {
    fn new(socket: WebSocket, permit: ConnectionPermit) -> Self {
        Self { socket, permit }
    }

    pub fn permit(&self) -> &ConnectionPermit {
        &self.permit
    }

    /// Receive one bounded message, or close when the Lease/shutdown expires.
    pub async fn recv(&mut self) -> Option<Result<Message, WebSocketSessionError>> {
        let next = {
            let closed = self.permit.wait_closed_reason();
            tokio::pin!(closed);
            tokio::select! {
                reason = &mut closed => NextMessage::Closed(reason),
                message = self.socket.next() => NextMessage::Socket(message),
            }
        };
        match next {
            NextMessage::Closed(reason) => {
                let _ = send_close(&mut self.socket, reason).await;
                None
            }
            NextMessage::Socket(None) => None,
            NextMessage::Socket(Some(Err(error))) => {
                Some(Err(WebSocketSessionError::Socket(error)))
            }
            NextMessage::Socket(Some(Ok(message))) => {
                if self
                    .permit
                    .validate_message_size(message_bytes(&message))
                    .is_err()
                {
                    let _ = send_close_code(
                        &mut self.socket,
                        CLOSE_MESSAGE_TOO_LARGE,
                        "message too large",
                    )
                    .await;
                    None
                } else {
                    Some(Ok(message))
                }
            }
        }
    }

    /// Send only while the Lease remains valid and the message is bounded.
    pub async fn send(&mut self, message: Message) -> Result<(), WebSocketSessionError> {
        if !self.permit.authorized().await {
            let _ = send_close(&mut self.socket, ConnectionCloseReason::LeaseInvalidated).await;
            return Err(WebSocketSessionError::Closed);
        }
        if self
            .permit
            .validate_message_size(message_bytes(&message))
            .is_err()
        {
            let _ = send_close_code(
                &mut self.socket,
                CLOSE_MESSAGE_TOO_LARGE,
                "message too large",
            )
            .await;
            return Err(WebSocketSessionError::MessageTooLarge);
        }
        self.socket
            .send(message)
            .await
            .map_err(WebSocketSessionError::Socket)
    }
}

async fn send_close(
    socket: &mut WebSocket,
    reason: ConnectionCloseReason,
) -> Result<(), axum::Error> {
    let (code, text) = match reason {
        ConnectionCloseReason::LeaseInvalidated => {
            (CLOSE_LEASE_INVALIDATED, "authorization expired")
        }
        ConnectionCloseReason::Shutdown => (CLOSE_SHUTDOWN, "service shutting down"),
    };
    send_close_code(socket, code, text).await
}

async fn send_close_code(
    socket: &mut WebSocket,
    code: u16,
    text: &'static str,
) -> Result<(), axum::Error> {
    socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: text.into(),
        })))
        .await
}

/// Perform a Lease-checked WebSocket upgrade and run a protocol-owned handler.
/// Host/Origin validation remains the responsibility of the HTTP security
/// middleware that surrounds the route.
pub async fn upgrade_websocket<F, Fut>(
    upgrade: WebSocketUpgrade,
    registry: ConnectionRegistry,
    lease: AuthLease,
    handler: F,
) -> Response
where
    F: FnOnce(WebSocketSession) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let permit = match registry.accept(&lease).await {
        Ok(permit) => permit,
        Err(error) => return rejection(error),
    };
    upgrade
        .max_message_size(registry.policy().max_message_bytes)
        .on_upgrade(move |socket| async move {
            handler(WebSocketSession::new(socket, permit)).await;
        })
}

fn rejection(error: ConnectionError) -> Response {
    let (status, message) = match error {
        ConnectionError::Access(_) => (StatusCode::UNAUTHORIZED, "unauthorized"),
        ConnectionError::AtCapacity => (
            StatusCode::TOO_MANY_REQUESTS,
            "connection capacity exceeded",
        ),
        ConnectionError::ShuttingDown => {
            (StatusCode::SERVICE_UNAVAILABLE, "service is shutting down")
        }
        ConnectionError::MessageTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "message too large"),
    };
    (status, Json(json!({"error": message}))).into_response()
}

enum NextMessage {
    Closed(ConnectionCloseReason),
    Socket(Option<Result<Message, axum::Error>>),
}

fn message_bytes(message: &Message) -> usize {
    match message {
        Message::Text(text) => text.len(),
        Message::Binary(bytes) | Message::Ping(bytes) | Message::Pong(bytes) => bytes.len(),
        Message::Close(_) => 0,
    }
}
