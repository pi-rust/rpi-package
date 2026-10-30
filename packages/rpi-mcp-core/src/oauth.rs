//! Managed public-client OAuth for HTTP MCP. No ACP/native-agent authentication.
pub mod network;
mod callback;
mod provider;
pub use provider::*;
pub use network::{NetworkPolicy, OAuthNetwork};
