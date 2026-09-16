use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmployeeCreatedEvent {
    #[serde(rename = "employee")]
    pub employee: Option<Employee>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmployeeUpdatedEvent {
    #[serde(rename = "employee")]
    pub employee: Option<Employee>,
    #[serde(rename = "old_employee")]
    pub old_employee: Option<Employee>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmployeeDeletedEvent {
    #[serde(rename = "employee_id")]
    pub employee_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorehrDepartmentCreatedEvent {
    #[serde(rename = "department")]
    pub department: Option<CorehrDepartment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorehrDepartmentUpdatedEvent {
    #[serde(rename = "department")]
    pub department: Option<CorehrDepartment>,
    #[serde(rename = "old_department")]
    pub old_department: Option<CorehrDepartment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorehrDepartmentDeletedEvent {
    #[serde(rename = "department_id")]
    pub department_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Employee {
    #[serde(rename = "employee_id")]
    pub employee_id: Option<String>,
    #[serde(rename = "employee_no")]
    pub employee_no: Option<String>,
    #[serde(rename = "name")]
    pub name: Option<String>,
    #[serde(rename = "status")]
    pub status: Option<i32>,
    #[serde(rename = "department_id")]
    pub department_id: Option<String>,
    #[serde(rename = "manager_id")]
    pub manager_id: Option<String>,
    #[serde(rename = "hire_date")]
    pub hire_date: Option<String>,
    #[serde(rename = "termination_date")]
    pub termination_date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorehrDepartment {
    #[serde(rename = "department_id")]
    pub department_id: Option<String>,
    #[serde(rename = "name")]
    pub name: Option<String>,
    #[serde(rename = "parent_department_id")]
    pub parent_department_id: Option<String>,
    #[serde(rename = "manager_id")]
    pub manager_id: Option<String>,
    #[serde(rename = "status")]
    pub status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_employee_created_event() {
        let json = r#"{
            "employee": {
                "employee_id": "emp_xxx",
                "employee_no": "E001",
                "name": "John Doe"
            }
        }"#;
        let event: EmployeeCreatedEvent = serde_json::from_str(json).unwrap();
        assert_eq!(
            event.employee.unwrap().employee_id,
            Some("emp_xxx".to_string())
        );
    }
}
