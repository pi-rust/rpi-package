use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CardAction {
    #[serde(rename = "open_id")]
    pub open_id: Option<String>,

    #[serde(rename = "user_id")]
    pub user_id: Option<String>,

    #[serde(rename = "union_id")]
    pub union_id: Option<String>,

    #[serde(rename = "open_message_id")]
    pub open_message_id: Option<String>,

    #[serde(rename = "token")]
    pub token: Option<String>,

    #[serde(rename = "action")]
    pub action: Option<CardActionValue>,

    #[serde(rename = "type")]
    pub type_: Option<String>,

    #[serde(rename = "challenge")]
    pub challenge: Option<String>,

    #[serde(skip)]
    pub event_req: Option<crate::event::EventReq>,
}

impl CardAction {
    pub fn is_challenge(&self) -> bool {
        self.type_.as_deref() == Some("url_verification") || self.challenge.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardActionValue {
    #[serde(rename = "value")]
    pub value: Option<serde_json::Value>,

    #[serde(rename = "tag")]
    pub tag: Option<String>,

    #[serde(rename = "option")]
    pub option: Option<String>,

    #[serde(rename = "timezone")]
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardResponse {
    #[serde(rename = "toast")]
    pub toast: Option<CardToast>,

    #[serde(rename = "card")]
    pub card: Option<serde_json::Value>,
}

impl CardResponse {
    pub fn new() -> Self {
        Self {
            toast: None,
            card: None,
        }
    }

    pub fn toast(mut self, toast: CardToast) -> Self {
        self.toast = Some(toast);
        self
    }

    pub fn card(mut self, card: serde_json::Value) -> Self {
        self.card = Some(card);
        self
    }
}

impl Default for CardResponse {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardToast {
    #[serde(rename = "type")]
    pub type_: Option<String>,

    #[serde(rename = "content")]
    pub content: Option<String>,
    #[serde(rename = "duration")]
    pub duration: Option<i64>,
}

impl CardToast {
    pub fn info(content: impl Into<String>) -> Self {
        Self {
            type_: Some("info".to_string()),
            content: Some(content.into()),
            duration: None,
        }
    }

    pub fn success(content: impl Into<String>) -> Self {
        Self {
            type_: Some("success".to_string()),
            content: Some(content.into()),
            duration: None,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            type_: Some("error".to_string()),
            content: Some(content.into()),
            duration: None,
        }
    }

    pub fn duration(mut self, duration: i64) -> Self {
        self.duration = Some(duration);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomResp {
    pub status_code: u16,
    pub body: serde_json::Value,
}

impl CustomResp {
    pub fn new(status_code: u16, body: serde_json::Value) -> Self {
        Self { status_code, body }
    }

    pub fn ok(body: serde_json::Value) -> Self {
        Self::new(200, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_card_action_is_challenge() {
        let action = CardAction {
            type_: Some("url_verification".to_string()),
            ..Default::default()
        };
        assert!(action.is_challenge());

        let action = CardAction {
            challenge: Some("test".to_string()),
            ..Default::default()
        };
        assert!(action.is_challenge());
    }

    #[test]
    fn test_card_response_builder() {
        let response = CardResponse::new().toast(CardToast::success("操作成功"));

        assert!(response.toast.is_some());
        assert_eq!(response.toast.unwrap().type_, Some("success".to_string()));
    }

    #[test]
    fn test_card_toast() {
        let toast = CardToast::error("操作失败").duration(3000);
        assert_eq!(toast.type_, Some("error".to_string()));
        assert_eq!(toast.content, Some("操作失败".to_string()));
        assert_eq!(toast.duration, Some(3000));
    }

    #[test]
    fn test_card_action_deserialize() {
        let json = r#"{
            "open_id": "ou_xxx",
            "user_id": "xxx",
            "token": "verify_token",
            "action": {
                "value": {"key": "value"},
                "tag": "button"
            }
        }"#;

        let action: CardAction = serde_json::from_str(json).unwrap();
        assert_eq!(action.open_id, Some("ou_xxx".to_string()));
        assert!(action.action.is_some());
    }
}
