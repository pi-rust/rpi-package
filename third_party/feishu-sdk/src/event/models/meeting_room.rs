use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingRoomCreatedEvent {
    #[serde(rename = "room")]
    pub room: Option<MeetingRoom>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingRoomUpdatedEvent {
    #[serde(rename = "room")]
    pub room: Option<MeetingRoom>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingRoomDeletedEvent {
    #[serde(rename = "room_id")]
    pub room_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingBuildingCreatedEvent {
    #[serde(rename = "building")]
    pub building: Option<Building>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingBuildingUpdatedEvent {
    #[serde(rename = "building")]
    pub building: Option<Building>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingBuildingDeletedEvent {
    #[serde(rename = "building_id")]
    pub building_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingRoom {
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
    #[serde(rename = "equipment")]
    pub equipment: Option<Vec<Equipment>>,
    #[serde(rename = "status")]
    pub status: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Building {
    #[serde(rename = "building_id")]
    pub building_id: Option<String>,
    #[serde(rename = "name")]
    pub name: Option<String>,
    #[serde(rename = "address")]
    pub address: Option<String>,
    #[serde(rename = "city")]
    pub city: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Equipment {
    #[serde(rename = "equipment_id")]
    pub equipment_id: Option<String>,
    #[serde(rename = "name")]
    pub name: Option<String>,
    #[serde(rename = "count")]
    pub count: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_meeting_room_created_event() {
        let json = r#"{
            "room": {
                "room_id": "room_xxx",
                "name": "Conference Room A",
                "capacity": 10
            }
        }"#;
        let event: MeetingRoomCreatedEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.room.unwrap().room_id, Some("room_xxx".to_string()));
    }
}
