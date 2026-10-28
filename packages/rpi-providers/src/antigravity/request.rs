//! Build once per logical request; credentials enter only the physical-send envelope.
use super::{
    catalog, convert,
    json::describe,
    metadata::{self, SessionStore},
};
use serde_json::{json, Value};

#[derive(Clone)]
pub struct PreparedRequest {
    pub wire_model: String,
    pub request_id: String,
    pub request: Value,
}
impl PreparedRequest {
    pub fn body(&self, project_id: &str) -> String {
        describe(
            &json!({"project":project_id,"requestId":self.request_id,"request":self.request,
            "model":self.wire_model,"userAgent":"antigravity","requestType":"agent"}),
        )
    }
}
pub fn prepare(
    model: &Value,
    context: &Value,
    options: &Value,
    sessions: &mut SessionStore,
    now: i64,
    next_id: &mut impl FnMut() -> String,
) -> Result<PreparedRequest, String> {
    let id = model["id"].as_str().ok_or("Invalid Antigravity model ID")?;
    let resolved = catalog::resolve_pi(
        id,
        model["reasoning"].as_bool().unwrap_or(false),
        options["reasoning"].as_str(),
    );
    if catalog::is_retired(id) || catalog::is_retired(&resolved.actual_model) {
        return Err(catalog::RETIRED_MESSAGE.into());
    }
    let mut request = convert::build_gemini_request(
        context,
        convert::Options {
            provider: model["provider"].as_str().map(str::to_owned),
            model: Some(id.into()),
        },
    )?;
    let mut generation = json!({});
    if let Some(level) = resolved.thinking_level {
        generation["thinkingConfig"] = json!({"includeThoughts":true,"thinkingLevel":level});
    } else if let Some(budget) = resolved.thinking_budget {
        generation["thinkingConfig"] = json!({"includeThoughts":true,"thinkingBudget":budget});
    }
    let max_tokens = if options["maxTokens"].is_null() {
        &model["maxTokens"]
    } else {
        &options["maxTokens"]
    };
    if max_tokens.is_number() {
        generation["maxOutputTokens"] = max_tokens.clone();
    }
    metadata::apply_generation(&resolved.actual_model, &mut generation);
    if !generation.as_object().unwrap().is_empty() {
        request["generationConfig"] = generation;
    }
    let key = metadata::session_key(context, options);
    let (session, timestamp) = sessions.begin_request(&key, now, next_id);
    let request_id = metadata::finalize(&mut request, &resolved.actual_model, session, timestamp);
    Ok(PreparedRequest {
        wire_model: resolved.actual_model,
        request_id,
        request,
    })
}
