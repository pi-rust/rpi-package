//! JSON-RPC 2.0 and MCP tool result types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    pub fn new(id: u64, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Response {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<ErrorObject>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
}

pub fn tool_list(result: &Value) -> Result<Vec<Tool>, String> {
    result
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| "MCP tools/list result has no tools array".to_string())?
        .iter()
        .cloned()
        .map(|value| {
            serde_json::from_value(value).map_err(|error| format!("invalid MCP tool: {error}"))
        })
        .collect()
}

pub fn call_result_text(result: &Value) -> String {
    let mut chunks = Vec::new();
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for item in content {
            match item.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        chunks.push(text.to_string());
                    }
                }
                Some("image") => chunks.push("[MCP image content omitted]".into()),
                _ => chunks.push(item.to_string()),
            }
        }
    }
    if let Some(structured) = result.get("structuredContent") {
        chunks.push(structured.to_string());
    }
    if chunks.is_empty() {
        result.to_string()
    } else {
        chunks.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_request_and_reads_tools() {
        let request = Request::new(7, "tools/list", None);
        assert_eq!(request.id, 7);
        let tools = tool_list(
            &serde_json::json!({"tools":[{"name":"read","inputSchema":{"type":"object"}}]}),
        )
        .unwrap();
        assert_eq!(tools[0].name, "read");
    }

    #[test]
    fn converts_call_result_without_losing_structured_content() {
        let text = call_result_text(&serde_json::json!({
            "content":[{"type":"text","text":"ok"}],
            "structuredContent":{"value":1}
        }));
        assert!(text.contains("ok"));
        assert!(text.contains("value"));
    }
}
