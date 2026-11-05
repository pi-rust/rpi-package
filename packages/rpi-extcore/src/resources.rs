//! Typed exact native resource declarations and bounded observations.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod local;
mod lock;
pub mod saved;
pub use saved::{
    ResourceHistoryCursor, ResourceHistoryRequest, ResourceSavedBinding, ResourceSavedHistory,
    ResourceSavedPending,
};
#[cfg(test)]
#[path = "resources/tests.rs"]
mod tests;

pub const MAX_CONFIG_BYTES: usize = 64 * 1024;
pub const MAX_STATE_BYTES: usize = 256 * 1024;
pub const MAX_OPERATIONS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSelection {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_package: Option<MainPackage>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub instructions: Vec<String>,
    pub tools: ResourceTools,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MainPackage {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ResourceScope>,
    #[serde(default)]
    pub instructions: Vec<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceScope {
    Project,
    Global,
}
impl ResourceScope {
    pub(crate) fn argument(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Global => "global",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum ResourceTools {
    Default {},
    Only { names: Vec<String> },
    None {},
}

#[derive(Clone, Debug)]
pub enum ResourceStartup {
    Exact(ResourceSelection),
    Transition(ResourceTransition),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeBinding {
    pub revision: u64,
    pub fingerprint: Option<String>,
    #[serde(
        rename = "operationId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub operation_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedBinding {
    pub revision: u64,
    pub fingerprint: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceTransition {
    pub version: u32,
    pub operation_id: String,
    pub expected: ExpectedBinding,
    pub target: ResourceSelection,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyResourcesRequest {
    pub expected_revision: u64,
    pub operation_id: String,
    pub target: ResourceSelection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes_operation_id: Option<String>,
}
impl ApplyResourcesRequest {
    pub(crate) fn validate(&self) -> ResourceResult<()> {
        validate_operation_id(&self.operation_id)?;
        self.target.validate()?;
        if let Some(prior) = &self.supersedes_operation_id {
            validate_operation_id(prior)?;
            if prior == &self.operation_id {
                return Err(ResourceError::new(
                    ResourceErrorKind::InvalidInput,
                    "a new operation ID is required to supersede",
                ));
            }
        }
        if self.expected_revision == u64::MAX {
            return Err(ResourceError::conflict(
                "application resource revision overflow",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceOperationStatus {
    Pending,
    Blocked,
    Applied,
    Superseded,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceOperation {
    pub request: ApplyResourcesRequest,
    pub transition: ResourceTransition,
    /// None means no current owned capture: restore native baseline, NOT a
    /// promise that native persists live setters. Some is immutable intent.
    /// Pending retries always retain their original captured choices.
    #[serde(default)]
    pub restore_settings: Option<ResourceSessionSettings>,
    pub status: ResourceOperationStatus,
    pub result_revision: Option<u64>,
    pub observation: Option<ResourceObservation>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceConfiguration {
    pub selection: ResourceSelection,
    pub revision: u64,
    #[serde(default)]
    pub settings: ResourceSessionSettings,
    /// Newest owned-idle-stop capture; consumed atomically with final metadata
    /// only after resource verification and durable attachment succeed.
    /// Never substitute the historical operation settings above for this field.
    #[serde(default)]
    pub current_settings: Option<ResourceSettingsCapture>,
    pub observation: Option<ResourceObservation>,
    pub operations: Vec<ResourceOperation>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSettingsCapture {
    pub settings: ResourceSessionSettings,
    pub captured_at_ms: u64,
    pub host_id: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSessionSettings {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub thinking: Option<String>,
    pub permission_gate: bool,
    pub bypass: bool,
}
/// Bounded versioned native body; inventory and provenance remain native-owned.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceObservation {
    pub generation: String,
    pub session_id: String,
    pub binding: NativeBinding,
    pub body: Value,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceErrorKind {
    InvalidInput,
    Unsupported,
    Conflict,
    LimitExceeded,
    Unavailable,
    Storage,
}
#[derive(Clone, Debug)]
pub struct ResourceError {
    pub kind: ResourceErrorKind,
    pub message: String,
}
impl ResourceError {
    pub(crate) fn new(kind: ResourceErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
    pub(crate) fn unavailable(message: impl ToString) -> Self {
        Self::new(ResourceErrorKind::Unavailable, message.to_string())
    }
    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::new(ResourceErrorKind::Conflict, message)
    }
}
impl std::fmt::Display for ResourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ResourceError {}
pub type ResourceResult<T> = Result<T, ResourceError>;
impl ResourceSelection {
    pub fn validate(&self) -> ResourceResult<()> {
        if self.version != 1 {
            return Err(ResourceError::new(
                ResourceErrorKind::Unsupported,
                "unsupported exact resource version",
            ));
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|_| ResourceError::unavailable("resource serialization failed"))?;
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(ResourceError::new(
                ResourceErrorKind::LimitExceeded,
                "resource selection exceeds 64 KiB",
            ));
        }
        Ok(())
    }
}
pub(crate) fn validate_operation_id(id: &str) -> ResourceResult<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        return Err(ResourceError::new(
            ResourceErrorKind::InvalidInput,
            "invalid resource operation ID",
        ));
    }
    Ok(())
}
impl ResourceObservation {
    pub fn parse(body: Value) -> ResourceResult<Self> {
        if serde_json::to_vec(&body)
            .map_err(|_| ResourceError::unavailable("invalid state"))?
            .len()
            > MAX_STATE_BYTES
        {
            return Err(ResourceError::new(
                ResourceErrorKind::LimitExceeded,
                "resource state exceeds 256 KiB",
            ));
        }
        if body["version"] != 1 {
            return Err(ResourceError::new(
                ResourceErrorKind::Unsupported,
                "unsupported resource observation",
            ));
        }
        let generation = body["hostId"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256)
            .ok_or_else(|| ResourceError::unavailable("missing native generation"))?
            .to_owned();
        let session_id = body["sessionId"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256)
            .ok_or_else(|| ResourceError::unavailable("missing native session ID"))?
            .to_owned();
        let binding: NativeBinding = serde_json::from_value(body["binding"].clone())
            .map_err(|_| ResourceError::unavailable("invalid native resource binding"))?;
        if binding.revision == 0
            || !binding.fingerprint.as_ref().is_some_and(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            })
        {
            return Err(ResourceError::unavailable(
                "invalid exact native fingerprint",
            ));
        }
        for field in [
            "loadedSkills",
            "loadedExtensions",
            "registeredTools",
            "modelEffectiveTools",
        ] {
            if !body[field].is_array() {
                return Err(ResourceError::unavailable(format!(
                    "missing native {field}"
                )));
            }
        }
        Ok(Self {
            generation,
            session_id,
            binding,
            body,
        })
    }
    pub(crate) fn verify(&self, id: &str, selection: &ResourceSelection) -> ResourceResult<()> {
        let mut actual: ResourceSelection = serde_json::from_value(self.body["selection"].clone())
            .map_err(|_| ResourceError::unavailable("invalid observed declaration"))?;
        let mut expected = selection.clone();
        for selected in [&mut actual, &mut expected] {
            if let Some(package) = selected.main_package.as_mut() {
                if package.scope == Some(ResourceScope::Project) {
                    package.scope = None;
                }
            }
        }
        if self.session_id != id || actual != expected {
            return Err(ResourceError::conflict(
                "native loaded selection/session differs",
            ));
        }
        let effective = self.body["modelEffectiveTools"]
            .as_array()
            .ok_or_else(|| ResourceError::unavailable("invalid effective tools"))?;
        match &expected.tools {
            ResourceTools::None {} if !effective.is_empty() => {
                return Err(ResourceError::conflict("native widened exact none tools"))
            }
            ResourceTools::Only { names } => {
                let actual: Option<std::collections::HashSet<&str>> =
                    effective.iter().map(Value::as_str).collect();
                let requested: std::collections::HashSet<&str> =
                    names.iter().map(String::as_str).collect();
                if actual.as_ref() != Some(&requested) || effective.len() != names.len() {
                    return Err(ResourceError::conflict(
                        "native effective tools differ from exact only selection",
                    ));
                }
            }
            _ => {}
        }
        Ok(())
    }
    pub(crate) fn validate_record(&self) -> ResourceResult<()> {
        let parsed = Self::parse(self.body.clone())?;
        if self.generation != parsed.generation
            || self.binding != parsed.binding
            || self.session_id != parsed.session_id
        {
            return Err(ResourceError::conflict(
                "corrupt native observation association",
            ));
        }
        Ok(())
    }
}
impl ExpectedBinding {
    pub(crate) fn validate(&self) -> ResourceResult<()> {
        let digest = self.fingerprint.as_ref().is_some_and(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
        if (self.revision == 0 && self.fingerprint.is_some()) || (self.revision != 0 && !digest) {
            return Err(ResourceError::conflict("invalid expected native binding"));
        }
        Ok(())
    }
}
impl ResourceConfiguration {
    pub(crate) fn validate(&self) -> ResourceResult<()> {
        self.selection.validate()?;
        if self.operations.len() > MAX_OPERATIONS
            || serde_json::to_vec(self)
                .map_err(|_| ResourceError::unavailable("invalid resource record"))?
                .len()
                > 2 * 1024 * 1024
        {
            return Err(ResourceError::new(
                ResourceErrorKind::LimitExceeded,
                "resource record limit exceeded",
            ));
        }
        if self.current_settings.as_ref().is_some_and(|capture| {
            capture.host_id.is_empty()
                || capture.host_id.len() > 256
                || capture.host_id.chars().any(char::is_control)
        }) {
            return Err(ResourceError::conflict(
                "invalid owned settings capture identity",
            ));
        }
        for settings in std::iter::once(&self.settings)
            .chain(
                self.current_settings
                    .iter()
                    .map(|capture| &capture.settings),
            )
            .chain(
                self.operations
                    .iter()
                    .filter_map(|op| op.restore_settings.as_ref()),
            )
        {
            for value in [&settings.model, &settings.provider, &settings.thinking]
                .into_iter()
                .flatten()
            {
                if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
                    return Err(ResourceError::conflict(
                        "invalid persisted resource session settings",
                    ));
                }
            }
        }
        if let Some(observation) = &self.observation {
            observation.validate_record()?;
            observation.verify(&observation.session_id, &self.selection)?;
        }
        let mut revision = self
            .operations
            .first()
            .map_or(1, |op| op.request.expected_revision);
        if revision > 1 {
            return Err(ResourceError::conflict(
                "resource ledger lost initial identities",
            ));
        }
        let mut ids = std::collections::HashSet::new();
        let mut blocked = false;
        let mut native_expected: Option<ExpectedBinding> = None;
        for (index, op) in self.operations.iter().enumerate() {
            op.request.validate()?;
            op.transition.expected.validate()?;
            if blocked
                || op.request.expected_revision != revision
                || !ids.insert(&op.request.operation_id)
                || op.transition.operation_id != op.request.operation_id
                || op.transition.target != op.request.target
                || op.transition.version != 1
            {
                return Err(ResourceError::conflict(
                    "corrupt resource operation identity/revision",
                ));
            }
            if op.error.as_ref().is_some_and(|s| s.chars().count() > 1024) {
                return Err(ResourceError::conflict(
                    "oversized resource operation diagnostic",
                ));
            }
            if let Some(expected) = &native_expected {
                if expected != &op.transition.expected {
                    return Err(ResourceError::conflict(
                        "resource ledger native CAS chain changed",
                    ));
                }
            }
            native_expected = Some(op.transition.expected.clone());
            if let Some(prior) = &op.request.supersedes_operation_id {
                if index == 0
                    || self.operations[index - 1].status != ResourceOperationStatus::Superseded
                    || &self.operations[index - 1].request.operation_id != prior
                {
                    return Err(ResourceError::conflict(
                        "invalid explicit resource supersede association",
                    ));
                }
            } else if index > 0
                && self.operations[index - 1].status == ResourceOperationStatus::Superseded
            {
                return Err(ResourceError::conflict(
                    "superseded operation lost its replacement identity",
                ));
            }
            match op.status {
                ResourceOperationStatus::Applied => {
                    revision = revision
                        .checked_add(1)
                        .ok_or_else(|| ResourceError::conflict("application revision overflow"))?;
                    let observation = op.observation.as_ref().ok_or_else(|| {
                        ResourceError::conflict("applied operation lacks observation")
                    })?;
                    observation.validate_record()?;
                    observation.verify(&observation.session_id, &op.request.target)?;
                    if op.result_revision != Some(revision)
                        || op.error.is_some()
                        || observation.binding.operation_id.as_deref()
                            != Some(&op.request.operation_id)
                        || op.transition.expected.revision.checked_add(1)
                            != Some(observation.binding.revision)
                    {
                        return Err(ResourceError::conflict(
                            "corrupt applied native operation association",
                        ));
                    }
                    native_expected = Some(ExpectedBinding {
                        revision: observation.binding.revision,
                        fingerprint: observation.binding.fingerprint.clone(),
                    });
                }
                ResourceOperationStatus::Superseded => {
                    let next = self.operations.get(index + 1).ok_or_else(|| {
                        ResourceError::conflict("terminal superseded operation lacks replacement")
                    })?;
                    if op.result_revision.is_some()
                        || op.observation.is_some()
                        || next.request.supersedes_operation_id.as_ref()
                            != Some(&op.request.operation_id)
                        || next.transition.expected != op.transition.expected
                        || next.request.expected_revision != op.request.expected_revision
                    {
                        return Err(ResourceError::conflict(
                            "supersede changed native/application expectation or claimed success",
                        ));
                    }
                }
                ResourceOperationStatus::Pending | ResourceOperationStatus::Blocked => {
                    blocked = true;
                    if op.result_revision.is_some() || op.observation.is_some() {
                        return Err(ResourceError::conflict(
                            "unverified resource operation claims a result",
                        ));
                    }
                }
            }
        }
        if self.revision != revision {
            return Err(ResourceError::conflict(
                "corrupt application resource revision",
            ));
        }
        if let Some(last) = self
            .operations
            .iter()
            .rev()
            .find(|op| op.status == ResourceOperationStatus::Applied)
        {
            if self.selection != last.request.target
                || self.observation.as_ref().map(|o| &o.binding)
                    != last.observation.as_ref().map(|o| &o.binding)
            {
                return Err(ResourceError::conflict(
                    "current resource record differs from applied binding",
                ));
            }
        } else if let Some(first) = self.operations.first() {
            let original = match &self.observation {
                Some(observation) => ExpectedBinding {
                    revision: observation.binding.revision,
                    fingerprint: observation.binding.fingerprint.clone(),
                },
                None if self.revision == 0 => ExpectedBinding {
                    revision: 0,
                    fingerprint: None,
                },
                None => {
                    return Err(ResourceError::conflict(
                        "configured resource intent has no original native observation",
                    ))
                }
            };
            if first.transition.expected != original {
                return Err(ResourceError::conflict(
                    "resource intent changed its original native binding",
                ));
            }
        }
        Ok(())
    }
    pub(crate) fn validate_session(&self, id: &str) -> ResourceResult<()> {
        self.validate()?;
        for observation in self.observation.iter().chain(
            self.operations
                .iter()
                .filter_map(|op| op.observation.as_ref()),
        ) {
            if observation.session_id != id {
                return Err(ResourceError::conflict(
                    "resource record belongs to a different native session",
                ));
            }
        }
        Ok(())
    }
    pub(crate) fn blocked(&self) -> bool {
        self.operations.iter().any(|o| {
            matches!(
                o.status,
                ResourceOperationStatus::Pending | ResourceOperationStatus::Blocked
            )
        })
    }
}
