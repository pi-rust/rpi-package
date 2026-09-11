use std::io::Read;

mod kit;

use crate::kit::{http_client, string_param, validate_public_url};
use serde_json::Value;

fn mcp_request(params: &Value) -> Result<String, String> {
    let url = validate_public_url(&string_param(params, "url")?)?;
    let method = string_param(params, "method")?;
    if method.trim().is_empty() || method.len() > 200 {
        return Err("MCP method must contain 1-200 characters".into());
    }
    let id = params.get("id").cloned().unwrap_or(Value::from(1));
    let call_params = params
        .get("params")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": call_params,
    });
    let timeout = params
        .get("timeoutSeconds")
        .and_then(Value::as_u64)
        .unwrap_or(15);
    let mut response = http_client(timeout)?
        .post(url)
        .json(&request)
        .send()
        .map_err(|err| format!("MCP request failed: {err}"))?;
    let status = response.status();
    let mut body = String::new();
    response
        .by_ref()
        .take(1_048_577)
        .read_to_string(&mut body)
        .map_err(|err| format!("failed to read MCP response: {err}"))?;
    if body.len() > 1_048_576 {
        return Err("MCP response exceeded the 1 MiB limit".into());
    }
    if !status.is_success() {
        return Err(format!("MCP server returned {status}: {body}"));
    }
    let json: Value = serde_json::from_str(&body)
        .map_err(|err| format!("MCP server returned invalid JSON: {err}"))?;
    Ok(json.to_string())
}

export_single_tool_plugin!(
    mcp_request,
    "mcp_request",
    "Send a bounded JSON-RPC 2.0 request to an HTTP MCP server.",
    r#"{"type":"object","properties":{"url":{"type":"string"},"method":{"type":"string"},"params":{},"id":{},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":30}},"required":["url","method"]}"#
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_private_mcp_endpoint() {
        let value = serde_json::json!({"url":"http://127.0.0.1:3000","method":"tools/list"});
        assert!(mcp_request(&value).unwrap_err().contains("not allowed"));
    }
}
