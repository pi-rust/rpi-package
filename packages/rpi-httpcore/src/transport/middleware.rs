use super::router::{admission_response, TransportState};
use axum::extract::State;

pub(super) async fn admit_owner(
    State(state): State<TransportState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let permit = match state.admission.try_admit().await {
        Ok(permit) => permit,
        Err(error) => return admission_response(error),
    };
    let response = next.run(request).await;
    drop(permit);
    response
}
