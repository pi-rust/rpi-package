//! Existing rpi plugin ABI entry point for the API-only HTTP/SSE service.
//!
//! The crate is a thin adapter: HTTP extraction, authentication, Host/Origin
//! checks, SSE, listener lifecycle and the shared core service all live in
//! `rpi-http` and `rpi-extcore`. There is no Work node owner or Work-specific
//! component protocol here.

#![forbid(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod plugin;

/// Configuration and standalone launch support from the HTTP adapter.
pub use rpi_http::config;
