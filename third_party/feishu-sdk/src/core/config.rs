use std::time::Duration;

use base64::Engine;
use reqwest::header::HeaderMap;

use super::cache::{CacheRef, default_cache};
use super::http_client::{HttpClientRef, default_http_client};
use super::logger::{LogLevel, LoggerRef, new_logger};
use crate::utils::{SerializerRef, default_serializer};

pub const FEISHU_BASE_URL: &str = "https://open.feishu.cn";
pub const LARK_BASE_URL: &str = "https://open.larksuite.com";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum AppType {
    #[default]
    SelfBuilt,
    Marketplace,
}

#[derive(Clone, Debug)]
pub struct HelpdeskCredentials {
    pub helpdesk_id: String,
    pub helpdesk_token: String,
}

impl HelpdeskCredentials {
    pub fn auth_header_value(&self) -> String {
        base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", self.helpdesk_id, self.helpdesk_token))
    }
}

#[derive(Clone)]
pub struct Config {
    pub base_url: String,
    pub app_id: String,
    pub app_secret: String,
    pub app_type: AppType,
    pub enable_token_cache: bool,
    pub app_ticket: Option<String>,
    pub default_headers: HeaderMap,
    pub logger: LoggerRef,
    pub log_level: LogLevel,
    pub token_cache: CacheRef,
    pub http_client: Option<HttpClientRef>,
    pub serializer: SerializerRef,
    pub request_timeout: Option<Duration>,
    pub helpdesk_credentials: Option<HelpdeskCredentials>,
    pub skip_sign_verify: bool,
    pub log_req_at_debug: bool,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("base_url", &self.base_url)
            .field("app_id", &self.app_id)
            .field("app_type", &self.app_type)
            .field("enable_token_cache", &self.enable_token_cache)
            .field("log_level", &self.log_level)
            .field("request_timeout", &self.request_timeout)
            .field("skip_sign_verify", &self.skip_sign_verify)
            .field("log_req_at_debug", &self.log_req_at_debug)
            .finish()
    }
}

impl Config {
    pub fn builder(app_id: impl Into<String>, app_secret: impl Into<String>) -> ConfigBuilder {
        ConfigBuilder {
            base_url: FEISHU_BASE_URL.to_string(),
            app_id: app_id.into(),
            app_secret: app_secret.into(),
            app_type: AppType::SelfBuilt,
            enable_token_cache: true,
            app_ticket: None,
            default_headers: HeaderMap::new(),
            log_level: LogLevel::Info,
            request_timeout: None,
            http_client: None,
            serializer: default_serializer(),
            helpdesk_id: None,
            helpdesk_token: None,
            skip_sign_verify: false,
            log_req_at_debug: false,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.app_id.is_empty() {
            return Err("app_id is required".to_string());
        }
        if self.app_secret.is_empty() {
            return Err("app_secret is required".to_string());
        }
        if self.app_type == AppType::Marketplace && self.app_ticket.is_none() {
            return Err("app_ticket is required for marketplace apps".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ConfigBuilder {
    base_url: String,
    app_id: String,
    app_secret: String,
    app_type: AppType,
    enable_token_cache: bool,
    app_ticket: Option<String>,
    default_headers: HeaderMap,
    log_level: LogLevel,
    request_timeout: Option<Duration>,
    http_client: Option<HttpClientRef>,
    serializer: SerializerRef,
    helpdesk_id: Option<String>,
    helpdesk_token: Option<String>,
    skip_sign_verify: bool,
    log_req_at_debug: bool,
}

impl ConfigBuilder {
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn app_type(mut self, app_type: AppType) -> Self {
        self.app_type = app_type;
        self
    }

    pub fn marketplace_app(mut self) -> Self {
        self.app_type = AppType::Marketplace;
        self
    }

    pub fn enable_token_cache(mut self, enable: bool) -> Self {
        self.enable_token_cache = enable;
        self
    }

    pub fn app_ticket(mut self, app_ticket: impl Into<String>) -> Self {
        self.app_ticket = Some(app_ticket.into());
        self
    }

    pub fn default_headers(mut self, default_headers: HeaderMap) -> Self {
        self.default_headers = default_headers;
        self
    }

    pub fn log_level(mut self, log_level: LogLevel) -> Self {
        self.log_level = log_level;
        self
    }

    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    pub fn http_client(mut self, http_client: HttpClientRef) -> Self {
        self.http_client = Some(http_client);
        self
    }

    pub fn serializer(mut self, serializer: SerializerRef) -> Self {
        self.serializer = serializer;
        self
    }

    pub fn helpdesk(
        mut self,
        helpdesk_id: impl Into<String>,
        helpdesk_token: impl Into<String>,
    ) -> Self {
        self.helpdesk_id = Some(helpdesk_id.into());
        self.helpdesk_token = Some(helpdesk_token.into());
        self
    }

    pub fn skip_sign_verify(mut self, skip: bool) -> Self {
        self.skip_sign_verify = skip;
        self
    }

    pub fn log_req_at_debug(mut self, enabled: bool) -> Self {
        self.log_req_at_debug = enabled;
        self
    }

    pub fn build(self) -> Config {
        Config {
            base_url: self.base_url.clone(),
            app_id: self.app_id,
            app_secret: self.app_secret,
            app_type: self.app_type,
            enable_token_cache: self.enable_token_cache,
            app_ticket: self.app_ticket,
            default_headers: self.default_headers,
            logger: new_logger(self.log_level),
            log_level: self.log_level,
            token_cache: default_cache(),
            http_client: Some(
                self.http_client
                    .unwrap_or_else(|| default_http_client(&self.base_url)),
            ),
            serializer: self.serializer,
            request_timeout: self.request_timeout,
            helpdesk_credentials: match (self.helpdesk_id, self.helpdesk_token) {
                (Some(id), Some(token)) => Some(HelpdeskCredentials {
                    helpdesk_id: id,
                    helpdesk_token: token,
                }),
                _ => None,
            },
            skip_sign_verify: self.skip_sign_verify,
            log_req_at_debug: self.log_req_at_debug,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::utils::{JsonSerializer, deserialize_with, serialize_with};

    #[test]
    fn test_config_builder() {
        let config = Config::builder("app_id", "app_secret")
            .base_url(LARK_BASE_URL)
            .log_level(LogLevel::Debug)
            .request_timeout(Duration::from_secs(30))
            .skip_sign_verify(true)
            .log_req_at_debug(true)
            .helpdesk("hd_123", "token_abc")
            .build();

        assert_eq!(config.base_url, LARK_BASE_URL);
        assert_eq!(config.log_level, LogLevel::Debug);
        assert_eq!(config.request_timeout, Some(Duration::from_secs(30)));
        assert!(config.helpdesk_credentials.is_some());
        assert!(config.http_client.is_some());
        assert!(config.skip_sign_verify);
        assert!(config.log_req_at_debug);
    }

    #[test]
    fn test_config_validate() {
        let config = Config::builder("", "secret").build();
        assert!(config.validate().is_err());

        let config = Config::builder("app_id", "app_secret").build();
        assert!(config.validate().is_ok());

        let config = Config::builder("app_id", "app_secret")
            .marketplace_app()
            .build();
        assert!(config.validate().is_err());

        let config = Config::builder("app_id", "app_secret")
            .marketplace_app()
            .app_ticket("ticket")
            .build();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_custom_serializer() {
        let serializer = Arc::new(JsonSerializer::new());
        let config = Config::builder("app_id", "app_secret")
            .serializer(serializer)
            .build();

        let body = serde_json::json!({"ok": true});
        let bytes = serialize_with(config.serializer.as_ref(), &body).expect("serialize");
        let decoded: serde_json::Value =
            deserialize_with(config.serializer.as_ref(), &bytes).expect("deserialize");
        assert_eq!(decoded.get("ok").and_then(|v| v.as_bool()), Some(true));
    }
}
