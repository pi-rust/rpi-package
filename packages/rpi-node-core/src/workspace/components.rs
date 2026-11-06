//! Component calls use the same owner as native and HTTP workspace access.
use super::{LocalError, LocalWorkspace, WorkspaceError};
use rpi_components::{Call, InstallRequest, Installed};

impl LocalWorkspace {
    pub fn component_host(&self) -> &rpi_components::host::Host {
        &self.inner.components
    }

    pub fn component_catalog(&self) -> Result<Vec<Installed>, WorkspaceError> {
        drop(self.lock()?);
        self.inner
            .components
            .catalog()
            .map_err(|e| LocalError::InvalidInput(e).into())
    }
    pub fn component_panel(&self, id: &str, panel: &str) -> Result<String, WorkspaceError> {
        drop(self.lock()?);
        self.inner
            .components
            .panel(id, panel)
            .map_err(|e| LocalError::InvalidInput(e).into())
    }
    pub fn component_call(
        &self,
        id: &str,
        call: Call,
        local: bool,
    ) -> Result<serde_json::Value, WorkspaceError> {
        drop(self.lock()?);
        self.inner
            .components
            .call(id, call, local)
            .map_err(|e| LocalError::InvalidInput(e).into())
    }
    pub fn component_install(&self, request: InstallRequest) -> Result<Installed, WorkspaceError> {
        drop(self.lock()?);
        self.inner
            .components
            .install(request)
            .map_err(|e| LocalError::InvalidInput(e).into())
    }
    pub fn component_remove(&self, id: &str) -> Result<(), WorkspaceError> {
        drop(self.lock()?);
        self.inner
            .components
            .remove(id)
            .map_err(|e| LocalError::InvalidInput(e).into())
    }
}
