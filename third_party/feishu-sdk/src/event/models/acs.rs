use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessEvent {
    #[serde(rename = "access_record_id")]
    pub access_record_id: Option<String>,
    #[serde(rename = "user")]
    pub user: Option<AcsUser>,
    #[serde(rename = "device")]
    pub device: Option<AcsDevice>,
    #[serde(rename = "access_time")]
    pub access_time: Option<i64>,
    #[serde(rename = "access_type")]
    pub access_type: Option<String>,
    #[serde(rename = "location")]
    pub location: Option<String>,
    #[serde(rename = "result")]
    pub result: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceStatusEvent {
    #[serde(rename = "device_id")]
    pub device_id: Option<String>,
    #[serde(rename = "status")]
    pub status: Option<String>,
    #[serde(rename = "update_time")]
    pub update_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcsUser {
    #[serde(rename = "user_id")]
    pub user_id: Option<String>,
    #[serde(rename = "name")]
    pub name: Option<String>,
    #[serde(rename = "employee_no")]
    pub employee_no: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcsDevice {
    #[serde(rename = "device_id")]
    pub device_id: Option<String>,
    #[serde(rename = "device_name")]
    pub device_name: Option<String>,
    #[serde(rename = "device_type")]
    pub device_type: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_access_event() {
        let json = r#"{
            "access_record_id": "rec_xxx",
            "access_type": "enter",
            "result": "success"
        }"#;
        let event: AccessEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.access_record_id, Some("rec_xxx".to_string()));
    }
}
