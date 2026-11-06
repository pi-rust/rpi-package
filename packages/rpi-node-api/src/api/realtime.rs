//! Opt-in JSON-text protocol for `/api/v1/realtime`; independent of the REST version.
//!
//! Decode at the transport boundary with `decode_text`, and validate constructed frames
//! before sending. Serde alone does not enforce semantic limits. Binary messages are
//! unsupported. Authentication, subscription ownership, outstanding-request limits and
//! operation authorization remain the server's responsibility. An [`OperationRef`] is
//! an opaque reference, **not** a bearer credential or permission to execute.
use std::{fmt, io};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

pub use super::WireEvent;

pub const VERSION: u32 = 1;
pub const PATH: &str = "/api/v1/realtime";
pub const SUBPROTOCOL: &str = "rpi-work.realtime.v1";
pub const MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_ID_BYTES: usize = 256;
pub const MAX_EVENTS_PER_FRAME: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationRef {
    pub owner_epoch: String,
    pub operation_id: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RealtimeOperationKind {
    SessionCommand,
    SetConfiguration,
    DialogResponse,
    CancelDialog,
}

/// Result of the adapter RPC, not proof of Agent acceptance or settlement.
/// A Succeeded value can itself be a HostResponse with success:false; consumers
/// must preserve and interpret that original result before clearing submissions.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "camelCase", deny_unknown_fields)]
pub enum OperationOutcome {
    Succeeded { value: Value },
    Failed { error: RealtimeError },
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RealtimeError {
    pub code: RealtimeErrorCode,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RealtimeErrorCode {
    InvalidInput,
    NotFound,
    AccessDenied,
    Conflict,
    Unsupported,
    UnsupportedContent,
    Unavailable,
    LimitExceeded,
    OutcomeUnknown,
    Internal,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
pub enum OperationStatus {
    Reserved,
    Dispatching,
    KnownResult { outcome: OperationOutcome },
    OutcomeUnknown,
}

impl<'de> Deserialize<'de> for OperationStatus {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Serde's internally tagged unit variants ignore extra fields even with
        // deny_unknown_fields. Empty struct variants enforce the wire contract
        // while keeping the public Rust API's unit variants intact.
        #[derive(Deserialize)]
        #[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
        enum StrictStatus {
            Reserved {},
            Dispatching {},
            KnownResult { outcome: OperationOutcome },
            OutcomeUnknown {},
        }
        Ok(match StrictStatus::deserialize(deserializer)? {
            StrictStatus::Reserved {} => Self::Reserved,
            StrictStatus::Dispatching {} => Self::Dispatching,
            StrictStatus::KnownResult { outcome } => Self::KnownResult { outcome },
            StrictStatus::OutcomeUnknown {} => Self::OutcomeUnknown,
        })
    }
}

/// Only JSON text is advertised in version 1; these are protocol, not REST limits.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Capabilities {
    pub json_text: bool,
    pub binary: bool,
    /// Older realtime drafts omit this and continue to use HTTP for queries.
    #[serde(default)]
    pub command_queries: bool,
    pub max_message_bytes: usize,
    pub max_events_per_frame: usize,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            json_text: true,
            binary: false,
            command_queries: true,
            max_message_bytes: MAX_MESSAGE_BYTES,
            max_events_per_frame: MAX_EVENTS_PER_FRAME,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Discovery {
    pub version: u32,
    pub node_id: String,
    pub owner_epoch: String,
    pub path: String,
    pub subprotocol: String,
    pub capabilities: Capabilities,
}

impl Discovery {
    /// Identity comes from the authenticated owner, never a static default.
    pub fn new(node_id: String, owner_epoch: String) -> Self {
        Self {
            version: VERSION,
            node_id,
            owner_epoch,
            path: PATH.into(),
            subprotocol: SUBPROTOCOL.into(),
            capabilities: Capabilities::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hello {
    pub version: u32,
    pub node_id: String,
    pub owner_epoch: String,
    pub connection_id: String,
    pub capabilities: Capabilities,
}

/// Reserve binds a session and kind before execute supplies the opaque payload.
/// Never retry an uncertain execute by reserving a new operation; query status.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ClientFrame {
    Reserve {
        version: u32,
        request_id: String,
        session_id: String,
        client_instance_id: String,
        kind: RealtimeOperationKind,
    },
    Execute {
        version: u32,
        request_id: String,
        operation: OperationRef,
        payload: Value,
    },
    /// Explicitly whitelisted read-only commands do not consume operation IDs.
    Query {
        version: u32,
        request_id: String,
        session_id: String,
        payload: Value,
    },
    Status {
        version: u32,
        request_id: String,
        operation: OperationRef,
    },
    Subscribe {
        version: u32,
        request_id: String,
        session_id: String,
        subscription_id: String,
        after_seq: Option<u64>,
    },
    Unsubscribe {
        version: u32,
        request_id: String,
        subscription_id: String,
    },
    /// Application heartbeat: browsers cannot send protocol-level ping frames.
    Ping { version: u32, request_id: String },
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ServerFrame {
    Hello {
        version: u32,
        hello: Hello,
    },
    Reserved {
        version: u32,
        request_id: String,
        operation: OperationRef,
    },
    Status {
        version: u32,
        request_id: String,
        operation: OperationRef,
        status: OperationStatus,
    },
    Result {
        version: u32,
        request_id: String,
        value: Value,
    },
    Subscribed {
        version: u32,
        request_id: String,
        session_id: String,
        subscription_id: String,
    },
    Unsubscribed {
        version: u32,
        request_id: String,
        subscription_id: String,
    },
    /// Events retain their native WireEvent identity, sequence and wire shape.
    /// There is no connection-wide/global sequence number.
    Events {
        version: u32,
        session_id: String,
        subscription_id: String,
        events: Vec<WireEvent>,
    },
    Error {
        version: u32,
        request_id: Option<String>,
        error: RealtimeError,
    },
    Pong {
        version: u32,
        request_id: String,
    },
}

// Never include opaque commands, event data, result values or error text in logs.
macro_rules! redacted_debug {
    ($($ty:ty),+ $(,)?) => {$(
        impl fmt::Debug for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($ty), "(<redacted>)"))
            }
        }
    )+};
}
redacted_debug!(ClientFrame, ServerFrame, OperationOutcome, RealtimeError);

impl fmt::Display for RealtimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Messages may originate from remote peers; do not reflect them in logs.
        write!(f, "realtime error: {:?}", self.code)
    }
}
impl std::error::Error for RealtimeError {}

fn invalid() -> RealtimeError {
    RealtimeError {
        code: RealtimeErrorCode::InvalidInput,
        message: "Invalid realtime message".into(),
    }
}
fn limit() -> RealtimeError {
    RealtimeError {
        code: RealtimeErrorCode::LimitExceeded,
        message: "Realtime message exceeds protocol limits".into(),
    }
}
fn check_version(version: u32) -> Result<(), RealtimeError> {
    if version != VERSION {
        return Err(RealtimeError {
            code: RealtimeErrorCode::Unsupported,
            message: "Unsupported realtime protocol version".into(),
        });
    }
    Ok(())
}
fn check_id(id: &str) -> Result<(), RealtimeError> {
    if id.trim().is_empty() || id.len() > MAX_ID_BYTES || id.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(())
}

impl OperationRef {
    pub fn validate(&self) -> Result<(), RealtimeError> {
        check_id(&self.owner_epoch)?;
        check_id(&self.operation_id)
    }
}
impl Capabilities {
    pub fn validate(&self) -> Result<(), RealtimeError> {
        if !self.json_text
            || self.binary
            || self.max_message_bytes != MAX_MESSAGE_BYTES
            || self.max_events_per_frame != MAX_EVENTS_PER_FRAME
        {
            return Err(invalid());
        }
        Ok(())
    }
}

// Count serialized bytes without allocating a second payload-sized buffer.
struct ByteBudget(usize);
impl io::Write for ByteBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_MESSAGE_BYTES.saturating_sub(self.0) {
            return Err(io::Error::other("realtime byte budget exceeded"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn check_bytes(value: &impl Serialize) -> Result<(), RealtimeError> {
    serde_json::to_writer(ByteBudget(0), value).map_err(|_| limit())
}
fn decode<T: DeserializeOwned>(text: &str) -> Result<T, RealtimeError> {
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(limit());
    }
    // Do not return serde's error: unknown fields/variants can contain secrets.
    serde_json::from_str(text).map_err(|_| invalid())
}

impl Discovery {
    pub fn validate(&self) -> Result<(), RealtimeError> {
        check_version(self.version)?;
        check_id(&self.node_id)?;
        check_id(&self.owner_epoch)?;
        if self.path != PATH || self.subprotocol != SUBPROTOCOL {
            return Err(invalid());
        }
        self.capabilities.validate()?;
        check_bytes(self)
    }
}
impl Hello {
    pub fn validate(&self) -> Result<(), RealtimeError> {
        check_version(self.version)?;
        check_id(&self.node_id)?;
        check_id(&self.owner_epoch)?;
        check_id(&self.connection_id)?;
        self.capabilities.validate()?;
        check_bytes(self)
    }
}
/// Built-in queries only, never arbitrary extensions or a `get_` prefix test.
/// These keep their original HostResponse and pass the same owner validation.
pub fn is_read_only_session_command(payload: &Value) -> bool {
    matches!(
        payload.get("type").and_then(Value::as_str),
        Some(
            "get_state"
                | "get_history"
                | "get_messages"
                | "get_session_branch"
                | "get_auth_status"
                | "get_available_tools"
                | "get_available_models"
                | "get_available_thinking_levels"
                | "get_queue"
                | "get_message_summaries"
                | "get_message_window"
        )
    )
}

impl ClientFrame {
    pub fn validate(&self) -> Result<(), RealtimeError> {
        match self {
            Self::Query {
                version,
                request_id,
                session_id,
                payload,
            } => {
                check_version(*version)?;
                check_id(request_id)?;
                check_id(session_id)?;
                if !is_read_only_session_command(payload) {
                    return Err(invalid());
                }
            }
            Self::Reserve {
                version,
                request_id,
                session_id,
                client_instance_id,
                ..
            } => {
                check_version(*version)?;
                check_id(request_id)?;
                check_id(session_id)?;
                check_id(client_instance_id)?;
            }
            Self::Execute {
                version,
                request_id,
                operation,
                ..
            }
            | Self::Status {
                version,
                request_id,
                operation,
            } => {
                check_version(*version)?;
                check_id(request_id)?;
                operation.validate()?;
            }
            Self::Subscribe {
                version,
                request_id,
                session_id,
                subscription_id,
                ..
            } => {
                check_version(*version)?;
                check_id(request_id)?;
                check_id(session_id)?;
                check_id(subscription_id)?;
            }
            Self::Unsubscribe {
                version,
                request_id,
                subscription_id,
            } => {
                check_version(*version)?;
                check_id(request_id)?;
                check_id(subscription_id)?;
            }
            Self::Ping {
                version,
                request_id,
            } => {
                check_version(*version)?;
                check_id(request_id)?;
            }
        }
        check_bytes(self)
    }
}
impl ServerFrame {
    pub fn validate(&self) -> Result<(), RealtimeError> {
        match self {
            Self::Hello { version, hello } => {
                check_version(*version)?;
                hello.validate()?;
            }
            Self::Reserved {
                version,
                request_id,
                operation,
            }
            | Self::Status {
                version,
                request_id,
                operation,
                ..
            } => {
                check_version(*version)?;
                check_id(request_id)?;
                operation.validate()?;
            }
            Self::Subscribed {
                version,
                request_id,
                session_id,
                subscription_id,
            } => {
                check_version(*version)?;
                check_id(request_id)?;
                check_id(session_id)?;
                check_id(subscription_id)?;
            }
            Self::Unsubscribed {
                version,
                request_id,
                subscription_id,
            } => {
                check_version(*version)?;
                check_id(request_id)?;
                check_id(subscription_id)?;
            }
            Self::Events {
                version,
                session_id,
                subscription_id,
                events,
            } => {
                check_version(*version)?;
                check_id(session_id)?;
                check_id(subscription_id)?;
                if events.is_empty() || events.len() > MAX_EVENTS_PER_FRAME {
                    return Err(limit());
                }
                for event in events {
                    if let Some(id) = &event.id {
                        check_id(id)?;
                    }
                    check_id(&event.event_type)?;
                }
            }
            Self::Error {
                version,
                request_id,
                ..
            } => {
                check_version(*version)?;
                if let Some(id) = request_id {
                    check_id(id)?;
                }
            }
            Self::Result {
                version,
                request_id,
                ..
            }
            | Self::Pong {
                version,
                request_id,
            } => {
                check_version(*version)?;
                check_id(request_id)?;
            }
        }
        check_bytes(self)
    }
}

macro_rules! text_codec {
    ($($ty:ty),+ $(,)?) => {$(
        impl $ty {
            /// Decode JSON text and enforce semantic and byte limits. Binary is unsupported.
            pub fn decode_text(text: &str) -> Result<Self, RealtimeError> {
                let frame: Self = decode(text)?;
                frame.validate()?;
                Ok(frame)
            }
            /// Validate before encoding, including the complete serialized byte budget.
            pub fn encode_text(&self) -> Result<String, RealtimeError> {
                self.validate()?;
                serde_json::to_string(self).map_err(|_| invalid())
            }
        }
    )+};
}
text_codec!(Discovery, Hello, ClientFrame, ServerFrame);
