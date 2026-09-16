use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarAttendee {
    pub user_id: Option<String>,
    pub display_name: Option<String>,
    pub response_status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEventBody {
    pub calendar_id: String,
    pub event_id: String,
    pub summary: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    #[serde(default)]
    pub attendees: Vec<CalendarAttendee>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEventCreated {
    #[serde(flatten)]
    pub event: CalendarEventBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEventUpdated {
    #[serde(flatten)]
    pub event: CalendarEventBody,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calendar_event_body_deserialize() {
        let json = r#"{
            "calendar_id":"cal_123",
            "event_id":"evt_123",
            "summary":"Weekly sync",
            "start_time":"2026-03-03T09:00:00Z",
            "end_time":"2026-03-03T09:30:00Z",
            "attendees":[{"user_id":"ou_1","display_name":"Alice","response_status":"accept"}]
        }"#;
        let event: CalendarEventBody = serde_json::from_str(json).expect("calendar event");
        assert_eq!(event.calendar_id, "cal_123");
        assert_eq!(event.event_id, "evt_123");
        assert_eq!(event.summary.as_deref(), Some("Weekly sync"));
        assert_eq!(event.attendees.len(), 1);
        assert_eq!(event.attendees[0].display_name.as_deref(), Some("Alice"));
    }
}
