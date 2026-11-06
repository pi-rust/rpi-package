//! Shared node DTOs and service port; no UI projection, networking or plugin ABI.
pub mod api;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportIdentity {
    pub protocol: String,
    pub version: Option<String>,
    pub transport: String,
    pub agent_name: Option<String>,
}

impl TransportIdentity {
    #[must_use]
    pub fn new(
        protocol: impl Into<String>,
        version: Option<String>,
        transport: impl Into<String>,
    ) -> Self {
        Self {
            protocol: protocol.into(),
            version,
            transport: transport.into(),
            agent_name: None,
        }
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitySet {
    pub sessions: bool,
    pub session_load: bool,
    pub session_resume: bool,
    pub prompt: bool,
    pub streaming: bool,
    pub abort: bool,
    pub dialogs: bool,
    pub projects: bool,
    pub files: bool,
    pub git_diff: bool,
    pub models: bool,
    pub thinking_levels: bool,
    pub tools: bool,
    pub provider_management: bool,
    pub queue: bool,
    pub history: bool,
}

impl CapabilitySet {
    #[must_use]
    pub fn supports(&self, operation: &str) -> bool {
        match operation {
            "sessions" => self.sessions,
            "session_load" => self.session_load,
            "session_resume" => self.session_resume,
            "prompt" => self.prompt,
            "streaming" => self.streaming,
            "abort" => self.abort,
            "dialogs" => self.dialogs,
            "projects" => self.projects,
            "files" => self.files,
            "git_diff" => self.git_diff,
            "models" => self.models,
            "thinking_levels" => self.thinking_levels,
            "tools" => self.tools,
            "provider_management" => self.provider_management,
            "queue" => self.queue,
            "history" => self.history,
            _ => false,
        }
    }

    #[must_use]
    pub const fn http_default() -> Self {
        Self {
            sessions: true,
            session_load: true,
            session_resume: false,
            prompt: true,
            streaming: true,
            abort: true,
            dialogs: true,
            projects: true,
            files: true,
            git_diff: true,
            models: true,
            thinking_levels: true,
            tools: true,
            provider_management: true,
            queue: true,
            history: true,
        }
    }

    #[must_use]
    pub const fn rpc_default() -> Self {
        Self {
            sessions: true,
            session_load: false,
            session_resume: false,
            prompt: true,
            streaming: true,
            abort: true,
            dialogs: true,
            projects: false,
            files: false,
            git_diff: false,
            models: true,
            thinking_levels: true,
            tools: true,
            provider_management: false,
            queue: true,
            history: true,
        }
    }

    #[must_use]
    pub const fn acp_default() -> Self {
        Self {
            sessions: true,
            session_load: false,
            session_resume: false,
            prompt: true,
            streaming: true,
            abort: true,
            dialogs: true,
            projects: false,
            files: false,
            git_diff: false,
            models: false,
            thinking_levels: false,
            // Streamed tool events do not imply a tool catalog or mutation API.
            tools: false,
            provider_management: false,
            queue: false,
            history: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDescriptor {
    pub identity: TransportIdentity,
    pub capabilities: CapabilitySet,
}

impl AgentDescriptor {
    #[must_use]
    pub fn supports(&self, operation: &str) -> bool {
        self.capabilities.supports(operation)
    }
}
