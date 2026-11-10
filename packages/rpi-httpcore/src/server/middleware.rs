use super::AppState;
use axum::{
    extract::State,
    response::{IntoResponse, Response},
};
use serde_json::json;

pub(super) async fn admit_owner(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let permit = match state.admission.try_admit().await {
        Ok(permit) => permit,
        Err(crate::access::AdmissionError::AtCapacity) => {
            return (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                axum::Json(json!({"error":"owner admission capacity exceeded"})),
            )
                .into_response();
        }
        Err(crate::access::AdmissionError::ShuttingDown) => {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(json!({"error":"service is shutting down"})),
            )
                .into_response();
        }
    };
    let response = next.run(request).await;
    drop(permit);
    response
}

pub(super) async fn touch_session_activity(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if let Some(id) = request
        .uri()
        .path()
        .strip_prefix("/api/v1/sessions/")
        .and_then(|path| path.split('/').next())
        .filter(|id| !id.is_empty())
    {
        state.backend.touch_session(id).await;
    }
    next.run(request).await
}

pub(super) async fn image_cache_control(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let image = request.uri().path().starts_with("/api/v1/projects/")
        && request.uri().path().ends_with("/image");
    let mut response = next.run(request).await;
    if image {
        response.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("private, no-store"),
        );
    }
    response
}
