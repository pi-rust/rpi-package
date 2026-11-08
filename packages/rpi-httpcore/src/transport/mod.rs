//! Generic connection lifecycle primitives for SSE/WebSocket adapters.
//!
//! This module owns admission, authorization invalidation and bounded
//! connection resources. It intentionally does not define frames, operations
//! or a concrete protocol.

mod connection;
mod policy;
mod websocket;

pub use connection::{ConnectionPermit, ConnectionRegistry};
pub use policy::{ConnectionCloseReason, ConnectionConfigError, ConnectionError, ConnectionPolicy};
pub use websocket::{
    upgrade_websocket, WebSocketSession, WebSocketSessionError, CLOSE_LEASE_INVALIDATED,
    CLOSE_MESSAGE_TOO_LARGE, CLOSE_SHUTDOWN,
};
