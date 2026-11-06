//! Strict, bounded wire parameters; the host owns pagination and leaf pins.
use super::{ApiError, StatusCode};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

fn invalid() -> ApiError {
    ApiError(
        StatusCode::BAD_REQUEST,
        "invalid native message query".into(),
    )
}

pub(super) fn reject_duplicates(uri: &axum::http::Uri) -> Result<(), ApiError> {
    let mut seen = HashSet::new();
    for (key, _) in url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes()) {
        if !seen.insert(key.into_owned()) {
            return Err(invalid());
        }
    }
    Ok(())
}

pub(super) fn parameters(query: &HashMap<String, String>, window: bool) -> Result<Value, ApiError> {
    let allowed: &[&str] = if window {
        &["limit", "anchorId", "beforeId", "afterId", "leafId"]
    } else {
        &["limit", "role", "beforeId", "leafId"]
    };
    if query.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid());
    }
    let limit = match query.get("limit") {
        Some(value) if !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit()) => {
            value.parse::<u64>().map_err(|_| invalid())?
        }
        Some(_) => return Err(invalid()),
        None => {
            if window {
                48
            } else {
                10
            }
        }
    };
    if !(1..=100).contains(&limit) {
        return Err(invalid());
    }
    let mut payload = json!({"limit":limit});
    if !window {
        if query.get("role").is_some_and(|role| role != "user") {
            return Err(invalid());
        }
        payload["role"] = json!("user");
    }
    for key in ["anchorId", "beforeId", "afterId", "leafId"] {
        if let Some(id) = query.get(key) {
            if id.trim().is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
                return Err(invalid());
            }
            // Opaque host ID: do not trim, parse, sort, or replace it.
            payload[key] = json!(id);
        }
    }
    if window
        && ["anchorId", "beforeId", "afterId"]
            .iter()
            .filter(|key| query.contains_key(**key))
            .count()
            != 1
    {
        return Err(invalid());
    }
    Ok(payload)
}
