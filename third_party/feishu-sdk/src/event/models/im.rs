use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEvent {
    #[serde(rename = "sender")]
    pub sender: MessageSender,

    #[serde(rename = "message")]
    pub message: Message,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSender {
    #[serde(rename = "sender_id")]
    pub sender_id: Option<MessageId>,

    #[serde(rename = "sender_type")]
    pub sender_type: Option<String>,

    #[serde(rename = "tenant_key")]
    pub tenant_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageId {
    #[serde(rename = "open_id")]
    pub open_id: Option<String>,

    #[serde(rename = "user_id")]
    pub user_id: Option<String>,

    #[serde(rename = "union_id")]
    pub union_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    #[serde(rename = "message_id")]
    pub message_id: Option<String>,

    #[serde(rename = "root_id")]
    pub root_id: Option<String>,

    #[serde(rename = "parent_id")]
    pub parent_id: Option<String>,

    #[serde(rename = "create_time")]
    pub create_time: Option<String>,

    #[serde(rename = "chat_id")]
    pub chat_id: Option<String>,

    #[serde(rename = "message_type")]
    pub message_type: Option<String>,

    #[serde(rename = "content")]
    pub content: Option<String>,

    #[serde(rename = "mentions")]
    pub mentions: Option<Vec<MessageMention>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageMention {
    #[serde(rename = "key")]
    pub key: Option<String>,

    #[serde(rename = "id")]
    pub id: Option<MessageId>,

    #[serde(rename = "name")]
    pub name: Option<String>,

    #[serde(rename = "tenant_key")]
    pub tenant_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageReadEvent {
    #[serde(rename = "reader")]
    pub reader: MessageId,

    #[serde(rename = "read_time")]
    pub read_time: Option<String>,

    #[serde(rename = "message_id_list")]
    pub message_id_list: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatDisbandedEvent {
    #[serde(rename = "chat_id")]
    pub chat_id: Option<String>,

    #[serde(rename = "operator_id")]
    pub operator_id: Option<MessageId>,

    #[serde(rename = "operator_tenant_key")]
    pub operator_tenant_key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_event_deserialize() {
        let json = r#"{
            "sender": {
                "sender_id": {
                    "open_id": "ou_xxx",
                    "user_id": "xxx"
                },
                "sender_type": "user",
                "tenant_key": "tenant_xxx"
            },
            "message": {
                "message_id": "om_xxx",
                "chat_id": "oc_xxx",
                "message_type": "text",
                "content": "{\"text\":\"hello\"}"
            }
        }"#;

        let event: MessageEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.sender.sender_type, Some("user".to_string()));
        assert_eq!(event.message.message_type, Some("text".to_string()));
    }
}
