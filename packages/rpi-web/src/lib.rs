//! Capability-driven Web bridge for the native `rpi --mode rpc` host.

#![forbid(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod assets;
pub mod config;
mod file_links;
mod plugin;
mod server;
mod startup;

pub use server::{run, AppConfig};
