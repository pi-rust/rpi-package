use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriveFileEvent {
    pub file_token: String,
    pub file_name: String,
    pub file_type: Option<String>,
    pub owner_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrivePermissionEvent {
    pub file_token: String,
    pub operator_id: Option<String>,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriveFileCreatedEvent {
    #[serde(flatten)]
    pub file: DriveFileEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrivePermissionChangedEvent {
    #[serde(flatten)]
    pub permission: DrivePermissionEvent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_drive_file_event_deserialize() {
        let json = r#"{
            "file_token":"boxcn123",
            "file_name":"Roadmap.md",
            "file_type":"docx",
            "owner_id":"ou_123"
        }"#;
        let event: DriveFileEvent = serde_json::from_str(json).expect("drive file event");
        assert_eq!(event.file_token, "boxcn123");
        assert_eq!(event.file_name, "Roadmap.md");
        assert_eq!(event.file_type.as_deref(), Some("docx"));
        assert_eq!(event.owner_id.as_deref(), Some("ou_123"));
    }

    #[test]
    fn test_drive_permission_event_deserialize() {
        let json = r#"{
            "file_token":"boxcn123",
            "operator_id":"ou_456",
            "action":"permission_updated"
        }"#;
        let event: DrivePermissionEvent =
            serde_json::from_str(json).expect("drive permission event");
        assert_eq!(event.file_token, "boxcn123");
        assert_eq!(event.operator_id.as_deref(), Some("ou_456"));
        assert_eq!(event.action, "permission_updated");
    }
}
