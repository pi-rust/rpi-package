use std::io::Read;

mod kit;

use crate::kit::{http_client, string_param, validate_public_url};
use serde_json::Value;

fn web_fetch(params: &Value) -> Result<String, String> {
    let url = validate_public_url(&string_param(params, "url")?)?;
    let max_chars = params
        .get("maxChars")
        .and_then(Value::as_u64)
        .unwrap_or(12_000)
        .clamp(256, 50_000) as usize;
    let timeout = params
        .get("timeoutSeconds")
        .and_then(Value::as_u64)
        .unwrap_or(15);
    let mut response = http_client(timeout)?
        .get(url)
        .send()
        .map_err(|err| format!("web fetch failed: {err}"))?;
    let status = response.status();
    let final_url = response.url().to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(1_048_577)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("failed to read response: {err}"))?;
    if bytes.len() > 1_048_576 {
        return Err("response exceeded the 1 MiB limit".into());
    }
    let text = String::from_utf8_lossy(&bytes);
    let clipped: String = text.chars().take(max_chars).collect();
    let suffix = if text.chars().count() > max_chars {
        "\n[truncated]"
    } else {
        ""
    };
    Ok(format!(
        "status: {status}\nurl: {final_url}\ncontent-type: {content_type}\n\n{clipped}{suffix}"
    ))
}

export_single_tool_plugin!(
    web_fetch,
    "web_fetch",
    "Fetch a public HTTP(S) resource with strict size and time limits.",
    r#"{"type":"object","properties":{"url":{"type":"string"},"maxChars":{"type":"integer","minimum":256,"maximum":50000},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":30}},"required":["url"]}"#
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_file_urls() {
        let value = serde_json::json!({"url":"file:///etc/passwd"});
        assert!(web_fetch(&value).unwrap_err().contains("http"));
    }
}
