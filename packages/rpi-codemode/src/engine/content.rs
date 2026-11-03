//! Explicit native output selection; never infer images from arbitrary JSON.
use super::{public_value, MAX_JSON};
use serde_json::{json, Value};

pub(super) const MAX_BLOCKS: usize = 128;

/// Validate the full selection before accepting any block. Remove reserved
/// transport fields recursively, then reject fields the native host cannot retain.
pub(super) fn blocks(mut value: Value) -> Result<Vec<Value>, String> {
    public_value(&mut value);
    let value = if value.is_array() {
        value
    } else {
        value
            .as_object_mut()
            .and_then(|m| m.remove("content"))
            .ok_or("native output requires a result with content or a content array")?
    };
    let array = value.as_array().ok_or("native content must be an array")?;
    if array.len() > MAX_BLOCKS {
        return Err("native content exceeds 128 blocks".into());
    }
    if value.to_string().len() > MAX_JSON {
        return Err("native content exceeds 1 MiB".into());
    }
    for block in array {
        let kind = block.get("type").and_then(Value::as_str);
        let supported: &[&str] = match kind {
            Some("text") => &["type", "text", "textSignature"],
            Some("image") => &["type", "data", "mimeType"],
            _ => return Err("unsupported native content block (only text/image supported)".into()),
        };
        if block
            .as_object()
            .is_some_and(|object| object.keys().any(|key| !supported.contains(&key.as_str())))
        {
            return Err("unsupported native content block metadata".into());
        }
        match kind {
            Some("text") if block.get("text").is_some_and(Value::is_string) => {
                if block
                    .get("textSignature")
                    .is_some_and(|value| !value.is_null() && !value.is_string())
                {
                    return Err("native textSignature must be null or a string".into());
                }
            }
            Some("image") => {
                let mime = block
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .ok_or("native image requires mimeType")?;
                if !matches!(
                    mime,
                    "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                ) {
                    return Err("unsupported native image MIME type".into());
                }
                let data = block
                    .get("data")
                    .and_then(Value::as_str)
                    .ok_or("native image requires base64 data")?;
                if !base64(data.as_bytes()) {
                    return Err("native image data must be nonempty canonical base64".into());
                }
            }
            Some("text") => return Err("native text requires string text".into()),
            _ => return Err("unsupported native content block (only text/image supported)".into()),
        }
    }
    let Value::Array(array) = value else {
        unreachable!()
    };
    Ok(array)
}

fn base64(data: &[u8]) -> bool {
    if data.is_empty() || data.len() % 4 != 0 {
        return false;
    }
    let padding = data.iter().rev().take_while(|&&b| b == b'=').count();
    if padding > 2 {
        return false;
    }
    let payload = &data[..data.len() - padding];
    let digit = |b: u8| match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    if !payload.iter().all(|&b| digit(b).is_some()) {
        return false;
    }
    let last = digit(*payload.last().unwrap_or(&0)).unwrap_or(0);
    match padding {
        2 => last & 15 == 0,
        1 => last & 3 == 0,
        _ => true,
    }
}

/// Move selected blocks out of the JSON summary so base64 appears only in the
/// outer native content. Other finalized fields remain ordinary summary data,
/// NOT the containing tool's usage/addedToolNames.
pub(super) fn promote(result: &mut Value) -> Result<Option<Vec<Value>>, String> {
    let Some(object) = result.as_object_mut() else {
        return Ok(None);
    };
    let Some(content) = object.remove("content") else {
        return Ok(None);
    };
    let selected = blocks(content)?;
    object.insert(
        "content".into(),
        json!({"promoted":true,"count":selected.len()}),
    );
    Ok(Some(selected))
}
