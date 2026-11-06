//! Session configuration access, bound and validated by the existing owner.
use super::{LocalError, LocalWorkspace, WorkspaceError, decode, local, service, unsupported};
use rpi_node_api::api::{SessionConfiguration, SessionSummary};
use serde_json::Value;

// This is a bounded configuration surface, not a second protocol command list.
const CONFIGURATION_COMMANDS: &[&str] = &[
    "get_available_models",
    "get_available_thinking_levels",
    "get_available_tools",
    "get_state",
    "get_session_branch",
    "compact",
    "set_model",
    "set_thinking_level",
    "set_active_tools",
];

impl LocalWorkspace {
    /// Observe the bound runtime; unsupported queries never imply full access.
    pub async fn session_configuration(
        &self,
        id: &str,
    ) -> Result<SessionConfiguration, WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        local(self.lock().map(|_| ()))?;
        if self.inner.agents.contains(id)? {
            let agent = self.inner.agents.prepare(id, false).await?;
            return Ok(SessionConfiguration {
                protocol: agent.descriptor().identity.protocol,
                supported_commands: CONFIGURATION_COMMANDS
                    .iter()
                    .filter(|command| agent.supports_command(command))
                    .map(|command| (*command).into())
                    .collect(),
                config_options: agent.session_config_options(id),
            });
        }
        let service = self.session_service(id)?;
        let _: SessionSummary = decode(service::open_session(&service, id.into()).await)?;
        let host = service.live_session(id).await.ok_or_else(|| {
            WorkspaceError::Local(LocalError::NotFound(format!("live session {id}")))
        })?;
        let exact = service
            .resource_configuration(id)
            .await
            .map_err(super::resources::resource_error)?
            .is_some()
            || !service
                .resource_operations(id)
                .await
                .map_err(super::resources::resource_error)?
                .is_empty();
        Ok(SessionConfiguration {
            protocol: "rpi".into(),
            supported_commands: CONFIGURATION_COMMANDS
                .iter()
                .filter(|command| {
                    !(exact && **command == "set_active_tools")
                        && rpi_extcore::protocol::SUPPORTED_COMMANDS.contains(command)
                        && host.supports_command(command)
                })
                .map(|command| (*command).into())
                .collect(),
            config_options: vec![],
        })
    }

    /// Change a negotiated ACP option, or a known RPI model/thinking alias.
    /// Runtime validation remains authoritative; this does not grant permission
    /// or promise that tools/resources were loaded. Callers retain their idle UI.
    pub async fn set_session_config_option(
        &self,
        id: &str,
        config_id: &str,
        value: Value,
    ) -> Result<Value, WorkspaceError> {
        // Do not nest a lifecycle read lease around existing command methods:
        // a queued shutdown writer must not deadlock recursive read acquisition.
        let lease = self.inner.lifecycle.read().await;
        local(self.lock().map(|_| ()))?;
        if self.inner.agents.contains(id)? {
            let agent = self.inner.agents.prepare(id, false).await?;
            // Match command(): preparation is protected, but an unresponsive
            // runtime response must not prevent shutdown from stopping children.
            drop(lease);
            return agent.set_config_option(id, config_id, value).await;
        }
        // Validate the session even when the requested option is unsupported.
        let _ = self.session_service(id)?;
        drop(lease);
        match config_id {
            "model" | "thinking" => {
                let value = value.as_str().ok_or_else(|| {
                    WorkspaceError::Local(LocalError::InvalidInput(
                        "configuration value must be a string".into(),
                    ))
                })?;
                if config_id == "model" {
                    self.set_model(id, value).await
                } else {
                    self.set_thinking_level(id, value).await
                }
            }
            _ => Err(unsupported("session/set_config_option", "rpi")),
        }
    }
}
