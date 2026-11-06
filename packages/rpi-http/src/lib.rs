//! HTTP/SSE adapter for rpi-extcore; does not embed a frontend or load plugins.
#![forbid(unsafe_code)]
pub mod config;
mod security;
mod server;
pub use server::{router, run, serve_until, AppConfig, Listening};

/// Shared node transport for existing owners. Does not create a second Service.
pub use rpi_http_runtime as runtime;

/// HTTP adapter for an existing multi-project owner.
pub use rpi_http_node as node;
