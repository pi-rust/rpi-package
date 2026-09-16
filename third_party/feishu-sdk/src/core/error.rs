use thiserror::Error;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub code: i64,
    pub msg: String,
    pub request_id: Option<String>,
    pub raw_body: String,
    pub http_status: Option<u16>,
}

impl ApiError {
    pub fn new(code: i64, msg: impl Into<String>) -> Self {
        Self {
            code,
            msg: msg.into(),
            request_id: None,
            raw_body: String::new(),
            http_status: None,
        }
    }

    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    pub fn with_raw_body(mut self, raw_body: impl Into<String>) -> Self {
        self.raw_body = raw_body.into();
        self
    }

    pub fn with_http_status(mut self, status: u16) -> Self {
        self.http_status = Some(status);
        self
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "api error: code={}, msg={}", self.code, self.msg)?;
        if let Some(ref request_id) = self.request_id {
            write!(f, ", request_id={}", request_id)?;
        }
        if let Some(status) = self.http_status {
            write!(f, ", http_status={}", status)?;
        }
        Ok(())
    }
}

impl std::error::Error for ApiError {}

#[derive(Debug, Error)]
pub enum Error {
    #[error("missing config value: {0}")]
    MissingConfig(&'static str),

    #[error("missing path param: {0}")]
    MissingPathParam(String),

    #[error("invalid base url/path: {0}")]
    InvalidUrl(String),

    #[error("invalid http method: {0}")]
    InvalidHttpMethod(String),

    #[error("missing tenant key for tenant token request")]
    MissingTenantKey,

    #[error("missing app ticket for marketplace app")]
    MissingAppTicket,

    #[error("helpdesk API requires helpdesk credentials")]
    MissingHelpdeskCredentials,

    #[error("missing access token when token cache is disabled")]
    MissingAccessToken,

    #[error("token type {0} not supported by this endpoint")]
    UnsupportedTokenType(&'static str),

    #[error("request timeout after {0:?}")]
    RequestTimeout(std::time::Duration),

    #[error("retry failed after {0} attempts")]
    RetryFailed(u32),

    #[error("event decryption failed: {0}")]
    EventDecryption(String),

    #[error("event signature verification failed")]
    EventSignatureVerification,

    #[error("card signature verification failed")]
    CardSignatureVerification,

    #[error("invalid event format: {0}")]
    InvalidEventFormat(String),

    #[error("invalid card action format: {0}")]
    InvalidCardActionFormat(String),

    #[error("websocket connection error: {0}")]
    WebSocketError(String),

    #[error("stream client error: code={code}, msg={msg}")]
    StreamClientError { code: i32, msg: String },

    #[error("stream server error: code={code}, msg={msg}")]
    StreamServerError { code: i32, msg: String },

    #[error("serialization error: {0}")]
    SerializationError(String),

    #[error("deserialization error: {0}")]
    DeserializationError(String),

    #[error("cache error: {0}")]
    CacheError(String),

    #[error("IO error: {0}")]
    IoError(String),

    #[error(transparent)]
    Http(#[from] reqwest::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    HeaderValue(#[from] reqwest::header::InvalidHeaderValue),

    #[error(transparent)]
    HeaderName(#[from] reqwest::header::InvalidHeaderName),

    #[error(transparent)]
    Api(#[from] ApiError),
}

impl Error {
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Http(e) => e.is_timeout() || e.is_connect(),
            Error::RequestTimeout(_) => true,
            Error::RetryFailed(_) => false,
            _ => false,
        }
    }

    pub fn is_api_error(&self) -> bool {
        matches!(self, Error::Api(_))
    }

    pub fn api_code(&self) -> Option<i64> {
        match self {
            Error::Api(api_error) => Some(api_error.code),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_error_builder() {
        let error = ApiError::new(99991663, "token expired")
            .with_request_id("req_123")
            .with_http_status(401)
            .with_raw_body("{\"code\":99991663,\"msg\":\"token expired\"}");

        assert_eq!(error.code, 99991663);
        assert_eq!(error.msg, "token expired");
        assert_eq!(error.request_id, Some("req_123".to_string()));
        assert_eq!(error.http_status, Some(401));
        assert!(!error.raw_body.is_empty());
    }

    #[test]
    fn test_api_error_display() {
        let error = ApiError::new(100, "test error").with_request_id("req_456");
        let display = format!("{}", error);
        assert!(display.contains("code=100"));
        assert!(display.contains("msg=test error"));
        assert!(display.contains("request_id=req_456"));
    }

    #[test]
    fn test_error_is_retryable() {
        let error = Error::RequestTimeout(std::time::Duration::from_secs(30));
        assert!(error.is_retryable());

        let error = Error::MissingConfig("app_id");
        assert!(!error.is_retryable());
    }

    #[test]
    fn test_error_is_api_error() {
        let api_error = ApiError::new(100, "test");
        let error = Error::Api(api_error);
        assert!(error.is_api_error());
        assert_eq!(error.api_code(), Some(100));

        let error = Error::MissingConfig("app_id");
        assert!(!error.is_api_error());
        assert_eq!(error.api_code(), None);
    }
}
