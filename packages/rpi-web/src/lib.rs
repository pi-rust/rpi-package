//! Capability-driven Web bridge for the native `rpi --mode rpc` host.

#![forbid(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod assets;
pub mod config;
mod host;
mod plugin;
mod projects;
mod protocol;
mod registry;
mod security;
mod server;
mod timing;
mod workspace;

pub use server::{run, AppConfig};
