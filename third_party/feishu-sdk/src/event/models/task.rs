use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCreatedEvent {
    #[serde(rename = "task")]
    pub task: Option<Task>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskUpdatedEvent {
    #[serde(rename = "task")]
    pub task: Option<Task>,
    #[serde(rename = "old_task")]
    pub old_task: Option<Task>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDeletedEvent {
    #[serde(rename = "task_id")]
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCompletedEvent {
    #[serde(rename = "task")]
    pub task: Option<Task>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    #[serde(rename = "task_id")]
    pub task_id: Option<String>,
    #[serde(rename = "title")]
    pub title: Option<String>,
    #[serde(rename = "description")]
    pub description: Option<String>,
    #[serde(rename = "status")]
    pub status: Option<i32>,
    #[serde(rename = "priority")]
    pub priority: Option<i32>,
    #[serde(rename = "creator")]
    pub creator: Option<TaskUser>,
    #[serde(rename = "assignees")]
    pub assignees: Option<Vec<TaskUser>>,
    #[serde(rename = "due_time")]
    pub due_time: Option<i64>,
    #[serde(rename = "create_time")]
    pub create_time: Option<i64>,
    #[serde(rename = "update_time")]
    pub update_time: Option<i64>,
    #[serde(rename = "complete_time")]
    pub complete_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskUser {
    #[serde(rename = "open_id")]
    pub open_id: Option<String>,
    #[serde(rename = "user_id")]
    pub user_id: Option<String>,
    #[serde(rename = "name")]
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_created_event() {
        let json = r#"{
            "task": {
                "task_id": "task_xxx",
                "title": "Complete report",
                "status": 0
            }
        }"#;
        let event: TaskCreatedEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.task.unwrap().task_id, Some("task_xxx".to_string()));
    }
}
