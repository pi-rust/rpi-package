use super::*;
use axum::{
    Json,
    extract::{Extension, State},
    http::{HeaderMap, header},
};
use rpi_http_runtime::{SharingControl, pairing, security};
use rpi_node_core::workspace::LocalWorkspace;
use serde_json::json;
use tokio::sync::{mpsc, watch};

struct Fixture {
    state: Arc<ServerState>,
    pending: mpsc::Receiver<ExecutorRequest>,
    auth: AuthLease,
    headers: HeaderMap,
    _shutdown: watch::Sender<bool>,
    _root: tempfile::TempDir,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        // Invalid executable ensures these authorization tests cannot launch any
        // process, even if an assertion exposes an accidental owner invocation.
        let workspace = Arc::new(
            LocalWorkspace::with_session_dir(
                root.path().join("must-not-launch-agent"),
                root.path().join("data"),
                Some(root.path().join("sessions")),
            )
            .unwrap(),
        );
        let (requests, pending) = mpsc::channel(1);
        let (shutdown, stopped) = watch::channel(false);
        let state = Arc::new(ServerState {
            workspace,
            access: Arc::new(rpi_http_runtime::AccessState {
                security: security::Security::new(
                    "isolated-test",
                    vec!["localhost".into()],
                    "executor-tests",
                )
                .unwrap(),
                shutdown: stopped,
                node_id: "executor-tests".into(),
                sharing: SharingControl::new(true),
                pairing: Arc::new(pairing::Pairing::new("executor-tests".into())),
            }),
            assets: None,
            web_proxy: false,
            requests,
            admission: Arc::new(tokio::sync::Semaphore::new(64)),
            realtime: Arc::new(crate::realtime::RealtimeState::new()),
        });
        let response = security::login(
            State(state.access.clone()),
            Extension(state.sharing.snapshot()),
            Json(serde_json::from_value(json!({"password":"isolated-test"})).unwrap()),
        )
        .await;
        assert_eq!(response.status(), 200);
        let cookie = response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, cookie.parse().unwrap());
        let auth = state.security.authenticate(&headers, 0).await.unwrap();
        Self {
            state,
            pending,
            auth,
            headers,
            _shutdown: shutdown,
            _root: root,
        }
    }
    fn reservation(&self) -> OperationRef {
        self.state
            .workspace
            .remote_operations()
            .reserve(
                self.auth.principal(),
                "client",
                "session",
                RealtimeOperationKind::DialogResponse,
            )
            .unwrap()
    }
    fn message(
        &self,
        action: RealtimeAction,
    ) -> (
        ExecutorRequest,
        oneshot::Receiver<Result<RealtimeReply, RealtimeError>>,
    ) {
        let (reply, result) = oneshot::channel();
        let permit = self.state.admission.clone().try_acquire_owned().unwrap();
        (
            ExecutorRequest::Realtime(RealtimeRequest {
                action,
                auth: self.auth.clone(),
                reply,
                _permit: permit,
            }),
            result,
        )
    }
    async fn logout(&self) {
        assert_eq!(
            security::logout(State(self.state.access.clone()), self.headers.clone())
                .await
                .status(),
            200
        );
    }
}

#[tokio::test]
async fn revoke_while_waiting_for_queue_capacity_prevents_publication() {
    let mut fixture = Fixture::new().await;
    let reference = fixture.reservation();
    let (blocker, _) = fixture.message(RealtimeAction::Status {
        operation: reference.clone(),
    });
    fixture
        .state
        .requests
        .send(blocker)
        .await
        .unwrap_or_else(|_| panic!("queue closed"));
    let (message, _) = fixture.message(RealtimeAction::Execute {
        operation: reference.clone(),
        payload: Value::Null,
    });
    let state = fixture.state.clone();
    let auth = fixture.auth.clone();
    let task = tokio::spawn(async move { publish_authenticated(&state, &auth, message).await });
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    fixture.logout().await;
    // Revocation must wake a capacity waiter even if no executor slot opens.
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .unwrap();
    assert_eq!(
        result.unwrap().unwrap_err().code,
        RealtimeErrorCode::AccessDenied
    );
    drop(fixture.pending.recv().await.unwrap());
    assert!(fixture.pending.try_recv().is_err());
    assert_eq!(
        fixture
            .state
            .workspace
            .remote_operations()
            .status(fixture.auth.principal(), &reference)
            .unwrap(),
        OperationStatus::Reserved
    );
    assert_eq!(fixture.state.admission.available_permits(), 64);
}

#[tokio::test]
async fn logout_after_publication_before_dequeue_cannot_claim_operation() {
    let mut fixture = Fixture::new().await;
    let reference = fixture.reservation();
    let (message, result) = fixture.message(RealtimeAction::Execute {
        operation: reference.clone(),
        payload: Value::Null,
    });
    publish_authenticated(&fixture.state, &fixture.auth, message)
        .await
        .unwrap();
    fixture.logout().await;
    execute(fixture.state.clone(), fixture.pending.recv().await.unwrap()).await;
    assert!(
        matches!(result.await.unwrap(), Err(err) if err.code == RealtimeErrorCode::AccessDenied)
    );
    assert_eq!(
        fixture
            .state
            .workspace
            .remote_operations()
            .status(fixture.auth.principal(), &reference)
            .unwrap(),
        OperationStatus::Reserved
    );
}

#[tokio::test]
async fn off_on_fences_old_queued_authorization_even_when_enabled_again() {
    let mut fixture = Fixture::new().await;
    let reference = fixture.reservation();
    let (message, result) = fixture.message(RealtimeAction::Execute {
        operation: reference.clone(),
        payload: Value::Null,
    });
    publish_authenticated(&fixture.state, &fixture.auth, message)
        .await
        .unwrap();
    fixture.state.sharing.set_enabled(false);
    fixture.state.sharing.set_enabled(true);
    execute(fixture.state.clone(), fixture.pending.recv().await.unwrap()).await;
    assert!(
        matches!(result.await.unwrap(), Err(err) if err.code == RealtimeErrorCode::AccessDenied)
    );
    assert_eq!(
        fixture
            .state
            .workspace
            .remote_operations()
            .status(fixture.auth.principal(), &reference)
            .unwrap(),
        OperationStatus::Reserved
    );
}

#[tokio::test]
async fn dropped_waiter_does_not_discard_known_result_or_rerun_it() {
    let fixture = Fixture::new().await;
    let reference = fixture.reservation();
    // Dialog validation rejects before any owner/Agent I/O. This is a known
    // result, retained even if the network peer dropped its response receiver.
    let (message, result) = fixture.message(RealtimeAction::Execute {
        operation: reference.clone(),
        payload: json!({"responseId":"forged"}),
    });
    drop(result);
    execute(fixture.state.clone(), message).await;
    let status = fixture
        .state
        .workspace
        .remote_operations()
        .status(fixture.auth.principal(), &reference)
        .unwrap();
    assert!(
        matches!(&status, OperationStatus::KnownResult { outcome: OperationOutcome::Failed { error } } if error.code == RealtimeErrorCode::InvalidInput)
    );
    let (duplicate, response) = fixture.message(RealtimeAction::Execute {
        operation: reference,
        payload: json!({"responseId":"forged"}),
    });
    execute(fixture.state.clone(), duplicate).await;
    assert!(
        matches!(response.await.unwrap(), Ok(RealtimeReply::Status(retained)) if retained == status)
    );
}

#[tokio::test]
async fn network_request_is_send_and_shares_the_existing_admission_budget() {
    let mut fixture = Fixture::new().await;
    let state = fixture.state.clone();
    let auth = fixture.auth.clone();
    let task = tokio::spawn(async move {
        request(
            state,
            auth,
            RealtimeAction::Reserve {
                client_instance_id: "client".into(),
                session_id: "session".into(),
                kind: RealtimeOperationKind::DialogResponse,
            },
        )
        .await
    });
    let message = fixture.pending.recv().await.unwrap();
    assert_eq!(fixture.state.admission.available_permits(), 63);
    execute(fixture.state.clone(), message).await;
    let reference = match task
        .await
        .unwrap()
        .unwrap_or_else(|_| panic!("reserve failed"))
    {
        RealtimeReply::Reserved(reference) => reference,
        _ => panic!("wrong response"),
    };
    assert_eq!(fixture.state.admission.available_permits(), 64);
    let (message, result) = fixture.message(RealtimeAction::Status {
        operation: reference,
    });
    execute(fixture.state.clone(), message).await;
    assert!(matches!(
        result.await.unwrap(),
        Ok(RealtimeReply::Status(OperationStatus::Reserved))
    ));
    let all = fixture
        .state
        .admission
        .clone()
        .acquire_many_owned(64)
        .await
        .unwrap();
    let rejected = request(
        fixture.state.clone(),
        fixture.auth.clone(),
        RealtimeAction::Subscribe {
            session_id: "session".into(),
            after: 0,
        },
    )
    .await;
    assert!(matches!(rejected, Err(err) if err.code == RealtimeErrorCode::LimitExceeded));
    drop(all);
}

#[tokio::test]
async fn shutdown_before_dispatch_leaves_the_reservation_unexecuted() {
    let fixture = Fixture::new().await;
    let reference = fixture.reservation();
    let (message, result) = fixture.message(RealtimeAction::Execute {
        operation: reference.clone(),
        payload: Value::Null,
    });
    fixture._shutdown.send_replace(true);
    execute(fixture.state.clone(), message).await;
    assert!(
        matches!(result.await.unwrap(), Err(err) if err.code == RealtimeErrorCode::Unavailable)
    );
    assert_eq!(
        fixture
            .state
            .workspace
            .remote_operations()
            .status(fixture.auth.principal(), &reference)
            .unwrap(),
        OperationStatus::Reserved
    );
}

#[test]
fn post_owner_failures_are_uncertain_not_safe_rejections() {
    use rpi_node_core::workspace::{LocalError, WorkspaceError};
    let failed =
        routes::ApiError::from(WorkspaceError::Local(LocalError::Shutdown)).into_realtime();
    assert_eq!(failed.code, RealtimeErrorCode::OutcomeUnknown);
    assert!(!failed.message.contains("workspace"));
}
