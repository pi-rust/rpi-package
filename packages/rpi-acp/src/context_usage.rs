//! ACP-only context presentation. Native parsing and bounded reads live in core.
use super::Writer;
use rpi_extcore::context_usage::ContextUsage;
use serde_json::{json, Value};

/// Only successfully delivered used/window pairs are retained, not token stats.
#[derive(Default)]
pub(crate) struct Presentation {
    delivered: Option<(u64, u64)>,
}

pub(crate) fn project(expected: &str, usage: &ContextUsage) -> Option<Value> {
    if usage.session_id != expected {
        return None;
    }
    Some(json!({"jsonrpc":"2.0","method":"session/update","params":{
        "sessionId":expected,"update":{"sessionUpdate":"usage_update",
        "used":usage.used_tokens,"size":usage.window_tokens}
    }}))
}

impl Presentation {
    pub(crate) async fn deliver(
        &mut self,
        writer: &Writer,
        expected: &str,
        usage: &ContextUsage,
        initial: bool,
    ) {
        let Some(frame) = project(expected, usage) else {
            return;
        };
        let pair = (usage.used_tokens, usage.window_tokens);
        if !initial && self.delivered == Some(pair) {
            return;
        }
        if writer.write_context_usage(&frame).await.is_ok() {
            self.delivered = Some(pair);
        }
    }
}

#[cfg(test)]
#[path = "context_usage_tests.rs"]
mod tests;
