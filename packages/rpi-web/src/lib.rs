//! Capability-driven Web bridge for the native `rpi --mode rpc` host.

mod host;
mod projects;
mod protocol;
mod registry;
mod security;
mod server;
mod workspace;

pub use server::{run, AppConfig};
