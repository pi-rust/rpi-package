use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, HeaderName, HeaderValue};
use serde::Deserialize;

use super::config::Config;
use super::error::{ApiError, Error};
use super::http_client::{HttpClientRef, default_http_client};
use super::logger::LogLevel;
use super::request::{
    AccessTokenType, ApiRequest, ApiRequestBody, ApiResponse, HEADER_OAPI_REQUEST_ID,
    RequestOptions,
};
use super::token::TokenManager;

const USER_AGENT: &str = "oapi-sdk-rust/0.1";
const ERROR_CODE_ACCESS_TOKEN_INVALID: i64 = 99_991_671;
const ERROR_CODE_APP_ACCESS_TOKEN_INVALID: i64 = 99_991_664;
const ERROR_CODE_TENANT_ACCESS_TOKEN_INVALID: i64 = 99_991_663;

#[derive(Debug, Clone)]
pub struct CoreClient {
    config: Arc<Config>,
    token_http: reqwest::Client,
    http_client: HttpClientRef,
    token_manager: TokenManager,
}

impl CoreClient {
    pub fn new(config: Config) -> Result<Self, Error> {
        if config.app_id.is_empty() {
            return Err(Error::MissingConfig("app_id"));
        }
        if config.app_secret.is_empty() {
            return Err(Error::MissingConfig("app_secret"));
        }
        let token_cache = config.token_cache.clone();
        let token_http = reqwest::Client::builder().user_agent(USER_AGENT).build()?;
        let http_client = config
            .http_client
            .clone()
            .unwrap_or_else(|| default_http_client(&config.base_url));
        Ok(Self {
            config: Arc::new(config),
            token_http,
            http_client,
            token_manager: TokenManager::new(token_cache),
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub async fn request(
        &self,
        req: &ApiRequest,
        options: &RequestOptions,
    ) -> Result<ApiResponse, Error> {
        let token_type = determine_token_type(
            &req.supported_token_types,
            options,
            self.config.enable_token_cache,
        );

        self.config.logger.debug(&format!(
            "Request: {} {} (token_type={:?})",
            req.method, req.api_path, token_type
        ));
        if self.config.log_req_at_debug && self.config.logger.is_enabled(LogLevel::Debug) {
            self.config.logger.debug(&format!(
                "Request details: query={:?}, path_params={:?}, body={}",
                req.query,
                req.path_params,
                preview_body(req.body.as_ref())
            ));
        }

        let mut resp = self.request_with_retry(req, options, token_type).await?;
        if let Some(code_error) = parse_code_error(&resp)?
            && should_retry_for_token_error(
                code_error.code,
                token_type,
                self.config.enable_token_cache,
            )
        {
            self.config.logger.warn(&format!(
                "Token invalid (code={}), retrying...",
                code_error.code
            ));
            self.token_manager
                .invalidate(
                    token_type,
                    &self.config.app_id,
                    options.tenant_key.as_deref(),
                )
                .await;
            resp = self.request_once(req, options, token_type).await?;
        }

        if let Some(code_error) = parse_code_error(&resp)?
            && code_error.code != 0
        {
            self.config.logger.error(&format!(
                "API error: code={}, msg={}",
                code_error.code, code_error.msg
            ));
            return Err(Error::Api(ApiError {
                code: code_error.code,
                msg: code_error.msg,
                request_id: resp.request_id(),
                raw_body: String::from_utf8_lossy(&resp.body).to_string(),
                http_status: Some(resp.status),
            }));
        }

        self.config.logger.debug(&format!(
            "Response: status={}, size={}bytes",
            resp.status,
            resp.body.len()
        ));
        if self.config.log_req_at_debug && self.config.logger.is_enabled(LogLevel::Debug) {
            self.config.logger.debug(&format!(
                "Response details: headers={:?}, body={}",
                resp.headers,
                preview_bytes(&resp.body)
            ));
        }

        Ok(resp)
    }

    async fn request_with_retry(
        &self,
        req: &ApiRequest,
        options: &RequestOptions,
        token_type: AccessTokenType,
    ) -> Result<ApiResponse, Error> {
        let retry_count = options.retry_count.unwrap_or(0);
        let retry_delay = options.retry_delay.unwrap_or(Duration::from_millis(200));
        let mut attempts: u32 = 0;

        loop {
            attempts += 1;
            match self.request_once(req, options, token_type).await {
                Ok(resp) => return Ok(resp),
                Err(err) => {
                    if attempts <= retry_count && err.is_retryable() {
                        self.config.logger.warn(&format!(
                            "Request failed on attempt {attempts}, retrying after {:?}: {}",
                            retry_delay, err
                        ));
                        tokio::time::sleep(retry_delay).await;
                        continue;
                    }
                    if attempts > 1 && err.is_retryable() {
                        return Err(Error::RetryFailed(attempts));
                    }
                    return Err(err);
                }
            }
        }
    }

    async fn request_once(
        &self,
        req: &ApiRequest,
        options: &RequestOptions,
        token_type: AccessTokenType,
    ) -> Result<ApiResponse, Error> {
        let mut merged_options = options.clone();
        for (key, value) in &self.config.default_headers {
            merged_options.headers.insert(key.clone(), value.clone());
        }
        if matches!(req.body, Some(ApiRequestBody::Json(_)))
            && !merged_options.headers.contains_key(CONTENT_TYPE)
        {
            merged_options.headers.insert(
                CONTENT_TYPE,
                "application/json; charset=utf-8"
                    .parse()
                    .expect("valid content type"),
            );
        }
        if let Some(request_id) = &merged_options.request_id
            && !merged_options.headers.contains_key(HEADER_OAPI_REQUEST_ID)
        {
            merged_options.headers.insert(
                HeaderName::from_static(HEADER_OAPI_REQUEST_ID),
                HeaderValue::from_str(request_id)?,
            );
        }
        if merged_options.need_helpdesk_auth {
            let credentials = self
                .config
                .helpdesk_credentials
                .as_ref()
                .ok_or(Error::MissingHelpdeskCredentials)?;
            merged_options.headers.insert(
                HeaderName::from_static("x-lark-helpdesk-authorization"),
                HeaderValue::from_str(&credentials.auth_header_value())?,
            );
        }
        if merged_options.timeout.is_none() {
            merged_options.timeout = self.config.request_timeout;
        }

        let token = self
            .token_manager
            .resolve_token(&self.token_http, &self.config, token_type, &merged_options)
            .await?;

        self.http_client
            .execute(req.clone(), &merged_options, token)
            .await
    }
}

fn should_retry_for_token_error(
    code: i64,
    token_type: AccessTokenType,
    enable_token_cache: bool,
) -> bool {
    if !enable_token_cache {
        return false;
    }
    if token_type == AccessTokenType::None || token_type == AccessTokenType::User {
        return false;
    }
    matches!(
        code,
        ERROR_CODE_ACCESS_TOKEN_INVALID
            | ERROR_CODE_APP_ACCESS_TOKEN_INVALID
            | ERROR_CODE_TENANT_ACCESS_TOKEN_INVALID
    )
}

fn determine_token_type(
    supported: &[AccessTokenType],
    options: &RequestOptions,
    enable_token_cache: bool,
) -> AccessTokenType {
    if supported.is_empty() {
        return AccessTokenType::None;
    }

    if !enable_token_cache {
        if options.user_access_token.is_some() {
            return AccessTokenType::User;
        }
        if options.tenant_access_token.is_some() {
            return AccessTokenType::Tenant;
        }
        if options.app_access_token.is_some() {
            return AccessTokenType::App;
        }
        return AccessTokenType::None;
    }

    let mut token_type = supported[0];
    if supported.contains(&AccessTokenType::Tenant) {
        token_type = AccessTokenType::Tenant;
    }
    if options.tenant_key.is_some() && supported.contains(&AccessTokenType::Tenant) {
        token_type = AccessTokenType::Tenant;
    }
    if options.user_access_token.is_some() && supported.contains(&AccessTokenType::User) {
        token_type = AccessTokenType::User;
    }
    if options.app_access_token.is_some() && supported.contains(&AccessTokenType::App) {
        token_type = AccessTokenType::App;
    }
    token_type
}

#[derive(Debug, Deserialize)]
struct CodeError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    msg: String,
}

fn parse_code_error(resp: &ApiResponse) -> Result<Option<CodeError>, Error> {
    let content_type = resp
        .headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !content_type.contains("json") {
        return Ok(None);
    }
    let parsed = serde_json::from_slice::<CodeError>(&resp.body)?;
    Ok(Some(parsed))
}

fn preview_body(body: Option<&ApiRequestBody>) -> String {
    match body {
        None => "<empty>".to_string(),
        Some(ApiRequestBody::Json(value)) => {
            let text = value.to_string();
            truncate_for_log(&text, 512)
        }
        Some(ApiRequestBody::Multipart(form)) => {
            format!("<multipart fields={}>", form.fields.len())
        }
    }
}

fn preview_bytes(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body).to_string();
    truncate_for_log(&text, 512)
}

fn truncate_for_log(input: &str, max_len: usize) -> String {
    if input.len() <= max_len {
        return input.to_string();
    }
    format!("{}...(truncated)", &input[..max_len])
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use async_trait::async_trait;
    use reqwest::Method;
    use reqwest::header::HeaderMap;
    use tokio::sync::Mutex;

    use super::*;
    use crate::core::http_client::HttpClient;

    #[derive(Debug)]
    struct MockHttpClient {
        status: u16,
        body: Vec<u8>,
    }

    #[async_trait]
    impl HttpClient for MockHttpClient {
        async fn execute(
            &self,
            _request: ApiRequest,
            _options: &RequestOptions,
            _token: Option<String>,
        ) -> Result<ApiResponse, Error> {
            Ok(ApiResponse {
                status: self.status,
                headers: HeaderMap::new(),
                body: self.body.clone(),
            })
        }
    }

    #[derive(Debug)]
    struct FlakyHttpClient {
        calls: Arc<AtomicU32>,
    }

    #[async_trait]
    impl HttpClient for FlakyHttpClient {
        async fn execute(
            &self,
            _request: ApiRequest,
            _options: &RequestOptions,
            _token: Option<String>,
        ) -> Result<ApiResponse, Error> {
            let current = self.calls.fetch_add(1, Ordering::SeqCst);
            if current == 0 {
                return Err(Error::RequestTimeout(Duration::from_millis(1)));
            }
            Ok(ApiResponse {
                status: 200,
                headers: HeaderMap::new(),
                body: br#"{"code":0,"msg":"ok"}"#.to_vec(),
            })
        }
    }

    #[derive(Debug, Default)]
    struct CaptureHttpClient {
        headers: Arc<Mutex<Option<HeaderMap>>>,
    }

    #[async_trait]
    impl HttpClient for CaptureHttpClient {
        async fn execute(
            &self,
            _request: ApiRequest,
            options: &RequestOptions,
            _token: Option<String>,
        ) -> Result<ApiResponse, Error> {
            *self.headers.lock().await = Some(options.headers.clone());
            Ok(ApiResponse {
                status: 200,
                headers: HeaderMap::new(),
                body: br#"{"code":0,"msg":"ok"}"#.to_vec(),
            })
        }
    }

    #[tokio::test]
    async fn core_client_uses_custom_http_client() {
        let custom_http = Arc::new(MockHttpClient {
            status: 200,
            body: br#"{"code":0,"msg":"ok"}"#.to_vec(),
        });
        let config = Config::builder("app", "secret")
            .http_client(custom_http)
            .build();
        let client = CoreClient::new(config).expect("core client");

        let mut req = ApiRequest::new(Method::GET, "/open-apis/mock/v1/ping");
        req.supported_token_types = vec![AccessTokenType::None];
        let resp = client
            .request(&req, &RequestOptions::default())
            .await
            .expect("request should pass");

        assert_eq!(resp.status, 200);
    }

    #[tokio::test]
    async fn core_client_retries_retryable_errors() {
        let calls = Arc::new(AtomicU32::new(0));
        let flaky = Arc::new(FlakyHttpClient {
            calls: calls.clone(),
        });
        let config = Config::builder("app", "secret").http_client(flaky).build();
        let client = CoreClient::new(config).expect("core client");

        let mut req = ApiRequest::new(Method::GET, "/open-apis/mock/v1/ping");
        req.supported_token_types = vec![AccessTokenType::None];
        let options = RequestOptions::new().retry(1, Duration::from_millis(1));

        let resp = client
            .request(&req, &options)
            .await
            .expect("request should retry");
        assert_eq!(resp.status, 200);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn core_client_injects_request_id_and_helpdesk_auth_headers() {
        let capture = Arc::new(CaptureHttpClient::default());
        let config = Config::builder("app", "secret")
            .helpdesk("hd_123", "token_456")
            .http_client(capture.clone())
            .build();
        let client = CoreClient::new(config).expect("core client");

        let mut req = ApiRequest::new(Method::GET, "/open-apis/mock/v1/ping");
        req.supported_token_types = vec![AccessTokenType::None];

        client
            .request(
                &req,
                &RequestOptions::new()
                    .request_id("req_123")
                    .need_helpdesk_auth(),
            )
            .await
            .expect("request should pass");

        let headers = capture
            .headers
            .lock()
            .await
            .clone()
            .expect("captured headers");
        assert_eq!(
            headers
                .get(HEADER_OAPI_REQUEST_ID)
                .and_then(|value| value.to_str().ok()),
            Some("req_123")
        );
        assert!(headers.contains_key("x-lark-helpdesk-authorization"));
    }

    #[tokio::test]
    async fn core_client_requires_helpdesk_credentials_when_requested() {
        let config = Config::builder("app", "secret").build();
        let client = CoreClient::new(config).expect("core client");

        let mut req = ApiRequest::new(Method::GET, "/open-apis/mock/v1/ping");
        req.supported_token_types = vec![AccessTokenType::None];

        let err = client
            .request(&req, &RequestOptions::new().need_helpdesk_auth())
            .await
            .expect_err("helpdesk auth should require credentials");
        assert!(matches!(err, Error::MissingHelpdeskCredentials));
    }

    #[test]
    fn truncate_for_log_adds_suffix() {
        let long = "a".repeat(600);
        let shortened = truncate_for_log(&long, 32);
        assert!(shortened.ends_with("...(truncated)"));
    }
}
