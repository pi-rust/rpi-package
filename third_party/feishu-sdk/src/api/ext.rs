use serde::Serialize;
use serde_json::Value;

use crate::Client;
use crate::core::{ApiResponse, Error, RequestOptions};
use crate::generated::ops;

pub struct ExtApi<'a> {
    client: &'a Client,
}

impl<'a> ExtApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub async fn authen_access_token<T: Serialize>(
        &self,
        body: &T,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        self.client
            .call_with_body(
                ops::authen::v1::access_token::CREATE,
                std::collections::HashMap::new(),
                Vec::new(),
                body,
                options,
            )
            .await
    }

    pub async fn refresh_authen_access_token<T: Serialize>(
        &self,
        body: &T,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        self.client
            .call_with_body(
                ops::authen::v1::refresh_access_token::CREATE,
                std::collections::HashMap::new(),
                Vec::new(),
                body,
                options,
            )
            .await
    }

    pub async fn authen_user_info(
        &self,
        query: Vec<(String, String)>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        self.client
            .call(
                ops::authen::v1::user_info::GET,
                std::collections::HashMap::new(),
                query,
                None,
                options,
            )
            .await
    }

    pub async fn drive_explorer_create_file(
        &self,
        body: Option<Value>,
        options: RequestOptions,
    ) -> Result<ApiResponse, Error> {
        self.client
            .post(
                "/open-apis/drive/explorer/v2/file/create",
                Vec::new(),
                body,
                options,
            )
            .await
    }
}
