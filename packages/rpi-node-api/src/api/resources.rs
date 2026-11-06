//! Work application resource authority. Native observations remain native facts.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NativeScope {
    Project,
    Global,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NativeKind {
    Package,
    Skill,
    Extension,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum NativeTools {
    Default {},
    Only { names: Vec<String> },
    None {},
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeSelection {
    pub main_package: String,
    #[serde(default)]
    pub package_instructions: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    pub tools: NativeTools,
    #[serde(default)]
    pub supplementary_instructions: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeApply {
    pub expected_revision: u64,
    pub operation_id: String,
    pub target: NativeSelection,
    #[serde(default)]
    pub supersedes_operation_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "camelCase", deny_unknown_fields)]
pub enum NativeManagement {
    Inspect {
        reference: String,
    },
    Install {
        scope: NativeScope,
        kind: NativeKind,
        name: String,
        source: String,
        version: Option<String>,
        trusted: bool,
    },
    Update {
        reference: String,
        source: String,
        version: Option<String>,
        trusted: bool,
    },
    Reinstall {
        reference: String,
        source: String,
        version: Option<String>,
        trusted: bool,
    },
    Remove {
        reference: String,
    },
    Recover {
        scope: NativeScope,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeResource {
    pub reference: String,
    pub scope: NativeScope,
    pub kind: NativeKind,
    pub name: String,
    pub facts: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeCatalog {
    pub profile: Value,
    pub resources: Vec<NativeResource>,
    pub diagnostics: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeReceipt {
    pub success: bool,
    pub observation: Value,
    pub diagnostics: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeReleaseReport {
    pub released: Vec<String>,
    pub blocked: Vec<NativeReleaseBlock>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeReleaseBlock {
    pub project_id: String,
    pub diagnostic: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSessionResources {
    /// None is legacy, never a fabricated revision-zero configuration.
    pub configuration: Option<NativeConfiguration>,
    pub operations: Vec<NativeOperation>,
    pub readiness: NativeReadiness,
    /// None means the owner could not establish current execution readiness.
    pub executable: Option<bool>,
    /// Fresh actual host state only; saved configuration observations are historical.
    pub actual: Option<Value>,
    pub generation: Option<String>,
    pub diagnostic: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeReference {
    pub reference: Option<String>,
    pub identity: String,
    pub diagnostic: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeDesired {
    pub main_package: Option<NativeReference>,
    pub package_instructions: Vec<String>,
    pub skills: Vec<NativeReference>,
    pub extensions: Vec<NativeReference>,
    pub tools: NativeTools,
    pub supplementary_instructions: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeConfiguration {
    pub revision: u64,
    pub desired: NativeDesired,
    pub historical_observation: Option<Value>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeReadiness {
    Legacy,
    Verified,
    Closed,
    Applying,
    Blocked,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeOperationStatus {
    Pending,
    Blocked,
    Applied,
    Superseded,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeOperation {
    pub retry: Option<NativeApply>,
    pub operation_id: String,
    pub expected_revision: u64,
    pub target: NativeDesired,
    pub supersedes_operation_id: Option<String>,
    pub status: NativeOperationStatus,
    pub result_revision: Option<u64>,
    pub error: Option<String>,
    pub generation: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeCreate {
    pub developer: bool,
    pub selection: Option<NativeSelection>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
pub enum AuthoringRequest {
    Read,
    Create {
        id: String,
        kind: NativeKind,
    },
    Save {
        id: String,
        revision: u64,
        digest: String,
        sources: BTreeMap<String, String>,
    },
    Validate {
        id: String,
        revision: u64,
        digest: String,
    },
    Test {
        id: String,
        revision: u64,
        digest: String,
    },
    ExecuteTest {
        id: String,
        test_id: String,
        prompt: String,
    },
    RecordEvidence {
        id: String,
        test_id: String,
    },
    Publish {
        id: String,
        revision: u64,
        digest: String,
        test_id: String,
        version: String,
    },
    /// Explicit migration copies inert legacy bytes as reference files, never as authority.
    ImportLegacy {
        id: String,
        original: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoringDraft {
    pub id: String,
    pub kind: NativeKind,
    pub revision: u64,
    pub digest: String,
    pub path: String,
    pub sources: BTreeMap<String, String>,
    pub validation: Option<AuthoringValidation>,
    pub tests: Vec<AuthoringTest>,
    pub releases: Vec<AuthoringRelease>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoringValidation {
    pub revision: u64,
    pub digest: String,
    pub valid: bool,
    pub diagnostics: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoringTest {
    pub id: String,
    pub revision: u64,
    pub digest: String,
    pub snapshot: String,
    pub session_id: String,
    pub readback: Value,
    /// Owned successful run settlement tied to the immutable test binding/digest,
    /// never a transcript inference, schema check or UI checkbox.
    pub evidence: Option<Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoringRelease {
    pub version: String,
    pub digest: String,
    pub test_id: String,
    pub source: String,
    pub installed_reference: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoringState {
    pub drafts: Vec<AuthoringDraft>,
    pub diagnostics: Vec<String>,
}

#[cfg(test)]
mod tests;
