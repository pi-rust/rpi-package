use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserCreatedEvent {
    #[serde(rename = "user")]
    pub user: Option<ContactUser>,

    #[serde(rename = "operator")]
    pub operator: Option<ContactOperator>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserUpdatedEvent {
    #[serde(rename = "user")]
    pub user: Option<ContactUser>,

    #[serde(rename = "operator")]
    pub operator: Option<ContactOperator>,

    #[serde(rename = "old_user")]
    pub old_user: Option<ContactUser>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDeletedEvent {
    #[serde(rename = "user_id")]
    pub user_id: Option<ContactUserId>,

    #[serde(rename = "operator")]
    pub operator: Option<ContactOperator>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepartmentCreatedEvent {
    #[serde(rename = "department")]
    pub department: Option<ContactDepartment>,

    #[serde(rename = "operator")]
    pub operator: Option<ContactOperator>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepartmentUpdatedEvent {
    #[serde(rename = "department")]
    pub department: Option<ContactDepartment>,

    #[serde(rename = "operator")]
    pub operator: Option<ContactOperator>,

    #[serde(rename = "old_department")]
    pub old_department: Option<ContactDepartment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepartmentDeletedEvent {
    #[serde(rename = "department_id")]
    pub department_id: Option<String>,

    #[serde(rename = "operator")]
    pub operator: Option<ContactOperator>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactUser {
    #[serde(rename = "user_id")]
    pub user_id: Option<ContactUserId>,

    #[serde(rename = "name")]
    pub name: Option<String>,

    #[serde(rename = "en_name")]
    pub en_name: Option<String>,

    #[serde(rename = "nickname")]
    pub nickname: Option<String>,

    #[serde(rename = "mobile")]
    pub mobile: Option<String>,

    #[serde(rename = "gender")]
    pub gender: Option<i32>,

    #[serde(rename = "email")]
    pub email: Option<String>,

    #[serde(rename = "status")]
    pub status: Option<ContactUserStatus>,

    #[serde(rename = "department_ids")]
    pub department_ids: Option<Vec<String>>,

    #[serde(rename = "leader_user_id")]
    pub leader_user_id: Option<ContactUserId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactUserId {
    #[serde(rename = "open_id")]
    pub open_id: Option<String>,

    #[serde(rename = "user_id")]
    pub user_id: Option<String>,

    #[serde(rename = "union_id")]
    pub union_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactUserStatus {
    #[serde(rename = "is_frozen")]
    pub is_frozen: Option<bool>,

    #[serde(rename = "is_resigned")]
    pub is_resigned: Option<bool>,

    #[serde(rename = "is_unactivate")]
    pub is_unactivate: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactOperator {
    #[serde(rename = "operator_id")]
    pub operator_id: Option<ContactUserId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactDepartment {
    #[serde(rename = "department_id")]
    pub department_id: Option<String>,

    #[serde(rename = "name")]
    pub name: Option<String>,

    #[serde(rename = "en_name")]
    pub en_name: Option<String>,

    #[serde(rename = "parent_department_id")]
    pub parent_department_id: Option<String>,

    #[serde(rename = "leader_user_id")]
    pub leader_user_id: Option<String>,

    #[serde(rename = "chat_id")]
    pub chat_id: Option<String>,

    #[serde(rename = "order")]
    pub order: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_created_event_deserialize() {
        let json = r#"{
            "user": {
                "user_id": {
                    "open_id": "ou_xxx",
                    "user_id": "xxx"
                },
                "name": "张三",
                "mobile": "+8613800138000"
            },
            "operator": {
                "operator_id": {
                    "open_id": "ou_yyy"
                }
            }
        }"#;

        let event: UserCreatedEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.user.unwrap().name, Some("张三".to_string()));
    }
}
