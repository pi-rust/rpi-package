use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::Client;
use crate::core::{Error, RequestOptions};
use crate::generated::ops;

pub struct AuthV3Api<'a> {
    client: &'a Client,
}

impl<'a> AuthV3Api<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub async fn app_access_token_internal(
        &self,
        req: &SelfBuiltAppAccessTokenReq,
    ) -> Result<AppAccessTokenResp, Error> {
        let resp = self
            .client
            .call_with_body(
                ops::auth::v3::app_access_token::INTERNAL,
                HashMap::new(),
                Vec::new(),
                req,
                RequestOptions::default(),
            )
            .await?;
        resp.json()
    }

    pub async fn app_access_token_create(
        &self,
        req: &MarketplaceAppAccessTokenReq,
    ) -> Result<AppAccessTokenResp, Error> {
        let resp = self
            .client
            .call_with_body(
                ops::auth::v3::app_access_token::CREATE,
                HashMap::new(),
                Vec::new(),
                req,
                RequestOptions::default(),
            )
            .await?;
        resp.json()
    }

    pub async fn tenant_access_token_internal(
        &self,
        req: &SelfBuiltTenantAccessTokenReq,
    ) -> Result<TenantAccessTokenResp, Error> {
        let resp = self
            .client
            .call_with_body(
                ops::auth::v3::tenant_access_token::INTERNAL,
                HashMap::new(),
                Vec::new(),
                req,
                RequestOptions::default(),
            )
            .await?;
        resp.json()
    }

    pub async fn tenant_access_token_create(
        &self,
        req: &MarketplaceTenantAccessTokenReq,
    ) -> Result<TenantAccessTokenResp, Error> {
        let resp = self
            .client
            .call_with_body(
                ops::auth::v3::tenant_access_token::CREATE,
                HashMap::new(),
                Vec::new(),
                req,
                RequestOptions::default(),
            )
            .await?;
        resp.json()
    }

    pub async fn resend_app_ticket(
        &self,
        req: &ResendAppTicketReq,
    ) -> Result<ResendAppTicketResp, Error> {
        let resp = self
            .client
            .call_with_body(
                ops::auth::v3::app_ticket::RESEND,
                HashMap::new(),
                Vec::new(),
                req,
                RequestOptions::default(),
            )
            .await?;
        resp.json()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfBuiltAppAccessTokenReq {
    pub app_id: String,
    pub app_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfBuiltTenantAccessTokenReq {
    pub app_id: String,
    pub app_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceAppAccessTokenReq {
    pub app_id: String,
    pub app_secret: String,
    pub app_ticket: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceTenantAccessTokenReq {
    pub app_access_token: String,
    pub tenant_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResendAppTicketReq {
    pub app_id: String,
    pub app_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppAccessTokenResp {
    pub code: i64,
    pub msg: String,
    pub expire: i64,
    pub app_access_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantAccessTokenResp {
    pub code: i64,
    pub msg: String,
    pub expire: i64,
    pub tenant_access_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResendAppTicketResp {
    pub code: i64,
    pub msg: String,
}
