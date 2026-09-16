use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewCreatedEvent {
    #[serde(rename = "review")]
    pub review: Option<Review>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewUpdatedEvent {
    #[serde(rename = "review")]
    pub review: Option<Review>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewSubmittedEvent {
    #[serde(rename = "review")]
    pub review: Option<Review>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewCompletedEvent {
    #[serde(rename = "review")]
    pub review: Option<Review>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    #[serde(rename = "review_id")]
    pub review_id: Option<String>,
    #[serde(rename = "reviewee")]
    pub reviewee: Option<PerformanceUser>,
    #[serde(rename = "reviewer")]
    pub reviewer: Option<PerformanceUser>,
    #[serde(rename = "review_period")]
    pub review_period: Option<String>,
    #[serde(rename = "status")]
    pub status: Option<i32>,
    #[serde(rename = "score")]
    pub score: Option<f64>,
    #[serde(rename = "create_time")]
    pub create_time: Option<i64>,
    #[serde(rename = "submit_time")]
    pub submit_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceUser {
    #[serde(rename = "open_id")]
    pub open_id: Option<String>,
    #[serde(rename = "user_id")]
    pub user_id: Option<String>,
    #[serde(rename = "name")]
    pub name: Option<String>,
    #[serde(rename = "employee_id")]
    pub employee_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_review_created_event() {
        let json = r#"{
            "review": {
                "review_id": "rev_xxx",
                "review_period": "2024-Q1",
                "status": 0
            }
        }"#;
        let event: ReviewCreatedEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.review.unwrap().review_id, Some("rev_xxx".to_string()));
    }
}
