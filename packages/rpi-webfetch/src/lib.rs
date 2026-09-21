use rpi_plugin_sdk::{
    register_entrypoint, FreeStringFn, PluginApiVt, StableToolSchema, StbString, StbStringRef,
    StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use url::Url;
struct Drive {
    params: Value,
    cancelled: AtomicBool,
    done: bool,
}
fn validate(input: &str) -> Result<Url, String> {
    let u = Url::parse(input).map_err(|e| format!("invalid URL: {e}"))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err("only http and https URLs are allowed".into());
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("embedded credentials are not allowed".into());
    }
    let h = u.host_str().ok_or("URL has no host")?;
    if h.eq_ignore_ascii_case("localhost") || h.ends_with(".localhost") {
        return Err("local hosts are not allowed".into());
    }
    if let Ok(ip) = h.parse::<IpAddr>() {
        if blocked_ip(ip) {
            return Err("private or local IPs are not allowed".into());
        }
    } else {
        // Fail closed for hostnames that resolve to loopback, RFC1918,
        // link-local, multicast, or other non-public addresses. This catches
        // the common DNS-based SSRF case that a literal-IP check misses.
        let port = u.port_or_known_default().unwrap_or(443);
        if let Ok(addrs) = (h, port).to_socket_addrs() {
            if addrs.into_iter().any(|addr| blocked_ip(addr.ip())) {
                return Err("hostname resolves to a private or local IP".into());
            }
        }
    }
    Ok(u)
}

fn blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.octets()[0] == 0
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
        }
    }
}
fn fetch(p: &Value) -> Result<String, String> {
    let url = validate(
        p.get("url")
            .and_then(Value::as_str)
            .ok_or("url is required")?,
    )?;
    let max = p
        .get("maxChars")
        .and_then(Value::as_u64)
        .unwrap_or(20480)
        .clamp(256, 50000) as usize;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("rpi-webfetch/0.1")
        .build()
        .map_err(|e| e.to_string())?;
    let mut r = client
        .get(url)
        .send()
        .map_err(|e| format!("web fetch failed: {e}"))?;
    let status = r.status();
    let final_url = r.url().to_string();
    validate(&final_url)?;
    let ct = r
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut b = Vec::new();
    r.by_ref()
        .take(1_048_577)
        .read_to_end(&mut b)
        .map_err(|e| e.to_string())?;
    if b.len() > 1_048_576 {
        return Err("response exceeded 1 MiB".into());
    }
    if !(ct.is_empty()
        || ct.contains("text/")
        || ct.contains("json")
        || ct.contains("xml")
        || ct.contains("html"))
    {
        return Ok(json!({"url":final_url,"status":status.as_u16(),"contentType":ct,"skipped":"non-text response"}).to_string());
    }
    let raw = String::from_utf8_lossy(&b);
    let text = if ct.contains("html") {
        strip_html(&raw)
    } else {
        raw.to_string()
    };
    let clipped: String = text.chars().take(max).collect();
    Ok(json!({"url":final_url,"status":status.as_u16(),"contentType":ct,"text":clipped,"truncated":text.chars().count()>max}).to_string())
}
fn strip_html(s: &str) -> String {
    let mut out = s.replace("\r", " ");
    for tag in [
        "script", "style", "nav", "footer", "header", "aside", "noscript",
    ] {
        let re_start = format!("<{}", tag);
        while let Some(a) = out.to_ascii_lowercase().find(&re_start) {
            if let Some(b) = out[a..].find(&format!("</{}>", tag)) {
                out.replace_range(a..a + b + tag.len() + 3, " ");
            } else {
                break;
            }
        }
    }
    out = out.replace("><", ">\n<");
    let mut result = String::new();
    let mut inside = false;
    for c in out.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            '_' if inside => {}
            c if !inside => result.push(c),
            _ => {}
        }
    }
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}
extern "C" fn execute(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let t = params.to_string_lossy();
    params.free_with(free);
    Box::into_raw(Box::new(Drive {
        params: serde_json::from_str(&t).unwrap_or(Value::Null),
        cancelled: AtomicBool::new(false),
        done: false,
    })) as StepHandle
}
extern "C" fn poll(h: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if h.is_null() {
        return StepResult::err(StbString::from_string("null webfetch handle".into()));
    }
    let d = unsafe { &mut *(h as *mut Drive) };
    if d.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("webfetch cancelled".into()));
    }
    if d.done {
        return StepResult::err(StbString::from_string(
            "webfetch polled after completion".into(),
        ));
    }
    d.done = true;
    match fetch(&d.params) {
        Ok(t) => StepResult::done(StbString::from_string(
            json!({"content":[{"type":"text","text":t}]}).to_string(),
        )),
        Err(e) => StepResult::done(StbString::from_string(
            json!({
                "content":[{"type":"text","text":format!("webfetch failed: {e}. Proceed with another reference or retry.")}],
                "details":{"error":true,"message":e}
            })
            .to_string(),
        )),
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
    unsafe { register_entrypoint(api, abi, |api| {
        let Some(register) = api.register_tool else {
            return 1;
        };
        let schema=Box::new(StableToolSchema{name:StbString::from_string("webfetch".into()),description:StbString::from_string("Fetch bounded readable text from a public HTTP(S) URL.".into()),parameters:StbString::from_string(r#"{"type":"object","properties":{"url":{"type":"string"},"maxChars":{"type":"integer","minimum":256,"maximum":50000}},"required":["url"]}"#.into())});
        let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
        drop(schema);
        rc
    }) }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_private() {
        assert!(validate("http://127.0.0.1").is_err());
    }
}
