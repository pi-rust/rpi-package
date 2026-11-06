//! Shared admission and `LocalSet` dispatch. Network handlers only send owned,
//! Send messages; the supplied workspace remains the sole !Send RPC owner.
use crate::{ServerState, routes, security::AuthLease};
use rpi_node_api::api::realtime::{
    OperationOutcome, OperationRef, OperationStatus, RealtimeError, RealtimeErrorCode,
    RealtimeOperationKind,
};
use rpi_node_core::workspace::{
    WorkspaceEventStream,
    operations::{OperationClaim, OperationError},
};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, oneshot};

pub(crate) enum ExecutorRequest {
    Http(routes::ApiRequest),
    Realtime(RealtimeRequest),
}
// Not Debug: these actions can contain private prompt or Dialog data.
pub(crate) enum RealtimeAction {
    Reserve {
        client_instance_id: String,
        session_id: String,
        kind: RealtimeOperationKind,
    },
    Execute {
        operation: OperationRef,
        payload: Value,
    },
    Status {
        operation: OperationRef,
    },
    Query {
        session_id: String,
        payload: Value,
    },
    Subscribe {
        session_id: String,
        after: u64,
    },
}
pub(crate) enum RealtimeReply {
    Reserved(OperationRef),
    Status(OperationStatus),
    Result(Value),
    Subscribed(WorkspaceEventStream),
}
pub(crate) struct RealtimeRequest {
    action: RealtimeAction,
    auth: AuthLease,
    reply: oneshot::Sender<Result<RealtimeReply, RealtimeError>>,
    _permit: OwnedSemaphorePermit,
}

pub(crate) fn error(code: RealtimeErrorCode) -> RealtimeError {
    let message = match code {
        RealtimeErrorCode::InvalidInput => "Invalid session request",
        RealtimeErrorCode::NotFound => "Session not found",
        RealtimeErrorCode::AccessDenied => "Session access denied",
        RealtimeErrorCode::Conflict => "Operation payload conflicts with its original request",
        RealtimeErrorCode::Unsupported => "Session operation unsupported",
        RealtimeErrorCode::UnsupportedContent => "Session content unsupported",
        RealtimeErrorCode::Unavailable => "Session transport unavailable",
        RealtimeErrorCode::LimitExceeded => "Session transport limit exceeded",
        RealtimeErrorCode::OutcomeUnknown => {
            "Operation outcome unknown; query status, do not execute again"
        }
        RealtimeErrorCode::Internal => "Session operation failed",
    };
    RealtimeError {
        code,
        message: message.into(),
    }
}
pub(crate) fn operation_error(value: OperationError) -> RealtimeError {
    error(match value {
        OperationError::InvalidInput => RealtimeErrorCode::InvalidInput,
        OperationError::AccessDenied => RealtimeErrorCode::AccessDenied,
        OperationError::Conflict => RealtimeErrorCode::Conflict,
        OperationError::UnknownOperation => RealtimeErrorCode::OutcomeUnknown,
        OperationError::LimitExceeded => RealtimeErrorCode::LimitExceeded,
        OperationError::Unavailable => RealtimeErrorCode::Unavailable,
    })
}

/// Reserve queue capacity before taking the auth/sharing commit locks. No lock
/// spans await or socket I/O. A cancelled waiter never publishes its request.
pub(crate) async fn publish_authenticated(
    state: &Arc<ServerState>,
    auth: &AuthLease,
    request: ExecutorRequest,
) -> Result<(), RealtimeError> {
    let slot = tokio::select! {
        biased;
        () = auth.invalidated(&state.sharing, &state.shutdown) => {
            return Err(error(if *state.shutdown.borrow() {
                RealtimeErrorCode::Unavailable
            } else { RealtimeErrorCode::AccessDenied }));
        },
        slot = state.requests.reserve() => slot.map_err(|_| error(RealtimeErrorCode::Unavailable))?,
    };
    let committed = state
        .security
        .while_authorized(auth, &state.sharing, || {
            if *state.shutdown.borrow() {
                return false;
            }
            slot.send(request);
            true
        })
        .await;
    match committed {
        Some(true) => Ok(()),
        Some(false) => Err(error(RealtimeErrorCode::Unavailable)),
        None => Err(error(RealtimeErrorCode::AccessDenied)),
    }
}

/// Called from a Send network task; no owner future is created here.
pub(crate) async fn request(
    state: Arc<ServerState>,
    auth: AuthLease,
    action: RealtimeAction,
) -> Result<RealtimeReply, RealtimeError> {
    let possibly_mutating = matches!(&action, RealtimeAction::Execute { .. });
    let permit = state
        .admission
        .clone()
        .try_acquire_owned()
        .map_err(|_| error(RealtimeErrorCode::LimitExceeded))?;
    let (reply, result) = oneshot::channel();
    let message = ExecutorRequest::Realtime(RealtimeRequest {
        action,
        auth: auth.clone(),
        reply,
        _permit: permit,
    });
    publish_authenticated(&state, &auth, message).await?;
    // Only an execute can have affected the Agent. Other requests are safe to
    // retry as reads/control setup, even if their executor reply is lost.
    result.await.map_err(|_| {
        error(if possibly_mutating {
            RealtimeErrorCode::OutcomeUnknown
        } else {
            RealtimeErrorCode::Unavailable
        })
    })?
}

pub(crate) async fn execute(state: Arc<ServerState>, request: ExecutorRequest) {
    match request {
        ExecutorRequest::Http(request) => routes::execute(state, request).await,
        ExecutorRequest::Realtime(request) => execute_realtime(state, request).await,
    }
}

enum Prepared {
    Reply(RealtimeReply),
    Execute(OperationClaim),
    Query { session_id: String, payload: Value },
    Subscribe { session_id: String, after: u64 },
}
#[allow(clippy::too_many_lines)] // One admission/settlement lifecycle for each request.
async fn execute_realtime(state: Arc<ServerState>, request: RealtimeRequest) {
    let RealtimeRequest {
        action,
        auth,
        reply,
        _permit,
    } = request;
    // This is the execution authorization linearization point. Subsequent
    // revocation suppresses delivery but does not cancel already-granted work.
    let prepared = state
        .security
        .while_authorized(&auth, &state.sharing, || {
            if *state.shutdown.borrow() {
                return Err(error(RealtimeErrorCode::Unavailable));
            }
            let registry = state.workspace.remote_operations();
            match action {
                RealtimeAction::Reserve {
                    client_instance_id,
                    session_id,
                    kind,
                } => registry
                    .reserve(auth.principal(), &client_instance_id, &session_id, kind)
                    .map(|reference| Prepared::Reply(RealtimeReply::Reserved(reference)))
                    .map_err(operation_error),
                RealtimeAction::Execute { operation, payload } => registry
                    .claim(auth.principal(), &operation, payload)
                    .map(Prepared::Execute)
                    .map_err(operation_error),
                RealtimeAction::Status { operation } => registry
                    .status(auth.principal(), &operation)
                    .map(|status| Prepared::Reply(RealtimeReply::Status(status)))
                    .map_err(operation_error),
                RealtimeAction::Query {
                    session_id,
                    payload,
                } => {
                    if !rpi_node_api::api::realtime::is_read_only_session_command(&payload) {
                        return Err(error(RealtimeErrorCode::InvalidInput));
                    }
                    Ok(Prepared::Query {
                        session_id,
                        payload,
                    })
                }
                RealtimeAction::Subscribe { session_id, after } => {
                    Ok(Prepared::Subscribe { session_id, after })
                }
            }
        })
        .await
        .unwrap_or_else(|| Err(error(RealtimeErrorCode::AccessDenied)));
    let result = match prepared {
        Ok(Prepared::Reply(reply)) => Ok(reply),
        Ok(Prepared::Execute(OperationClaim::Existing(status))) => {
            Ok(RealtimeReply::Status(status))
        }
        Ok(Prepared::Execute(OperationClaim::Execute(permit))) => {
            let outcome = match routes::session_operations::perform(
                &state.workspace,
                permit.session_id(),
                permit.kind(),
                permit.payload(),
            )
            .await
            {
                Ok(value) => OperationOutcome::Succeeded { value },
                Err(error) => OperationOutcome::Failed {
                    error: error.into_realtime(),
                },
            };
            permit
                .finish(outcome)
                .map(RealtimeReply::Status)
                .map_err(operation_error)
        }
        Ok(Prepared::Query {
            session_id,
            payload,
        }) => routes::session_operations::perform(
            &state.workspace,
            &session_id,
            RealtimeOperationKind::SessionCommand,
            &payload,
        )
        .await
        .map(RealtimeReply::Result)
        .map_err(|err| {
            let err = err.into_realtime();
            if err.code == RealtimeErrorCode::OutcomeUnknown {
                error(RealtimeErrorCode::Unavailable)
            } else {
                err
            }
        }),
        Ok(Prepared::Subscribe { session_id, after }) => state
            .workspace
            .subscribe(&session_id, after)
            .await
            .map(RealtimeReply::Subscribed)
            .map_err(|err| routes::ApiError::from(err).into_realtime()),
        Err(err) => Err(err),
    };
    // Even when a peer is gone, the operation above finishes and persists its
    // receipt. Revalidation does not disclose a result to a revoked login.
    let mut outgoing = Some((reply, result));
    let sent = state
        .security
        .while_authorized(&auth, &state.sharing, || {
            if let Some((reply, result)) = outgoing.take() {
                let _ = reply.send(result);
            }
        })
        .await;
    if sent.is_none() {
        if let Some((reply, _)) = outgoing.take() {
            let _ = reply.send(Err(error(RealtimeErrorCode::AccessDenied)));
        }
    }
}

#[cfg(test)]
mod tests;
