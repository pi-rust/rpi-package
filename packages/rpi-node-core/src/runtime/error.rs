use thiserror::Error;

/// Native protocol failures without an HTTP client or a workspace dependency.
/// These retain internal diagnostics; service adapters must redact as needed.
#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("operation `{operation}` is unsupported by {protocol}")]
    Unsupported { operation: String, protocol: String },
    #[error("RPC transport error: {0}")]
    Rpc(String),
    #[error("ACP transport error: {0}")]
    Acp(String),
    #[error("agent protocol mismatch: {0}")]
    ProtocolMismatch(String),
    #[error("agent request timed out: {0}")]
    RequestTimeout(String),
    #[error("agent process failed: {0}")]
    Process(String),
    #[error("agent process exited: {0}")]
    ChildExited(String),
}

impl From<rpi_node_api::api::AcpPermissionError> for RuntimeError {
    fn from(error: rpi_node_api::api::AcpPermissionError) -> Self {
        Self::Acp(error.to_string())
    }
}
