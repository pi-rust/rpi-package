use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use reqwest::header::HeaderMap;
use reqwest::{Method, Url};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::error::Error;

pub const HEADER_LOG_ID: &str = "x-tt-logid";
pub const HEADER_REQUEST_ID: &str = "x-request-id";
pub const HEADER_OAPI_REQUEST_ID: &str = "oapi-sdk-request-id";
pub const HEADER_CONTENT_DISPOSITION: &str = "content-disposition";

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum AccessTokenType {
    None,
    App,
    Tenant,
    User,
}

impl AccessTokenType {
    pub fn as_str(self) -> &'static str {
        match self {
            AccessTokenType::None => "none_access_token",
            AccessTokenType::App => "app_access_token",
            AccessTokenType::Tenant => "tenant_access_token",
            AccessTokenType::User => "user_access_token",
        }
    }

    pub fn from_go_name(input: &str) -> Option<Self> {
        if input.ends_with("AccessTokenTypeNone") || input == "none_access_token" {
            return Some(Self::None);
        }
        if input.ends_with("AccessTokenTypeApp") || input == "app_access_token" {
            return Some(Self::App);
        }
        if input.ends_with("AccessTokenTypeTenant") || input == "tenant_access_token" {
            return Some(Self::Tenant);
        }
        if input.ends_with("AccessTokenTypeUser") || input == "user_access_token" {
            return Some(Self::User);
        }
        None
    }
}

#[derive(Debug, Clone)]
pub struct RequestOptions {
    pub headers: HeaderMap,
    pub user_access_token: Option<String>,
    pub tenant_access_token: Option<String>,
    pub app_access_token: Option<String>,
    pub tenant_key: Option<String>,
    pub app_ticket: Option<String>,
    pub request_id: Option<String>,
    pub need_helpdesk_auth: bool,
    pub timeout: Option<Duration>,
    pub retry_count: Option<u32>,
    pub retry_delay: Option<Duration>,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            headers: HeaderMap::new(),
            user_access_token: None,
            tenant_access_token: None,
            app_access_token: None,
            tenant_key: None,
            app_ticket: None,
            request_id: None,
            need_helpdesk_auth: false,
            timeout: None,
            retry_count: None,
            retry_delay: None,
        }
    }
}

impl RequestOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn header(mut self, key: &str, value: &str) -> Self {
        use reqwest::header::{HeaderName, HeaderValue};
        if let Ok(header_name) = key.parse::<HeaderName>()
            && let Ok(header_value) = value.parse::<HeaderValue>()
        {
            self.headers.insert(header_name, header_value);
        }
        self
    }

    pub fn user_access_token(mut self, token: impl Into<String>) -> Self {
        self.user_access_token = Some(token.into());
        self
    }

    pub fn tenant_access_token(mut self, token: impl Into<String>) -> Self {
        self.tenant_access_token = Some(token.into());
        self
    }

    pub fn app_access_token(mut self, token: impl Into<String>) -> Self {
        self.app_access_token = Some(token.into());
        self
    }

    pub fn tenant_key(mut self, key: impl Into<String>) -> Self {
        self.tenant_key = Some(key.into());
        self
    }

    pub fn app_ticket(mut self, ticket: impl Into<String>) -> Self {
        self.app_ticket = Some(ticket.into());
        self
    }

    pub fn request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    pub fn need_helpdesk_auth(mut self) -> Self {
        self.need_helpdesk_auth = true;
        self
    }

    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    pub fn retry(mut self, count: u32, delay: Duration) -> Self {
        self.retry_count = Some(count);
        self.retry_delay = Some(delay);
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct MultipartForm {
    pub fields: Vec<MultipartField>,
}

impl MultipartForm {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.push(MultipartField::text(name, value));
        self
    }

    pub fn file(
        mut self,
        name: impl Into<String>,
        file_name: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Self {
        self.fields
            .push(MultipartField::file(name, file_name, bytes.into()));
        self
    }

    pub fn file_with_content_type(
        mut self,
        name: impl Into<String>,
        file_name: impl Into<String>,
        content_type: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Self {
        self.fields.push(MultipartField::file_with_content_type(
            name,
            file_name,
            content_type,
            bytes.into(),
        ));
        self
    }
}

#[derive(Debug, Clone)]
pub struct MultipartField {
    pub name: String,
    pub value: MultipartFieldValue,
}

impl MultipartField {
    pub fn text(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: MultipartFieldValue::Text(value.into()),
        }
    }

    pub fn file(name: impl Into<String>, file_name: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            value: MultipartFieldValue::File(MultipartFile {
                file_name: file_name.into(),
                content_type: None,
                bytes,
            }),
        }
    }

    pub fn file_with_content_type(
        name: impl Into<String>,
        file_name: impl Into<String>,
        content_type: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Self {
        Self {
            name: name.into(),
            value: MultipartFieldValue::File(MultipartFile {
                file_name: file_name.into(),
                content_type: Some(content_type.into()),
                bytes,
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub enum MultipartFieldValue {
    Text(String),
    File(MultipartFile),
}

#[derive(Debug, Clone)]
pub struct MultipartFile {
    pub file_name: String,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum ApiRequestBody {
    Json(Value),
    Multipart(MultipartForm),
}

#[derive(Debug, Clone)]
pub struct ApiRequest {
    pub method: Method,
    pub api_path: String,
    pub query: Vec<(String, String)>,
    pub path_params: HashMap<String, String>,
    pub body: Option<ApiRequestBody>,
    pub supported_token_types: Vec<AccessTokenType>,
}

impl ApiRequest {
    pub fn new(method: Method, api_path: impl Into<String>) -> Self {
        Self {
            method,
            api_path: api_path.into(),
            query: Vec::new(),
            path_params: HashMap::new(),
            body: None,
            supported_token_types: vec![AccessTokenType::None],
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApiResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl ApiResponse {
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, Error> {
        Ok(serde_json::from_slice(&self.body)?)
    }

    pub fn json_value(&self) -> Result<Value, Error> {
        Ok(serde_json::from_slice(&self.body)?)
    }

    pub fn request_id(&self) -> Option<String> {
        self.headers
            .get(HEADER_LOG_ID)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .or_else(|| {
                self.headers
                    .get(HEADER_REQUEST_ID)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string())
            })
    }

    pub fn file_name(&self) -> Option<String> {
        let disposition = self
            .headers
            .get(HEADER_CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())?;
        parse_content_disposition_filename(disposition)
    }

    pub fn downloaded_file(&self) -> DownloadedFile {
        DownloadedFile {
            file_name: self.file_name(),
            bytes: self.body.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DownloadedFile {
    pub file_name: Option<String>,
    pub bytes: Vec<u8>,
}

impl DownloadedFile {
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<(), Error> {
        std::fs::write(path, &self.bytes).map_err(|err| Error::IoError(err.to_string()))
    }
}

fn parse_content_disposition_filename(disposition: &str) -> Option<String> {
    for segment in disposition.split(';') {
        let segment = segment.trim();
        if let Some(encoded) = segment.strip_prefix("filename*=") {
            let encoded = encoded.trim_matches('"');
            let value = encoded
                .split_once("''")
                .map(|(_, value)| value)
                .unwrap_or(encoded);
            return urlencoding::decode(value).ok().map(|v| v.into_owned());
        }
    }

    for segment in disposition.split(';') {
        let segment = segment.trim();
        if let Some(file_name) = segment.strip_prefix("filename=") {
            return Some(file_name.trim_matches('"').to_string());
        }
    }

    None
}

pub fn build_url(
    base_url: &str,
    api_path: &str,
    path_params: &HashMap<String, String>,
    query: &[(String, String)],
) -> Result<Url, Error> {
    let replaced_path = if api_path.starts_with("http://") || api_path.starts_with("https://") {
        api_path.to_string()
    } else {
        let mut segments = Vec::new();
        for segment in api_path.split('/') {
            if let Some(name) = segment.strip_prefix(':') {
                let value = path_params
                    .get(name)
                    .ok_or_else(|| Error::MissingPathParam(name.to_string()))?;
                segments.push(urlencoding::encode(value).to_string());
            } else {
                segments.push(segment.to_string());
            }
        }
        let merged = segments.join("/");
        format!("{}{}", base_url.trim_end_matches('/'), merged)
    };

    let mut url = Url::parse(&replaced_path).map_err(|e| Error::InvalidUrl(e.to_string()))?;
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (k, v) in query {
            pairs.append_pair(k, v);
        }
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_url_replaces_path_and_query() {
        let mut path_params = HashMap::new();
        path_params.insert("chat_id".to_string(), "oc_123/abc".to_string());
        let query = vec![("page_size".to_string(), "20".to_string())];
        let url = build_url(
            "https://open.feishu.cn",
            "/open-apis/im/v1/chats/:chat_id",
            &path_params,
            &query,
        )
        .expect("url must build");

        assert_eq!(
            url.as_str(),
            "https://open.feishu.cn/open-apis/im/v1/chats/oc_123%2Fabc?page_size=20"
        );
    }

    #[test]
    fn parse_access_token_type_from_go_name() {
        assert_eq!(
            AccessTokenType::from_go_name("larkcore.AccessTokenTypeTenant"),
            Some(AccessTokenType::Tenant)
        );
        assert_eq!(
            AccessTokenType::from_go_name("user_access_token"),
            Some(AccessTokenType::User)
        );
        assert_eq!(AccessTokenType::from_go_name("unknown"), None);
    }

    #[test]
    fn test_request_options_builder() {
        let options = RequestOptions::new()
            .user_access_token("u_token")
            .tenant_key("tenant_123")
            .request_id("req_123")
            .need_helpdesk_auth()
            .timeout(Duration::from_secs(30))
            .retry(3, Duration::from_secs(1))
            .header("X-Custom-Header", "value");

        assert_eq!(options.user_access_token, Some("u_token".to_string()));
        assert_eq!(options.tenant_key, Some("tenant_123".to_string()));
        assert_eq!(options.request_id.as_deref(), Some("req_123"));
        assert!(options.need_helpdesk_auth);
        assert_eq!(options.timeout, Some(Duration::from_secs(30)));
        assert_eq!(options.retry_count, Some(3));
        assert_eq!(options.retry_delay, Some(Duration::from_secs(1)));
    }

    #[test]
    fn multipart_form_collects_fields() {
        let form = MultipartForm::new()
            .text("file_name", "demo.txt")
            .file_with_content_type("file", "demo.txt", "text/plain", b"hello".to_vec());

        assert_eq!(form.fields.len(), 2);
        assert!(matches!(
            form.fields[0].value,
            MultipartFieldValue::Text(ref value) if value == "demo.txt"
        ));
        assert!(matches!(
            form.fields[1].value,
            MultipartFieldValue::File(ref file)
                if file.file_name == "demo.txt"
                    && file.content_type.as_deref() == Some("text/plain")
        ));
    }

    #[test]
    fn api_response_parses_download_file_name() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HEADER_CONTENT_DISPOSITION,
            "attachment; filename=\"demo.txt\"".parse().expect("header"),
        );
        let response = ApiResponse {
            status: 200,
            headers,
            body: b"hello".to_vec(),
        };

        assert_eq!(response.file_name().as_deref(), Some("demo.txt"));
        assert_eq!(response.downloaded_file().bytes, b"hello".to_vec());
    }
}
