use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostCreatedEvent {
    #[serde(rename = "post")]
    pub post: Option<Post>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostDeletedEvent {
    #[serde(rename = "post_id")]
    pub post_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostVisibilityChangedEvent {
    #[serde(rename = "post_id")]
    pub post_id: Option<String>,
    #[serde(rename = "from_visibility")]
    pub from_visibility: Option<String>,
    #[serde(rename = "to_visibility")]
    pub to_visibility: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentCreatedEvent {
    #[serde(rename = "comment")]
    pub comment: Option<Comment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentDeletedEvent {
    #[serde(rename = "comment_id")]
    pub comment_id: Option<String>,
    #[serde(rename = "post_id")]
    pub post_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LikeCreatedEvent {
    #[serde(rename = "like")]
    pub like: Option<Like>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LikeDeletedEvent {
    #[serde(rename = "like_id")]
    pub like_id: Option<String>,
    #[serde(rename = "post_id")]
    pub post_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Post {
    #[serde(rename = "post_id")]
    pub post_id: Option<String>,
    #[serde(rename = "content")]
    pub content: Option<String>,
    #[serde(rename = "visibility")]
    pub visibility: Option<String>,
    #[serde(rename = "author")]
    pub author: Option<MomentsUser>,
    #[serde(rename = "create_time")]
    pub create_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    #[serde(rename = "comment_id")]
    pub comment_id: Option<String>,
    #[serde(rename = "post_id")]
    pub post_id: Option<String>,
    #[serde(rename = "content")]
    pub content: Option<String>,
    #[serde(rename = "author")]
    pub author: Option<MomentsUser>,
    #[serde(rename = "create_time")]
    pub create_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Like {
    #[serde(rename = "like_id")]
    pub like_id: Option<String>,
    #[serde(rename = "post_id")]
    pub post_id: Option<String>,
    #[serde(rename = "user")]
    pub user: Option<MomentsUser>,
    #[serde(rename = "create_time")]
    pub create_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentsUser {
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
    fn test_post_created_event() {
        let json = r#"{
            "post": {
                "post_id": "post_xxx",
                "content": "Hello World",
                "visibility": "public"
            }
        }"#;
        let event: PostCreatedEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.post.unwrap().post_id, Some("post_xxx".to_string()));
    }
}
