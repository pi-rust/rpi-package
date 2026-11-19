//! The pinned OpenAI Codex catalog: the default ChatGPT Plus / Team models, in Pi's order.
use rpi_ai::{types::ThinkingLevel, Api, InputModality, Model, ModelCost, ThinkingLevelMap};
use serde::Deserialize;
use serde_json::Value;
use std::sync::OnceLock;

/// Pi's registered base URL. Requests append `/codex/responses`, as Pi's stream does.
pub const BASE_URL: &str = "https://chatgpt.com/backend-api";
pub const API: &str = "openai-codex-responses";

#[derive(Deserialize)]
struct Data {
    models: Vec<Entry>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    id: String,
    name: String,
    reasoning: bool,
    input: Vec<String>,
    cost: ModelCost,
    context_window: u64,
    max_tokens: u64,
    #[serde(default)]
    thinking_level_map: Option<ThinkingLevelMap>,
}
fn data() -> &'static Data {
    static DATA: OnceLock<Data> = OnceLock::new();
    DATA.get_or_init(|| serde_json::from_str(include_str!("catalog.json")).expect("pinned catalog"))
}

/// Registration catalog: the default entries as host models, in Pi's order.
pub(crate) fn models(provider: &str) -> Vec<Model> {
    data()
        .models
        .iter()
        .map(|entry| {
            let mut model = Model::new(
                entry.id.clone(),
                entry.name.clone(),
                Api::OpenaiCodexResponses,
                provider,
                BASE_URL,
            );
            model.reasoning = entry.reasoning;
            model.input = entry
                .input
                .iter()
                .map(|value| match value.as_str() {
                    "image" => InputModality::Image,
                    _ => InputModality::Text,
                })
                .collect();
            model.cost = entry.cost.clone();
            model.cost_configured = true;
            model.context_window = entry.context_window;
            model.max_tokens = entry.max_tokens;
            model.thinking_level_map = entry.thinking_level_map.clone();
            model
        })
        .collect()
}

/// The reasoning effort for a request level, or `None` when the model disables
/// reasoning for that level (its map entry is an explicit null).
pub(crate) fn mapped_effort(model: &Value, level: ThinkingLevel) -> Option<Option<String>> {
    let key = serde_json::to_value(level).ok()?;
    let key = key.as_str()?.to_owned();
    let entry = model["thinkingLevelMap"].as_object()?.get(&key)?.clone();
    if entry.is_null() {
        return Some(None);
    }
    Some(Some(entry.as_str().unwrap_or_default().to_owned()))
}
/// Whether the model maps the `off` level to a string effort; models with an
/// explicit null omit `reasoning` entirely when thinking is off.
pub(crate) fn off_maps_to_effort(model: &Value) -> bool {
    model["reasoning"].as_bool().unwrap_or(false)
        && matches!(
            mapped_effort(model, ThinkingLevel::Off),
            Some(Some(_)) | None
        )
}
