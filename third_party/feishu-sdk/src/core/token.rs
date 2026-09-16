use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use super::cache::CacheRef;
use super::config::{AppType, Config};
use super::error::{ApiError, Error};
use super::request::{AccessTokenType, RequestOptions};

const APP_ACCESS_TOKEN_INTERNAL_PATH: &str = "/open-apis/auth/v3/app_access_token/internal";
const APP_ACCESS_TOKEN_PATH: &str = "/open-apis/auth/v3/app_access_token";
const TENANT_ACCESS_TOKEN_INTERNAL_PATH: &str = "/open-apis/auth/v3/tenant_access_token/internal";
const TENANT_ACCESS_TOKEN_PATH: &str = "/open-apis/auth/v3/tenant_access_token";
const EXPIRY_DELTA: Duration = Duration::from_secs(180);

#[derive(Debug, Clone)]
pub struct TokenManager {
    cache: CacheRef,
}

impl Default for TokenManager {
    fn default() -> Self {
        Self {
            cache: Arc::new(super::cache::InMemoryCache::new()),
        }
    }
}

impl TokenManager {
    pub fn new(cache: CacheRef) -> Self {
        Self { cache }
    }

    pub async fn resolve_token(
        &self,
        http: &reqwest::Client,
        config: &Config,
        token_type: AccessTokenType,
        options: &RequestOptions,
    ) -> Result<Option<String>, Error> {
        match token_type {
            AccessTokenType::None => Ok(None),
            AccessTokenType::User => {
                if let Some(token) = options.user_access_token.clone() {
                    return Ok(Some(token));
                }
                if !config.enable_token_cache {
                    return Err(Error::MissingAccessToken);
                }
                Err(Error::UnsupportedTokenType("user_access_token"))
            }
            AccessTokenType::App => {
                if let Some(token) = options.app_access_token.clone() {
                    return Ok(Some(token));
                }
                if !config.enable_token_cache {
                    return Err(Error::MissingAccessToken);
                }
                let token = self.get_app_access_token(http, config, options).await?;
                Ok(Some(token))
            }
            AccessTokenType::Tenant => {
                if let Some(token) = options.tenant_access_token.clone() {
                    return Ok(Some(token));
                }
                if !config.enable_token_cache {
                    return Err(Error::MissingAccessToken);
                }
                let token = self.get_tenant_access_token(http, config, options).await?;
                Ok(Some(token))
            }
        }
    }

    pub async fn invalidate(
        &self,
        token_type: AccessTokenType,
        app_id: &str,
        tenant_key: Option<&str>,
    ) {
        match token_type {
            AccessTokenType::App => {
                self.cache.remove(&app_access_token_key(app_id)).await;
            }
            AccessTokenType::Tenant => {
                self.cache
                    .remove(&tenant_access_token_key(app_id, tenant_key))
                    .await;
            }
            AccessTokenType::None | AccessTokenType::User => {}
        }
    }

    async fn get_app_access_token(
        &self,
        http: &reqwest::Client,
        config: &Config,
        options: &RequestOptions,
    ) -> Result<String, Error> {
        let cache_key = app_access_token_key(&config.app_id);
        if let Some(token) = self.cache.get(&cache_key).await {
            return Ok(token);
        }

        let path = match config.app_type {
            AppType::SelfBuilt => APP_ACCESS_TOKEN_INTERNAL_PATH,
            AppType::Marketplace => APP_ACCESS_TOKEN_PATH,
        };
        let app_ticket = options
            .app_ticket
            .as_deref()
            .or(config.app_ticket.as_deref())
            .map(str::to_string);

        let body = match config.app_type {
            AppType::SelfBuilt => json!({
                "app_id": config.app_id,
                "app_secret": config.app_secret
            }),
            AppType::Marketplace => json!({
                "app_id": config.app_id,
                "app_secret": config.app_secret,
                "app_ticket": app_ticket.ok_or(Error::MissingAppTicket)?
            }),
        };

        let token_resp = fetch_token(http, &config.base_url, path, body).await?;
        let token = token_resp
            .app_access_token
            .ok_or(Error::UnsupportedTokenType("app_access_token"))?;
        let ttl = ttl_from_expire(token_resp.expire);
        self.cache.set(cache_key, token.clone(), ttl).await;
        Ok(token)
    }

    async fn get_tenant_access_token(
        &self,
        http: &reqwest::Client,
        config: &Config,
        options: &RequestOptions,
    ) -> Result<String, Error> {
        let cache_key = tenant_access_token_key(&config.app_id, options.tenant_key.as_deref());
        if let Some(token) = self.cache.get(&cache_key).await {
            return Ok(token);
        }

        let token_resp = match config.app_type {
            AppType::SelfBuilt => {
                let body = json!({
                    "app_id": config.app_id,
                    "app_secret": config.app_secret
                });
                fetch_token(
                    http,
                    &config.base_url,
                    TENANT_ACCESS_TOKEN_INTERNAL_PATH,
                    body,
                )
                .await?
            }
            AppType::Marketplace => {
                let tenant_key = options
                    .tenant_key
                    .as_deref()
                    .ok_or(Error::MissingTenantKey)?
                    .to_string();
                let app_access_token = self.get_app_access_token(http, config, options).await?;
                let body = json!({
                    "app_access_token": app_access_token,
                    "tenant_key": tenant_key
                });
                fetch_token(http, &config.base_url, TENANT_ACCESS_TOKEN_PATH, body).await?
            }
        };

        let token = token_resp
            .tenant_access_token
            .ok_or(Error::UnsupportedTokenType("tenant_access_token"))?;
        let ttl = ttl_from_expire(token_resp.expire);
        self.cache.set(cache_key, token.clone(), ttl).await;
        Ok(token)
    }
}

fn ttl_from_expire(expire_seconds: i64) -> Duration {
    let full = Duration::from_secs(expire_seconds.max(0) as u64);
    full.saturating_sub(EXPIRY_DELTA)
}

fn app_access_token_key(app_id: &str) -> String {
    format!("app_access_token-{app_id}")
}

fn tenant_access_token_key(app_id: &str, tenant_key: Option<&str>) -> String {
    let tenant = tenant_key.unwrap_or("__default_tenant__");
    format!("tenant_access_token-{app_id}-{tenant}")
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    expire: i64,
    app_access_token: Option<String>,
    tenant_access_token: Option<String>,
}

async fn fetch_token(
    http: &reqwest::Client,
    base_url: &str,
    path: &str,
    body: serde_json::Value,
) -> Result<TokenResponse, Error> {
    let url = format!("{}{}", base_url.trim_end_matches('/'), path);
    let resp = http.post(url).json(&body).send().await?;
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let raw = resp.bytes().await?;
    let parsed: TokenResponse = serde_json::from_slice(&raw)?;
    if parsed.code != 0 {
        let request_id = headers
            .get("x-tt-logid")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .or_else(|| {
                headers
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            });
        return Err(Error::Api(ApiError {
            code: parsed.code,
            msg: if parsed.msg.is_empty() {
                format!("token api failed, status={status}")
            } else {
                parsed.msg
            },
            request_id,
            raw_body: String::from_utf8_lossy(&raw).to_string(),
            http_status: Some(status),
        }));
    }
    Ok(parsed)
}
