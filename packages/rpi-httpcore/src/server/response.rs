use axum::{extract::rejection::JsonRejection, http::StatusCode, response::IntoResponse, Json};
use rpi_extcore::reply::{IntoReply, ReplyKind, ServiceReply, ServiceResult};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

pub(super) struct HttpReply(pub(super) ServiceReply);
impl IntoResponse for HttpReply {
    fn into_response(self) -> axum::response::Response {
        let status = match self.0.kind {
            ReplyKind::Success => StatusCode::OK,
            ReplyKind::Created => StatusCode::CREATED,
            ReplyKind::InvalidInput => StatusCode::BAD_REQUEST,
            ReplyKind::NotFound => StatusCode::NOT_FOUND,
            ReplyKind::AccessDenied => StatusCode::FORBIDDEN,
            ReplyKind::LimitExceeded => StatusCode::PAYLOAD_TOO_LARGE,
            ReplyKind::UnsupportedContent => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ReplyKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            ReplyKind::HostUnavailable => StatusCode::BAD_GATEWAY,
            ReplyKind::Timeout => StatusCode::GATEWAY_TIMEOUT,
            ReplyKind::Unsupported => StatusCode::NOT_IMPLEMENTED,
            ReplyKind::Conflict => StatusCode::CONFLICT,
            ReplyKind::Empty => StatusCode::NO_CONTENT,
            ReplyKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        };
        if matches!(self.0.kind, ReplyKind::Empty) {
            status.into_response()
        } else {
            (status, Json(self.0.value)).into_response()
        }
    }
}
// Transport-only conversion: validation and error categories stay in core.
pub(super) fn typed_reply<T: Serialize>(result: ServiceResult<T>) -> HttpReply {
    HttpReply(
        result
            .and_then(|value| {
                serde_json::to_value(value).map_err(|_| {
                    rpi_extcore::reply::failure(
                        ReplyKind::Internal,
                        "response serialization failed",
                    )
                })
            })
            .into_reply(),
    )
}

pub(super) fn strict_json<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, HttpReply> {
    body.map(|Json(value)| value).map_err(|_| {
        HttpReply(
            (
                ReplyKind::InvalidInput,
                json!({"error":"invalid JSON request body"}),
            )
                .into_reply(),
        )
    })
}

pub(super) fn optional_json<T: DeserializeOwned + Default>(
    body: &[u8],
) -> Result<T, (StatusCode, Json<Value>)> {
    if body.is_empty() {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid JSON request body"})),
        )
    })
}
