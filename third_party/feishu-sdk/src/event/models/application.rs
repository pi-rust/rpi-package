use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStatusUpdatedEvent {
    #[serde(rename = "app_id")]
    pub app_id: Option<String>,

    #[serde(rename = "tenant_key")]
    pub tenant_key: Option<String>,

    #[serde(rename = "status")]
    pub status: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppVersionUpdatedEvent {
    #[serde(rename = "app_id")]
    pub app_id: Option<String>,

    #[serde(rename = "version")]
    pub version: Option<String>,

    #[serde(rename = "status")]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInstalledEvent {
    #[serde(rename = "app_id")]
    pub app_id: Option<String>,

    #[serde(rename = "tenant_key")]
    pub tenant_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppUninstalledEvent {
    #[serde(rename = "app_id")]
    pub app_id: Option<String>,

    #[serde(rename = "tenant_key")]
    pub tenant_key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_status_updated_event() {
        let json = r#"{
            "app_id": "cli_xxx",
            "tenant_key": "tenant_xxx",
            "status": 1
        }"#;

        let event: AppStatusUpdatedEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.app_id, Some("cli_xxx".to_string()));
        assert_eq!(event.status, Some(1));
    }

    #[test]
    fn test_app_installed_event() {
        let json = r#"{
            "app_id": "cli_xxx",
            "tenant_key": "tenant_xxx"
        }"#;

        let event: AppInstalledEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.app_id, Some("cli_xxx".to_string()));
    }
}
