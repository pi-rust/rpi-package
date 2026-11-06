//! One command/config/Dialog mapping for HTTP and realtime. No new owner or
//! arbitrary HTTP/JSON-RPC envelope forwarding is introduced here.
use super::{ApiError, StatusCode, text};
use rpi_node_api::api::realtime::RealtimeOperationKind;
use rpi_node_core::workspace::LocalWorkspace;
use serde_json::{Value, json};

pub(crate) async fn perform(
    workspace: &LocalWorkspace,
    session_id: &str,
    kind: RealtimeOperationKind,
    body: &Value,
) -> Result<Value, ApiError> {
    match kind {
        RealtimeOperationKind::SessionCommand => Ok(json!(
            workspace.command::<Value>(session_id, body.clone()).await?
        )),
        RealtimeOperationKind::SetConfiguration => {
            let value = body
                .get("value")
                .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "missing value".into()))?;
            Ok(workspace
                .set_session_config_option(session_id, text(body, "configId")?, value.clone())
                .await?)
        }
        RealtimeOperationKind::DialogResponse | RealtimeOperationKind::CancelDialog => {
            let request_id = text(body, "requestId")?;
            let fields = body.as_object().ok_or_else(|| {
                ApiError(StatusCode::BAD_REQUEST, "invalid dialog response".into())
            })?;
            if fields
                .keys()
                .any(|key| !matches!(key.as_str(), "requestId" | "answer" | "cancelled"))
            {
                return Err(ApiError(
                    StatusCode::BAD_REQUEST,
                    "unknown dialog response field".into(),
                ));
            }
            match (kind, body.get("answer"), body.get("cancelled")) {
                (RealtimeOperationKind::DialogResponse, Some(answer), None) => Ok(workspace
                    .dialog_response(session_id, request_id, answer.clone())
                    .await?),
                (RealtimeOperationKind::CancelDialog, None, Some(Value::Bool(true))) => {
                    Ok(workspace.cancel_dialog(session_id, request_id).await?)
                }
                _ => Err(ApiError(
                    StatusCode::BAD_REQUEST,
                    "provide answer or cancelled:true".into(),
                )),
            }
        }
    }
}
