use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthenLoginEvent {
    #[serde(rename = "user")]
    pub user: Option<ExtUser>,
    #[serde(rename = "login_time")]
    pub login_time: Option<i64>,
    #[serde(rename = "login_type")]
    pub login_type: Option<String>,
    #[serde(rename = "device_info")]
    pub device_info: Option<DeviceInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthenLogoutEvent {
    #[serde(rename = "user")]
    pub user: Option<ExtUser>,
    #[serde(rename = "logout_time")]
    pub logout_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileCreatedEvent {
    #[serde(rename = "file")]
    pub file: Option<ExtFile>,
    #[serde(rename = "creator")]
    pub creator: Option<ExtUser>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDeletedEvent {
    #[serde(rename = "file_token")]
    pub file_token: Option<String>,
    #[serde(rename = "deleter")]
    pub deleter: Option<ExtUser>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtUser {
    #[serde(rename = "open_id")]
    pub open_id: Option<String>,
    #[serde(rename = "union_id")]
    pub union_id: Option<String>,
    #[serde(rename = "user_id")]
    pub user_id: Option<String>,
    #[serde(rename = "name")]
    pub name: Option<String>,
    #[serde(rename = "tenant_key")]
    pub tenant_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtFile {
    #[serde(rename = "token")]
    pub token: Option<String>,
    #[serde(rename = "name")]
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub type_: Option<String>,
    #[serde(rename = "size")]
    pub size: Option<i64>,
    #[serde(rename = "create_time")]
    pub create_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    #[serde(rename = "device_id")]
    pub device_id: Option<String>,
    #[serde(rename = "device_type")]
    pub device_type: Option<String>,
    #[serde(rename = "os")]
    pub os: Option<String>,
    #[serde(rename = "ip")]
    pub ip: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authen_login_event() {
        let json = r#"{
            "user": {
                "open_id": "ou_xxx"
            },
            "login_type": "password"
        }"#;
        let event: AuthenLoginEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.login_type, Some("password".to_string()));
    }
}
