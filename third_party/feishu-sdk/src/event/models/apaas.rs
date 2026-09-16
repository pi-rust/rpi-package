use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordCreatedEvent {
    #[serde(rename = "record")]
    pub record: Option<ApaasRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordUpdatedEvent {
    #[serde(rename = "record")]
    pub record: Option<ApaasRecord>,
    #[serde(rename = "old_record")]
    pub old_record: Option<ApaasRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordDeletedEvent {
    #[serde(rename = "record_id")]
    pub record_id: Option<String>,
    #[serde(rename = "object_type")]
    pub object_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApaasRecord {
    #[serde(rename = "record_id")]
    pub record_id: Option<String>,
    #[serde(rename = "object_type")]
    pub object_type: Option<String>,
    #[serde(rename = "fields")]
    pub fields: Option<serde_json::Value>,
    #[serde(rename = "creator_id")]
    pub creator_id: Option<String>,
    #[serde(rename = "create_time")]
    pub create_time: Option<i64>,
    #[serde(rename = "update_time")]
    pub update_time: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_created_event() {
        let json = r#"{
            "record": {
                "record_id": "rec_xxx",
                "object_type": "custom_object"
            }
        }"#;
        let event: RecordCreatedEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.record.unwrap().record_id, Some("rec_xxx".to_string()));
    }
}
