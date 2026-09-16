use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::ApiEnvelope;
use crate::Client;
use crate::core::{ApiResponse, Error, RequestOptions};
use crate::generated::ops;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateApprovalBody {
    pub approval_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateInstanceBody {
    pub approval_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub form: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetInstanceQuery {
    pub locale: Option<String>,
    pub user_id_type: Option<String>,
    pub department_id_type: Option<String>,
}

impl GetInstanceQuery {
    pub fn to_query_pairs(&self) -> Vec<(String, String)> {
        let mut query = Vec::new();
        if let Some(v) = &self.locale {
            query.push(("locale".to_string(), v.clone()));
        }
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
pub struct ApprovalInstance {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_time: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateInstanceResponseData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<ApprovalInstance>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetInstanceResponseData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<ApprovalInstance>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApproveTaskBody {
    pub approval_code: String,
    pub instance_id: String,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RejectTaskBody {
    pub approval_code: String,
    pub instance_id: String,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CancelInstanceBody {
    pub approval_code: String,
    pub instance_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

pub struct ApprovalV4ApprovalApi<'a> {
    client: &'a Client,
}

impl<'a> ApprovalV4ApprovalApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub async fn create(
        &self,
        body: &CreateApprovalBody,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        self.client
            .call_with_body(
                ops::approval::v4::approval::CREATE,
                HashMap::new(),
                Vec::new(),
                body,
                options,
            )
            .await
    }

    pub async fn get(
        &self,
        approval_code: impl Into<String>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("approval_code".to_string(), approval_code.into());
        self.client
            .call(
                ops::approval::v4::approval::GET,
                path_params,
                Vec::new(),
                None,
                options,
            )
            .await
    }

    pub async fn subscribe(
        &self,
        approval_code: impl Into<String>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("approval_code".to_string(), approval_code.into());
        self.client
            .call(
                ops::approval::v4::approval::SUBSCRIBE,
                path_params,
                Vec::new(),
                None,
                options,
            )
            .await
    }

    pub async fn unsubscribe(
        &self,
        approval_code: impl Into<String>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("approval_code".to_string(), approval_code.into());
        self.client
            .call(
                ops::approval::v4::approval::UNSUBSCRIBE,
                path_params,
                Vec::new(),
                None,
                options,
            )
            .await
    }
}

pub struct ApprovalV4InstanceApi<'a> {
    client: &'a Client,
}

impl<'a> ApprovalV4InstanceApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub async fn create(
        &self,
        body: &CreateInstanceBody,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        self.client
            .call_with_body(
                ops::approval::v4::instance::CREATE,
                HashMap::new(),
                Vec::new(),
                body,
                options,
            )
            .await
    }

    pub async fn create_typed(
        &self,
        body: &CreateInstanceBody,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<CreateInstanceResponseData>, Error> {
        let raw = self.create(body, options).await?;
        raw.json()
    }

    pub async fn get(
        &self,
        instance_id: impl Into<String>,
        query: Vec<(String, String)>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let mut path_params = HashMap::new();
        path_params.insert("instance_id".to_string(), instance_id.into());
        self.client
            .call(
                ops::approval::v4::instance::GET,
                path_params,
                query,
                None,
                options,
            )
            .await
    }

    pub async fn get_typed(
        &self,
        instance_id: impl Into<String>,
        query: &GetInstanceQuery,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<GetInstanceResponseData>, Error> {
        let raw = self
            .get(instance_id, query.to_query_pairs(), options)
            .await?;
        raw.json()
    }

    pub async fn cancel(
        &self,
        body: &CancelInstanceBody,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        self.client
            .call_with_body(
                ops::approval::v4::instance::CANCEL,
                HashMap::new(),
                Vec::new(),
                body,
                options,
            )
            .await
    }
}

pub struct ApprovalV4TaskApi<'a> {
    client: &'a Client,
}

impl<'a> ApprovalV4TaskApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub async fn approve(
        &self,
        body: &ApproveTaskBody,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        self.client
            .call_with_body(
                ops::approval::v4::task::APPROVE,
                HashMap::new(),
                Vec::new(),
                body,
                options,
            )
            .await
    }

    pub async fn reject(
        &self,
        body: &RejectTaskBody,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        self.client
            .call_with_body(
                ops::approval::v4::task::REJECT,
                HashMap::new(),
                Vec::new(),
                body,
                options,
            )
            .await
    }
}

pub struct ApprovalV4Api<'a> {
    pub approval: ApprovalV4ApprovalApi<'a>,
    pub instance: ApprovalV4InstanceApi<'a>,
    pub task: ApprovalV4TaskApi<'a>,
}

impl<'a> ApprovalV4Api<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self {
            approval: ApprovalV4ApprovalApi::new(client),
            instance: ApprovalV4InstanceApi::new(client),
            task: ApprovalV4TaskApi::new(client),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_query_serializes() {
        let query = GetInstanceQuery {
            locale: Some("zh-CN".to_string()),
            user_id_type: Some("open_id".to_string()),
            ..Default::default()
        };
        let pairs = query.to_query_pairs();
        assert!(pairs.contains(&("locale".to_string(), "zh-CN".to_string())));
        assert!(pairs.contains(&("user_id_type".to_string(), "open_id".to_string())));
    }
}
