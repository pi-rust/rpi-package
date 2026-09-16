use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayslipGeneratedEvent {
    #[serde(rename = "payslip")]
    pub payslip: Option<Payslip>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayslipUpdatedEvent {
    #[serde(rename = "payslip")]
    pub payslip: Option<Payslip>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayslipDeletedEvent {
    #[serde(rename = "payslip_id")]
    pub payslip_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payslip {
    #[serde(rename = "payslip_id")]
    pub payslip_id: Option<String>,
    #[serde(rename = "employee_id")]
    pub employee_id: Option<String>,
    #[serde(rename = "pay_period")]
    pub pay_period: Option<String>,
    #[serde(rename = "gross_pay")]
    pub gross_pay: Option<f64>,
    #[serde(rename = "net_pay")]
    pub net_pay: Option<f64>,
    #[serde(rename = "currency")]
    pub currency: Option<String>,
    #[serde(rename = "status")]
    pub status: Option<i32>,
    #[serde(rename = "generate_time")]
    pub generate_time: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_payslip_generated_event() {
        let json = r#"{
            "payslip": {
                "payslip_id": "ps_xxx",
                "employee_id": "emp_xxx",
                "pay_period": "2024-01",
                "net_pay": 10000.00
            }
        }"#;
        let event: PayslipGeneratedEvent = serde_json::from_str(json).unwrap();
        assert_eq!(
            event.payslip.unwrap().payslip_id,
            Some("ps_xxx".to_string())
        );
    }
}
