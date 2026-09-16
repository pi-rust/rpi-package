use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ApiEnvelope;
use crate::Client;
use crate::core::{ApiResponse, Error, RequestOptions};
use crate::generated::ops;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetContactUserQuery {
    pub user_id_type: Option<String>,
    pub department_id_type: Option<String>,
}

impl GetContactUserQuery {
    pub fn to_query_pairs(&self) -> Vec<(String, String)> {
        let mut query = Vec::new();
        if let Some(v) = &self.user_id_type {
            query.push(("user_id_type".to_string(), v.clone()));
        }
        if let Some(v) = &self.department_id_type {
            query.push(("department_id_type".to_string(), v.clone()));
        }
        query
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListContactUserQuery {
    pub user_id_type: Option<String>,
    pub department_id_type: Option<String>,
    pub department_id: Option<String>,
    pub page_token: Option<String>,
    pub page_size: Option<u32>,
}

impl ListContactUserQuery {
    pub fn to_query_pairs(&self) -> Vec<(String, String)> {
        let mut query = Vec::new();
        if let Some(v) = &self.user_id_type {
            query.push(("user_id_type".to_string(), v.clone()));
        }
        if let Some(v) = &self.department_id_type {
            query.push(("department_id_type".to_string(), v.clone()));
        }
        if let Some(v) = &self.department_id {
            query.push(("department_id".to_string(), v.clone()));
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
pub struct ContactUserInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub union_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub en_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mobile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub department_ids: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetContactUserResponseData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<ContactUserInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListContactUserResponseData {
    #[serde(default)]
    pub items: Vec<ContactUserInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_more: Option<bool>,
}

pub struct ContactV3UserApi<'a> {
    client: &'a Client,
}

impl<'a> ContactV3UserApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub async fn get(
        &self,
        user_id: impl Into<String>,
        query: Vec<(String, String)>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("user_id".to_string(), user_id.into());
        self.client
            .call(
                ops::contact::v3::user::GET,
                path_params,
                query,
                None,
                options,
            )
            .await
    }

    pub async fn get_typed(
        &self,
        user_id: impl Into<String>,
        query: &GetContactUserQuery,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<GetContactUserResponseData>, Error> {
        let raw = self.get(user_id, query.to_query_pairs(), options).await?;
        raw.json()
    }

    pub async fn list(
        &self,
        query: Vec<(String, String)>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        self.client
            .call(
                ops::contact::v3::user::LIST,
                HashMap::new(),
                query,
                None,
                options,
            )
            .await
    }

    pub async fn list_typed(
        &self,
        query: &ListContactUserQuery,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<ListContactUserResponseData>, Error> {
        let raw = self.list(query.to_query_pairs(), options).await?;
        raw.json()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_list_query_serializes_to_pairs() {
        let query = ListContactUserQuery {
            user_id_type: Some("open_id".to_string()),
            department_id_type: Some("open_department_id".to_string()),
            department_id: Some("od_xxx".to_string()),
            page_token: Some("token".to_string()),
            page_size: Some(20),
        };
        assert_eq!(
            query.to_query_pairs(),
            vec![
                ("user_id_type".to_string(), "open_id".to_string()),
                (
                    "department_id_type".to_string(),
                    "open_department_id".to_string()
                ),
                ("department_id".to_string(), "od_xxx".to_string()),
                ("page_token".to_string(), "token".to_string()),
                ("page_size".to_string(), "20".to_string())
            ]
        );
    }
}
