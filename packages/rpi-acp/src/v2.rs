//! Explicit ACP v2 baseline mapping.
//!
//! v2 is negotiated independently from the stable v1 wire shape. This module
//! owns the v2 initialize envelope and the small set of v2 auth/state helpers;
//! it intentionally does not reinterpret v1 capability fields as v2 without
//! an explicit conversion.

use serde_json::{json, Value};

pub const PROTOCOL_VERSION: u64 = 2;

pub fn valid_initialize_params(params: &Value) -> Result<(), String> {
    if !requested_version(params).is_some_and(|v| (PROTOCOL_VERSION..=65535).contains(&v)) {
        return Err("ACP v2 initialize requires a uint16 protocolVersion >= 2".to_owned());
    }
    let Some(info) = params.get("info").filter(|value| value.is_object()) else {
        return Err("ACP v2 initialize requires an object `info`".to_owned());
    };
    if !info.get("name").is_some_and(Value::is_string)
        || !info.get("version").is_some_and(Value::is_string)
    {
        return Err("ACP v2 initialize `info` requires string name and version".to_owned());
    }
    if let Some(capabilities) = params.get("capabilities") {
        if !capabilities.is_object() {
            return Err("ACP v2 initialize `capabilities` must be an object".to_owned());
        }
        if let Some(auth) = capabilities.get("auth") {
            if !auth.is_object() && !auth.is_null() {
                return Err("ACP v2 client capabilities.auth must be an object or null".to_owned());
            }
        }
        if let Some(elicitation) = capabilities.get("elicitation") {
            if !elicitation.is_object() && !elicitation.is_null() {
                return Err(
                    "ACP v2 client capabilities.elicitation must be an object or null".to_owned(),
                );
            }
        }
        for pointer in ["/auth/terminal", "/elicitation/form", "/elicitation/url"] {
            if let Some(value) = capabilities.pointer(pointer) {
                if !value.is_object() && !value.is_null() {
                    return Err(format!(
                        "ACP v2 capability {pointer} must be an object or null"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Validate v2 setup against the pinned transport shapes; replay remains separate.
pub fn valid_session_setup(params: &Value) -> Result<(), String> {
    valid_session_setup_with_extensions(params, false)
}

/// Only the revision-pinned session RFD opt-in enables unstable setup fields.
pub fn valid_session_setup_with_extensions(params: &Value, enabled: bool) -> Result<(), String> {
    params.as_object().ok_or_else(|| "session setup params must be an object".to_owned())?;
    crate::system_prompt::parse(params, enabled)?;
    crate::mcp::parse(params, true, enabled)?;
    replay_from(params)?;
    Ok(())
}

/// Parse the inclusive v2 replay cursor. `None` means resume without replay;
/// `Some(Start)` requests all retained history. Unknown cursors MUST NOT silently
/// degrade to either mode. Shared with the protocol/history adapter owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayFrom {
    Start,
}

pub fn replay_from(params: &Value) -> Result<Option<ReplayFrom>, String> {
    if !params.is_object() {
        return Err("session setup params must be an object".to_owned());
    }
    match params.get("replayFrom") {
        None | Some(Value::Null) => Ok(None),
        Some(value)
            if value.is_object() && value.get("type").and_then(Value::as_str) == Some("start") =>
        {
            Ok(Some(ReplayFrom::Start))
        }
        _ => Err("malformed or unsupported replayFrom cursor".to_owned()),
    }
}

/// v2's agent authentication methods are identified by `methodId`, and the
/// request must select a method advertised during initialization.
pub fn valid_login_params(params: &Value, method_id: &str) -> Result<(), String> {
    let object = params
        .as_object()
        .ok_or_else(|| "auth/login params must be an object".to_owned())?;
    if object.get("methodId").and_then(Value::as_str) != Some(method_id) {
        return Err("unsupported authentication method".to_owned());
    }
    Ok(())
}

/// Convert the stable v1 result into the independently shaped ACP v2
/// initialize result. v2 nests session and prompt capabilities rather than
/// copying v1's flat `agentCapabilities` keys.
pub fn initialize_result(v1: &Value) -> Value {
    let v1_capabilities = v1
        .get("agentCapabilities")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let prompt = json!({
        "image": {},
        "audio": {},
        "embeddedContext": {},
    });
    let mut session = json!({
        "prompt": prompt,
        "delete": {},
        "additionalDirectories": {},
        "mcp": {"stdio": {}, "http": {}},
    });
    if v1_capabilities.get("_meta").is_some() {
        session["_meta"] = v1_capabilities["_meta"].clone();
    }
    // Auth methods advertise login/logout, not capabilities.auth. We support
    // no auth extension, so do not infer one from the v1 auth-state surface.
    let capabilities = json!({"session": session});
    let auth_methods = v1
        .get("authMethods")
        .and_then(Value::as_array)
        .map(|methods| {
            methods
                .iter()
                .map(|method| {
                    let mut method = method.clone();
                    if let Some(id) = method.get("id").cloned() {
                        method["methodId"] = id;
                        if let Some(object) = method.as_object_mut() {
                            object.remove("id");
                        }
                    }
                    // v1 represents terminal environment overrides as an
                    // object; v2 uses the schema's ordered EnvVariable list.
                    if let Some(env) = method.get("env").and_then(Value::as_object).cloned() {
                        method["env"] = Value::Array(
                            env.into_iter()
                                .map(|(name, value)| json!({"name": name, "value": value}))
                                .collect(),
                        );
                    }
                    method
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut result = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "info": v1.get("agentInfo").cloned().unwrap_or_else(|| {
            json!({"name":"rpi", "title":"rpi", "version": env!("CARGO_PKG_VERSION")})
        }),
        "capabilities": capabilities,
        "authMethods": auth_methods,
    });
    if let Some(meta) = v1.get("_meta") {
        result["_meta"] = meta.clone();
    }
    result
}

/// Normalize v2's `capabilities` into the internal legacy negotiation view.
/// This is an adapter-only value; it is never exposed as a v2 response field.
pub fn legacy_capability_view(params: &Value) -> Value {
    let mut view = params.clone();
    // Never honor v1's flat terminal/meta opt-in from a v2 peer. Only the
    // explicit v2 auth.terminal object enables the terminal method.
    let capabilities = params
        .get("capabilities")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let terminal = capabilities
        .pointer("/auth/terminal")
        .is_some_and(Value::is_object);
    view["clientCapabilities"] = capabilities;
    view["clientCapabilities"]["auth"] = json!({"terminal": terminal});
    view["clientCapabilities"]["terminal"] = json!(false);
    view
}

pub fn login_result() -> Value {
    json!({})
}

pub fn logout_result() -> Value {
    json!({})
}

pub fn prompt_result(message_id: impl Into<String>) -> Value {
    json!({"messageId": message_id.into()})
}

/// v2 keeps the stable update discriminators but requires the v2 session
/// envelope. Reject malformed updates instead of silently emitting a v1 shape.
pub fn update_envelope(session_id: &str, update: Value) -> Result<Value, String> {
    if !update.is_object() {
        return Err("ACP v2 session update must be an object".to_owned());
    }
    if !update.get("sessionUpdate").is_some_and(Value::is_string) {
        return Err("ACP v2 session update requires a string discriminator".to_owned());
    }
    Ok(json!({"sessionId": session_id, "update": update}))
}

/// Map only the top-level ACP config control list, never arbitrary nested data.
/// v2 SessionConfigOption and SessionConfigSelectGroup use configId/groupId;
/// value IDs, categories, boolean payloads and metadata retain their meaning.
pub fn map_config_options(value: &mut Value) {
    let Some(options) = value.get_mut("configOptions").and_then(Value::as_array_mut) else {
        return;
    };
    for option in options {
        let Some(option) = option.as_object_mut() else {
            continue;
        };
        if let Some(id) = option.remove("id") {
            option.entry("configId").or_insert(id);
        }
        if option.get("type").and_then(Value::as_str) == Some("select") {
            if let Some(groups) = option.get_mut("options").and_then(Value::as_array_mut) {
                for group in groups {
                    if let Some(group) = group.as_object_mut() {
                        if group.contains_key("options") {
                            if let Some(id) = group.remove("group") {
                                group.entry("groupId").or_insert(id);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Adapt legacy native tool updates to the explicit v2 discriminator and
/// structured diff content. Text/message updates already use the same v2
/// envelope and are intentionally left untouched.
pub fn map_session_update(mut update: Value) -> Value {
    if update.get("sessionUpdate").and_then(Value::as_str) == Some("config_option_update") {
        map_config_options(&mut update);
    }
    if update.get("sessionUpdate").and_then(Value::as_str) == Some("available_commands_update") {
        if let Some(commands) = update.get_mut("availableCommands").and_then(Value::as_array_mut) {
            for command in commands {
                if let Some(command) = command.as_object_mut() {
                    command.entry("type").or_insert_with(|| json!("command"));
                    if let Some(input) = command.get_mut("input").and_then(Value::as_object_mut) {
                        input.entry("type").or_insert_with(|| json!("text"));
                    }
                }
            }
        }
    }
    if update.get("sessionUpdate").and_then(Value::as_str) == Some("tool_call") {
        update["sessionUpdate"] = Value::String("tool_call_update".to_owned());
    }
    if update.get("sessionUpdate").and_then(Value::as_str) == Some("plan") {
        let entries = update.get("entries").cloned().unwrap_or_else(|| json!([]));
        let mut mapped = json!({
            "sessionUpdate": "plan_update",
            "plan": {"type": "items", "planId": "rpi-plan", "entries": entries}
        });
        if let Some(meta) = update.get("_meta") {
            mapped["_meta"] = meta.clone();
        }
        return mapped;
    }
    if let Some(content) = update.get_mut("content").and_then(Value::as_array_mut) {
        for item in content {
            if item.get("type").and_then(Value::as_str) != Some("diff") {
                continue;
            }
            // Already-structured v2 diffs (including add/delete/move/copy and
            // real Git patches) must survive replay unchanged.
            if item.get("changes").is_some() {
                continue;
            }
            let Some(path) = item.get("path").and_then(Value::as_str) else {
                continue;
            };
            // Native edit's oldText/newText are replacement *fragments*, not
            // whole-file snapshots. Native write uses oldText:null even when
            // overwriting an existing file. Neither provides enough evidence
            // for a truthful whole-file patch or add/delete classification.
            // An empty replacement clears content; it does NOT delete a file.
            let operation = if item.get("oldText").is_some_and(Value::is_string) {
                "modify"
            } else {
                "_write"
            };
            let mut mapped = json!({
                "type": "diff",
                "changes": [{"operation": operation, "path": path}],
            });
            if let Some(meta) = item.get("_meta") {
                mapped["_meta"] = meta.clone();
            }
            // `patch` is optional in v2. Omit it rather than mislabel fragments
            // as git_patch (or fabricate context, paths, or newline markers).
            *item = mapped;
        }
    }
    update
}

/// Map native/v1 permission context only at the negotiated v2 writer boundary.
pub fn map_request(method: &str, mut params: Value) -> Value {
    if method == "session/request_permission" {
        if let Some(tool_call) = params.as_object_mut().and_then(|p| p.remove("toolCall")) {
            // session::permission_request derives this title from the actual UI question.
            params["title"] = tool_call["title"].clone();
            params["subject"] = json!({"type":"tool_call", "toolCall":tool_call});
        }
    }
    params
}

/// URI-only native resources are references, not embedded null text payloads.
pub fn prompt_resource(uri: &str, text: Option<&str>) -> Value {
    match text {
        Some(text) => json!({"type":"resource", "resource":{"uri":uri,"text":text}}),
        None => json!({"type":"resource_link", "uri":uri, "name":uri}),
    }
}

pub fn auth_method(method_id: &str, name: &str, description: &str) -> Value {
    json!({
        "methodId": method_id,
        "name": name,
        "description": description,
        "type": "terminal"
    })
}

pub fn is_v2_method(method: &str) -> bool {
    matches!(method, "auth/login" | "auth/logout")
}

pub fn requested_version(params: &Value) -> Option<u64> {
    params.get("protocolVersion").and_then(Value::as_u64)
}

pub fn is_v2(params: &Value) -> bool {
    requested_version(params).is_some_and(|version| version >= PROTOCOL_VERSION)
}

pub fn method_id(params: &Value) -> Option<&str> {
    params.get("methodId").and_then(Value::as_str)
}

pub fn empty_meta() -> Value {
    json!({})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_requires_info_and_uses_distinct_response_fields() {
        assert!(valid_initialize_params(&json!({})).is_err());
        let result = initialize_result(&json!({
            "agentInfo": {"name":"rpi"},
            "agentCapabilities": {"promptCapabilities":{"audio":true}},
            "authMethods": []
        }));
        assert_eq!(result["protocolVersion"], 2);
        assert_eq!(result["info"]["name"], "rpi");
        assert_eq!(
            result["capabilities"]["session"]["prompt"]["audio"],
            json!({})
        );
        assert_eq!(result["authMethods"], json!([]));
        assert_eq!(prompt_result("assistant-1")["messageId"], "assistant-1");
    }

    #[test]
    fn v2_session_setup_rejects_unadvertised_mcp_and_replay() {
        assert!(valid_session_setup(&json!({"mcpServers": []})).is_ok());
        assert!(valid_session_setup(&json!({
            "mcpServers": [{"type":"stdio","name":"tools","command":"/bin/tools"}]
        }))
        .is_err());
        assert!(valid_session_setup(&json!({"replayFrom":{"type":"start"}})).is_ok());
        let setup = json!({"mcpServers":[],"systemPrompt":{"mode":"append","content":[{"type":"text","text":"instructions"}]}});
        assert!(valid_session_setup(&setup).is_err());
        assert!(valid_session_setup_with_extensions(&setup, true).is_ok());
        assert!(valid_session_setup(&json!({"systemPrompt":null})).is_err());
    }

    #[test]
    fn v2_login_requires_the_advertised_method_id() {
        assert!(
            valid_login_params(&json!({"methodId":"rpi-credentials"}), "rpi-credentials").is_ok()
        );
        assert!(valid_login_params(&json!({"methodId":"other"}), "rpi-credentials").is_err());
    }
    #[test]
    fn v2_updates_use_tool_call_update_and_structured_diffs() {
        let update = map_session_update(json!({
            "sessionUpdate": "tool_call",
            "content": [{"type": "diff", "path": "C:/work/a.txt", "oldText": "", "newText": "hello"}]
        }));
        assert_eq!(update["sessionUpdate"], "tool_call_update");
        assert_eq!(update["content"][0]["changes"][0]["operation"], "modify");
        assert_eq!(update["content"][0]["changes"][0]["path"], "C:/work/a.txt");
        assert!(update["content"][0].get("patch").is_none());
    }

    #[test]
    fn v2_capabilities_are_adapted_only_for_internal_negotiation() {
        let view = legacy_capability_view(&json!({"capabilities":{"terminal":true}}));
        assert_eq!(view["clientCapabilities"]["terminal"], false);
        assert!(view.get("capabilities").is_some());
    }
}
