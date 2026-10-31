#![deny(unsafe_code)]
#![doc = include_str!("../README.md")]
// The sole audited platform-FFI boundary. Runtime/protocol modules inherit deny.
pub mod config;
mod configured;
pub mod connection;
pub mod control;
pub mod http;
pub mod jsonrpc;
mod kit;
#[allow(unsafe_code)]
mod process;
mod runtime;
pub mod transport;
pub use config::{Exposure, HttpServer, McpConfig, McpServerConfig, StdioServer};
pub use configured::{ConfiguredStdio, DiscoveryLimits};
pub use connection::{
    initialize_request, initialized_notification, parse_initialize, parse_tools_call,
    parse_tools_list, tools_call_request, tools_list_request, ServerInfo, PROTOCOL_VERSION,
};
pub use control::Control;
pub use jsonrpc::{
    call_result_text, tool_list, ErrorObject, Notification, Request, Response, Tool,
};
pub use runtime::{
    ClientIdentity, CoreRequestContext, DiscoveredServer, McpRuntime, ResourceUpdate,
    ResourceUpdateBatch,
};
pub use transport::{http_request, StdioTransport};
