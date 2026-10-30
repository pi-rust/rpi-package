//! Lossless MCP 2025-06-18 prompt and resource-template data.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub type PromptArguments = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptArgument {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptDescriptor {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<PromptArgument>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Content remains structured: image/audio data, embedded resources, annotations,
/// model hints and future content fields are retained, never converted to text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptContent {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptMessage {
    /// Kept as a string to retain future roles without silently changing them.
    pub role: String,
    pub content: PromptContent,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub messages: Vec<PromptMessage>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTemplate {
    pub uri_template: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptListResult {
    pub prompts: Vec<PromptDescriptor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTemplateListResult {
    pub resource_templates: Vec<ResourceTemplate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Identifiers are opaque server names, not paths or executable instructions.
/// Allow Unicode and punctuation, but reject blank, control and oversized input.
pub(crate) fn validate_identifier(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.len() > 200 || name.chars().any(char::is_control) {
        return Err(
            "MCP identifier must be nonblank, at most 200 bytes and contain no controls".into(),
        );
    }
    Ok(())
}
