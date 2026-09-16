use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompensationCreatedEvent {
    #[serde(rename = "compensation")]
    pub compensation: Option<Compensation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompensationUpdatedEvent {
    #[serde(rename = "compensation")]
    pub compensation: Option<Compensation>,
    #[serde(rename = "old_compensation")]
    pub old_compensation: Option<Compensation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompensationDeletedEvent {
    #[serde(rename = "compensation_id")]
    pub compensation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Compensation {
    #[serde(rename = "compensation_id")]
    pub compensation_id: Option<String>,
    #[serde(rename = "employee_id")]
    pub employee_id: Option<String>,
    #[serde(rename = "compensation_type")]
    pub compensation_type: Option<String>,
    #[serde(rename = "amount")]
    pub amount: Option<f64>,
    #[serde(rename = "currency")]
    pub currency: Option<String>,
    #[serde(rename = "effective_date")]
    pub effective_date: Option<String>,
    #[serde(rename = "end_date")]
    pub end_date: Option<String>,
    #[serde(rename = "status")]
    pub status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compensation_created_event() {
        let json = r#"{
            "compensation": {
                "compensation_id": "comp_xxx",
                "employee_id": "emp_xxx",
                "amount": 50000.00
            }
        }"#;
        let event: CompensationCreatedEvent = serde_json::from_str(json).unwrap();
        assert_eq!(
            event.compensation.unwrap().compensation_id,
            Some("comp_xxx".to_string())
        );
    }
}
