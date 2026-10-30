//! Bounded catalog operations sharing the runtime's instance-owned sessions.
use super::*;
use crate::prompts::validate_identifier;
use crate::{PromptArguments, PromptListResult, PromptResult, ResourceTemplateListResult};
use serde::de::DeserializeOwned;

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_ITEMS: usize = 16_384;

fn bounded_result(value: &Value, limit: usize) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?.len();
    if bytes > limit {
        return Err(format!("MCP catalog response exceeded {limit} byte limit"));
    }
    Ok(())
}
fn byte_limit(params: &Value) -> Result<usize, String> {
    match params.get("maxResponseBytes") {
        None => Ok(1024 * 1024),
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=MAX_BYTES as u64).contains(n))
            .map(|n| n as usize)
            .ok_or_else(|| "maxResponseBytes must be 1..16777216".into()),
    }
}
fn decode<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|e| format!("invalid MCP catalog result: {e}"))
}
fn request(
    session: &mut Session,
    method: &str,
    params: Value,
    deadline: &Deadline,
) -> Result<Value, String> {
    deadline.check()?;
    // Each transport request uses the remainder, not a fresh per-page timeout.
    let remaining = deadline
        .end
        .saturating_duration_since(std::time::Instant::now());
    session
        .transport
        .timeout(remaining.as_secs().saturating_add(1).clamp(1, 300));
    let value = response_result(session.request(method, params, &deadline.control)?)?;
    deadline.check()?;
    Ok(value)
}

impl RuntimeState {
    /// Initialize a configured target without probing unsupported catalog methods.
    pub fn server_info(
        &self,
        context: &RequestContext,
        params: &Value,
        control: &Control,
    ) -> Result<ServerInfo, String> {
        let (key, _, config, timeout) = self.target(context, params)?;
        self.with_session(key, config, timeout, control, None, |session, _, _| {
            Ok(session.info.clone())
        })
    }

    /// One bounded page, retaining metadata and nextCursor.
    pub fn resources_list_page(
        &self,
        context: &RequestContext,
        params: &Value,
        control: &Control,
    ) -> Result<Value, String> {
        self.catalog_list(context, params, control, "resources/list", "resources")
    }

    /// Lossless resource read sharing one deadline across admission and request.
    pub fn resources_read_bounded(
        &self,
        context: &RequestContext,
        params: &Value,
        control: &Control,
    ) -> Result<Value, String> {
        control.check()?;
        let uri = string_param(params, "uri")?;
        if uri.trim().is_empty() || uri.len() > 8192 || uri.chars().any(char::is_control) {
            return Err(
                "resource URI must be nonblank, at most 8192 bytes and contain no controls".into(),
            );
        }
        let limit = byte_limit(params)?;
        let (key, _, config, timeout) = self.target(context, params)?;
        let deadline = Deadline::new(timeout, "resources/read", control);
        self.with_session(
            key,
            config,
            timeout,
            control,
            None,
            |session, control, _| {
                let deadline = Deadline {
                    control: control.clone(),
                    ..deadline
                };
                let result = request(session, "resources/read", json!({"uri":uri}), &deadline)?;
                bounded_result(&result, limit)?;
                if !result.get("contents").is_some_and(Value::is_array) {
                    return Err("MCP resources/read requires contents array".into());
                }
                Ok(result)
            },
        )
    }

    /// One page by default; `cursor` resumes and `maxPages` (1..100) opts into
    /// bounded aggregation. Aggregated raw pages are retained in `pageResults`
    /// so per-page unknown fields/metadata are never silently overwritten.
    fn catalog_list(
        &self,
        context: &RequestContext,
        params: &Value,
        control: &Control,
        method: &str,
        field: &str,
    ) -> Result<Value, String> {
        control.check()?;
        let limit = byte_limit(params)?;
        let max_pages = match params.get("maxPages") {
            None => 1,
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=100).contains(n))
                .ok_or("maxPages must be 1..100")?,
        };
        let mut cursor = match params.get("cursor") {
            None => None,
            Some(Value::String(s)) if s.len() <= 8192 => Some(s.clone()),
            _ => return Err("cursor must be a string of at most 8192 bytes".into()),
        };
        let mut seen = HashSet::new();
        if let Some(cursor) = &cursor {
            seen.insert(cursor.clone());
        }
        let (key, _, config, timeout) = self.target(context, params)?;
        let deadline = Deadline::new(timeout, method, control);
        self.with_session(
            key,
            config,
            timeout,
            control,
            None,
            |session, control, _| {
                let deadline = Deadline {
                    control: control.clone(),
                    ..deadline
                };
                let mut items = Vec::new();
                let mut pages = Vec::new();
                let mut total_bytes = 0;
                for _ in 0..max_pages {
                    let wire = cursor
                        .as_ref()
                        .map(|c| json!({"cursor":c}))
                        .unwrap_or_else(|| json!({}));
                    let result = request(session, method, wire, &deadline)?;
                    bounded_result(&result, limit.saturating_sub(total_bytes))?;
                    total_bytes += serde_json::to_vec(&result)
                        .map_err(|e| e.to_string())?
                        .len();
                    let page_items = result
                        .get(field)
                        .and_then(Value::as_array)
                        .ok_or_else(|| format!("MCP {method} requires {field} array"))?;
                    if items.len() + page_items.len() > MAX_ITEMS {
                        return Err("MCP catalog exceeded 16384 items".into());
                    }
                    cursor = match result.get("nextCursor") {
                        None => None,
                        Some(Value::String(s)) if s.len() <= 8192 => {
                            if !seen.insert(s.clone()) {
                                return Err(format!(
                                    "MCP {method} returned a repeated pagination cursor"
                                ));
                            }
                            Some(s.clone())
                        }
                        _ => {
                            return Err(
                                "MCP nextCursor must be a string of at most 8192 bytes".into()
                            )
                        }
                    };
                    items.extend(page_items.iter().cloned());
                    pages.push(result);
                    if cursor.is_none() {
                        break;
                    }
                }
                let mut result = pages[0].clone();
                result[field] = Value::Array(items);
                let object = result
                    .as_object_mut()
                    .ok_or("MCP list result must be an object")?;
                object.remove("nextCursor");
                if let Some(cursor) = cursor {
                    object.insert("nextCursor".into(), cursor.into());
                }
                if pages.len() > 1 {
                    if object.contains_key("pageResults") {
                        return Err("MCP result uses reserved aggregation field pageResults".into());
                    }
                    object.insert("pageResults".into(), Value::Array(pages));
                }
                bounded_result(&result, limit)?;
                Ok(result)
            },
        )
    }

    pub fn prompts_list(
        &self,
        context: &RequestContext,
        params: &Value,
        control: &Control,
    ) -> Result<PromptListResult, String> {
        let result: PromptListResult =
            decode(self.catalog_list(context, params, control, "prompts/list", "prompts")?)?;
        for prompt in &result.prompts {
            validate_identifier(&prompt.name)?;
            if let Some(arguments) = &prompt.arguments {
                for argument in arguments {
                    validate_identifier(&argument.name)?;
                }
            }
        }
        Ok(result)
    }

    /// Fetches structured messages exactly once; never runs tools or flattens content.
    pub fn prompts_get(
        &self,
        context: &RequestContext,
        params: &Value,
        control: &Control,
    ) -> Result<PromptResult, String> {
        control.check()?;
        let name = string_param(params, "name")?;
        validate_identifier(&name)?;
        let arguments: PromptArguments = decode(
            params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )?;
        for name in arguments.keys() {
            validate_identifier(name)?;
        }
        let limit = byte_limit(params)?;
        let (key, _, config, timeout) = self.target(context, params)?;
        let deadline = Deadline::new(timeout, "prompts/get", control);
        self.with_session(
            key,
            config,
            timeout,
            control,
            None,
            |session, control, _| {
                let deadline = Deadline {
                    control: control.clone(),
                    ..deadline
                };
                let result = request(
                    session,
                    "prompts/get",
                    json!({"name":name,"arguments":arguments}),
                    &deadline,
                )?;
                bounded_result(&result, limit)?;
                decode(result)
            },
        )
    }

    pub fn resource_templates_list(
        &self,
        context: &RequestContext,
        params: &Value,
        control: &Control,
    ) -> Result<ResourceTemplateListResult, String> {
        let result: ResourceTemplateListResult = decode(self.catalog_list(
            context,
            params,
            control,
            "resources/templates/list",
            "resourceTemplates",
        )?)?;
        for template in &result.resource_templates {
            validate_identifier(&template.name)?;
        }
        Ok(result)
    }

    /// JSON dispatch matching `resources`; the adapter need not participate.
    pub fn prompts(
        &self,
        context: &RequestContext,
        params: &Value,
        control: &Control,
    ) -> Result<Value, String> {
        match params.get("action") {
            None => serde_json::to_value(self.prompts_list(context, params, control)?),
            Some(Value::String(action)) if action == "list" => {
                serde_json::to_value(self.prompts_list(context, params, control)?)
            }
            Some(Value::String(action)) if action == "get" => {
                serde_json::to_value(self.prompts_get(context, params, control)?)
            }
            _ => return Err("mcp_prompt action must be list or get".into()),
        }
        .map_err(|e| e.to_string())
    }
}
