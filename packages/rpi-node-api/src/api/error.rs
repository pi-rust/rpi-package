use std::{error::Error, fmt};

/// Transport-independent outcome categories. Adapters decide HTTP/protocol codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeErrorKind {
    InvalidInput,
    NotFound,
    AccessDenied,
    Conflict,
    Unsupported,
    Unavailable,
    LimitExceeded,
    UnsupportedContent,
    Timeout,
    Internal,
}

/// A node-facing error without HTTP clients, native reply types or I/O handles.
///
/// Adapters must redact internal details before constructing a public message;
/// classification alone is not a guarantee that a message is safe to display.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeError {
    pub kind: NodeErrorKind,
    pub message: String,
}

impl NodeError {
    #[must_use]
    pub fn new(kind: NodeErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for NodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for NodeError {}

/// Invalid or ambiguous ACP permission data. No request payload is echoed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcpPermissionError;

impl fmt::Display for AcpPermissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid or ambiguous ACP permission selection")
    }
}

impl Error for AcpPermissionError {}

/// A required event field was absent or had the wrong type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventDecodeError {
    field: String,
}

impl EventDecodeError {
    pub fn missing_field(field: &str) -> Self {
        Self {
            field: field.to_owned(),
        }
    }
}

impl fmt::Display for EventDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "event is missing {}", self.field)
    }
}

impl Error for EventDecodeError {}
