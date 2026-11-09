use super::{AppState, HttpReply};
use crate::security::{self, RequestAuthorization};
use axum::{
    extract::{Extension, Json, Multipart, Path, Query, State},
    response::{IntoResponse, Response},
};
use rpi_extcore::{
    reply::{IntoReply, ReplyKind},
    service::{
        self, CreateProjectRequest, FileQuery, ProjectFoldersQuery, ProjectMetadataRequest,
        RenameProjectRequest,
    },
};
use serde_json::json;

pub(super) async fn project_metadata(
    State(state): State<AppState>,
    Extension(authorization): Extension<RequestAuthorization>,
    Path(id): Path<String>,
    Json(request): Json<ProjectMetadataRequest>,
) -> Response {
    if !security::authorize_before_commit(&state.security, &authorization).await {
        return security::unauthorized_response();
    }
    HttpReply(
        service::project_metadata(&state.backend, id, request)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) async fn upload_project_file(
    State(state): State<AppState>,
    Extension(authorization): Extension<RequestAuthorization>,
    Path(id): Path<String>,
    mut multipart: Multipart,
) -> Response {
    if !security::authorize_before_commit(&state.security, &authorization).await {
        return security::unauthorized_response();
    }
    let Some(field) = (match multipart.next_field().await {
        Ok(field) => field,
        Err(_error) => {
            return HttpReply(
                (ReplyKind::InvalidInput, json!({"error":"invalid upload"})).into_reply(),
            )
            .into_response();
        }
    }) else {
        return HttpReply(
            (ReplyKind::InvalidInput, json!({"error":"file is required"})).into_reply(),
        )
        .into_response();
    };
    let filename = field.file_name().unwrap_or("upload").to_string();
    let bytes = match field.bytes().await {
        Ok(bytes) => bytes.to_vec(),
        Err(_error) => {
            return HttpReply(
                (
                    ReplyKind::InvalidInput,
                    json!({"error":"cannot read upload"}),
                )
                    .into_reply(),
            )
            .into_response();
        }
    };
    HttpReply(
        service::upload_project_file(&state.backend, id, filename, bytes)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) async fn list_projects(State(state): State<AppState>) -> axum::response::Response {
    HttpReply(service::list_projects(&state.backend).await.into_reply()).into_response()
}

pub(super) async fn list_project_folders(
    State(state): State<AppState>,
    Query(query): Query<ProjectFoldersQuery>,
) -> axum::response::Response {
    HttpReply(
        service::list_project_folders(&state.backend, query)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) async fn create_project(
    State(state): State<AppState>,
    Extension(authorization): Extension<RequestAuthorization>,
    Json(request): Json<CreateProjectRequest>,
) -> axum::response::Response {
    if !security::authorize_before_commit(&state.security, &authorization).await {
        return security::unauthorized_response();
    }
    HttpReply(
        service::create_project(&state.backend, request)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) async fn rename_project(
    State(state): State<AppState>,
    Extension(authorization): Extension<RequestAuthorization>,
    Path(id): Path<String>,
    Json(request): Json<RenameProjectRequest>,
) -> axum::response::Response {
    if !security::authorize_before_commit(&state.security, &authorization).await {
        return security::unauthorized_response();
    }
    HttpReply(
        service::rename_project(&state.backend, id, request)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) async fn project_files(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> axum::response::Response {
    HttpReply(
        service::project_files(&state.backend, id, query)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) async fn project_file(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> axum::response::Response {
    HttpReply(
        service::project_file(&state.backend, id, query)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) async fn project_image(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> axum::response::Response {
    match service::project_image(&state.backend, id, query).await {
        Ok(image) => (
            [(axum::http::header::CONTENT_TYPE, image.media_type)],
            image.bytes,
        )
            .into_response(),
        Err(error) => HttpReply(error.into_reply()).into_response(),
    }
}

pub(super) async fn project_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    HttpReply(
        service::project_status(&state.backend, id)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) async fn project_diff(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> axum::response::Response {
    HttpReply(
        service::project_diff(&state.backend, id, query)
            .await
            .into_reply(),
    )
    .into_response()
}

pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/api/v1/projects",
            axum::routing::get(list_projects).post(create_project),
        )
        .route(
            "/api/v1/project-folders",
            axum::routing::get(list_project_folders),
        )
        .route(
            "/api/v1/projects/{id}/rename",
            axum::routing::post(rename_project),
        )
        .route(
            "/api/v1/projects/{id}/metadata",
            axum::routing::post(project_metadata),
        )
        .route(
            "/api/v1/projects/{id}/files",
            axum::routing::get(project_files),
        )
        .route(
            "/api/v1/projects/{id}/upload",
            axum::routing::post(upload_project_file)
                .layer(axum::extract::DefaultBodyLimit::max(20 * 1024 * 1024)),
        )
        .route(
            "/api/v1/projects/{id}/file",
            axum::routing::get(project_file),
        )
        .route(
            "/api/v1/projects/{id}/image",
            axum::routing::get(project_image),
        )
        .route(
            "/api/v1/projects/{id}/git/status",
            axum::routing::get(project_status),
        )
        .route(
            "/api/v1/projects/{id}/git/diff",
            axum::routing::get(project_diff),
        )
}
