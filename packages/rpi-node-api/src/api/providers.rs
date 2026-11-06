//! Node-owned native provider configuration. Secrets are write-only; observations
//! describe saved files, never remote authentication or a live runtime reload.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderApi {
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    #[serde(rename = "openai-completions")]
    OpenaiCompletions,
    #[serde(rename = "openai-responses")]
    OpenaiResponses,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderModel {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub reasoning: bool,
    pub context_window: Option<u64>,
    pub max_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderDefinition {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub api: ProviderApi,
    pub base_url: String,
    pub models: Vec<ProviderModel>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StoredCredential {
    None,
    ApiKey,
    OAuth,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEntry {
    pub id: String,
    /// None for an auth-only record or an unsupported advanced definition.
    pub definition: Option<ProviderDefinition>,
    pub configured: bool,
    pub config_key_present: bool,
    pub stored_credential: StoredCredential,
    pub diagnostic: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalog {
    /// Opaque owner-scoped CAS, including BOTH raw files; never a key digest.
    pub revision: String,
    pub providers: Vec<ProviderEntry>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CredentialLocation {
    Stored,
    Config,
}

// Deliberately no Debug: secret-bearing requests must not enter diagnostic logs.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderMutation {
    pub expected_revision: String,
    pub action: ProviderAction,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum ProviderAction {
    Upsert {
        definition: ProviderDefinition,
    },
    /// Removes the custom definition only. Stored credentials remain visible and
    /// must be cleared explicitly; deleting an override does not uninstall builtins.
    RemoveConfig {
        id: String,
    },
    SetKey {
        id: String,
        key: String,
    },
    /// Removes only the selected saved source, not environment/CLI credentials or
    /// a remote OAuth grant. No multi-file atomicity is falsely promised.
    ClearCredential {
        id: String,
        location: CredentialLocation,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderReceipt {
    pub catalog: ProviderCatalog,
    pub requires_restart: bool,
}
