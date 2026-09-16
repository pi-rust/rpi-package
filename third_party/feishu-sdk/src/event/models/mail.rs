use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailGroupCreatedEvent {
    #[serde(rename = "mail_group")]
    pub mail_group: Option<MailGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailGroupDeletedEvent {
    #[serde(rename = "mail_group_id")]
    pub mail_group_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailGroupUpdatedEvent {
    #[serde(rename = "mail_group")]
    pub mail_group: Option<MailGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailGroupMemberAddedEvent {
    #[serde(rename = "mail_group_id")]
    pub mail_group_id: Option<String>,
    #[serde(rename = "member")]
    pub member: Option<MailGroupMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailGroupMemberRemovedEvent {
    #[serde(rename = "mail_group_id")]
    pub mail_group_id: Option<String>,
    #[serde(rename = "member_id")]
    pub member_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicMailboxCreatedEvent {
    #[serde(rename = "public_mailbox")]
    pub public_mailbox: Option<PublicMailbox>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicMailboxDeletedEvent {
    #[serde(rename = "public_mailbox_id")]
    pub public_mailbox_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicMailboxUpdatedEvent {
    #[serde(rename = "public_mailbox")]
    pub public_mailbox: Option<PublicMailbox>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailGroup {
    #[serde(rename = "mail_group_id")]
    pub mail_group_id: Option<String>,
    #[serde(rename = "email")]
    pub email: Option<String>,
    #[serde(rename = "name")]
    pub name: Option<String>,
    #[serde(rename = "description")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailGroupMember {
    #[serde(rename = "member_id")]
    pub member_id: Option<String>,
    #[serde(rename = "member_type")]
    pub member_type: Option<String>,
    #[serde(rename = "email")]
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicMailbox {
    #[serde(rename = "public_mailbox_id")]
    pub public_mailbox_id: Option<String>,
    #[serde(rename = "email")]
    pub email: Option<String>,
    #[serde(rename = "name")]
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mail_group_created_event() {
        let json = r#"{
            "mail_group": {
                "mail_group_id": "mg_xxx",
                "email": "team@example.com",
                "name": "Team Mail"
            }
        }"#;
        let event: MailGroupCreatedEvent = serde_json::from_str(json).unwrap();
        assert_eq!(
            event.mail_group.unwrap().mail_group_id,
            Some("mg_xxx".to_string())
        );
    }
}
