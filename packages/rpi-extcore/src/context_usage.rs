//! Bounded, on-demand observation of native context usage (without a run lease).
use crate::host::HostSession;
use serde_json::{json, Value};
use std::time::Duration;

/// Native token accounting, independent of any adapter's wire representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextUsage {
    pub session_id: String,
    pub used_tokens: u64,
    pub window_tokens: u64,
    pub estimated: Option<bool>,
    pub trailing_tokens: Option<u64>,
}

/// Parse the data returned by native `get_state`, not its response envelope.
/// Unknown context is not zero usage. Identity is required only for known usage;
/// checking it against an adapter's expected session remains the adapter's job.
pub fn parse_context_usage(state: &Value) -> Result<Option<ContextUsage>, String> {
    let state = state.as_object().ok_or("context usage: invalid state")?;
    let context = match state.get("context") {
        None | Some(Value::Null) => return Ok(None),
        Some(value) => value.as_object().ok_or("context usage: invalid context")?,
    };
    let used_tokens = context
        .get("tokens")
        .and_then(Value::as_u64)
        .ok_or("context usage: invalid tokens")?;
    let window_tokens = context
        .get("window")
        .and_then(Value::as_u64)
        .ok_or("context usage: invalid window")?;
    let estimated = match context.get("estimated") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_bool().ok_or("context usage: invalid estimated")?),
    };
    let trailing_tokens = match context.get("trailingTokens") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .ok_or("context usage: invalid trailing tokens")?,
        ),
    };
    if window_tokens == 0 {
        return Ok(None);
    }
    let session_id = state
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 4096)
        .ok_or("context usage: invalid session ID")?;
    Ok(Some(ContextUsage {
        session_id: session_id.to_owned(),
        used_tokens,
        window_tokens,
        estimated,
        trailing_tokens,
    }))
}

impl HostSession {
    /// One lease-free read, with no queue, background worker, or cached usage.
    /// Dropping the caller drops command correlation and releases admission.
    pub async fn context_usage(&self) -> Result<Option<ContextUsage>, String> {
        if !self.supports_command("get_state") {
            return Ok(None);
        }
        let _gate = self
            .usage_gate
            .try_lock()
            .map_err(|_| "context usage: busy")?;
        let mut closed = self.closed_receiver();
        let budget = Duration::from_secs(5);
        tokio::select! {
            biased;
            _ = closed.wait_for(|value| *value) => Err("context usage: host closed".into()),
            result = tokio::time::timeout(budget, self.command("get_state", json!({}), budget)) => {
                let state = result
                    .map_err(|_| "context usage: timed out")?
                    .map_err(|_| "context usage: read failed")?;
                parse_context_usage(&state)
            }
        }
    }
}

#[cfg(test)]
#[path = "context_usage_tests.rs"]
mod tests;
