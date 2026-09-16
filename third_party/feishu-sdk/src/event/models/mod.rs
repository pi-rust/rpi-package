pub mod acs;
pub mod apaas;
pub mod application;
pub mod approval;
pub mod calendar;
pub mod compensation;
pub mod contact;
pub mod corehr;
pub mod drive;
pub mod ext;
pub mod helpdesk;
pub mod hire;
pub mod im;
pub mod mail;
pub mod meeting_room;
pub mod moments;
pub mod payroll;
pub mod performance;
pub mod security_and_compliance;
pub mod task;
pub mod vc;

pub use acs::*;
pub use apaas::*;
pub use application::*;
pub use approval::*;
pub use calendar::*;
pub use compensation::*;
pub use contact::*;
pub use corehr::*;
pub use drive::*;
pub use ext::*;
pub use helpdesk::*;
pub use hire::*;
pub use im::*;
pub use mail::*;
pub use meeting_room::*;
pub use moments::*;
pub use payroll::*;
pub use performance::*;
pub use security_and_compliance::*;
use serde::{Deserialize, Serialize};
pub use task::*;
pub use vc::*;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EventHeader {
    pub event_id: Option<String>,
    pub event_type: Option<String>,
    pub app_id: Option<String>,
    pub tenant_key: Option<String>,
    pub create_time: Option<String>,
    pub token: Option<String>,
    pub app_ticket: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Event<T = serde_json::Value> {
    pub schema: Option<String>,
    pub header: Option<EventHeader>,
    #[serde(default)]
    pub event: Option<T>,
    pub event_type: Option<String>,
    pub token: Option<String>,
    pub challenge: Option<String>,
    #[serde(rename = "type")]
    pub type_: Option<String>,
}

impl<T> Event<T> {
    pub fn event_id(&self) -> Option<&str> {
        self.header.as_ref()?.event_id.as_deref()
    }

    pub fn event_type(&self) -> Option<&str> {
        self.header
            .as_ref()?
            .event_type
            .as_deref()
            .or(self.event_type.as_deref())
    }

    pub fn tenant_key(&self) -> Option<&str> {
        self.header.as_ref()?.tenant_key.as_deref()
    }

    pub fn app_id(&self) -> Option<&str> {
        self.header.as_ref()?.app_id.as_deref()
    }

    pub fn is_challenge(&self) -> bool {
        self.type_.as_deref() == Some("url_verification") || self.challenge.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeResponse {
    pub challenge: String,
}

impl ChallengeResponse {
    pub fn new(challenge: impl Into<String>) -> Self {
        Self {
            challenge: challenge.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventReq {
    pub header: std::collections::HashMap<String, Vec<String>>,
    pub body: Vec<u8>,
    pub request_uri: String,
}

impl EventReq {
    pub fn get_header(&self, key: &str) -> Option<&str> {
        self.header
            .get(key)
            .and_then(|v| v.first())
            .map(String::as_str)
    }
}

#[derive(Debug, Clone)]
pub struct EventResp {
    pub status_code: u16,
    pub headers: std::collections::HashMap<String, Vec<String>>,
    pub body: Vec<u8>,
}

impl EventResp {
    pub fn ok(body: Vec<u8>) -> Self {
        let mut headers = std::collections::HashMap::new();
        headers.insert(
            "Content-Type".to_string(),
            vec!["application/json".to_string()],
        );
        Self {
            status_code: 200,
            headers,
            body,
        }
    }

    pub fn challenge(challenge: impl Into<String>) -> Self {
        let body = serde_json::to_string(&ChallengeResponse::new(challenge))
            .unwrap_or_default()
            .into_bytes();
        Self::ok(body)
    }

    pub fn error(status: u16, message: impl Into<String>) -> Self {
        let mut headers = std::collections::HashMap::new();
        headers.insert(
            "Content-Type".to_string(),
            vec!["application/json".to_string()],
        );
        Self {
            status_code: status,
            headers,
            body: message.into().into_bytes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_is_challenge() {
        let event: Event = Event {
            type_: Some("url_verification".to_string()),
            ..Default::default()
        };
        assert!(event.is_challenge());

        let event: Event = Event {
            challenge: Some("test".to_string()),
            ..Default::default()
        };
        assert!(event.is_challenge());
    }

    #[test]
    fn test_event_helpers() {
        let event: Event = Event {
            header: Some(EventHeader {
                event_id: Some("evt_123".to_string()),
                event_type: Some("im.message.receive".to_string()),
                tenant_key: Some("tenant_456".to_string()),
                app_id: Some("app_789".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert_eq!(event.event_id(), Some("evt_123"));
        assert_eq!(event.event_type(), Some("im.message.receive"));
        assert_eq!(event.tenant_key(), Some("tenant_456"));
        assert_eq!(event.app_id(), Some("app_789"));
    }

    #[test]
    fn test_challenge_response() {
        let resp = EventResp::challenge("test_challenge");
        assert_eq!(resp.status_code, 200);
        let body_str = String::from_utf8_lossy(&resp.body);
        assert!(body_str.contains("test_challenge"));
    }

    #[test]
    fn test_event_preserves_nested_event_payload() {
        let json = r#"{
            "schema": "2.0",
            "header": {
                "event_type": "im.message.receive_v1"
            },
            "event": {
                "message": {
                    "message_id": "om_xxx",
                    "chat_id": "oc_xxx",
                    "message_type": "text",
                    "content": "{\"text\":\"hello\"}"
                }
            }
        }"#;

        let event: Event = serde_json::from_str(json).unwrap();
        let payload = event.event.expect("event payload");
        assert_eq!(
            payload
                .get("message")
                .and_then(|message| message.get("message_id"))
                .and_then(|value| value.as_str()),
            Some("om_xxx")
        );
    }
}
