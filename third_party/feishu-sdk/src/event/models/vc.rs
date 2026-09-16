use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingStartedEvent {
    #[serde(rename = "meeting")]
    pub meeting: Option<Meeting>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingEndedEvent {
    #[serde(rename = "meeting")]
    pub meeting: Option<Meeting>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingJoinEvent {
    #[serde(rename = "meeting")]
    pub meeting: Option<Meeting>,
    #[serde(rename = "participant")]
    pub participant: Option<Participant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingLeaveEvent {
    #[serde(rename = "meeting")]
    pub meeting: Option<Meeting>,
    #[serde(rename = "participant")]
    pub participant: Option<Participant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingRecordingStartedEvent {
    #[serde(rename = "meeting")]
    pub meeting: Option<Meeting>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingRecordingEndedEvent {
    #[serde(rename = "meeting")]
    pub meeting: Option<Meeting>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingRecordingReadyEvent {
    #[serde(rename = "meeting")]
    pub meeting: Option<Meeting>,
    #[serde(rename = "recording")]
    pub recording: Option<Recording>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingShareStartedEvent {
    #[serde(rename = "meeting")]
    pub meeting: Option<Meeting>,
    #[serde(rename = "sharer")]
    pub sharer: Option<Participant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingShareEndedEvent {
    #[serde(rename = "meeting")]
    pub meeting: Option<Meeting>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomCreatedEvent {
    #[serde(rename = "room")]
    pub room: Option<Room>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomUpdatedEvent {
    #[serde(rename = "room")]
    pub room: Option<Room>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomDeletedEvent {
    #[serde(rename = "room")]
    pub room: Option<Room>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meeting {
    #[serde(rename = "meeting_id")]
    pub meeting_id: Option<String>,
    #[serde(rename = "topic")]
    pub topic: Option<String>,
    #[serde(rename = "start_time")]
    pub start_time: Option<i64>,
    #[serde(rename = "end_time")]
    pub end_time: Option<i64>,
    #[serde(rename = "status")]
    pub status: Option<String>,
    #[serde(rename = "host")]
    pub host: Option<Participant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    #[serde(rename = "participant_id")]
    pub participant_id: Option<String>,
    #[serde(rename = "user_id")]
    pub user_id: Option<String>,
    #[serde(rename = "display_name")]
    pub display_name: Option<String>,
    #[serde(rename = "join_time")]
    pub join_time: Option<i64>,
    #[serde(rename = "leave_time")]
    pub leave_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recording {
    #[serde(rename = "recording_id")]
    pub recording_id: Option<String>,
    #[serde(rename = "start_time")]
    pub start_time: Option<i64>,
    #[serde(rename = "end_time")]
    pub end_time: Option<i64>,
    #[serde(rename = "file_url")]
    pub file_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    #[serde(rename = "room_id")]
    pub room_id: Option<String>,
    #[serde(rename = "name")]
    pub name: Option<String>,
    #[serde(rename = "capacity")]
    pub capacity: Option<i32>,
    #[serde(rename = "building_id")]
    pub building_id: Option<String>,
    #[serde(rename = "floor_id")]
    pub floor_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_meeting_started_event() {
        let json = r#"{
            "meeting": {
                "meeting_id": "mtg_xxx",
                "topic": "Team Meeting",
                "status": "started"
            }
        }"#;
        let event: MeetingStartedEvent = serde_json::from_str(json).unwrap();
        assert_eq!(
            event.meeting.unwrap().meeting_id,
            Some("mtg_xxx".to_string())
        );
    }
}
