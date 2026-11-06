//! Node service port implemented by the sole workspace owner.
use super::WorkspaceError;
use super::{LocalError, LocalWorkspace};
use crate::runtime::RuntimeError;
use futures_util::StreamExt;
use rpi_extcore::reply::ReplyKind;
use rpi_node_api::api::{
    AgentBackendOption, AuthState, AuthStatus, Capabilities, Diff, FileContent, FileListing,
    GitStatus, Health, History, HostResponse, ModelDescriptor, NodeApi, NodeError, NodeErrorKind,
    NodeEventStream, NodeFuture, Project, ProjectFolders, QueueCancellation, QueueMutation,
    QueueSnapshot, RuntimeState, SessionAccess, SessionAgentBinding, SessionBranch,
    SessionConfiguration, SessionMutation, SessionPreferences, SessionSummary, Snapshot,
    ToolCatalog,
};
use serde_json::Value;

impl NodeApi for LocalWorkspace {
    fn provider_catalog(
        &self,
    ) -> rpi_node_api::api::NodeFuture<'_, rpi_node_api::api::ProviderCatalog> {
        Box::pin(async move {
            LocalWorkspace::provider_catalog(self)
                .await
                .map_err(node_error)
        })
    }
    fn provider_mutate(
        &self,
        request: rpi_node_api::api::ProviderMutation,
    ) -> rpi_node_api::api::NodeFuture<'_, rpi_node_api::api::ProviderReceipt> {
        Box::pin(async move {
            LocalWorkspace::provider_mutate(self, request)
                .await
                .map_err(node_error)
        })
    }
    fn provider_release_hosts(
        &self,
    ) -> rpi_node_api::api::NodeFuture<'_, rpi_node_api::api::NativeReleaseReport> {
        Box::pin(async move {
            LocalWorkspace::provider_release_hosts(self)
                .await
                .map_err(node_error)
        })
    }

    fn package_catalog<'a>(
        &'a self,
        project: &'a str,
    ) -> rpi_node_api::api::NodeFuture<'a, rpi_node_api::api::PackageCatalog> {
        Box::pin(async move {
            LocalWorkspace::package_catalog(self, project)
                .await
                .map_err(node_error)
        })
    }
    fn package_bundle<'a>(
        &'a self,
        project: &'a str,
        package: rpi_node_api::api::PackageRef,
    ) -> rpi_node_api::api::NodeFuture<'a, rpi_node_api::api::PackageBundle> {
        Box::pin(async move {
            LocalWorkspace::package_bundle(self, project, package)
                .await
                .map_err(node_error)
        })
    }
    fn package_acquire<'a>(
        &'a self,
        project: &'a str,
        package: rpi_node_api::api::PackageRef,
    ) -> rpi_node_api::api::NodeFuture<'a, rpi_node_api::api::PackageAcquisition> {
        Box::pin(async move {
            LocalWorkspace::package_acquire(self, project, package)
                .await
                .map_err(node_error)
        })
    }
    fn package_install<'a>(
        &'a self,
        project: &'a str,
        request: rpi_node_api::api::PackageInstall,
    ) -> rpi_node_api::api::NodeFuture<'a, rpi_node_api::api::NativeReceipt> {
        Box::pin(async move {
            LocalWorkspace::package_install(self, project, request)
                .await
                .map_err(node_error)
        })
    }
    fn native_catalog<'a>(
        &'a self,
        project: &'a str,
    ) -> rpi_node_api::api::NodeFuture<'a, rpi_node_api::api::NativeCatalog> {
        Box::pin(async move {
            LocalWorkspace::native_catalog(self, project)
                .await
                .map_err(node_error)
        })
    }
    fn native_manage<'a>(
        &'a self,
        project: &'a str,
        request: rpi_node_api::api::NativeManagement,
    ) -> rpi_node_api::api::NodeFuture<'a, rpi_node_api::api::NativeReceipt> {
        Box::pin(async move {
            LocalWorkspace::native_manage(self, project, request)
                .await
                .map_err(node_error)
        })
    }
    fn native_session_resources<'a>(
        &'a self,
        id: &'a str,
    ) -> rpi_node_api::api::NodeFuture<'a, rpi_node_api::api::NativeSessionResources> {
        Box::pin(async move {
            LocalWorkspace::native_session_resources(self, id)
                .await
                .map_err(node_error)
        })
    }
    fn native_apply<'a>(
        &'a self,
        id: &'a str,
        request: rpi_node_api::api::NativeApply,
    ) -> rpi_node_api::api::NodeFuture<'a, rpi_node_api::api::NativeOperation> {
        Box::pin(async move {
            LocalWorkspace::native_apply(self, id, request)
                .await
                .map_err(node_error)
        })
    }
    fn native_create<'a>(
        &'a self,
        project: &'a str,
        request: rpi_node_api::api::NativeCreate,
    ) -> rpi_node_api::api::NodeFuture<'a, String> {
        Box::pin(async move {
            LocalWorkspace::native_create(self, project, request)
                .await
                .map_err(node_error)
        })
    }
    fn native_release<'a>(
        &'a self,
        project: &'a str,
        scope: rpi_node_api::api::NativeScope,
    ) -> rpi_node_api::api::NodeFuture<'a, rpi_node_api::api::NativeReleaseReport> {
        Box::pin(async move {
            LocalWorkspace::native_release(self, project, scope)
                .await
                .map_err(node_error)
        })
    }
    fn native_saved_history<'a>(
        &'a self,
        id: &'a str,
        cursor: Option<String>,
    ) -> rpi_node_api::api::NodeFuture<'a, Value> {
        Box::pin(async move {
            LocalWorkspace::native_saved_history(self, id, cursor)
                .await
                .map_err(node_error)
        })
    }
    fn authoring<'a>(
        &'a self,
        project: &'a str,
        request: rpi_node_api::api::AuthoringRequest,
    ) -> rpi_node_api::api::NodeFuture<'a, rpi_node_api::api::AuthoringState> {
        Box::pin(async move {
            LocalWorkspace::authoring(self, project, request)
                .await
                .map_err(node_error)
        })
    }

    fn health(&self) -> NodeFuture<'_, Health> {
        Box::pin(async move { LocalWorkspace::health(self).await.map_err(node_error) })
    }

    fn auth(&self) -> NodeFuture<'_, AuthState> {
        Box::pin(async move { LocalWorkspace::auth(self).await.map_err(node_error) })
    }

    fn login<'a>(&'a self, password: &'a str) -> NodeFuture<'a, AuthState> {
        Box::pin(async move {
            LocalWorkspace::login(self, password)
                .await
                .map_err(node_error)
        })
    }

    fn logout(&self) -> NodeFuture<'_, AuthState> {
        Box::pin(async move { LocalWorkspace::logout(self).await.map_err(node_error) })
    }

    fn capabilities(&self) -> NodeFuture<'_, Capabilities> {
        Box::pin(async move { LocalWorkspace::capabilities(self).await.map_err(node_error) })
    }

    fn projects(&self) -> NodeFuture<'_, Vec<Project>> {
        Box::pin(async move { LocalWorkspace::projects(self).await.map_err(node_error) })
    }

    fn rename_project<'a>(&'a self, project_id: &'a str, name: &'a str) -> NodeFuture<'a, Project> {
        Box::pin(async move {
            LocalWorkspace::rename_project(self, project_id, name)
                .await
                .map_err(node_error)
        })
    }

    fn project_folders<'a>(&'a self, path: &'a str) -> NodeFuture<'a, ProjectFolders> {
        Box::pin(async move {
            LocalWorkspace::project_folders(self, path)
                .await
                .map_err(node_error)
        })
    }

    fn create_project<'a>(
        &'a self,
        name: &'a str,
        path: Option<&'a str>,
        init_git: bool,
    ) -> NodeFuture<'a, Project> {
        Box::pin(async move {
            LocalWorkspace::create_project(self, name, path, init_git)
                .await
                .map_err(node_error)
        })
    }

    fn files<'a>(&'a self, project_id: &'a str, path: &'a str) -> NodeFuture<'a, FileListing> {
        Box::pin(async move {
            LocalWorkspace::files(self, project_id, path)
                .await
                .map_err(node_error)
        })
    }

    fn file<'a>(&'a self, project_id: &'a str, path: &'a str) -> NodeFuture<'a, FileContent> {
        Box::pin(async move {
            LocalWorkspace::file(self, project_id, path)
                .await
                .map_err(node_error)
        })
    }

    fn image<'a>(
        &'a self,
        project_id: &'a str,
        path: &'a str,
    ) -> NodeFuture<'a, rpi_node_api::api::ProjectImage> {
        Box::pin(async move {
            LocalWorkspace::image(self, project_id, path)
                .await
                .map_err(node_error)
        })
    }

    fn git_status<'a>(&'a self, project_id: &'a str) -> NodeFuture<'a, GitStatus> {
        Box::pin(async move {
            LocalWorkspace::git_status(self, project_id)
                .await
                .map_err(node_error)
        })
    }

    fn diff<'a>(
        &'a self,
        project_id: &'a str,
        path: &'a str,
        staged: bool,
    ) -> NodeFuture<'a, Diff> {
        Box::pin(async move {
            LocalWorkspace::diff(self, project_id, path, staged)
                .await
                .map_err(node_error)
        })
    }

    fn agent_backends(&self) -> NodeFuture<'_, Vec<AgentBackendOption>> {
        Box::pin(async move {
            LocalWorkspace::agent_backends(self)
                .await
                .map_err(node_error)
        })
    }

    fn session_agent_binding<'a>(&'a self, id: &'a str) -> NodeFuture<'a, SessionAgentBinding> {
        Box::pin(async move {
            LocalWorkspace::session_agent_binding(self, id)
                .await
                .map_err(node_error)
        })
    }

    fn create_session_with_backend<'a>(
        &'a self,
        project_id: Option<&'a str>,
        backend: &'a str,
    ) -> NodeFuture<'a, String> {
        Box::pin(async move {
            LocalWorkspace::create_session_with_backend(self, project_id, backend)
                .await
                .map_err(node_error)
        })
    }

    fn sessions(&self) -> NodeFuture<'_, Vec<SessionSummary>> {
        Box::pin(async move { LocalWorkspace::sessions(self).await.map_err(node_error) })
    }

    fn create_session<'a>(&'a self, project_id: Option<&'a str>) -> NodeFuture<'a, String> {
        Box::pin(async move {
            LocalWorkspace::create_session(self, project_id)
                .await
                .map_err(node_error)
        })
    }

    fn get_session<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, SessionSummary> {
        Box::pin(async move {
            LocalWorkspace::get_session(self, session_id)
                .await
                .map_err(node_error)
        })
    }

    fn rename_session<'a>(
        &'a self,
        session_id: &'a str,
        title: &'a str,
    ) -> NodeFuture<'a, SessionMutation> {
        Box::pin(async move {
            LocalWorkspace::rename_session(self, session_id, title)
                .await
                .map_err(node_error)
        })
    }

    fn session_preferences<'a>(
        &'a self,
        session_id: &'a str,
        archived: Option<bool>,
        pinned: Option<bool>,
    ) -> NodeFuture<'a, SessionPreferences> {
        Box::pin(async move {
            LocalWorkspace::session_preferences(self, session_id, archived, pinned)
                .await
                .map_err(node_error)
        })
    }

    fn fork_session<'a>(
        &'a self,
        session_id: &'a str,
        entry_id: Option<&'a str>,
        title: Option<&'a str>,
    ) -> NodeFuture<'a, SessionMutation> {
        Box::pin(async move {
            LocalWorkspace::fork_session(self, session_id, entry_id, title)
                .await
                .map_err(node_error)
        })
    }

    fn clone_session<'a>(
        &'a self,
        session_id: &'a str,
        title: Option<&'a str>,
    ) -> NodeFuture<'a, SessionMutation> {
        Box::pin(async move {
            LocalWorkspace::clone_session(self, session_id, title)
                .await
                .map_err(node_error)
        })
    }

    fn delete_session<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, ()> {
        Box::pin(async move {
            LocalWorkspace::delete_session(self, session_id)
                .await
                .map_err(node_error)
        })
    }

    fn open_session<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, SessionSummary> {
        Box::pin(async move {
            LocalWorkspace::open_session(self, session_id)
                .await
                .map_err(node_error)
        })
    }

    fn snapshot<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, Snapshot> {
        Box::pin(async move {
            LocalWorkspace::snapshot(self, session_id)
                .await
                .map_err(node_error)
        })
    }

    fn snapshot_with_limit<'a>(
        &'a self,
        session_id: &'a str,
        limit: u64,
    ) -> NodeFuture<'a, Snapshot> {
        Box::pin(async move {
            LocalWorkspace::snapshot_with_limit(self, session_id, limit)
                .await
                .map_err(node_error)
        })
    }

    fn session_access<'a>(&'a self, id: &'a str) -> NodeFuture<'a, SessionAccess> {
        Box::pin(async move {
            LocalWorkspace::session_access(self, id)
                .await
                .map_err(node_error)
        })
    }

    fn session_configuration<'a>(&'a self, id: &'a str) -> NodeFuture<'a, SessionConfiguration> {
        Box::pin(async move {
            LocalWorkspace::session_configuration(self, id)
                .await
                .map_err(node_error)
        })
    }

    fn set_session_config_option<'a>(
        &'a self,
        id: &'a str,
        config_id: &'a str,
        value: Value,
    ) -> NodeFuture<'a, Value> {
        Box::pin(async move {
            LocalWorkspace::set_session_config_option(self, id, config_id, value)
                .await
                .map_err(node_error)
        })
    }

    fn session_branch<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, SessionBranch> {
        Box::pin(async move {
            LocalWorkspace::session_branch(self, session_id)
                .await
                .map_err(node_error)
        })
    }

    fn history<'a>(
        &'a self,
        session_id: &'a str,
        after_seq: Option<u64>,
        before_seq: Option<u64>,
        limit: u64,
    ) -> NodeFuture<'a, History> {
        Box::pin(async move {
            LocalWorkspace::history(self, session_id, after_seq, before_seq, limit)
                .await
                .map_err(node_error)
        })
    }

    fn runtime_state<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, RuntimeState> {
        Box::pin(async move {
            LocalWorkspace::runtime_state(self, session_id)
                .await
                .map_err(node_error)
        })
    }

    fn active_tools_state<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, Vec<String>> {
        Box::pin(async move {
            LocalWorkspace::active_tools_state(self, session_id)
                .await
                .map_err(node_error)
        })
    }

    fn models<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, Vec<ModelDescriptor>> {
        Box::pin(async move {
            LocalWorkspace::models(self, session_id)
                .await
                .map_err(node_error)
        })
    }

    fn auth_status<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, AuthStatus> {
        Box::pin(async move {
            LocalWorkspace::auth_status(self, session_id)
                .await
                .map_err(node_error)
        })
    }

    fn thinking_levels<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, Vec<String>> {
        Box::pin(async move {
            LocalWorkspace::thinking_levels(self, session_id)
                .await
                .map_err(node_error)
        })
    }

    fn tools<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, ToolCatalog> {
        Box::pin(async move {
            LocalWorkspace::tools(self, session_id)
                .await
                .map_err(node_error)
        })
    }

    fn set_model<'a>(&'a self, session_id: &'a str, model_id: &'a str) -> NodeFuture<'a, Value> {
        Box::pin(async move {
            LocalWorkspace::set_model(self, session_id, model_id)
                .await
                .map_err(node_error)
        })
    }

    fn set_thinking_level<'a>(
        &'a self,
        session_id: &'a str,
        level: &'a str,
    ) -> NodeFuture<'a, Value> {
        Box::pin(async move {
            LocalWorkspace::set_thinking_level(self, session_id, level)
                .await
                .map_err(node_error)
        })
    }

    fn set_active_tools<'a>(
        &'a self,
        session_id: &'a str,
        tools: &'a [String],
    ) -> NodeFuture<'a, Value> {
        Box::pin(async move {
            LocalWorkspace::set_active_tools(self, session_id, tools)
                .await
                .map_err(node_error)
        })
    }

    fn resume_pending<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, Value> {
        Box::pin(async move {
            LocalWorkspace::resume_pending(self, session_id)
                .await
                .map_err(node_error)
        })
    }

    fn compact<'a>(
        &'a self,
        session_id: &'a str,
        custom_instructions: Option<&'a str>,
    ) -> NodeFuture<'a, Value> {
        Box::pin(async move {
            LocalWorkspace::compact(self, session_id, custom_instructions)
                .await
                .map_err(node_error)
        })
    }

    fn queue<'a>(
        &'a self,
        session_id: &'a str,
        include_completed: bool,
    ) -> NodeFuture<'a, QueueSnapshot> {
        Box::pin(async move {
            LocalWorkspace::queue(self, session_id, include_completed)
                .await
                .map_err(node_error)
        })
    }

    fn queue_message<'a>(
        &'a self,
        session_id: &'a str,
        message: &'a str,
        deliver_as: &'a str,
    ) -> NodeFuture<'a, QueueMutation> {
        Box::pin(async move {
            LocalWorkspace::queue_message(self, session_id, message, deliver_as)
                .await
                .map_err(node_error)
        })
    }

    fn cancel_queued<'a>(
        &'a self,
        session_id: &'a str,
        entry_id: &'a str,
    ) -> NodeFuture<'a, QueueCancellation> {
        Box::pin(async move {
            LocalWorkspace::cancel_queued(self, session_id, entry_id)
                .await
                .map_err(node_error)
        })
    }

    fn clear_queue<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, Value> {
        Box::pin(async move {
            LocalWorkspace::clear_queue(self, session_id)
                .await
                .map_err(node_error)
        })
    }

    fn dialog_response<'a>(
        &'a self,
        session_id: &'a str,
        request_id: &'a str,
        value: Value,
    ) -> NodeFuture<'a, Value> {
        Box::pin(async move {
            LocalWorkspace::dialog_response(self, session_id, request_id, value)
                .await
                .map_err(node_error)
        })
    }

    fn cancel_dialog<'a>(
        &'a self,
        session_id: &'a str,
        request_id: &'a str,
    ) -> NodeFuture<'a, Value> {
        Box::pin(async move {
            LocalWorkspace::cancel_dialog(self, session_id, request_id)
                .await
                .map_err(node_error)
        })
    }

    fn prompt<'a>(
        &'a self,
        session_id: &'a str,
        message: &'a str,
    ) -> NodeFuture<'a, HostResponse<Value>> {
        Box::pin(async move {
            LocalWorkspace::prompt(self, session_id, message)
                .await
                .map_err(node_error)
        })
    }

    fn prompt_content<'a>(
        &'a self,
        session_id: &'a str,
        content_blocks: &'a Value,
    ) -> NodeFuture<'a, HostResponse<Value>> {
        Box::pin(async move {
            LocalWorkspace::prompt_content(self, session_id, content_blocks)
                .await
                .map_err(node_error)
        })
    }

    fn abort<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, HostResponse<Value>> {
        Box::pin(async move {
            LocalWorkspace::abort(self, session_id)
                .await
                .map_err(node_error)
        })
    }

    fn subscribe<'a>(&'a self, session_id: &'a str, after: u64) -> NodeFuture<'a, NodeEventStream> {
        Box::pin(async move {
            LocalWorkspace::subscribe(self, session_id, after)
                .await
                .map_err(node_error)
                .map(|stream| {
                    Box::pin(stream.map(|item| item.map_err(node_error))) as NodeEventStream
                })
        })
    }
}

fn node_error(error: WorkspaceError) -> NodeError {
    use NodeErrorKind as Kind;
    let (kind, message) = match error {
        WorkspaceError::Local(LocalError::AuthoringIo(error)) => (
            Kind::Internal,
            format!(
                "Authoring filesystem operation failed (OS code {:?}). Keep your unsaved text. Source/metadata candidates and pending/test recovery records are retained under .rpi/work-authoring; inspect before explicit repair, never reset or blindly replay.",
                error.raw_os_error()
            ),
        ),
        WorkspaceError::Local(LocalError::InvalidInput(message)) => (Kind::InvalidInput, message),
        WorkspaceError::Local(LocalError::NotFound(_)) => (Kind::NotFound, "not found".into()),
        WorkspaceError::Local(LocalError::DuplicateProject(_)) => {
            (Kind::Conflict, "project already registered".into())
        }
        WorkspaceError::Local(LocalError::Unsupported(message)) => (Kind::Unsupported, message),
        WorkspaceError::Local(LocalError::Shutdown) => {
            (Kind::Unavailable, "workspace is shutting down".into())
        }
        WorkspaceError::Local(LocalError::Service { kind, .. }) => {
            let category = match kind {
                ReplyKind::InvalidInput => Kind::InvalidInput,
                ReplyKind::NotFound => Kind::NotFound,
                ReplyKind::AccessDenied => Kind::AccessDenied,
                ReplyKind::Conflict => Kind::Conflict,
                ReplyKind::Unsupported => Kind::Unsupported,
                ReplyKind::Unavailable | ReplyKind::HostUnavailable => Kind::Unavailable,
                ReplyKind::LimitExceeded => Kind::LimitExceeded,
                ReplyKind::UnsupportedContent => Kind::UnsupportedContent,
                ReplyKind::Timeout => Kind::Timeout,
                ReplyKind::Internal
                | ReplyKind::Success
                | ReplyKind::Created
                | ReplyKind::Empty => Kind::Internal,
            };
            // Even 4xx-like native errors may contain paths or echoed payloads.
            // Only local validation errors above have controlled public text.
            let message = match category {
                Kind::InvalidInput => "invalid workspace request",
                Kind::NotFound => "not found",
                Kind::AccessDenied => "workspace access denied",
                Kind::Conflict => "workspace operation conflicts with current state",
                Kind::Unsupported => "workspace operation unsupported",
                Kind::Unavailable => "workspace operation unavailable",
                Kind::LimitExceeded => "workspace request limit exceeded",
                Kind::UnsupportedContent => "workspace content unsupported",
                Kind::Timeout => "workspace operation timed out",
                Kind::Internal => "workspace operation failed",
            };
            (category, message.into())
        }
        WorkspaceError::Runtime(RuntimeError::Unsupported {
            operation,
            protocol,
        }) => (
            Kind::Unsupported,
            format!("{operation} unsupported by {protocol}"),
        ),
        WorkspaceError::Runtime(RuntimeError::RequestTimeout(_)) => {
            (Kind::Timeout, "workspace operation timed out".into())
        }
        WorkspaceError::Runtime(RuntimeError::ChildExited(_)) => {
            (Kind::Unavailable, "execution host exited".into())
        }
        _ => (Kind::Internal, "workspace operation failed".into()),
    };
    NodeError::new(kind, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_outcomes_have_transport_independent_categories() {
        for (reply, expected) in [
            (ReplyKind::InvalidInput, NodeErrorKind::InvalidInput),
            (ReplyKind::NotFound, NodeErrorKind::NotFound),
            (ReplyKind::AccessDenied, NodeErrorKind::AccessDenied),
            (ReplyKind::Conflict, NodeErrorKind::Conflict),
            (ReplyKind::Unsupported, NodeErrorKind::Unsupported),
            (ReplyKind::Unavailable, NodeErrorKind::Unavailable),
            (ReplyKind::HostUnavailable, NodeErrorKind::Unavailable),
            (ReplyKind::LimitExceeded, NodeErrorKind::LimitExceeded),
            (
                ReplyKind::UnsupportedContent,
                NodeErrorKind::UnsupportedContent,
            ),
            (ReplyKind::Timeout, NodeErrorKind::Timeout),
            (ReplyKind::Internal, NodeErrorKind::Internal),
            (ReplyKind::Success, NodeErrorKind::Internal),
            (ReplyKind::Created, NodeErrorKind::Internal),
            (ReplyKind::Empty, NodeErrorKind::Internal),
        ] {
            let error = node_error(WorkspaceError::Local(LocalError::Service {
                kind: reply,
                message: "native-sensitive-detail".into(),
            }));
            assert_eq!(error.kind, expected, "{reply:?}");
            assert!(
                !error.to_string().contains("native-sensitive-detail"),
                "{reply:?}"
            );
            assert!(
                !format!("{error:?}").contains("native-sensitive-detail"),
                "{reply:?}"
            );
        }
    }

    #[test]
    fn internal_errors_do_not_expose_paths_or_process_details() {
        let secret = "sensitive /private/runtime/path and process stderr";
        let errors = [
            WorkspaceError::Local(LocalError::Initialization(secret.into())),
            WorkspaceError::Local(LocalError::Io(std::io::Error::other(secret))),
            WorkspaceError::Local(LocalError::NotFound(secret.into())),
            WorkspaceError::Local(LocalError::DuplicateProject(secret.into())),
            WorkspaceError::Local(LocalError::Service {
                kind: ReplyKind::Internal,
                message: secret.into(),
            }),
            WorkspaceError::Local(LocalError::Service {
                kind: ReplyKind::HostUnavailable,
                message: secret.into(),
            }),
            WorkspaceError::Local(LocalError::Service {
                kind: ReplyKind::Timeout,
                message: secret.into(),
            }),
            WorkspaceError::Runtime(RuntimeError::Process(secret.into())),
            WorkspaceError::Runtime(RuntimeError::RequestTimeout(secret.into())),
            WorkspaceError::Runtime(RuntimeError::ChildExited(secret.into())),
        ];
        for error in errors {
            let error = node_error(error);
            assert!(!error.to_string().contains(secret));
            assert!(!format!("{error:?}").contains(secret));
        }
    }

    #[test]
    fn existing_actionable_input_errors_remain_actionable() {
        let error = node_error(WorkspaceError::Local(LocalError::InvalidInput(
            "name must not be empty".into(),
        )));
        assert_eq!(error.kind, NodeErrorKind::InvalidInput);
        assert_eq!(error.message, "name must not be empty");
        let error = node_error(WorkspaceError::Runtime(RuntimeError::Unsupported {
            operation: "snapshot".into(),
            protocol: "acp".into(),
        }));
        assert_eq!(error.kind, NodeErrorKind::Unsupported);
        assert_eq!(error.message, "snapshot unsupported by acp");
    }
}
