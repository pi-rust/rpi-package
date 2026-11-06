//! Neutral wire construction and dialog decoding; no client projection.
use super::{PendingDialog, WireEvent};
use serde_json::Value;

pub fn wire_event(
    id: Option<String>,
    seq: Option<u64>,
    event_type: impl Into<String>,
    data: Value,
) -> WireEvent {
    WireEvent {
        id,
        seq,
        event_type: event_type.into(),
        data,
    }
}

pub fn dialog_from_wire(wire: &WireEvent) -> Result<Option<PendingDialog>, serde_json::Error> {
    let Some(request_id) = wire.data.get("requestId").and_then(Value::as_str) else {
        return Ok(None);
    };
    let ui = wire
        .data
        .get("ui")
        .cloned()
        .unwrap_or_else(|| wire.data.clone());
    Ok(Some(PendingDialog {
        request_id: request_id.to_owned(),
        tool_call_id: wire
            .data
            .get("toolCallId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        response_id: wire.data.get("jsonRpcRequestId").cloned(),
        ui: serde_json::from_value(ui)?,
    }))
}
