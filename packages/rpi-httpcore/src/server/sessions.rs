use super::{
    response::{optional_json, HttpReply},
    AppState,
};
use crate::security::{self, RequestAuthorization};
use axum::{
    extract::{Extension, Json, Path, Query, State},
    response::IntoResponse,
};
use rpi_extcore::{
    protocol::ApiCommand,
    reply::{IntoReply, ReplyKind, ServiceReply},
    service::{
        self, CreateSessionRequest, ForkRequest, MetadataRequest, RenameRequest, RunTimingsQuery,
        SessionListQuery,
    },
};
use serde_json::json;

pub(super) async fn create_session(
    State(state): State<AppState>,
    Extension(authorization): Extension<RequestAuthorization>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let Some(_commit_fence) = security::commit_fence(&state.security, &authorization).await else {
        return security::unauthorized_response();
    };
    let request: CreateSessionRequest = match optional_json(&body) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    HttpReply(
        service::create_session(&state.backend, request)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) async fn rename_session(
    State(state): State<AppState>,
    Extension(authorization): Extension<RequestAuthorization>,
    Path(id): Path<String>,
    Json(request): Json<RenameRequest>,
) -> axum::response::Response {
    let Some(_commit_fence) = security::commit_fence(&state.security, &authorization).await else {
        return security::unauthorized_response();
    };
    HttpReply(
        service::rename_session(&state.backend, id, request)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) async fn session_metadata(
    State(state): State<AppState>,
    Extension(authorization): Extension<RequestAuthorization>,
    Path(id): Path<String>,
    Json(request): Json<MetadataRequest>,
) -> axum::response::Response {
    let Some(_commit_fence) = security::commit_fence(&state.security, &authorization).await else {
        return security::unauthorized_response();
    };
    HttpReply(
        service::session_metadata(&state.backend, id, request)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) async fn fork_session(
    State(state): State<AppState>,
    Extension(authorization): Extension<RequestAuthorization>,
    Path(id): Path<String>,
    Json(request): Json<ForkRequest>,
) -> axum::response::Response {
    let Some(_commit_fence) = security::commit_fence(&state.security, &authorization).await else {
        return security::unauthorized_response();
    };
    HttpReply(
        service::fork_session(&state.backend, id, request)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) async fn clone_session(
    State(state): State<AppState>,
    Extension(authorization): Extension<RequestAuthorization>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let Some(_commit_fence) = security::commit_fence(&state.security, &authorization).await else {
        return security::unauthorized_response();
    };
    let request: ForkRequest = match optional_json(&body) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    HttpReply(
        service::clone_session(&state.backend, id, request)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) async fn open_session(
    State(state): State<AppState>,
    Extension(authorization): Extension<RequestAuthorization>,
    Path(id): Path<String>,
) -> axum::response::Response {
    let Some(_commit_fence) = security::commit_fence(&state.security, &authorization).await else {
        return security::unauthorized_response();
    };
    HttpReply(service::open_session(&state.backend, id).await.into_reply()).into_response()
}

pub(super) async fn list_sessions(
    State(state): State<AppState>,
    Query(query): Query<SessionListQuery>,
) -> axum::response::Response {
    HttpReply(
        service::list_sessions(&state.backend, query)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    HttpReply(service::get_session(&state.backend, id).await.into_reply()).into_response()
}

pub(super) async fn delete_session(
    State(state): State<AppState>,
    Extension(authorization): Extension<RequestAuthorization>,
    Path(id): Path<String>,
) -> axum::response::Response {
    let Some(_commit_fence) = security::commit_fence(&state.security, &authorization).await else {
        return security::unauthorized_response();
    };
    HttpReply(
        service::delete_session(&state.backend, id)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) async fn command(
    State(state): State<AppState>,
    Extension(authorization): Extension<RequestAuthorization>,
    Path(id): Path<String>,
    Json(command): Json<ApiCommand>,
) -> axum::response::Response {
    let Some(_commit_fence) = security::commit_fence(&state.security, &authorization).await else {
        return security::unauthorized_response();
    };
    HttpReply(
        service::command(&state.backend, id, command)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) async fn run_timings(
    State(state): State<AppState>,
    Path(id): Path<String>,
    query: Result<Json<RunTimingsQuery>, axum::extract::rejection::JsonRejection>,
) -> HttpReply {
    let Json(query) = match query {
        Ok(query) => query,
        Err(_) => {
            return HttpReply(ServiceReply {
                kind: ReplyKind::InvalidInput,
                value: json!({"error":"invalid JSON request body"}),
            });
        }
    };
    HttpReply(service::run_timings(&state.backend, id, query).await)
}

pub(super) async fn message_summaries(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<service::MessageSummaryQuery>,
) -> HttpReply {
    HttpReply(service::message_summaries(&state.backend, id, query).await)
}

pub(super) async fn message_window(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<service::MessageWindowQuery>,
) -> HttpReply {
    HttpReply(service::message_window(&state.backend, id, query).await)
}

pub(super) async fn snapshot(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(options): Query<service::SnapshotOptions>,
) -> axum::response::Response {
    HttpReply(
        service::snapshot_with_options(&state.backend, id, options)
            .await
            .into_reply(),
    )
    .into_response()
}

// An absent body keeps the legacy default, but a malformed supplied body must
// never turn into a successful creation with silently discarded parameters.

pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/api/v1/sessions",
            axum::routing::post(create_session).get(list_sessions),
        )
        .route(
            "/api/v1/sessions/{id}",
            axum::routing::get(get_session).delete(delete_session),
        )
        .route(
            "/api/v1/sessions/{id}/open",
            axum::routing::post(open_session),
        )
        .route(
            "/api/v1/sessions/{id}/fork",
            axum::routing::post(fork_session),
        )
        .route(
            "/api/v1/sessions/{id}/clone",
            axum::routing::post(clone_session),
        )
        .route(
            "/api/v1/sessions/{id}/rename",
            axum::routing::post(rename_session),
        )
        .route(
            "/api/v1/sessions/{id}/metadata",
            axum::routing::post(session_metadata),
        )
        .route(
            "/api/v1/sessions/{id}/commands",
            axum::routing::post(command),
        )
        .route(
            "/api/v1/sessions/{id}/snapshot",
            axum::routing::get(snapshot),
        )
        .route(
            "/api/v1/sessions/{id}/message-summaries",
            axum::routing::get(message_summaries),
        )
        .route(
            "/api/v1/sessions/{id}/message-window",
            axum::routing::get(message_window),
        )
        .route(
            "/api/v1/sessions/{id}/run-timings/query",
            axum::routing::post(run_timings),
        )
}
