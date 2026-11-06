use std::{future::Future, pin::Pin};

use super::{
    AgentBackendOption, AuthState, AuthStatus, Capabilities, Diff, FileContent, FileListing,
    GitStatus, Health, History, HostResponse, ModelDescriptor, NodeError, NodeErrorKind, Project,
    ProjectFolders, QueueCancellation, QueueMutation, QueueSnapshot, RuntimeState, SessionAccess,
    SessionAgentBinding, SessionBranch, SessionConfiguration, SessionMutation, SessionPreferences,
    SessionSummary, Snapshot, ToolCatalog, WireEvent,
};
use serde_json::Value;

/// Local executors may own non-Send state; the port imposes no executor bounds.
pub type NodeFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, NodeError>> + 'a>>;

/// Existing wire events, with only the transport error normalized. The public
/// port permits browser-local streams; native runtime streams retain Send internally.
pub type NodeEventStream = Pin<Box<dyn futures_core::Stream<Item = Result<WireEvent, NodeError>>>>;

/// Typed application port implemented by an existing workspace owner or adapter.
///
/// Obtaining a handle creates no workspace or execution host. Authentication and
/// sharing checks remain with the owner/transport; capabilities are not permission
/// grants. Raw commands, host lifecycle and transport-specific QR operations are
/// deliberately excluded. Futures and implementations need not be Send or Sync.
/// New operations default to Unsupported to preserve existing five-method mocks.
pub trait NodeApi {
    fn provider_catalog(&self) -> NodeFuture<'_, super::ProviderCatalog> {
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "provider management unavailable",
            ))
        })
    }
    fn provider_mutate(
        &self,
        _request: super::ProviderMutation,
    ) -> NodeFuture<'_, super::ProviderReceipt> {
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "provider management unavailable",
            ))
        })
    }
    fn provider_release_hosts(&self) -> NodeFuture<'_, super::NativeReleaseReport> {
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "provider host release unavailable",
            ))
        })
    }
    fn package_catalog<'a>(&'a self, _project: &'a str) -> NodeFuture<'a, super::PackageCatalog> {
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "package distribution unavailable",
            ))
        })
    }
    fn package_bundle<'a>(
        &'a self,
        _project: &'a str,
        _package: super::PackageRef,
    ) -> NodeFuture<'a, super::PackageBundle> {
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "package distribution unavailable",
            ))
        })
    }
    fn package_acquire<'a>(
        &'a self,
        _project: &'a str,
        _package: super::PackageRef,
    ) -> NodeFuture<'a, super::PackageAcquisition> {
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "package distribution unavailable",
            ))
        })
    }
    fn package_install<'a>(
        &'a self,
        _project: &'a str,
        _request: super::PackageInstall,
    ) -> NodeFuture<'a, super::NativeReceipt> {
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "package distribution unavailable",
            ))
        })
    }
    fn native_catalog<'a>(&'a self, _project: &'a str) -> NodeFuture<'a, super::NativeCatalog> {
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "native resources unavailable",
            ))
        })
    }
    fn native_manage<'a>(
        &'a self,
        _project: &'a str,
        _request: super::NativeManagement,
    ) -> NodeFuture<'a, super::NativeReceipt> {
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "native management unavailable",
            ))
        })
    }
    fn native_session_resources<'a>(
        &'a self,
        _id: &'a str,
    ) -> NodeFuture<'a, super::NativeSessionResources> {
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "native session resources unavailable",
            ))
        })
    }
    fn native_apply<'a>(
        &'a self,
        _id: &'a str,
        _request: super::NativeApply,
    ) -> NodeFuture<'a, super::NativeOperation> {
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "native apply unavailable",
            ))
        })
    }
    fn native_create<'a>(
        &'a self,
        _project: &'a str,
        _request: super::NativeCreate,
    ) -> NodeFuture<'a, String> {
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "native creation unavailable",
            ))
        })
    }
    fn native_release<'a>(
        &'a self,
        _project: &'a str,
        _scope: super::NativeScope,
    ) -> NodeFuture<'a, super::NativeReleaseReport> {
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "owned idle release unavailable",
            ))
        })
    }
    fn native_saved_history<'a>(
        &'a self,
        _id: &'a str,
        _cursor: Option<String>,
    ) -> NodeFuture<'a, Value> {
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "saved native history unavailable",
            ))
        })
    }
    fn authoring<'a>(
        &'a self,
        _project: &'a str,
        _request: super::AuthoringRequest,
    ) -> NodeFuture<'a, super::AuthoringState> {
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node authoring unavailable",
            ))
        })
    }

    fn health(&self) -> NodeFuture<'_, Health> {
        let _ = (self,);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn auth(&self) -> NodeFuture<'_, AuthState> {
        let _ = (self,);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn login<'a>(&'a self, password: &'a str) -> NodeFuture<'a, AuthState> {
        let _ = (self, password);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn logout(&self) -> NodeFuture<'_, AuthState> {
        let _ = (self,);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn capabilities(&self) -> NodeFuture<'_, Capabilities>;

    fn projects(&self) -> NodeFuture<'_, Vec<Project>>;

    fn rename_project<'a>(&'a self, project_id: &'a str, name: &'a str) -> NodeFuture<'a, Project> {
        let _ = (self, project_id, name);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn project_folders<'a>(&'a self, path: &'a str) -> NodeFuture<'a, ProjectFolders> {
        let _ = (self, path);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn create_project<'a>(
        &'a self,
        name: &'a str,
        path: Option<&'a str>,
        init_git: bool,
    ) -> NodeFuture<'a, Project> {
        let _ = (self, name, path, init_git);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn files<'a>(&'a self, project_id: &'a str, path: &'a str) -> NodeFuture<'a, FileListing> {
        let _ = (self, project_id, path);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn file<'a>(&'a self, project_id: &'a str, path: &'a str) -> NodeFuture<'a, FileContent> {
        let _ = (self, project_id, path);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    /// Read a bounded project-relative raster through the authoritative owner.
    fn image<'a>(
        &'a self,
        project_id: &'a str,
        path: &'a str,
    ) -> NodeFuture<'a, super::ProjectImage> {
        let _ = (self, project_id, path);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn git_status<'a>(&'a self, project_id: &'a str) -> NodeFuture<'a, GitStatus> {
        let _ = (self, project_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn diff<'a>(
        &'a self,
        project_id: &'a str,
        path: &'a str,
        staged: bool,
    ) -> NodeFuture<'a, Diff> {
        let _ = (self, project_id, path, staged);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn agent_backends(&self) -> NodeFuture<'_, Vec<AgentBackendOption>> {
        let _ = (self,);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn session_agent_binding<'a>(&'a self, id: &'a str) -> NodeFuture<'a, SessionAgentBinding> {
        let _ = (self, id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn create_session_with_backend<'a>(
        &'a self,
        project_id: Option<&'a str>,
        backend: &'a str,
    ) -> NodeFuture<'a, String> {
        let _ = (self, project_id, backend);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn sessions(&self) -> NodeFuture<'_, Vec<SessionSummary>>;

    fn create_session<'a>(&'a self, project_id: Option<&'a str>) -> NodeFuture<'a, String> {
        let _ = (self, project_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn get_session<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, SessionSummary> {
        let _ = (self, session_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn rename_session<'a>(
        &'a self,
        session_id: &'a str,
        title: &'a str,
    ) -> NodeFuture<'a, SessionMutation> {
        let _ = (self, session_id, title);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn session_preferences<'a>(
        &'a self,
        session_id: &'a str,
        archived: Option<bool>,
        pinned: Option<bool>,
    ) -> NodeFuture<'a, SessionPreferences> {
        let _ = (self, session_id, archived, pinned);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn fork_session<'a>(
        &'a self,
        session_id: &'a str,
        entry_id: Option<&'a str>,
        title: Option<&'a str>,
    ) -> NodeFuture<'a, SessionMutation> {
        let _ = (self, session_id, entry_id, title);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn clone_session<'a>(
        &'a self,
        session_id: &'a str,
        title: Option<&'a str>,
    ) -> NodeFuture<'a, SessionMutation> {
        let _ = (self, session_id, title);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn delete_session<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, ()> {
        let _ = (self, session_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn open_session<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, SessionSummary>;

    fn snapshot<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, Snapshot>;

    fn snapshot_with_limit<'a>(
        &'a self,
        session_id: &'a str,
        limit: u64,
    ) -> NodeFuture<'a, Snapshot> {
        let _ = (self, session_id, limit);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn session_access<'a>(&'a self, id: &'a str) -> NodeFuture<'a, SessionAccess> {
        let _ = (self, id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn session_configuration<'a>(&'a self, id: &'a str) -> NodeFuture<'a, SessionConfiguration> {
        let _ = (self, id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn set_session_config_option<'a>(
        &'a self,
        id: &'a str,
        config_id: &'a str,
        value: Value,
    ) -> NodeFuture<'a, Value> {
        let _ = (self, id, config_id, value);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn session_branch<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, SessionBranch> {
        let _ = (self, session_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn history<'a>(
        &'a self,
        session_id: &'a str,
        after_seq: Option<u64>,
        before_seq: Option<u64>,
        limit: u64,
    ) -> NodeFuture<'a, History> {
        let _ = (self, session_id, after_seq, before_seq, limit);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn runtime_state<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, RuntimeState> {
        let _ = (self, session_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn active_tools_state<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, Vec<String>> {
        let _ = (self, session_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn models<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, Vec<ModelDescriptor>> {
        let _ = (self, session_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn auth_status<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, AuthStatus> {
        let _ = (self, session_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn thinking_levels<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, Vec<String>> {
        let _ = (self, session_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn tools<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, ToolCatalog> {
        let _ = (self, session_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn set_model<'a>(&'a self, session_id: &'a str, model_id: &'a str) -> NodeFuture<'a, Value> {
        let _ = (self, session_id, model_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn set_thinking_level<'a>(
        &'a self,
        session_id: &'a str,
        level: &'a str,
    ) -> NodeFuture<'a, Value> {
        let _ = (self, session_id, level);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn set_active_tools<'a>(
        &'a self,
        session_id: &'a str,
        tools: &'a [String],
    ) -> NodeFuture<'a, Value> {
        let _ = (self, session_id, tools);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn resume_pending<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, Value> {
        let _ = (self, session_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn compact<'a>(
        &'a self,
        session_id: &'a str,
        custom_instructions: Option<&'a str>,
    ) -> NodeFuture<'a, Value> {
        let _ = (self, session_id, custom_instructions);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn queue<'a>(
        &'a self,
        session_id: &'a str,
        include_completed: bool,
    ) -> NodeFuture<'a, QueueSnapshot> {
        let _ = (self, session_id, include_completed);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn queue_message<'a>(
        &'a self,
        session_id: &'a str,
        message: &'a str,
        deliver_as: &'a str,
    ) -> NodeFuture<'a, QueueMutation> {
        let _ = (self, session_id, message, deliver_as);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn cancel_queued<'a>(
        &'a self,
        session_id: &'a str,
        entry_id: &'a str,
    ) -> NodeFuture<'a, QueueCancellation> {
        let _ = (self, session_id, entry_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn clear_queue<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, Value> {
        let _ = (self, session_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn dialog_response<'a>(
        &'a self,
        session_id: &'a str,
        request_id: &'a str,
        value: Value,
    ) -> NodeFuture<'a, Value> {
        let _ = (self, session_id, request_id, value);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn cancel_dialog<'a>(
        &'a self,
        session_id: &'a str,
        request_id: &'a str,
    ) -> NodeFuture<'a, Value> {
        let _ = (self, session_id, request_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn prompt<'a>(
        &'a self,
        session_id: &'a str,
        message: &'a str,
    ) -> NodeFuture<'a, HostResponse<Value>> {
        let _ = (self, session_id, message);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    /// Ordered native prompt input. Unsupported by default for older adapters.
    fn prompt_content<'a>(
        &'a self,
        session_id: &'a str,
        content_blocks: &'a Value,
    ) -> NodeFuture<'a, HostResponse<Value>> {
        let _ = (self, session_id, content_blocks);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn abort<'a>(&'a self, session_id: &'a str) -> NodeFuture<'a, HostResponse<Value>> {
        let _ = (self, session_id);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }

    fn subscribe<'a>(&'a self, session_id: &'a str, after: u64) -> NodeFuture<'a, NodeEventStream> {
        let _ = (self, session_id, after);
        Box::pin(async {
            Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "node operation unsupported",
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        rc::Rc,
        task::{Context, Poll, Waker},
    };

    // Deliberately implements only the original five methods and owns !Send
    // state. This must remain usable behind dyn NodeApi on a local executor.
    struct LegacyMock(Rc<()>);

    impl NodeApi for LegacyMock {
        fn capabilities(&self) -> NodeFuture<'_, Capabilities> {
            let local = self.0.clone();
            Box::pin(async move {
                drop(local);
                Err(NodeError::new(NodeErrorKind::NotFound, "mock"))
            })
        }
        fn projects(&self) -> NodeFuture<'_, Vec<Project>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn sessions(&self) -> NodeFuture<'_, Vec<SessionSummary>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn open_session<'a>(&'a self, _: &'a str) -> NodeFuture<'a, SessionSummary> {
            Box::pin(async { Err(NodeError::new(NodeErrorKind::NotFound, "mock")) })
        }
        fn snapshot<'a>(&'a self, _: &'a str) -> NodeFuture<'a, Snapshot> {
            Box::pin(async { Err(NodeError::new(NodeErrorKind::NotFound, "mock")) })
        }
    }

    fn ready<T>(mut future: NodeFuture<'_, T>) -> Result<T, NodeError> {
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(result) => result,
            Poll::Pending => panic!("mock unexpectedly pending"),
        }
    }

    #[test]
    fn legacy_mock_is_object_safe_and_may_hold_non_send_state() {
        let mock = LegacyMock(Rc::new(()));
        let api: &dyn NodeApi = &mock;
        assert_eq!(ready(api.projects()).unwrap(), []);
        assert_eq!(ready(api.sessions()).unwrap(), []);
        assert_eq!(
            ready(api.capabilities()).unwrap_err().kind,
            NodeErrorKind::NotFound
        );
        assert_eq!(
            ready(api.health()).unwrap_err().kind,
            NodeErrorKind::Unsupported
        );
        assert_eq!(
            ready(api.session_access("id")).unwrap_err().kind,
            NodeErrorKind::Unsupported
        );
        assert_eq!(
            ready(api.set_session_config_option("id", "model", Value::Null))
                .unwrap_err()
                .kind,
            NodeErrorKind::Unsupported
        );
        // Event streams need not implement Debug (avoid unwrap_err's T: Debug).
        assert!(matches!(
            ready(api.subscribe("id", 0)),
            Err(NodeError {
                kind: NodeErrorKind::Unsupported,
                ..
            })
        ));
    }

    #[test]
    fn event_stream_contract_accepts_browser_local_state_without_a_runtime() {
        struct LocalEvents(Rc<()>);
        impl futures_core::Stream for LocalEvents {
            type Item = Result<WireEvent, NodeError>;
            fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
                let _ = &self.0;
                Poll::Ready(None)
            }
        }
        let _: NodeEventStream = Box::pin(LocalEvents(Rc::new(())));
    }
}
