use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalInstanceEvent {
    #[serde(rename = "approval_code")]
    pub approval_code: Option<String>,

    #[serde(rename = "instance_code")]
    pub instance_code: Option<String>,

    #[serde(rename = "status")]
    pub status: Option<String>,

    #[serde(rename = "start_time")]
    pub start_time: Option<i64>,

    #[serde(rename = "end_time")]
    pub end_time: Option<i64>,

    #[serde(rename = "user_id")]
    pub user_id: Option<String>,

    #[serde(rename = "open_id")]
    pub open_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalTaskEvent {
    #[serde(rename = "approval_code")]
    pub approval_code: Option<String>,

    #[serde(rename = "instance_code")]
    pub instance_code: Option<String>,

    #[serde(rename = "task_id")]
    pub task_id: Option<String>,

    #[serde(rename = "status")]
    pub status: Option<String>,

    #[serde(rename = "user_id")]
    pub user_id: Option<String>,

    #[serde(rename = "open_id")]
    pub open_id: Option<String>,

    #[serde(rename = "create_time")]
    pub create_time: Option<i64>,

    #[serde(rename = "operate_time")]
    pub operate_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalCreatedEvent {
    #[serde(rename = "approval_code")]
    pub approval_code: Option<String>,

    #[serde(rename = "approval_name")]
    pub approval_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDeletedEvent {
    #[serde(rename = "approval_code")]
    pub approval_code: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_approval_instance_event() {
        let json = r#"{
            "approval_code": "approval_xxx",
            "instance_code": "instance_xxx",
            "status": "APPROVED",
            "start_time": 1609459200,
            "end_time": 1609545600,
            "user_id": "user_xxx",
            "open_id": "ou_xxx"
        }"#;

        let event: ApprovalInstanceEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.approval_code, Some("approval_xxx".to_string()));
        assert_eq!(event.status, Some("APPROVED".to_string()));
    }

    #[test]
    fn test_approval_task_event() {
        let json = r#"{
            "approval_code": "approval_xxx",
            "instance_code": "instance_xxx",
            "task_id": "task_xxx",
            "status": "COMPLETED",
            "user_id": "user_xxx",
            "open_id": "ou_xxx",
            "create_time": 1609459200,
            "operate_time": 1609545600
        }"#;

        let event: ApprovalTaskEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.task_id, Some("task_xxx".to_string()));
        assert_eq!(event.status, Some("COMPLETED".to_string()));
    }
}
