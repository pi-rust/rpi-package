//! ACP plugin and standalone bridge, backed by the shared server implementation.
#![forbid(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
pub use rpi_acp_core::{diag, diagln, protocol, session, zed};
rpi_plugin_sdk::export_plugin!(|api| rpi_acp_core::register_plugin(api));
