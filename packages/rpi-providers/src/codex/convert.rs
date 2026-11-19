//! Pi transcript projection onto the OpenAI Responses request body, following
//! pi-ai's `openai-codex-responses.ts` (`buildRequestBody`) and
//! `openai-responses-shared.ts` (`convertResponsesMessages`).
use super::json::{entries, trim, truthy};
use rpi_ai::types::ThinkingLevel;
use serde_json::{json, Map, Value};

pub const DEFAULT_INSTRUCTIONS: &str = "You are a helpful assistant.";

fn array<'a>(value: &'a Value, field: &str) -> Result<&'a [Value], String> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| format!("Invalid Pi context: {field} must be an array"))
}
fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .as_str()
        .ok_or_else(|| format!("Invalid Pi context: {field} must be a string"))
}
fn content_text(content: &Value) -> Result<String, String> {
    if let Some(text) = content.as_str() {
        return Ok(text.into());
    }
    if content.is_null() {
        return Ok(String::new());
    }
    array(content, "content")?
        .iter()
        .filter(|block| block["type"] == "text")
        .map(|block| text(&block["text"], "text"))
        .collect::<Result<Vec<_>, _>>()
        .map(|parts| parts.join("\n"))
}

fn normalize_id_part(part: &str) -> String {
    let sanitized: String = part
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let normalized = if sanitized.len() > 64 {
        sanitized[..64].to_owned()
    } else {
        sanitized
    };
    normalized.trim_end_matches('_').to_owned()
}

// pi-ai shortHash uses wrapping 32-bit arithmetic over UTF-16 code units.
fn short_hash(value: &str) -> String {
    let mut h1 = 0xdeadbeef_u32;
    let mut h2 = 0x41c6ce57_u32;
    for ch in value.encode_utf16() {
        h1 = (h1 ^ u32::from(ch)).wrapping_mul(2654435761);
        h2 = (h2 ^ u32::from(ch)).wrapping_mul(1597334677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2246822507) ^ (h2 ^ (h2 >> 13)).wrapping_mul(3266489909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2246822507) ^ (h1 ^ (h1 >> 13)).wrapping_mul(3266489909);
    fn base36(mut value: u32) -> String {
        if value == 0 {
            return "0".into();
        }
        let mut digits = Vec::new();
        while value != 0 {
            digits.push(char::from_digit(value % 36, 36).unwrap());
            value /= 36;
        }
        digits.into_iter().rev().collect()
    }
    format!("{}{}", base36(h2), base36(h1))
}

/// Pi tool call ids carry `callId|itemId`; preserve the pair, hashing foreign item ids.
fn tool_call_id(id: &str, same_provider_api: bool) -> (String, Option<String>) {
    match id.split_once('|') {
        Some((call, item)) if same_provider_api => {
            let mut item = normalize_id_part(item);
            if !item.starts_with("fc_") {
                item = normalize_id_part(&format!("fc_{item}"));
            }
            (normalize_id_part(call), Some(item))
        }
        Some((call, item)) => (
            normalize_id_part(call),
            Some(format!("fc_{}", short_hash(item))),
        ),
        None => (normalize_id_part(id), None),
    }
}

fn parse_text_signature(signature: &str) -> Option<(String, Option<String>)> {
    if let Ok(parsed) = serde_json::from_str::<Value>(signature) {
        if parsed["v"] == 1 {
            let id = parsed["id"].as_str()?.to_owned();
            let phase = parsed["phase"]
                .as_str()
                .filter(|phase| *phase == "commentary" || *phase == "final_answer")
                .map(str::to_owned);
            return Some((id, phase));
        }
    }
    Some((signature.to_owned(), None))
}

fn user_content(content: &Value) -> Result<Vec<Value>, String> {
    if let Some(text) = content.as_str() {
        return Ok(if trim(text).is_empty() {
            vec![]
        } else {
            vec![json!({"type": "input_text", "text": text})]
        });
    }
    if content.is_null() {
        return Ok(vec![]);
    }
    let mut items = Vec::new();
    for block in array(content, "user.content")? {
        match block["type"].as_str() {
            Some("text") if truthy(&block["text"]) => {
                items.push(json!({"type": "input_text", "text": text(&block["text"], "text")?}));
            }
            Some("image") if truthy(&block["data"]) => {
                items.push(json!({
                    "type": "input_image",
                    "detail": "auto",
                    "image_url": format!("data:{};base64,{}", text(&block["mimeType"], "image.mimeType")?, text(&block["data"], "image.data")?),
                }));
            }
            _ => {}
        }
    }
    Ok(items)
}

fn assistant_output(
    message: &Value,
    model: &Value,
    msg_index: usize,
) -> Result<Vec<Value>, String> {
    let same = message["provider"] == model["provider"] && message["api"] == model["api"];
    let same_model = same && message["model"] == model["id"];
    let mut output = Vec::new();
    let mut text_block_index = 0;
    for block in array(&message["content"], "assistant.content")? {
        match block["type"].as_str() {
            Some("thinking") if same_model => {
                // Reasoning replays statelessly through the stored item (store: false).
                if let Some(signature) = block["thinkingSignature"].as_str() {
                    if signature.starts_with('{') {
                        if let Ok(item) = serde_json::from_str::<Value>(signature) {
                            output.push(item);
                        }
                    }
                }
            }
            Some("text") => {
                let body = text(&block["text"], "text")?;
                if body.is_empty() && !same {
                    continue;
                }
                let fallback = if text_block_index == 0 {
                    format!("msg_pi_{msg_index}")
                } else {
                    format!("msg_pi_{msg_index}_{text_block_index}")
                };
                text_block_index += 1;
                let (mut id, phase) = block
                    .get("textSignature")
                    .and_then(|signature| signature.as_str())
                    .and_then(parse_text_signature)
                    .unwrap_or((fallback, None));
                if id.len() > 64 {
                    id = normalize_id_part(&id);
                }
                let mut item = json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": body, "annotations": []}],
                    "status": "completed",
                    "id": id,
                });
                if let Some(phase) = phase {
                    item["phase"] = phase.into();
                }
                if same_model {
                    if let Some(namespace) = block.get("namespace").filter(|value| !value.is_null())
                    {
                        item["namespace"] = namespace.clone();
                    }
                }
                output.push(item);
            }
            Some("toolCall") => {
                let (call_id, item_id) = tool_call_id(text(&block["id"], "toolCall.id")?, same);
                let mut item = json!({
                    "type": "function_call",
                    "call_id": call_id,
                    "name": text(&block["name"], "toolCall.name")?,
                    "arguments": serde_json::to_string(&block["arguments"]).unwrap_or_else(|_| "{}".into()),
                });
                if let Some(item_id) = item_id {
                    item["id"] = item_id.into();
                }
                if same_model {
                    if let Some(namespace) = block.get("namespace").filter(|value| !value.is_null())
                    {
                        item["namespace"] = namespace.clone();
                    }
                }
                output.push(item);
            }
            _ => {}
        }
    }
    Ok(output)
}

fn tool_result_output(model: &Value, content: &Value) -> Result<Value, String> {
    let result_text = content_text(content)?;
    let accepts_images = model["input"]
        .as_array()
        .is_some_and(|input| input.iter().any(|item| item == "image"));
    let images: Vec<&Value> = if content.is_array() {
        array(content, "content")?
            .iter()
            .filter(|block| block["type"] == "image")
            .collect()
    } else {
        Vec::new()
    };
    if images.is_empty() || !accepts_images {
        return Ok(if result_text.trim().is_empty() {
            json!(if images.is_empty() {
                "(no tool output)"
            } else {
                "(see attached image)"
            })
        } else {
            json!(result_text)
        });
    }
    let mut output = Vec::new();
    if !result_text.trim().is_empty() {
        output.push(json!({"type": "input_text", "text": result_text}));
    }
    for image in images {
        output.push(json!({
            "type": "input_image",
            "detail": "auto",
            "image_url": format!("data:{};base64,{}", text(&image["mimeType"], "image.mimeType")?, text(&image["data"], "image.data")?),
        }));
    }
    Ok(json!(output))
}

/// The tools of one system message's `toolsAdded`, as Responses function tools.
fn convert_tools(tools: &[Value]) -> Result<Vec<Value>, String> {
    tools
        .iter()
        .map(|tool| {
            let mut function = Map::new();
            function.insert("type".into(), "function".into());
            function.insert("name".into(), text(&tool["name"], "tool.name")?.into());
            function.insert(
                "description".into(),
                text(&tool["description"], "tool.description")?.into(),
            );
            if let Some(parameters) = tool.get("parameters") {
                function.insert("parameters".into(), parameters.clone());
            }
            // Codex accepts strict null; grammar tools are not ported.
            function.insert("strict".into(), Value::Null);
            Ok(Value::Object(function))
        })
        .collect()
}

/// System messages after the first become developer-role items; tool changes they carry
/// become `additional_tools` items (the Codex models declare support for both).
fn system_items(
    message: &Value,
    unavailable: &mut Vec<String>,
    role: &str,
) -> Result<Vec<Value>, String> {
    let mut items = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    let content = content_text(&message["content"])?;
    if !content.is_empty() {
        parts.push(content);
    }
    for (_, value) in entries(&message["sections"]) {
        if let Some(text) = value.as_str() {
            if !text.is_empty() {
                parts.push(text.to_owned());
            }
        }
    }
    let added = message["toolsAdded"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for tool in added {
        let name = text(&tool["name"], "tool.name")?.to_owned();
        unavailable.retain(|known| *known != name);
    }
    let removed: Vec<String> = message["toolsRemoved"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .map(|tool| Ok(text(&tool["name"], "tool.name")?.to_owned()))
        .collect::<Result<_, String>>()?;
    let added_names: std::collections::HashSet<_> = added
        .iter()
        .map(|tool| text(&tool["name"], "tool.name").map(str::to_owned))
        .collect::<Result<_, String>>()?;
    let removed: Vec<String> = removed
        .into_iter()
        .filter(|name| !added_names.contains(name))
        .collect();
    if !removed.is_empty() {
        parts.push(format!(
            "The following tools are no longer available. Do not call them:\n{}",
            removed
                .iter()
                .map(|name| format!("- {name}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
        unavailable.extend(removed);
    }
    if !added.is_empty() {
        items.push(json!({
            "type": "additional_tools",
            "role": role,
            "tools": convert_tools(added)?,
        }));
    }
    let joined = parts.join("\n\n");
    if !joined.is_empty() {
        items.push(json!({"role": role, "content": joined}));
    }
    Ok(items)
}

/// The instruction role for system-derived items: reasoning models speak `developer`,
/// matching pi-ai's `instructionRole` rule.
fn instruction_role(model: &Value) -> &'static str {
    if model["reasoning"].as_bool().unwrap_or(false) {
        "developer"
    } else {
        "system"
    }
}

/// Builds the Responses request body. `reasoning` is the effective thinking level
/// (`None`/`Off` means off); `session_id` feeds `prompt_cache_key`.
pub fn build_request_body(
    model: &Value,
    context: &Value,
    reasoning: Option<ThinkingLevel>,
    session_id: Option<&str>,
    temperature: Option<f64>,
) -> Result<Value, String> {
    let messages = array(&context["messages"], "messages")?;
    let mut input: Vec<Value> = Vec::new();
    let mut initial_tools: Vec<Value> = Vec::new();
    if !context["tools"].is_null() {
        initial_tools = convert_tools(array(&context["tools"], "tools")?)?;
    }
    let mut instructions: Option<String> = None;
    let mut unavailable: Vec<String> = Vec::new();
    let mut msg_index = 0;
    let mut system_seen = false;
    let mut pending_calls: Vec<String> = Vec::new();
    for message in messages {
        if message["role"] != "toolResult" {
            for call_id in pending_calls.drain(..) {
                input.push(json!({"type": "function_call_output", "call_id": call_id,
                    "output": "No result provided"}));
            }
        }
        match message["role"].as_str() {
            Some("system") => {
                if !system_seen {
                    system_seen = true;
                    instructions = Some(content_text(&message["content"])?);
                    if instructions.as_deref().is_some_and(str::is_empty) {
                        instructions = None;
                    }
                    // The leading system message also declares tools.
                    if !message["toolsAdded"].is_null() {
                        initial_tools =
                            convert_tools(array(&message["toolsAdded"], "toolsAdded")?)?;
                    }
                } else {
                    input.extend(system_items(
                        message,
                        &mut unavailable,
                        instruction_role(model),
                    )?);
                }
            }
            Some("user") => {
                let content = user_content(&message["content"])?;
                if content.is_empty() {
                    continue;
                }
                input.push(json!({"role": "user", "content": content}));
                msg_index += 1;
            }
            Some("assistant") => {
                if matches!(message["stopReason"].as_str(), Some("error" | "aborted")) {
                    continue;
                }
                let output = assistant_output(message, model, msg_index)?;
                for item in &output {
                    if item["type"] == "function_call" {
                        if let Some(id) = item["call_id"].as_str() {
                            pending_calls.push(id.to_owned());
                        }
                    }
                }
                if output.is_empty() {
                    continue;
                }
                input.extend(output);
                msg_index += 1;
            }
            Some("toolResult") => {
                let (call_id, _) = tool_call_id(
                    text(&message["toolCallId"], "toolResult.toolCallId")?,
                    false,
                );
                if !pending_calls.contains(&call_id) {
                    continue;
                }
                pending_calls.retain(|id| id != &call_id);
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": tool_result_output(model, &message["content"])?,
                }));
            }
            _ => {}
        }
    }
    for call_id in pending_calls {
        input.push(json!({"type": "function_call_output", "call_id": call_id,
            "output": "No result provided"}));
    }
    let mut body = Map::new();
    body.insert("model".into(), model["id"].clone());
    body.insert("store".into(), false.into());
    body.insert("stream".into(), true.into());
    body.insert(
        "instructions".into(),
        instructions
            .filter(|text| !trim(text).is_empty())
            .unwrap_or_else(|| DEFAULT_INSTRUCTIONS.into())
            .into(),
    );
    body.insert("input".into(), input.into());
    body.insert("text".into(), json!({"verbosity": "low"}));
    body.insert("include".into(), json!(["reasoning.encrypted_content"]));
    body.insert("tool_choice".into(), "auto".into());
    body.insert("parallel_tool_calls".into(), true.into());
    if let Some(session_id) = session_id.filter(|id| !id.is_empty()) {
        body.insert("prompt_cache_key".into(), session_id.into());
    }
    if !initial_tools.is_empty() {
        body.insert("tools".into(), initial_tools.into());
    }
    if let Some(temperature) = temperature {
        body.insert("temperature".into(), temperature.into());
    }
    // Reasoning effort: the request level through the model's map; off falls back to
    // the model's `off` entry when it maps to an effort, or `none` when unmapped.
    // A model whose `off` entry is an explicit null omits `reasoning` entirely.
    let effort = match reasoning {
        None | Some(ThinkingLevel::Off) => {
            if !super::catalog::off_maps_to_effort(model) {
                None
            } else {
                match super::catalog::mapped_effort(model, ThinkingLevel::Off) {
                    Some(Some(effort)) => Some(effort),
                    _ => Some("none".into()),
                }
            }
        }
        Some(level) => match super::catalog::mapped_effort(model, level) {
            Some(Some(effort)) => Some(effort),
            Some(None) => None,
            None => Some(
                serde_json::to_value(level)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "none".into()),
            ),
        },
    };
    if let Some(effort) = effort {
        body.insert(
            "reasoning".into(),
            json!({"effort": effort, "summary": "auto"}),
        );
    }
    Ok(Value::Object(body))
}
