use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::ApiEnvelope;
use crate::Client;
use crate::core::{ApiResponse, Error, RequestOptions};
use crate::generated::ops;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SendMessageQuery {
    pub receive_id_type: Option<String>,
}

impl SendMessageQuery {
    pub fn to_query_pairs(&self) -> Vec<(String, String)> {
        let mut query = Vec::new();
        if let Some(v) = &self.receive_id_type {
            query.push(("receive_id_type".to_string(), v.clone()));
        }
        query
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetMessageQuery {
    pub user_id_type: Option<String>,
}

impl GetMessageQuery {
    pub fn to_query_pairs(&self) -> Vec<(String, String)> {
        let mut query = Vec::new();
        if let Some(v) = &self.user_id_type {
            query.push(("user_id_type".to_string(), v.clone()));
        }
        query
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListMessageQuery {
    pub container_id_type: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub sort_type: Option<String>,
    pub page_token: Option<String>,
    pub page_size: Option<u32>,
}

impl ListMessageQuery {
    pub fn to_query_pairs(&self) -> Vec<(String, String)> {
        let mut query = Vec::new();
        if let Some(v) = &self.container_id_type {
            query.push(("container_id_type".to_string(), v.clone()));
        }
        if let Some(v) = &self.start_time {
            query.push(("start_time".to_string(), v.clone()));
        }
        if let Some(v) = &self.end_time {
            query.push(("end_time".to_string(), v.clone()));
        }
        if let Some(v) = &self.sort_type {
            query.push(("sort_type".to_string(), v.clone()));
        }
        if let Some(v) = &self.page_token {
            query.push(("page_token".to_string(), v.clone()));
        }
        if let Some(v) = self.page_size {
            query.push(("page_size".to_string(), v.to_string()));
        }
        query
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReplyMessageQuery {
    pub msg_type: Option<String>,
    pub receive_id_type: Option<String>,
}

impl ReplyMessageQuery {
    pub fn to_query_pairs(&self) -> Vec<(String, String)> {
        let mut query = Vec::new();
        if let Some(v) = &self.msg_type {
            query.push(("msg_type".to_string(), v.clone()));
        }
        if let Some(v) = &self.receive_id_type {
            query.push(("receive_id_type".to_string(), v.clone()));
        }
        query
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SendMessageBody {
    pub receive_id: String,
    pub msg_type: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReplyMessageBody {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender: Option<MessageSender>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageSender {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_key: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SendMessageResponseData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetMessageResponseData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<MessageInfo>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListMessageResponseData {
    #[serde(default)]
    pub items: Vec<MessageInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_more: Option<bool>,
}

pub struct ImV1MessageApi<'a> {
    client: &'a Client,
}

impl<'a> ImV1MessageApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub async fn send(
        &self,
        query: Vec<(String, String)>,
        body: Value,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        self.client
            .call_with_body(
                ops::im::v1::message::CREATE,
                HashMap::new(),
                query,
                &body,
                options,
            )
            .await
    }

    pub async fn send_typed(
        &self,
        query: &SendMessageQuery,
        body: &SendMessageBody,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<SendMessageResponseData>, Error> {
        let raw = self
            .client
            .call_with_body(
                ops::im::v1::message::CREATE,
                HashMap::new(),
                query.to_query_pairs(),
                body,
                options,
            )
            .await?;
        raw.json()
    }

    pub async fn get(
        &self,
        message_id: impl Into<String>,
        query: Vec<(String, String)>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("message_id".to_string(), message_id.into());
        self.client
            .call(ops::im::v1::message::GET, path_params, query, None, options)
            .await
    }

    pub async fn get_typed(
        &self,
        message_id: impl Into<String>,
        query: &GetMessageQuery,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<GetMessageResponseData>, Error> {
        let raw = self
            .get(message_id, query.to_query_pairs(), options)
            .await?;
        raw.json()
    }

    pub async fn list(
        &self,
        chat_id: impl Into<String>,
        query: Vec<(String, String)>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("container_id".to_string(), chat_id.into());
        self.client
            .call(
                ops::im::v1::message::LIST,
                path_params,
                query,
                None,
                options,
            )
            .await
    }

    pub async fn list_typed(
        &self,
        chat_id: impl Into<String>,
        query: &ListMessageQuery,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<ListMessageResponseData>, Error> {
        let raw = self.list(chat_id, query.to_query_pairs(), options).await?;
        raw.json()
    }

    pub async fn delete(
        &self,
        message_id: impl Into<String>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("message_id".to_string(), message_id.into());
        self.client
            .call(
                ops::im::v1::message::DELETE,
                path_params,
                Vec::new(),
                None,
                options,
            )
            .await
    }

    pub async fn reply(
        &self,
        message_id: impl Into<String>,
        query: Vec<(String, String)>,
        body: Value,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("message_id".to_string(), message_id.into());
        self.client
            .call_with_body(
                ops::im::v1::message::REPLY,
                path_params,
                query,
                &body,
                options,
            )
            .await
    }

    pub async fn reply_typed(
        &self,
        message_id: impl Into<String>,
        query: &ReplyMessageQuery,
        body: &ReplyMessageBody,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<SendMessageResponseData>, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("message_id".to_string(), message_id.into());
        let raw = self
            .client
            .call_with_body(
                ops::im::v1::message::REPLY,
                path_params,
                query.to_query_pairs(),
                body,
                options,
            )
            .await?;
        raw.json()
    }
}

use serde_json::Value;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_query_serializes() {
        let query = SendMessageQuery {
            receive_id_type: Some("open_id".to_string()),
        };
        assert_eq!(
            query.to_query_pairs(),
            vec![("receive_id_type".to_string(), "open_id".to_string())]
        );
    }
}
