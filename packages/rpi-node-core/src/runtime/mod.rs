//! Native execution adapters, without HTTP clients or client-side projections.
//!
//! A handle owns the same child/process state across clones. This is an adapter
//! boundary, not a workspace owner, server endpoint, or authorization service.
#![allow(clippy::missing_errors_doc)]

use futures_core::Stream;
use rpi_node_api::api::{PendingDialog, WireEvent};
use std::pin::Pin;

mod acp;
mod error;
mod native_lifecycle;
mod permissions;
mod rpc;
mod transport;

pub use acp::AcpClient;
pub use error::RuntimeError;
pub use rpc::RpiRpcClient;
pub use transport::RuntimeTransport;

/// Native event streams retain their existing cross-task delivery contract.
/// This does not impose `Send` on runtime operation futures or service ports.
pub type RuntimeEventStream = Pin<Box<dyn Stream<Item = Result<WireEvent, RuntimeError>> + Send>>;

fn unsupported(operation: &str, protocol: &str) -> RuntimeError {
    RuntimeError::Unsupported {
        operation: operation.into(),
        protocol: protocol.into(),
    }
}

fn dialog_from_wire(wire: &WireEvent) -> Result<Option<PendingDialog>, RuntimeError> {
    rpi_node_api::api::events::dialog_from_wire(wire).map_err(RuntimeError::from)
}
