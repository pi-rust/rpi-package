use serde_json::Value;
/// Translate the MCP result into the host's AgentToolResult wire shape.
/// Text and images remain native blocks; other MCP blocks stay in details and
/// receive a readable text representation because the host supports text/images.
pub(crate) fn agent_tool_result(result: &Value) -> Result<Value, String> {
    rpi_mcp_core::connection::check_tool_result(result)?;
    let mut content = Vec::new();
    if let Some(blocks) = result.get("content").and_then(Value::as_array) {
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => content.push(block.clone()),
                Some("image") => {
                    if block.get("data").and_then(Value::as_str).is_none()
                        || block.get("mimeType").and_then(Value::as_str).is_none()
                    {
                        return Err("MCP image must contain data and mimeType".into());
                    }
                    content.push(block.clone());
                }
                _ => content.push(serde_json::json!({"type":"text","text":block.to_string()})),
            }
        }
    }
    if let Some(value) = result.get("structuredContent") {
        content.push(serde_json::json!({"type":"text", "text":value.to_string()}));
    }
    if content.is_empty() {
        content.push(serde_json::json!({"type":"text","text":result.to_string()}));
    }
    Ok(serde_json::json!({"content":content,"details":{"mcpResult":result}}))
}
