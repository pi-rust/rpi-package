//! Maps the state machine's Pi-shaped JSON events onto the host's typed event stream.
use super::stream::EventSink;
use rpi_ai::{
    event_stream::AssistantMessageEventStreamProducer, types::ErrorReason, AssistantMessage,
    AssistantMessageEvent, DoneReason, StopReason, ToolCall,
};
use serde_json::Value;
use std::sync::Arc;

/// Forwards each event to the host stream. A closed consumer stops the driver.
pub struct Emitter {
    pub producer: AssistantMessageEventStreamProducer,
}
impl EventSink for Emitter {
    async fn emit(&mut self, event: Value) -> Result<(), String> {
        let mapped = to_event(&event)?;
        if self.producer.push(mapped) {
            Ok(())
        } else {
            Err("assistant message stream was closed".into())
        }
    }
}

fn stop_reason(value: &str) -> Result<StopReason, String> {
    Ok(match value {
        "stop" => StopReason::Stop,
        "length" => StopReason::Length,
        "toolUse" => StopReason::ToolUse,
        "error" => StopReason::Error,
        "aborted" => StopReason::Aborted,
        other => return Err(format!("unknown Antigravity stop reason {other}")),
    })
}

/// The state machine writes Pi's camelCase `stopReason`, which the host enum spells in lowercase.
fn message(value: &Value) -> Result<AssistantMessage, String> {
    let reason = stop_reason(value["stopReason"].as_str().unwrap_or_default())?;
    let mut value = value.clone();
    value["stopReason"] = Value::from("stop");
    let mut message: AssistantMessage = serde_json::from_value(value)
        .map_err(|error| format!("invalid Antigravity message: {error}"))?;
    message.stop_reason = reason;
    Ok(message)
}

fn index(event: &Value) -> usize {
    event["contentIndex"].as_u64().unwrap_or_default() as usize
}
fn text(event: &Value, field: &str) -> String {
    event[field].as_str().unwrap_or_default().to_owned()
}

fn to_event(event: &Value) -> Result<AssistantMessageEvent, String> {
    let partial = || message(&event["partial"]).map(Arc::new);
    Ok(match event["type"].as_str().unwrap_or_default() {
        "start" => AssistantMessageEvent::Start {
            partial: partial()?,
        },
        "text_start" => AssistantMessageEvent::TextStart {
            content_index: index(event),
            partial: partial()?,
        },
        "text_delta" => AssistantMessageEvent::TextDelta {
            content_index: index(event),
            delta: text(event, "delta"),
            partial: partial()?,
        },
        "text_end" => AssistantMessageEvent::TextEnd {
            content_index: index(event),
            content: text(event, "content"),
            partial: partial()?,
        },
        "thinking_start" => AssistantMessageEvent::ThinkingStart {
            content_index: index(event),
            partial: partial()?,
        },
        "thinking_delta" => AssistantMessageEvent::ThinkingDelta {
            content_index: index(event),
            delta: text(event, "delta"),
            partial: partial()?,
        },
        "thinking_end" => AssistantMessageEvent::ThinkingEnd {
            content_index: index(event),
            content: text(event, "content"),
            partial: partial()?,
        },
        "toolcall_start" => AssistantMessageEvent::ToolCallStart {
            content_index: index(event),
            partial: partial()?,
        },
        "toolcall_end" => AssistantMessageEvent::ToolCallEnd {
            content_index: index(event),
            tool_call: serde_json::from_value::<ToolCall>(event["toolCall"].clone())
                .map_err(|error| format!("invalid Antigravity tool call: {error}"))?,
            partial: partial()?,
        },
        "done" => {
            let message = message(&event["message"])?;
            let reason = match message.stop_reason {
                StopReason::Stop => DoneReason::Stop,
                StopReason::Length => DoneReason::Length,
                StopReason::ToolUse => DoneReason::ToolUse,
                other => return Err(format!("unexpected Antigravity done reason {other:?}")),
            };
            AssistantMessageEvent::Done { reason, message }
        }
        "error" => {
            let error = message(&event["error"])?;
            let reason = match error.stop_reason {
                StopReason::Aborted => ErrorReason::Aborted,
                _ => ErrorReason::Error,
            };
            AssistantMessageEvent::Error { reason, error }
        }
        other => return Err(format!("unknown Antigravity stream event {other}")),
    })
}
