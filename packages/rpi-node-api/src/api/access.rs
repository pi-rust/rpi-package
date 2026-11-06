//! Effective operations observed from one bound session, not authorization or
//! proof that a provider, package, extension or resource is ready/loaded.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAccess {
    pub descriptor: crate::AgentDescriptor,
    pub supported_commands: Vec<String>,
    pub snapshot_limit: bool,
    pub dialog_reply: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_content: Option<serde_json::Value>,
}
impl SessionAccess {
    /// Complete fixed native v1 profile; an absent/partial advertisement fails closed.
    #[must_use]
    pub fn supports_prompt_content(&self) -> bool {
        let Some(p) = self
            .prompt_content
            .as_ref()
            .and_then(serde_json::Value::as_object)
        else {
            return false;
        };
        let number = |key: &str| p.get(key).and_then(serde_json::Value::as_u64);
        let mimes = p
            .get("imageMimeTypes")
            .and_then(serde_json::Value::as_array);
        p.get("image").and_then(serde_json::Value::as_bool) == Some(true)
            && mimes.is_some_and(|m| {
                m.len() == 2
                    && m.iter().any(|v| v == "image/png")
                    && m.iter().any(|v| v == "image/jpeg")
            })
            && [
                ("contentBlocksVersion", 1),
                ("maxImages", 4),
                ("maxImageBytes", 262144),
                ("maxTotalImageBytes", 524288),
                ("maxImageBase64Bytes", 349528),
                ("maxPromptBytes", 786432),
                ("maxTextBytes", 32768),
                ("maxContentBlocks", 64),
                ("maxImageSide", 2000),
                ("maxImagePixels", 4000000),
                ("imageDecodeAllocBudgetBytes", 33554432),
            ]
            .iter()
            .all(|(key, expected)| number(key) == Some(*expected))
            && number("frameBytes").is_some_and(|n| n >= 786432)
    }

    #[must_use]
    pub fn supports_command(&self, command: &str) -> bool {
        self.supported_commands.iter().any(|item| item == command)
    }
}
