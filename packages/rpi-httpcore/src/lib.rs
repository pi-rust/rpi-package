//! HTTP/SSE adapter for rpi-extcore; does not embed a frontend or load plugins.
#![forbid(unsafe_code)]
pub mod access;
pub mod config;
pub mod node;
mod security;
mod server;
pub mod transport;
pub use config::AppConfig;
pub use node::node_discovery_router;
pub use server::{router, rpi_routes, rpi_routes_with_node_discovery, run, serve_until, Listening};
