use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::error::Error;
use super::request::{
    ApiRequest, ApiRequestBody, ApiResponse, MultipartFieldValue, RequestOptions,
};

#[async_trait]
pub trait HttpClient: Send + Sync + std::fmt::Debug {
    async fn execute(
        &self,
        request: ApiRequest,
        options: &RequestOptions,
        token: Option<String>,
    ) -> Result<ApiResponse, Error>;
}

pub type HttpClientRef = Arc<dyn HttpClient>;

#[derive(Debug)]
pub struct ReqwestHttpClient {
    client: reqwest::Client,
    base_url: String,
}

impl ReqwestHttpClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.to_string(),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        self
    }
}

#[async_trait]
impl HttpClient for ReqwestHttpClient {
    async fn execute(
        &self,
        request: ApiRequest,
        options: &RequestOptions,
        token: Option<String>,
    ) -> Result<ApiResponse, Error> {
        let url = super::request::build_url(
            &self.base_url,
            &request.api_path,
            &request.path_params,
            &request.query,
        )?;

        let mut req = self.client.request(request.method, url);

        if let Some(ref t) = token {
            req = req.bearer_auth(t);
        }

        for (key, value) in &options.headers {
            req = req.header(key, value);
        }

        if let Some(ref body) = request.body {
            match body {
                ApiRequestBody::Json(body) => {
                    req = req.json(body);
                }
                ApiRequestBody::Multipart(form) => {
                    let mut multipart = reqwest::multipart::Form::new();
                    for field in &form.fields {
                        multipart = match &field.value {
                            MultipartFieldValue::Text(value) => {
                                multipart.text(field.name.clone(), value.clone())
                            }
                            MultipartFieldValue::File(file) => {
                                let mut part = reqwest::multipart::Part::bytes(file.bytes.clone())
                                    .file_name(file.file_name.clone());
                                if let Some(content_type) = &file.content_type {
                                    part = part.mime_str(content_type).map_err(|err| {
                                        Error::SerializationError(err.to_string())
                                    })?;
                                }
                                multipart.part(field.name.clone(), part)
                            }
                        };
                    }
                    req = req.multipart(multipart);
                }
            }
        }

        let timeout = options.timeout;
        if let Some(t) = timeout {
            req = req.timeout(t);
        }

        let resp = req.send().await?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let body = resp.bytes().await?.to_vec();

        Ok(ApiResponse {
            status,
            headers,
            body,
        })
    }
}

pub fn default_http_client(base_url: &str) -> HttpClientRef {
    Arc::new(ReqwestHttpClient::new(base_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reqwest_http_client_builder() {
        let client = ReqwestHttpClient::new("https://open.feishu.cn")
            .with_timeout(std::time::Duration::from_secs(30));
        assert_eq!(client.base_url, "https://open.feishu.cn");
    }
}
