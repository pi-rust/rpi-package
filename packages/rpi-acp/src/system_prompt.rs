//! ACP-only client-system-prompt RFD conversion. Native owns branch persistence.
use rpi_extcore::system_prompt::Mode;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemPrompt {
    pub mode: Mode,
    pub text: String,
}

/// Validate before opening any native child. Even null requires negotiation;
/// once negotiated, null and omission both preserve the saved branch setting.
pub fn parse(params: &Value, enabled: bool) -> Result<Option<SystemPrompt>, String> {
    let Some(value) = params.get("systemPrompt") else { return Ok(None) };
    if !enabled {
        return Err("systemPrompt requires explicit unstable session RFD negotiation".into());
    }
    if value.is_null() { return Ok(None); }
    let object = value.as_object().ok_or("systemPrompt must be an object or null")?;
    let mode = match object.get("mode").and_then(Value::as_str) {
        Some("append") => Mode::Append,
        Some("override") => Mode::Replace,
        _ => return Err("unsupported systemPrompt mode (expected append or override)".into()),
    };
    if object.get("_meta").is_some_and(|m| !m.is_null() && !m.is_object()) {
        return Err("systemPrompt._meta must be an object or null".into());
    }
    let content = object.get("content").and_then(Value::as_array)
        .ok_or("systemPrompt.content must be a ContentBlock array")?;
    let mut text = String::new();
    for (index, block) in content.iter().enumerate() {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            return Err("systemPrompt supports only text content blocks".into());
        }
        let part = block.get("text").and_then(Value::as_str)
            .ok_or("systemPrompt text block requires string text")?;
        // Bound the aggregate UTF-8 size before growing the output, including
        // separators. Subtraction avoids overflow even for giant input blocks.
        const LIMIT: usize = 256 * 1024;
        let separator = usize::from(index > 0);
        let remaining = LIMIT - text.len();
        if separator > remaining || part.len() > remaining - separator {
            return Err("system prompt exceeds 256 KiB".into());
        }
        if index > 0 { text.push('\n'); }
        text.push_str(part);
    }
    Ok(Some(SystemPrompt { mode, text }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn aggregate_utf8_bound_includes_separators_before_growth() {
        let parse_blocks = |content| parse(&json!({"systemPrompt":{
            "mode":"append", "content":content}}), true);
        let block = |text| json!({"type":"text", "text":text});
        let limit = 256 * 1024;
        assert_eq!(parse_blocks(json!([block("é".repeat(limit / 2))]))
            .unwrap().unwrap().text.len(), limit);
        assert!(parse_blocks(json!([block("x".repeat(limit)), block(String::new())])).is_err());
        assert!(parse_blocks(json!([block(String::new()), block("x".repeat(limit))])).is_err());
        assert_eq!(parse_blocks(json!([block("x".repeat(limit - 1)), block(String::new())]))
            .unwrap().unwrap().text.len(), limit);
        assert!(parse_blocks(json!([block("x".repeat(4 * limit))])).is_err());
    }

    #[test]
    fn explicit_gate_and_text_only_baseline() {
        assert_eq!(parse(&json!({}), false).unwrap(), None);
        for value in [Value::Null, json!({"mode":"append","content":[]})] {
            assert!(parse(&json!({"systemPrompt":value}), false).is_err());
        }
        assert_eq!(parse(&json!({"systemPrompt":null}), true).unwrap(), None);
        for (wire, mode) in [("append", Mode::Append), ("override", Mode::Replace)] {
            assert_eq!(parse(&json!({"systemPrompt":{"mode":wire,"content":[
                {"type":"text","text":"one"},{"type":"text","text":"two"}]}}), true).unwrap(),
                Some(SystemPrompt {mode, text:"one\ntwo".into()}));
        }
        for value in [json!({}), json!({"mode":"replace","content":[]}),
            json!({"mode":"_custom","content":[]}), json!({"mode":"append","content":"x"}),
            json!({"mode":"append","content":[{"type":"text","text":3}]}),
            json!({"mode":"append","content":[{"type":"resource_link","uri":"file:///x"}]}),
            json!({"mode":"append","content":[{"type":"image","data":"x"}]}),
            json!({"mode":"append","content":[{"type":"future"}]}),
            json!({"mode":"append","content":[{"type":"text","text":"é".repeat(131073)}]})] {
            assert!(parse(&json!({"systemPrompt":value}), true).is_err());
        }
    }
}
