use std::collections::HashMap;

use prost::Message;
use serde::{Deserialize, Serialize};

use crate::core::Error;

pub const STREAM_ENDPOINT_URI: &str = "/callback/ws/endpoint";
pub const QUERY_DEVICE_ID: &str = "device_id";
pub const QUERY_SERVICE_ID: &str = "service_id";

pub const HEADER_TIMESTAMP: &str = "timestamp";
pub const HEADER_TYPE: &str = "type";
pub const HEADER_MESSAGE_ID: &str = "message_id";
pub const HEADER_SUM: &str = "sum";
pub const HEADER_SEQ: &str = "seq";
pub const HEADER_TRACE_ID: &str = "trace_id";
pub const HEADER_INSTANCE_ID: &str = "instance_id";
pub const HEADER_BIZ_RT: &str = "biz_rt";
pub const HEADER_HANDSHAKE_STATUS: &str = "Handshake-Status";
pub const HEADER_HANDSHAKE_MSG: &str = "Handshake-Msg";
pub const HEADER_HANDSHAKE_AUTH_ERR_CODE: &str = "Handshake-Autherrcode";

pub const STATUS_OK: i32 = 0;
pub const STATUS_SYSTEM_BUSY: i32 = 1;
pub const STATUS_FORBIDDEN: i32 = 403;
pub const STATUS_AUTH_FAILED: i32 = 514;
pub const STATUS_EXCEED_CONN_LIMIT: i32 = 1_000_040_350;
pub const STATUS_INTERNAL_ERROR: i32 = 1_000_040_343;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Event,
    Card,
    Ping,
    Pong,
}

impl MessageType {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageType::Event => "event",
            MessageType::Card => "card",
            MessageType::Ping => "ping",
            MessageType::Pong => "pong",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "event" => Some(Self::Event),
            "card" => Some(Self::Card),
            "ping" => Some(Self::Ping),
            "pong" => Some(Self::Pong),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Control = 0,
    Data = 1,
}

impl FrameType {
    pub fn parse(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Control),
            1 => Some(Self::Data),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct EndpointResponse {
    #[serde(default)]
    pub code: i32,
    #[serde(default)]
    pub msg: String,
    pub data: Option<Endpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Endpoint {
    #[serde(rename = "URL", default)]
    pub url: String,
    #[serde(rename = "ClientConfig")]
    pub client_config: Option<ClientConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ClientConfig {
    #[serde(rename = "ReconnectCount", default)]
    pub reconnect_count: i32,
    #[serde(rename = "ReconnectInterval", default)]
    pub reconnect_interval: i32,
    #[serde(rename = "ReconnectNonce", default)]
    pub reconnect_nonce: i32,
    #[serde(rename = "PingInterval", default)]
    pub ping_interval: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct StreamResponse {
    #[serde(rename = "code")]
    pub status_code: u16,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<u8>,
}

impl StreamResponse {
    pub fn ok() -> Self {
        Self::from_status(200)
    }

    pub fn from_status(status_code: u16) -> Self {
        Self {
            status_code,
            headers: HashMap::new(),
            data: Vec::new(),
        }
    }
}

impl From<&crate::event::EventResp> for StreamResponse {
    fn from(resp: &crate::event::EventResp) -> Self {
        Self {
            status_code: resp.status_code,
            headers: resp
                .headers
                .iter()
                .filter_map(|(key, values)| {
                    values.first().map(|value| (key.clone(), value.clone()))
                })
                .collect(),
            data: resp.body.clone(),
        }
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct Header {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(string, tag = "2")]
    pub value: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct Frame {
    #[prost(uint64, tag = "1")]
    pub seq_id: u64,
    #[prost(uint64, tag = "2")]
    pub log_id: u64,
    #[prost(int32, tag = "3")]
    pub service: i32,
    #[prost(int32, tag = "4")]
    pub method: i32,
    #[prost(message, repeated, tag = "5")]
    pub headers: Vec<Header>,
    #[prost(string, tag = "6")]
    pub payload_encoding: String,
    #[prost(string, tag = "7")]
    pub payload_type: String,
    #[prost(bytes = "vec", tag = "8")]
    pub payload: Vec<u8>,
    #[prost(string, tag = "9")]
    pub log_id_new: String,
}

impl Frame {
    pub fn decode_binary(data: &[u8]) -> Result<Self, Error> {
        Self::decode(data).map_err(|e| Error::SerializationError(e.to_string()))
    }

    /// Encode with proto2 semantics (explicitly emit every field, including
    /// zero values) to match the official Feishu/Lark SDK wire format.
    ///
    /// The prost-generated `encode_to_vec` uses proto3 semantics and omits
    /// zero-valued fields (SeqID=0, LogID=0, method=0). The Feishu long
    // connection server does not respond to frames that omit these fields,
    // so the protocol session never activates and no events are pushed.
    pub fn encode_binary(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        write_uint64(&mut out, 1, self.seq_id);
        write_uint64(&mut out, 2, self.log_id);
        write_int32(&mut out, 3, self.service);
        write_int32(&mut out, 4, self.method);
        for header in &self.headers {
            let mut body = Vec::new();
            write_string(&mut body, 1, &header.key);
            write_string(&mut body, 2, &header.value);
            write_len_delimited(&mut out, 5, &body);
        }
        if !self.payload_encoding.is_empty() {
            write_string(&mut out, 6, &self.payload_encoding);
        }
        if !self.payload_type.is_empty() {
            write_string(&mut out, 7, &self.payload_type);
        }
        if !self.payload.is_empty() {
            write_bytes(&mut out, 8, &self.payload);
        }
        if !self.log_id_new.is_empty() {
            write_string(&mut out, 9, &self.log_id_new);
        }
        out
    }

    pub fn ping(service_id: i32) -> Self {
        let mut frame = Self {
            service: service_id,
            method: FrameType::Control as i32,
            ..Default::default()
        };
        frame.set_header(HEADER_TYPE, MessageType::Ping.as_str());
        frame
    }

    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|header| header.key == key)
            .map(|header| header.value.as_str())
    }

    pub fn header_i32(&self, key: &str) -> Option<i32> {
        self.header(key)?.parse().ok()
    }

    pub fn set_header(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        if let Some(header) = self.headers.iter_mut().find(|header| header.key == key) {
            header.value = value;
            return;
        }
        self.headers.push(Header { key, value });
    }
}

fn write_key(out: &mut Vec<u8>, field: u32, wire_type: u32) {
    write_varint(out, (u64::from(field) << 3) | u64::from(wire_type));
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

fn write_uint64(out: &mut Vec<u8>, field: u32, value: u64) {
    write_key(out, field, 0);
    write_varint(out, value);
}

fn write_int32(out: &mut Vec<u8>, field: u32, value: i32) {
    write_key(out, field, 0);
    write_varint(out, value as u64);
}

fn write_len_delimited(out: &mut Vec<u8>, field: u32, data: &[u8]) {
    write_key(out, field, 2);
    write_varint(out, data.len() as u64);
    out.extend_from_slice(data);
}

fn write_string(out: &mut Vec<u8>, field: u32, value: &str) {
    write_len_delimited(out, field, value.as_bytes());
}

fn write_bytes(out: &mut Vec<u8>, field: u32, value: &[u8]) {
    write_len_delimited(out, field, value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip_matches_protobuf_shape() {
        let mut frame = Frame {
            seq_id: 1,
            log_id: 2,
            service: 3,
            method: FrameType::Data as i32,
            payload: b"hello".to_vec(),
            ..Default::default()
        };
        frame.set_header(HEADER_TYPE, MessageType::Event.as_str());
        frame.set_header(HEADER_MESSAGE_ID, "msg_123");

        let encoded = frame.encode_binary();
        let decoded = Frame::decode_binary(&encoded).expect("decode");

        assert_eq!(decoded.seq_id, 1);
        assert_eq!(decoded.log_id, 2);
        assert_eq!(decoded.service, 3);
        assert_eq!(decoded.method, FrameType::Data as i32);
        assert_eq!(decoded.payload, b"hello");
        assert_eq!(decoded.header(HEADER_TYPE), Some("event"));
        assert_eq!(decoded.header(HEADER_MESSAGE_ID), Some("msg_123"));
    }

    #[test]
    fn endpoint_response_deserializes_server_config() {
        let json = r#"{
            "code": 0,
            "msg": "ok",
            "data": {
                "URL": "wss://example.com/ws?device_id=dev_1&service_id=9",
                "ClientConfig": {
                    "ReconnectCount": 5,
                    "ReconnectInterval": 10,
                    "ReconnectNonce": 3,
                    "PingInterval": 20
                }
            }
        }"#;

        let endpoint: EndpointResponse = serde_json::from_str(json).expect("endpoint response");
        assert_eq!(endpoint.code, 0);
        assert_eq!(
            endpoint.data.as_ref().map(|data| data.url.as_str()),
            Some("wss://example.com/ws?device_id=dev_1&service_id=9")
        );
        let client_config = endpoint
            .data
            .as_ref()
            .and_then(|data| data.client_config.as_ref())
            .expect("client config");
        assert_eq!(client_config.reconnect_count, 5);
        assert_eq!(client_config.ping_interval, 20);
    }

    #[test]
    fn stream_response_flattens_http_style_headers() {
        let mut headers = HashMap::new();
        headers.insert(
            "Content-Type".to_string(),
            vec!["application/json".to_string()],
        );
        let resp = crate::event::EventResp {
            status_code: 200,
            headers,
            body: br#"{"ok":true}"#.to_vec(),
        };

        let stream_resp = StreamResponse::from(&resp);
        assert_eq!(stream_resp.status_code, 200);
        assert_eq!(
            stream_resp.headers.get("Content-Type").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(stream_resp.data, br#"{"ok":true}"#);
    }
}
