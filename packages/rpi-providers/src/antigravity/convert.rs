//! Pi transcript projection, including Pi 1.3.0's ordered system-message deltas.
use super::json::{entries, set, trim, truthy};
use super::schema::{self, Options as SchemaOptions};
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Options {
    pub provider: Option<String>,
    pub model: Option<String>,
}

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
    array(content, "content")?
        .iter()
        .filter(|block| block["type"] == "text")
        .map(|block| text(&block["text"], "text"))
        .collect::<Result<Vec<_>, _>>()
        .map(|parts| parts.join("\n"))
}
fn same_target(message: &Value, options: &Options) -> bool {
    match (&options.provider, &options.model) {
        (Some(provider), Some(model)) if !provider.is_empty() && !model.is_empty() => {
            message["provider"] == *provider && message["model"] == *model
        }
        _ => true,
    }
}
fn signature(part: &mut Value, block: &Value, field: &str, preserve: bool) {
    if preserve && truthy(&block[field]) {
        part["thoughtSignature"] = block[field].clone();
    }
}
fn assistant_parts(message: &Value, preserve: bool) -> Result<Vec<Value>, String> {
    let mut parts = Vec::new();
    for block in array(&message["content"], "assistant.content")? {
        let part = match block["type"].as_str() {
            Some("thinking") if preserve && truthy(&block["thinking"]) => {
                let mut part =
                    json!({"text": text(&block["thinking"], "thinking")?, "thought": true});
                signature(&mut part, block, "thinkingSignature", true);
                part
            }
            Some("text") => {
                let value = text(&block["text"], "text")?;
                if trim(value).is_empty() {
                    continue;
                }
                let mut part = json!({"text": value});
                signature(&mut part, block, "textSignature", preserve);
                part
            }
            Some("toolCall") => {
                let mut part = json!({"functionCall": {
                    "name": text(&block["name"], "toolCall.name")?,
                    "args": if block["arguments"].is_null() { json!({}) } else { block["arguments"].clone() },
                    "id": text(&block["id"], "toolCall.id")?
                }});
                signature(&mut part, block, "thoughtSignature", preserve);
                part
            }
            _ => continue,
        };
        parts.push(part);
    }
    Ok(parts)
}
fn user_parts(content: &Value) -> Result<Vec<Value>, String> {
    if let Some(text) = content.as_str() {
        return Ok(if trim(text).is_empty() {
            vec![]
        } else {
            vec![json!({"text": text})]
        });
    }
    let mut parts = Vec::new();
    for block in array(content, "user.content")? {
        if block["type"] == "text" && truthy(&block["text"]) {
            parts.push(json!({"text": text(&block["text"], "text")?}));
        } else if block["type"] == "image" && truthy(&block["data"]) {
            parts.push(json!({"inlineData": {
                "mimeType": text(&block["mimeType"], "image.mimeType")?,
                "data": text(&block["data"], "image.data")?
            }}));
        }
    }
    Ok(parts)
}
fn messages(messages: &[Value], options: &Options) -> Result<Vec<Value>, String> {
    let mut calls = HashMap::new();
    for message in messages
        .iter()
        .filter(|message| message["role"] == "assistant")
    {
        for block in array(&message["content"], "assistant.content")? {
            if block["type"] == "toolCall" {
                calls.insert(
                    text(&block["id"], "toolCall.id")?,
                    same_target(message, options),
                );
            }
        }
    }
    let mut contents: Vec<Value> = Vec::new();
    for message in messages {
        let (role, parts) = match message["role"].as_str() {
            Some("user") => ("user", user_parts(&message["content"])?),
            Some("assistant") => (
                "model",
                assistant_parts(message, same_target(message, options))?,
            ),
            Some("toolResult") => {
                let id = text(&message["toolCallId"], "toolResult.toolCallId")?;
                let role = if calls.get(id) == Some(&true) {
                    "model"
                } else {
                    "user"
                };
                let output = content_text(&message["content"])?;
                let response = if truthy(&message["isError"]) {
                    json!({"error": if output.is_empty() { "Error" } else { &output }})
                } else {
                    json!({"output": output})
                };
                let part = json!({"functionResponse": {
                    "name": text(&message["toolName"], "toolResult.toolName")?,
                    "response": response, "id": id
                }});
                if let Some(last) = contents.last_mut() {
                    if last["role"] == role
                        && last["parts"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|part| part.get("functionResponse").is_some())
                    {
                        last["parts"].as_array_mut().unwrap().push(part);
                        continue;
                    }
                }
                (role, vec![part])
            }
            _ => continue,
        };
        if !parts.is_empty() {
            contents.push(json!({"role": role, "parts": parts}));
        }
    }
    Ok(contents)
}
fn instruction_state(context: &Value, messages: &[Value]) -> Result<(String, Vec<Value>), String> {
    if !messages.iter().any(|message| message["role"] == "system") {
        let prompt = if context["systemPrompt"].is_null() {
            ""
        } else {
            text(&context["systemPrompt"], "systemPrompt")?
        };
        let tools = if context["tools"].is_null() {
            vec![]
        } else {
            array(&context["tools"], "tools")?.to_vec()
        };
        return Ok((prompt.into(), tools));
    }
    let mut content = Vec::new();
    let mut sections = IndexMap::new();
    let mut tools = IndexMap::new();
    let mut timestamp = None;
    for message in messages
        .iter()
        .filter(|message| message["role"] == "system")
    {
        if timestamp.is_none() || timestamp == Some(&Value::Null) {
            timestamp = message.get("timestamp");
        }
        let value = content_text(&message["content"])?;
        if !value.is_empty() {
            content.push(value);
        }
        for (name, value) in entries(&message["sections"]) {
            if value.is_null() {
                sections.shift_remove(&name);
            } else {
                sections.insert(name, text(value, "sections")?.to_owned());
            }
        }
        if !message["toolsRemoved"].is_null() {
            for tool in array(&message["toolsRemoved"], "toolsRemoved")? {
                tools.shift_remove(text(&tool["name"], "tool.name")?);
            }
        }
        if !message["toolsAdded"].is_null() {
            for tool in array(&message["toolsAdded"], "toolsAdded")? {
                tools.insert(text(&tool["name"], "tool.name")?.to_owned(), tool.clone());
            }
        }
    }
    let prompt = if timestamp.is_none() && tools.is_empty() {
        String::new()
    } else {
        // Pi turns its section Map back into an object, restoring integer-key order.
        let sections = Value::Object(
            sections
                .into_iter()
                .map(|(name, value)| (name, Value::String(value)))
                .collect(),
        );
        content.extend(entries(&sections).into_iter().filter_map(|(_, value)| {
            value
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        }));
        content.join("\n\n")
    };
    Ok((prompt, tools.into_values().collect()))
}

pub fn build_gemini_request(context: &Value, options: Options) -> Result<Value, String> {
    let transcript = array(&context["messages"], "messages")?;
    let mut request = Map::new();
    set(
        &mut request,
        "contents",
        Value::Array(messages(transcript, &options)?),
    );
    let (prompt, tools) = instruction_state(context, transcript)?;
    if !tools.is_empty() {
        let schema_options = SchemaOptions {
            move_numeric_constraints_to_description: options.model.as_deref().is_some_and(
                |model| {
                    model
                        .strip_prefix("antigravity-")
                        .unwrap_or(model)
                        .starts_with("gpt-oss-")
                },
            ),
            claude_validated: false,
        };
        let declarations = tools
            .iter()
            .map(|tool| {
                let mut declaration = Map::new();
                set(&mut declaration, "name", text(&tool["name"], "tool.name")?);
                set(
                    &mut declaration,
                    "description",
                    text(&tool["description"], "tool.description")?,
                );
                if let Some(parameters) = tool.get("parameters") {
                    set(
                        &mut declaration,
                        "parameters",
                        schema::to_gemini_schema(parameters, schema_options),
                    );
                }
                Ok(Value::Object(declaration))
            })
            .collect::<Result<Vec<_>, String>>()?;
        set(
            &mut request,
            "tools",
            json!([{"functionDeclarations": declarations}]),
        );
    }
    if !trim(&prompt).is_empty() {
        set(
            &mut request,
            "systemInstruction",
            json!({"parts": [{"text": prompt}]}),
        );
    }
    Ok(Value::Object(request))
}
