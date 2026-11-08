//! HTTP/SSE adapter for rpi-extcore; does not embed a frontend or load plugins.
#![forbid(unsafe_code)]
pub mod config;
mod security;
mod server;
pub use config::AppConfig;
pub use server::{router, run, serve_until, Listening};
