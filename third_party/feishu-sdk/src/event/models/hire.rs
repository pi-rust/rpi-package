use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationDeletedEvent {
    #[serde(rename = "application")]
    pub application: Option<Application>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationStageChangedEvent {
    #[serde(rename = "application")]
    pub application: Option<Application>,
    #[serde(rename = "from_stage")]
    pub from_stage: Option<Stage>,
    #[serde(rename = "to_stage")]
    pub to_stage: Option<Stage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EcoAccountCreatedEvent {
    #[serde(rename = "account_id")]
    pub account_id: Option<String>,
    #[serde(rename = "account_type")]
    pub account_type: Option<i32>,
    #[serde(rename = "custom_field_list")]
    pub custom_field_list: Option<Vec<CustomField>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EcoBackgroundCheckCanceledEvent {
    #[serde(rename = "background_check_id")]
    pub background_check_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EcoBackgroundCheckCreatedEvent {
    #[serde(rename = "background_check_id")]
    pub background_check_id: Option<String>,
    #[serde(rename = "candidate")]
    pub candidate: Option<Candidate>,
    #[serde(rename = "custom_field_list")]
    pub custom_field_list: Option<Vec<CustomField>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EcoExamCreatedEvent {
    #[serde(rename = "exam_id")]
    pub exam_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EhrImportTaskImportedEvent {
    #[serde(rename = "task_id")]
    pub task_id: Option<String>,
    #[serde(rename = "success_count")]
    pub success_count: Option<i32>,
    #[serde(rename = "fail_count")]
    pub fail_count: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferStatusChangedEvent {
    #[serde(rename = "offer")]
    pub offer: Option<Offer>,
    #[serde(rename = "from_status")]
    pub from_status: Option<i32>,
    #[serde(rename = "to_status")]
    pub to_status: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TalentDeletedEvent {
    #[serde(rename = "talent_id")]
    pub talent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Application {
    #[serde(rename = "application_id")]
    pub application_id: Option<String>,
    #[serde(rename = "recruitment_type")]
    pub recruitment_type: Option<i32>,
    #[serde(rename = "job")]
    pub job: Option<Job>,
    #[serde(rename = "candidate")]
    pub candidate: Option<Candidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stage {
    #[serde(rename = "stage_id")]
    pub stage_id: Option<String>,
    #[serde(rename = "stage_type")]
    pub stage_type: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    #[serde(rename = "candidate_id")]
    pub candidate_id: Option<String>,
    #[serde(rename = "name")]
    pub name: Option<String>,
    #[serde(rename = "mobile")]
    pub mobile: Option<String>,
    #[serde(rename = "email")]
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    #[serde(rename = "job_id")]
    pub job_id: Option<String>,
    #[serde(rename = "title")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Offer {
    #[serde(rename = "offer_id")]
    pub offer_id: Option<String>,
    #[serde(rename = "application_id")]
    pub application_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomField {
    #[serde(rename = "custom_field_id")]
    pub custom_field_id: Option<String>,
    #[serde(rename = "value")]
    pub value: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_application_deleted_event() {
        let json = r#"{
            "application": {
                "application_id": "app_xxx",
                "recruitment_type": 1
            }
        }"#;
        let event: ApplicationDeletedEvent = serde_json::from_str(json).unwrap();
        assert_eq!(
            event.application.unwrap().application_id,
            Some("app_xxx".to_string())
        );
    }
}
