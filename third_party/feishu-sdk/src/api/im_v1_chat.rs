use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ApiEnvelope;
use crate::Client;
use crate::core::{ApiResponse, Error, RequestOptions};
use crate::generated::ops;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateChatQuery {
    pub user_id_type: Option<String>,
    pub set_bot_manager: Option<bool>,
    pub uuid: Option<String>,
}

impl CreateChatQuery {
    pub fn to_query_pairs(&self) -> Vec<(String, String)> {
        let mut query = Vec::new();
        if let Some(v) = &self.user_id_type {
            query.push(("user_id_type".to_string(), v.clone()));
        }
        if let Some(v) = self.set_bot_manager {
            query.push(("set_bot_manager".to_string(), v.to_string()));
        }
        if let Some(v) = &self.uuid {
            query.push(("uuid".to_string(), v.clone()));
        }
        query
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetChatQuery {
    pub user_id_type: Option<String>,
}

impl GetChatQuery {
    pub fn to_query_pairs(&self) -> Vec<(String, String)> {
        let mut query = Vec::new();
        if let Some(v) = &self.user_id_type {
            query.push(("user_id_type".to_string(), v.clone()));
        }
        query
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeleteChatQuery {
    pub user_id_type: Option<String>,
}

impl DeleteChatQuery {
    pub fn to_query_pairs(&self) -> Vec<(String, String)> {
        let mut query = Vec::new();
        if let Some(v) = &self.user_id_type {
            query.push(("user_id_type".to_string(), v.clone()));
        }
        query
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListChatQuery {
    pub user_id_type: Option<String>,
    pub sort_type: Option<String>,
    pub page_token: Option<String>,
    pub page_size: Option<u32>,
}

impl ListChatQuery {
    pub fn to_query_pairs(&self) -> Vec<(String, String)> {
        let mut query = Vec::new();
        if let Some(v) = &self.user_id_type {
            query.push(("user_id_type".to_string(), v.clone()));
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
pub struct CreateChatBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id_list: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_id_list: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_message_type: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_type: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateChatResponseData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat: Option<ChatInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetChatResponseData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat: Option<ChatInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListChatResponseData {
    #[serde(default)]
    pub items: Vec<ChatInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_more: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeleteChatResponseData {}

pub struct ImV1ChatApi<'a> {
    client: &'a Client,
}

impl<'a> ImV1ChatApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub async fn create<T: Serialize>(
        &self,
        req_body: &T,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        self.client
            .call_with_body(
                ops::im::v1::chat::CREATE,
                HashMap::new(),
                Vec::new(),
                req_body,
                options,
            )
            .await
    }

    pub async fn create_typed(
        &self,
        query: &CreateChatQuery,
        body: &CreateChatBody,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<CreateChatResponseData>, Error> {
        let raw = self
            .client
            .call_with_body(
                ops::im::v1::chat::CREATE,
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
        chat_id: impl Into<String>,
        query: Vec<(String, String)>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("chat_id".to_string(), chat_id.into());
        self.client
            .call(ops::im::v1::chat::GET, path_params, query, None, options)
            .await
    }

    pub async fn get_typed(
        &self,
        chat_id: impl Into<String>,
        query: &GetChatQuery,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<GetChatResponseData>, Error> {
        let raw = self.get(chat_id, query.to_query_pairs(), options).await?;
        raw.json()
    }

    pub async fn list(
        &self,
        query: Vec<(String, String)>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        self.client
            .call(
                ops::im::v1::chat::LIST,
                HashMap::new(),
                query,
                None,
                options,
            )
            .await
    }

    pub async fn list_typed(
        &self,
        query: &ListChatQuery,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<ListChatResponseData>, Error> {
        let raw = self.list(query.to_query_pairs(), options).await?;
        raw.json()
    }

    pub async fn delete(
        &self,
        chat_id: impl Into<String>,
        query: Vec<(String, String)>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("chat_id".to_string(), chat_id.into());
        self.client
            .call(ops::im::v1::chat::DELETE, path_params, query, None, options)
            .await
    }

    pub async fn delete_typed(
        &self,
        chat_id: impl Into<String>,
        query: &DeleteChatQuery,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<DeleteChatResponseData>, Error> {
        let raw = self
            .delete(chat_id, query.to_query_pairs(), options)
            .await?;
        raw.json()
    }

    pub async fn create_raw(
        &self,
        body: Value,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        self.client
            .call(
                ops::im::v1::chat::CREATE,
                HashMap::new(),
                Vec::new(),
                Some(body),
                options,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_query_serializes_to_pairs() {
        let query = ListChatQuery {
            user_id_type: Some("user_id".to_string()),
            sort_type: Some("ByCreateTimeAsc".to_string()),
            page_token: Some("token".to_string()),
            page_size: Some(20),
        };
        assert_eq!(
            query.to_query_pairs(),
            vec![
                ("user_id_type".to_string(), "user_id".to_string()),
                ("sort_type".to_string(), "ByCreateTimeAsc".to_string()),
                ("page_token".to_string(), "token".to_string()),
                ("page_size".to_string(), "20".to_string())
            ]
        );
    }
}
