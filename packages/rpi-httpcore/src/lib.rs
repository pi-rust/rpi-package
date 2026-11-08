//! HTTP/SSE adapter for rpi-extcore; does not embed a frontend or load plugins.
#![forbid(unsafe_code)]
pub mod access;
pub mod config;
mod security;
mod server;
pub mod transport;
pub use config::AppConfig;
pub use server::{router, rpi_routes, run, serve_until, Listening};
