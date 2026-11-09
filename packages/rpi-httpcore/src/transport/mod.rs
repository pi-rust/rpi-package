//! Generic connection lifecycle primitives for SSE/WebSocket adapters.
//!
//! This module owns admission, authorization invalidation and bounded
//! connection resources. It intentionally does not define frames, operations
//! or a concrete protocol.

mod auth;
mod connection;
mod middleware;
mod origin;
mod policy;
mod router;
mod sse;
mod websocket;

#[cfg(test)]
mod integration_tests;

pub use auth::{bearer_auth_router, lease_from_request};
pub use connection::{ConnectionPermit, ConnectionRegistry};
pub use origin::{same_origin_router, HostOriginPolicy};
pub use policy::{ConnectionCloseReason, ConnectionConfigError, ConnectionError, ConnectionPolicy};
pub use router::{lifecycle_router, TransportConfigError, TransportState};
pub use sse::authorized_stream;
pub use websocket::{
    upgrade_websocket, WebSocketSession, WebSocketSessionError, CLOSE_LEASE_INVALIDATED,
    CLOSE_MESSAGE_TOO_LARGE, CLOSE_SHUTDOWN,
};
