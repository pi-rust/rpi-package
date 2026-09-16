use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketCreatedEvent {
    #[serde(rename = "ticket")]
    pub ticket: Option<Ticket>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketUpdatedEvent {
    #[serde(rename = "ticket")]
    pub ticket: Option<Ticket>,
    #[serde(rename = "old_ticket")]
    pub old_ticket: Option<Ticket>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketStatusChangedEvent {
    #[serde(rename = "ticket")]
    pub ticket: Option<Ticket>,
    #[serde(rename = "from_status")]
    pub from_status: Option<i32>,
    #[serde(rename = "to_status")]
    pub to_status: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    #[serde(rename = "ticket_id")]
    pub ticket_id: Option<String>,
    #[serde(rename = "title")]
    pub title: Option<String>,
    #[serde(rename = "description")]
    pub description: Option<String>,
    #[serde(rename = "status")]
    pub status: Option<i32>,
    #[serde(rename = "priority")]
    pub priority: Option<i32>,
    #[serde(rename = "category_id")]
    pub category_id: Option<String>,
    #[serde(rename = "creator")]
    pub creator: Option<TicketUser>,
    #[serde(rename = "assignee")]
    pub assignee: Option<TicketUser>,
    #[serde(rename = "create_time")]
    pub create_time: Option<i64>,
    #[serde(rename = "update_time")]
    pub update_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketUser {
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
    fn test_ticket_created_event() {
        let json = r#"{
            "ticket": {
                "ticket_id": "tkt_xxx",
                "title": "Help needed",
                "status": 1
            }
        }"#;
        let event: TicketCreatedEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.ticket.unwrap().ticket_id, Some("tkt_xxx".to_string()));
    }
}
