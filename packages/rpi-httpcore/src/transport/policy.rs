use crate::access::AccessError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionConfigError {
    ZeroConnections,
    TooManyConnections,
    ZeroMessageBytes,
    ZeroSendBuffer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionError {
    Access(AccessError),
    AtCapacity,
    ShuttingDown,
    MessageTooLarge,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionCloseReason {
    LeaseInvalidated,
    Shutdown,
}

impl From<AccessError> for ConnectionError {
    fn from(error: AccessError) -> Self {
        Self::Access(error)
    }
}

/// Limits that apply to each connection registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectionPolicy {
    pub max_connections: usize,
    pub max_message_bytes: usize,
    pub send_buffer: usize,
}

impl ConnectionPolicy {
    pub fn new(
        max_connections: usize,
        max_message_bytes: usize,
        send_buffer: usize,
    ) -> Result<Self, ConnectionConfigError> {
        if max_connections == 0 {
            return Err(ConnectionConfigError::ZeroConnections);
        }
        if max_connections > u32::MAX as usize {
            return Err(ConnectionConfigError::TooManyConnections);
        }
        if max_message_bytes == 0 {
            return Err(ConnectionConfigError::ZeroMessageBytes);
        }
        if send_buffer == 0 {
            return Err(ConnectionConfigError::ZeroSendBuffer);
        }
        Ok(Self {
            max_connections,
            max_message_bytes,
            send_buffer,
        })
    }
}
