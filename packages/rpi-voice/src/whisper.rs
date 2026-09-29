//! OpenAI-compatible STT (speech to text).
//!
//! POSTs WAV audio to `<base>/audio/transcriptions` and returns the text. The
//! OpenAI `/v1/audio/transcriptions` shape is a de-facto standard, so this client
//! works with:
//!
//! - OpenAI Whisper (`https://api.openai.com/v1`, requires a key)
//! - Groq (`https://api.groq.com/openai/v1`, free tier, requires a key)
//! - Self-hosted/free servers that expose the same endpoint, e.g. `speaches`
//!   (faster-whisper), `whisper.cpp`-backed gateways, LocalAI — no key needed
//!
//! Environment:
//! - `RPI_STT_API_BASE`  base URL (default `https://api.openai.com/v1`)
//! - `RPI_STT_API_KEY`   key; falls back to `OPENAI_API_KEY`
//! - `RPI_STT_MODEL`     model name (default `whisper-1`)

use reqwest::blocking::Client;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_API_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "whisper-1";

#[derive(Debug, Deserialize)]
struct WhisperResponse {
    text: String,
}

/// OpenAI-compatible STT client.
pub struct WhisperProvider {
    api_base: String,
    api_key: Option<String>,
    model: String,
    client: Client,
}

impl WhisperProvider {
    /// Create a new STT client.
    ///
    /// - `api_base`: base URL (falls back to `RPI_STT_API_BASE`, then OpenAI)
    /// - `api_key`: key (falls back to `RPI_STT_API_KEY`, then `OPENAI_API_KEY`).
    ///   **Optional for self-hosted servers**: a key is only required when the
    ///   resolved base URL is the hosted OpenAI endpoint, so a local server with
    ///   no auth works out of the box.
    pub fn new(api_base: Option<String>, api_key: Option<String>) -> Result<Self, String> {
        let api_base = api_base
            .or_else(|| std::env::var("RPI_STT_API_BASE").ok())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_API_BASE.to_string());

        let api_key = api_key
            .or_else(|| std::env::var("RPI_STT_API_KEY").ok())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .filter(|s| !s.trim().is_empty());

        // The hosted OpenAI endpoint must have a key; self-hosted servers need not.
        if api_key.is_none() && api_base.trim_end_matches('/') == DEFAULT_API_BASE {
            return Err(
                "STT needs a key for the OpenAI endpoint: set RPI_STT_API_KEY (or OPENAI_API_KEY), \
                 or point RPI_STT_API_BASE at a local/free server that requires none"
                    .to_string(),
            );
        }

        let model = std::env::var("RPI_STT_MODEL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());

        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| format!("HTTP client error: {e}"))?;

        Ok(Self {
            api_base,
            api_key,
            model,
            client,
        })
    }

    /// Transcribe WAV audio bytes to text.
    pub fn transcribe_wav(
        &self,
        wav_bytes: &[u8],
        language: Option<&str>,
    ) -> Result<String, String> {
        let file_part = reqwest::blocking::multipart::Part::bytes(wav_bytes.to_vec())
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|e| format!("Multipart error: {e}"))?;

        let mut form = reqwest::blocking::multipart::Form::new()
            .text("model", self.model.clone())
            .part("file", file_part);

        if let Some(lang) = language {
            form = form.text("language", lang.to_string());
        }

        let url = format!(
            "{}/audio/transcriptions",
            self.api_base.trim_end_matches('/')
        );

        let mut request = self.client.post(&url).multipart(form);
        if let Some(key) = &self.api_key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }

        let resp = request
            .send()
            .map_err(|e| format!("STT request failed ({url}): {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(format!("STT API error ({status}) from {url}: {body}"));
        }

        let result: WhisperResponse = resp
            .json()
            .map_err(|e| format!("Parse response error: {e}"))?;

        Ok(result.text.trim().to_string())
    }
}
