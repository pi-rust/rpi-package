//! Session-scoped configuration observations, not authorization or loaded resources.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Observed operations and opaque negotiated options for one bound session.
///
/// A successful observation may restore the runtime, but never submits input.
/// Absence of this response must not be interpreted as full support. Commands
/// are restricted to this configuration surface, not a raw-command catalog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfiguration {
    pub protocol: String,
    pub supported_commands: Vec<String>,
    pub config_options: Vec<Value>,
}

impl SessionConfiguration {
    #[must_use]
    pub fn supports_command(&self, command: &str) -> bool {
        self.supported_commands.iter().any(|item| item == command)
    }
}
