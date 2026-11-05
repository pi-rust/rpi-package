//! Binding for child readers that already own a live native host.
use super::{tool_program_queries, tool_program_snapshot, Service};
use crate::{
    host::HostSession,
    reply::{failure, ReplyKind, ServiceResult},
};
use std::sync::Arc;

pub(super) async fn host_id(host: &HostSession) -> ServiceResult<String> {
    host.bound_session_id()
        .filter(|id| tool_program_queries::valid_id(id))
        .map(str::to_owned)
        .ok_or_else(|| {
            failure(
                ReplyKind::HostUnavailable,
                "native child source identity unavailable",
            )
        })
}

pub(super) async fn check_identity(host: &HostSession, expected: &str) -> ServiceResult<()> {
    if host_id(host).await? != expected {
        return Err(failure(
            ReplyKind::HostUnavailable,
            "native child source identity unavailable",
        ));
    }
    Ok(())
}

pub(super) async fn read_health(host: &HostSession) -> ServiceResult<()> {
    let metadata = host.snapshot().await;
    if host.is_resource_retired()
        || *host.closed_receiver().borrow()
        || matches!(metadata.status.as_str(), "closed" | "error")
        || metadata.last_error.is_some()
    {
        return Err(failure(
            ReplyKind::HostUnavailable,
            "native child host unavailable",
        ));
    }
    Ok(())
}

pub(super) struct ChildAccess {
    pub(super) host: Arc<HostSession>,
    pub(super) id: String,
    service: Option<Service>,
}
impl ChildAccess {
    pub(super) async fn for_service(state: &Service, id: String) -> ServiceResult<Self> {
        state.check_session_admission(&id).await?;
        let host = state
            .live_session(&id)
            .await
            .ok_or_else(|| failure(ReplyKind::NotFound, "session not found"))?;
        let access = Self {
            host,
            id,
            service: Some(state.clone()),
        };
        access.check_binding().await?;
        Ok(access)
    }

    pub(super) async fn for_host(host: Arc<HostSession>) -> ServiceResult<Self> {
        let id = host_id(&host).await?;
        let access = Self {
            host,
            id,
            service: None,
        };
        access.check_read().await?;
        Ok(access)
    }

    // Live delivery checks ownership, not read health: buffered host_closed/error
    // must retain their precise typed resync cause instead of HostReplaced.
    pub(super) async fn check_binding(&self) -> ServiceResult<()> {
        if let Some(state) = &self.service {
            tool_program_snapshot::same_host(state, &self.id, &self.host).await?;
            // Unsupported legacy peers still take the explicit capability path.
            // A capable peer must match the route's source, not just its Arc.
            if tool_program_queries::supported(&self.host) {
                check_identity(&self.host, &self.id).await?;
            }
        }
        Ok(())
    }

    pub(super) async fn check_read(&self) -> ServiceResult<()> {
        self.check_binding().await?;
        if self.service.is_none() {
            read_health(&self.host).await?;
        }
        Ok(())
    }
}
