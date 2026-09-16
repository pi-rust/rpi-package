use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAccessedEvent {
    #[serde(rename = "file")]
    pub file: Option<SecurityFile>,
    #[serde(rename = "accessor")]
    pub accessor: Option<SecurityUser>,
    #[serde(rename = "access_time")]
    pub access_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDownloadedEvent {
    #[serde(rename = "file")]
    pub file: Option<SecurityFile>,
    #[serde(rename = "downloader")]
    pub downloader: Option<SecurityUser>,
    #[serde(rename = "download_time")]
    pub download_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSharedEvent {
    #[serde(rename = "file")]
    pub file: Option<SecurityFile>,
    #[serde(rename = "sharer")]
    pub sharer: Option<SecurityUser>,
    #[serde(rename = "share_to")]
    pub share_to: Option<Vec<SecurityUser>>,
    #[serde(rename = "share_time")]
    pub share_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionChangedEvent {
    #[serde(rename = "file")]
    pub file: Option<SecurityFile>,
    #[serde(rename = "operator")]
    pub operator: Option<SecurityUser>,
    #[serde(rename = "old_permission")]
    pub old_permission: Option<String>,
    #[serde(rename = "new_permission")]
    pub new_permission: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DlpAlertEvent {
    #[serde(rename = "alert_id")]
    pub alert_id: Option<String>,
    #[serde(rename = "alert_type")]
    pub alert_type: Option<String>,
    #[serde(rename = "severity")]
    pub severity: Option<String>,
    #[serde(rename = "user")]
    pub user: Option<SecurityUser>,
    #[serde(rename = "file")]
    pub file: Option<SecurityFile>,
    #[serde(rename = "trigger_time")]
    pub trigger_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityFile {
    #[serde(rename = "file_id")]
    pub file_id: Option<String>,
    #[serde(rename = "name")]
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub type_: Option<String>,
    #[serde(rename = "token")]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityUser {
    #[serde(rename = "open_id")]
    pub open_id: Option<String>,
    #[serde(rename = "user_id")]
    pub user_id: Option<String>,
    #[serde(rename = "name")]
    pub name: Option<String>,
    #[serde(rename = "department_id")]
    pub department_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_accessed_event() {
        let json = r#"{
            "file": {
                "file_id": "file_xxx",
                "name": "sensitive.docx"
            },
            "accessor": {
                "open_id": "ou_xxx"
            }
        }"#;
        let event: FileAccessedEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.file.unwrap().file_id, Some("file_xxx".to_string()));
    }
}
