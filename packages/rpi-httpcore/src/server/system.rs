use super::AppState;
use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::IntoResponse,
};
use rpi_extcore::protocol;
use serde::Deserialize;
use serde_json::{json, Value};

pub(super) async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"ok":true,"service":state.service_name,"protocol":"rpi"}))
}
pub(super) async fn capabilities() -> Json<Value> {
    Json(protocol::capabilities())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TtsRequest {
    text: String,
    voice: Option<String>,
}

pub(super) async fn tts(Json(request): Json<TtsRequest>) -> axum::response::Response {
    let text = request.text.trim().to_string();
    let voice = request
        .voice
        .unwrap_or_else(|| "zh-CN-XiaoxiaoNeural".to_string());
    if text.is_empty() || text.chars().count() > 12_000 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid TTS text"})),
        )
            .into_response();
    }
    if voice.is_empty()
        || voice.len() > 96
        || !voice
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid TTS voice"})),
        )
            .into_response();
    }
    let result = tokio::task::spawn_blocking(move || rpi_tts::synthesize(&text, &voice)).await;
    match result {
        Ok(Ok(audio)) => (
            [
                (axum::http::header::CONTENT_TYPE, "audio/mpeg"),
                (axum::http::header::CACHE_CONTROL, "no-store"),
            ],
            audio,
        )
            .into_response(),
        Ok(Err(error)) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("TTS synthesis failed: {error}")})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("TTS worker failed: {error}")})),
        )
            .into_response(),
    }
}

pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/v1/health", axum::routing::get(health))
        .route("/api/v1/capabilities", axum::routing::get(capabilities))
        .route("/api/v1/tts", axum::routing::post(tts))
}
