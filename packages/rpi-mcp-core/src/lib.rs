#![forbid(unsafe_code)]
//! Caller-owned MCP connections. No plugin ABI or process-global registry.
pub mod auth;
pub mod oauth;
pub use auth::{
    AuthBinding, AuthDecision, AuthError, AuthFuture, AuthReason, AuthRequest, BearerChallenge,
    CredentialLease, CredentialProvider,
};
pub mod config;
pub mod connection;
pub mod control;
pub mod http;
pub mod jsonrpc;
mod process;
pub mod prompts;
pub mod runtime;
pub mod sampling;
mod callback;
pub use sampling::{
    SamplingAuthorizer, SamplingOptions,
    execute_validated, SamplingContext, SamplingError, SamplingExecutor, SamplingFuture,
    SamplingPolicy, SamplingRequest, SamplingResult,
};
pub mod transport;

pub use config::{Exposure, HttpServer, McpConfig, McpServerConfig, StdioServer};
pub use connection::{
    initialize_request, initialized_notification, parse_initialize, parse_tools_call,
    parse_tools_list, tools_call_request, tools_list_request, ServerInfo, PROTOCOL_VERSION,
};
pub use control::Control;
pub use jsonrpc::{
    call_result_text, tool_list, ErrorObject, Notification, Request, Response, Tool,
};
pub use prompts::{
    PromptArgument, PromptArguments, PromptContent, PromptDescriptor, PromptListResult,
    PromptMessage, PromptResult, ResourceTemplate, ResourceTemplateListResult,
};
pub use runtime::{DiscoveredServer, McpRuntime, ResourceUpdate};
pub use transport::{http_request, StdioTransport};

/// Explicit invocation scope and project directory. The embedding adapter owns
/// host/session identity mapping; core never reads SDK environment variables.
#[derive(Clone, Debug)]
pub struct RequestContext {
    pub scope: String,
    pub cwd: std::path::PathBuf,
}
impl Default for RequestContext {
    fn default() -> Self {
        Self {
            scope: "library".into(),
            cwd: std::env::current_dir().unwrap_or_else(|_| ".".into()),
        }
    }
}
