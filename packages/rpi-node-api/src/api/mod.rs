//! Node-facing contracts, independent of clients, execution hosts and transports.
//!
//! These payloads preserve the existing wire format. [`NodeApi`] is an internal,
//! object-safe catalog/open/snapshot boundary, not a new network API or an extension
//! loader. Authentication and access policy are not supplied by these DTOs.

mod access;
mod configuration;
mod error;
pub mod events;
mod node;
mod packages;
mod providers;
pub mod realtime;
pub use providers::*;
mod permissions;
pub use packages::*;
mod resources;
mod types;
pub use resources::*;

pub use access::SessionAccess;
pub use configuration::SessionConfiguration;
pub use error::{AcpPermissionError, EventDecodeError, NodeError, NodeErrorKind};
pub use node::{NodeApi, NodeEventStream, NodeFuture};
pub use types::*;

/// Construct a typed command payload, preserving the existing overwrite semantics.
#[must_use]
pub fn payload_with_type(command: &str, payload: serde_json::Value) -> serde_json::Value {
    let mut object = match payload {
        serde_json::Value::Object(object) => object,
        _ => serde_json::Map::new(),
    };
    object.insert("type".into(), serde_json::Value::String(command.into()));
    serde_json::Value::Object(object)
}
