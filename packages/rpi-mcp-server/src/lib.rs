#![forbid(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod http;
mod plugin;
pub mod protocol;

pub use http::{serve_until as serve_http_until, HttpOptions};
pub use protocol::{run, serve, McpServer, RpcError, ServerOptions};

rpi_plugin_sdk::export_plugin!(plugin::register);
