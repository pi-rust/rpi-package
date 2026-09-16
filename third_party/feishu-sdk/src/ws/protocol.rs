use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};

use crate::core::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsMessage {
    pub msg_type: i32,
    pub msg_id: i64,
    pub timestamp: i64,
    pub payload: Vec<u8>,
}

impl WsMessage {
    pub fn new(msg_type: i32, payload: Vec<u8>) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        Self {
            msg_type,
            msg_id: timestamp,
            timestamp,
            payload,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut buf = BytesMut::new();
        buf.put_i32(self.msg_type);
        buf.put_i64(self.msg_id);
        buf.put_i64(self.timestamp);
        buf.put_u32(self.payload.len() as u32);
        buf.put_slice(&self.payload);
        Ok(buf.to_vec())
    }

    pub fn decode(data: &[u8]) -> Result<Self, Error> {
        if data.len() < 28 {
            return Err(Error::SerializationError(
                "Invalid message format: message too short".to_string(),
            ));
        }

        let mut buf = Bytes::copy_from_slice(data);

        let msg_type = buf.get_i32();
        let msg_id = buf.get_i64();
        let timestamp = buf.get_i64();
        let payload_len = buf.get_u32() as usize;

        if buf.remaining() < payload_len {
            return Err(Error::SerializationError(
                "Invalid message format: payload too short".to_string(),
            ));
        }

        let mut payload = vec![0u8; payload_len];
        buf.copy_to_slice(&mut payload);

        Ok(Self {
            msg_type,
            msg_id,
            timestamp,
            payload,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatMessage {
    pub timestamp: i64,
}

impl HeartbeatMessage {
    pub fn new() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        Self {
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut buf = BytesMut::new();
        buf.put_i64(self.timestamp);
        Ok(buf.to_vec())
    }

    pub fn decode(data: &[u8]) -> Result<Self, Error> {
        let mut buf = Bytes::copy_from_slice(data);
        let timestamp = buf.get_i64();
        Ok(Self { timestamp })
    }
}

impl Default for HeartbeatMessage {
    fn default() -> Self {
        Self::new()
    }
}

pub const MSG_TYPE_HEARTBEAT: i32 = 1;
pub const MSG_TYPE_DATA: i32 = 2;
pub const MSG_TYPE_ACK: i32 = 3;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_message_encode_decode() {
        let msg = WsMessage::new(MSG_TYPE_DATA, b"test payload".to_vec());
        let encoded = msg.encode().unwrap();
        let decoded = WsMessage::decode(&encoded).unwrap();

        assert_eq!(decoded.msg_type, MSG_TYPE_DATA);
        assert_eq!(decoded.payload, b"test payload");
    }

    #[test]
    fn test_heartbeat_message_encode_decode() {
        let msg = HeartbeatMessage::new();
        let encoded = msg.encode().unwrap();
        let decoded = HeartbeatMessage::decode(&encoded).unwrap();

        assert_eq!(decoded.timestamp, msg.timestamp);
    }

    #[test]
    fn test_ws_message_decode_invalid() {
        let data = [0, 0, 0];
        let result = WsMessage::decode(&data);
        assert!(result.is_err());
    }
}
