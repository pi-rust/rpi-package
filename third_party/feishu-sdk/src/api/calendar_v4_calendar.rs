use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::ApiEnvelope;
use crate::Client;
use crate::core::{ApiResponse, Error, RequestOptions};
use crate::generated::ops;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetCalendarQuery {
    pub user_id_type: Option<String>,
}

impl GetCalendarQuery {
    pub fn to_query_pairs(&self) -> Vec<(String, String)> {
        let mut query = Vec::new();
        if let Some(v) = &self.user_id_type {
            query.push(("user_id_type".to_string(), v.clone()));
        }
        query
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListCalendarQuery {
    pub page_token: Option<String>,
    pub page_size: Option<u32>,
    pub sync_token: Option<String>,
}

impl ListCalendarQuery {
    pub fn to_query_pairs(&self) -> Vec<(String, String)> {
        let mut query = Vec::new();
        if let Some(v) = &self.page_token {
            query.push(("page_token".to_string(), v.clone()));
        }
        if let Some(v) = self.page_size {
            query.push(("page_size".to_string(), v.to_string()));
        }
        if let Some(v) = &self.sync_token {
            query.push(("sync_token".to_string(), v.clone()));
        }
        query
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateCalendarBody {
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_alias: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateCalendarBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_alias: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CalendarInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calendar_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_deleted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_time: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetCalendarResponseData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calendar: Option<CalendarInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateCalendarResponseData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calendar: Option<CalendarInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListCalendarResponseData {
    #[serde(default)]
    pub calendar_list: Vec<CalendarInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_more: Option<bool>,
}

pub struct CalendarV4CalendarApi<'a> {
    client: &'a Client,
}

impl<'a> CalendarV4CalendarApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub async fn get(
        &self,
        calendar_id: impl Into<String>,
        query: Vec<(String, String)>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("calendar_id".to_string(), calendar_id.into());
        self.client
            .call(
                ops::calendar::v4::calendar::GET,
                path_params,
                query,
                None,
                options,
            )
            .await
    }

    pub async fn get_typed(
        &self,
        calendar_id: impl Into<String>,
        query: &GetCalendarQuery,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<GetCalendarResponseData>, Error> {
        let raw = self
            .get(calendar_id, query.to_query_pairs(), options)
            .await?;
        raw.json()
    }

    pub async fn list(
        &self,
        query: Vec<(String, String)>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        self.client
            .call(
                ops::calendar::v4::calendar::LIST,
                HashMap::new(),
                query,
                None,
                options,
            )
            .await
    }

    pub async fn list_typed(
        &self,
        query: &ListCalendarQuery,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<ListCalendarResponseData>, Error> {
        let raw = self.list(query.to_query_pairs(), options).await?;
        raw.json()
    }

    pub async fn create(
        &self,
        body: &CreateCalendarBody,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        self.client
            .call_with_body(
                ops::calendar::v4::calendar::CREATE,
                HashMap::new(),
                Vec::new(),
                body,
                options,
            )
            .await
    }

    pub async fn create_typed(
        &self,
        body: &CreateCalendarBody,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<CreateCalendarResponseData>, Error> {
        let raw = self.create(body, options).await?;
        raw.json()
    }

    pub async fn update(
        &self,
        calendar_id: impl Into<String>,
        body: &UpdateCalendarBody,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("calendar_id".to_string(), calendar_id.into());
        self.client
            .call_with_body(
                ops::calendar::v4::calendar::PATCH,
                path_params,
                Vec::new(),
                body,
                options,
            )
            .await
    }

    pub async fn delete(
        &self,
        calendar_id: impl Into<String>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("calendar_id".to_string(), calendar_id.into());
        self.client
            .call(
                ops::calendar::v4::calendar::DELETE,
                path_params,
                Vec::new(),
                None,
                options,
            )
            .await
    }

    pub async fn subscribe(
        &self,
        calendar_id: impl Into<String>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("calendar_id".to_string(), calendar_id.into());
        self.client
            .call(
                ops::calendar::v4::calendar::SUBSCRIBE,
                path_params,
                Vec::new(),
                None,
                options,
            )
            .await
    }

    pub async fn unsubscribe(
        &self,
        calendar_id: impl Into<String>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("calendar_id".to_string(), calendar_id.into());
        self.client
            .call(
                ops::calendar::v4::calendar::UNSUBSCRIBE,
                path_params,
                Vec::new(),
                None,
                options,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_query_serializes() {
        let query = ListCalendarQuery {
            page_size: Some(50),
            page_token: Some("token_xxx".to_string()),
            ..Default::default()
        };
        let pairs = query.to_query_pairs();
        assert!(pairs.contains(&("page_size".to_string(), "50".to_string())));
        assert!(pairs.contains(&("page_token".to_string(), "token_xxx".to_string())));
    }
}
