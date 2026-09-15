use rpi_plugin_sdk::{
    register_entrypoint, FreeStringFn, PluginApiVt, StableToolSchema, StbString, StbStringRef,
    StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use url::Url;

const DEFAULT_BASE_URL: &str = "https://api.firecrawl.dev/v2";
const MAX_CHARS: usize = 50_000;

type Builder = fn(&Value) -> Result<String, String>;

struct Drive {
    params: Value,
    builder: Builder,
    cancelled: AtomicBool,
    done: bool,
}

fn base_url() -> String {
    std::env::var("FIRECRAWL_BASE_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
}

fn api_key() -> Option<String> {
    std::env::var("FIRECRAWL_API_KEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn client(timeout_secs: u64) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent("rpi-firecrawl/0.1")
        .build()
        .map_err(|e| e.to_string())
}

/// POST a JSON body to a Firecrawl endpoint and return the decoded value.
/// Succeeds only when the HTTP request succeeds and `success: true`.
fn post_json(
    client: &reqwest::blocking::Client,
    path: &str,
    body: &Value,
) -> Result<Value, String> {
    let url = format!("{}{}", base_url(), path);
    let mut req = client.post(&url).json(body);
    if let Some(key) = api_key() {
        req = req.bearer_auth(key);
    }
    let resp = req
        .send()
        .map_err(|e| format!("firecrawl request failed: {e}"))?;
    let status = resp.status();
    let value: Value = resp
        .json()
        .map_err(|e| format!("invalid firecrawl response: {e}"))?;
    if !status.is_success() {
        let msg = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or(status.as_str());
        return Err(format!("firecrawl api error {status}: {msg}"));
    }
    if value.get("success").and_then(Value::as_bool) != Some(true) {
        return Err("firecrawl api returned success=false".into());
    }
    Ok(value)
}

pub fn search(p: &Value) -> Result<String, String> {
    let query = p
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .ok_or("query is required")?;
    let limit = p
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 20) as usize;
    // Fetching full page content costs extra credits. Default off for the
    // free/keyless tier; opt in with content=true.
    let with_content = p.get("content").and_then(Value::as_bool).unwrap_or(false);
    let mut body = json!({ "query": query, "limit": limit });
    if with_content {
        body["scrapeOptions"] = json!({ "formats": ["markdown"] });
    }
    let value = post_json(&client(60)?, "/search", &body)?;
    let data = value.get("data").unwrap_or(&Value::Null);
    // v2 returns data.web / data.news groups; older responses may be a flat array.
    let mut hits = Vec::new();
    if let Some(arr) = data.as_array() {
        for item in arr {
            hits.push(extract_search_hit(item, "web", with_content));
            if hits.len() >= limit {
                break;
            }
        }
    } else {
        for group in ["web", "news"] {
            if let Some(arr) = data.get(group).and_then(Value::as_array) {
                for item in arr {
                    hits.push(extract_search_hit(item, group, with_content));
                    if hits.len() >= limit {
                        break;
                    }
                }
            }
            if hits.len() >= limit {
                break;
            }
        }
    }
    Ok(json!({"query":query,"count":hits.len(),"engine":"firecrawl","results":hits}).to_string())
}

fn extract_search_hit(item: &Value, group: &str, with_content: bool) -> Value {
    let mut hit = json!({
        "group": group,
        "title": item.get("title").and_then(Value::as_str).unwrap_or(""),
        "url": item.get("url").and_then(Value::as_str).unwrap_or(""),
        "description": item.get("description").and_then(Value::as_str).unwrap_or(""),
    });
    if with_content {
        let md = item.get("markdown").and_then(Value::as_str).unwrap_or("");
        hit["content"] = json!(clip(md));
    }
    hit
}

pub fn scrape(p: &Value) -> Result<String, String> {
    let url = p
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or("url is required")?;
    let parsed = Url::parse(url).map_err(|e| format!("invalid url: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("only http and https urls are supported".into());
    }
    let max_chars = p
        .get("maxChars")
        .and_then(Value::as_u64)
        .unwrap_or(12_000)
        .clamp(256, 50_000) as usize;
    let body = json!({"url": url, "formats": ["markdown"]});
    let value = post_json(&client(90)?, "/scrape", &body)?;
    let data = value.get("data").unwrap_or(&Value::Null);
    let metadata = data.get("metadata").unwrap_or(&Value::Null);
    let status_code = metadata
        .get("statusCode")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let markdown = data.get("markdown").and_then(Value::as_str).unwrap_or("");
    let total = markdown.chars().count();
    let clipped: String = markdown.chars().take(max_chars).collect();
    let suffix = if total > max_chars {
        "\n[truncated]"
    } else {
        ""
    };
    Ok(json!({
        "url": url,
        "title": metadata.get("title").and_then(Value::as_str).unwrap_or(""),
        "statusCode": status_code,
        "text": format!("{clipped}{suffix}"),
        "truncated": total > max_chars,
    })
    .to_string())
}

/// Clip long text for model consumption.
fn clip(s: &str) -> String {
    let total = s.chars().count();
    if total <= MAX_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX_CHARS).collect();
    out.push_str("\n[truncated]");
    out
}

fn start(params: StbString, free: Option<FreeStringFn>, builder: Builder) -> StepHandle {
    let t = params.to_string_lossy();
    params.free_with(free);
    let params = serde_json::from_str(&t).unwrap_or(Value::Null);
    Box::into_raw(Box::new(Drive {
        params,
        builder,
        cancelled: AtomicBool::new(false),
        done: false,
    })) as StepHandle
}

extern "C" fn execute_search(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    start(params, free, search)
}

extern "C" fn execute_scrape(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    start(params, free, scrape)
}

extern "C" fn poll(h: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if h.is_null() {
        return StepResult::err(StbString::from_string("null firecrawl handle".into()));
    }
    let d = unsafe { &mut *(h as *mut Drive) };
    if d.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("firecrawl cancelled".into()));
    }
    if d.done {
        return StepResult::err(StbString::from_string(
            "firecrawl polled after completion".into(),
        ));
    }
    d.done = true;
    let result = (d.builder)(&d.params);
    match result {
        Ok(t) => StepResult::done(StbString::from_string(
            json!({"content":[{"type":"text","text":t}]}).to_string(),
        )),
        Err(e) => StepResult::err(StbString::from_string(e)),
    }
}

extern "C" fn cancel(h: StepHandle) {
    if !h.is_null() {
        unsafe {
            (&*(h as *mut Drive))
                .cancelled
                .store(true, Ordering::SeqCst);
        }
    }
}

extern "C" fn destroy(h: StepHandle) {
    if !h.is_null() {
        unsafe {
            drop(Box::from_raw(h as *mut Drive));
        }
    }
}

extern "C" fn free_string(s: StbString) {
    if !s.is_empty() && !s.ptr.is_null() {
        unsafe {
            let b = std::slice::from_raw_parts(s.ptr as *const u8, s.len);
            let _ = Box::from_raw(b as *const [u8] as *mut [u8]);
        }
    }
}

#[no_mangle]
pub extern "C" fn rpi_plugin_register_v2(api: *const PluginApiVt, abi: u32) -> i32 {
    register_entrypoint(api, abi, |api| {
        let Some(register) = api.register_tool else {
            return 1;
        };
        let schemas = [
            (
                "firecrawl_search",
                "Search the public web with Firecrawl. Returns titles, URLs, descriptions, and optionally full page content as Markdown. Set content=true to fetch page content (costs extra credits).",
                r#"{"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":20},"content":{"type":"boolean"}},"required":["query"]}"#,
            ),
            (
                "firecrawl_scrape",
                "Scrape a public HTTP(S) URL with Firecrawl and return clean Markdown text. Handles JS-rendered pages and PDFs. Bounded output size.",
                r#"{"type":"object","properties":{"url":{"type":"string"},"maxChars":{"type":"integer","minimum":256,"maximum":50000}},"required":["url"]}"#,
            ),
        ];
        for (name, desc, params) in schemas {
            let schema = Box::new(StableToolSchema {
                name: StbString::from_string(name.into()),
                description: StbString::from_string(desc.into()),
                parameters: StbString::from_string(params.into()),
            });
            let execute = if name == "firecrawl_search" {
                execute_search
            } else {
                execute_scrape
            };
            let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
            drop(schema);
            if rc != 0 {
                return rc;
            }
        }
        0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_query() {
        assert!(search(&json!({"query":""})).is_err());
    }

    #[test]
    fn rejects_non_http_url() {
        assert!(scrape(&json!({"url":"file:///etc/passwd"})).is_err());
    }

    #[test]
    fn clips_long_text() {
        let s = "x".repeat(60_000);
        let out = clip(&s);
        assert!(out.ends_with("[truncated]"));
        assert_eq!(
            out.chars().count(),
            MAX_CHARS + "\n[truncated]".chars().count()
        );
    }

    #[test]
    fn parses_search_hit() {
        let item = json!({"title":"T","url":"https://e.com","description":"D","markdown":"M"});
        let hit = extract_search_hit(&item, "web", true);
        assert_eq!(hit["content"], json!("M"));
        let hit = extract_search_hit(&item, "web", false);
        assert!(hit.get("content").is_none());
    }
}
