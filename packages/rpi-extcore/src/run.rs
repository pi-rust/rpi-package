//! Native run acceptance, settlement and text repair, independent of a wire protocol.
use crate::message::MessageProjection;
use serde_json::Value;
use tokio::sync::{broadcast, watch, OwnedMutexGuard};

pub struct PromptRun {
    pub events: broadcast::Receiver<Value>,
    pub cancellation: watch::Receiver<u64>,
    pub tracker: RunTracker,
    _lease: OwnedMutexGuard<()>,
}

impl PromptRun {
    pub(crate) fn new(
        events: broadcast::Receiver<Value>,
        cancellation: watch::Receiver<u64>,
        id: Value,
        kind: RunKind,
        lease: OwnedMutexGuard<()>,
    ) -> Self {
        Self {
            events,
            cancellation,
            tracker: RunTracker::for_kind(id, kind),
            _lease: lease,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Prompt,
    Compact,
}

impl RunKind {
    pub(crate) fn command(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Compact => "compact",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Completed,
    Cancelled,
    TokenLimit,
}

#[derive(Default)]
pub struct RunProgress {
    pub deltas: Vec<Value>,
    pub outcome: Option<Result<RunOutcome, String>>,
}

pub struct RunTracker {
    id: Value,
    kind: RunKind,
    compact_settled: bool,
    accepted: bool,
    messages: MessageProjection,
    stream_error: Option<String>,
}

impl RunTracker {
    pub fn new(id: Value) -> Self {
        Self::for_kind(id, RunKind::Prompt)
    }

    pub fn for_kind(id: Value, kind: RunKind) -> Self {
        Self {
            id,
            kind,
            compact_settled: false,
            accepted: false,
            messages: MessageProjection::default(),
            stream_error: None,
        }
    }

    pub fn observe(&mut self, event: &Value, cancelled: bool) -> RunProgress {
        let mut progress = RunProgress::default();
        if self.compact_settled {
            return progress;
        }
        if event["type"] == "response" && event["id"] == self.id {
            if event["success"] != true {
                progress.outcome = Some(if cancelled {
                    Ok(RunOutcome::Cancelled)
                } else {
                    Err(event["error"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("agent rejected the {}", self.kind.command())))
                });
            } else if event.pointer("/data/accepted") != Some(&Value::Bool(true)) {
                progress.outcome = Some(Err(format!(
                    "rpi did not acknowledge {} acceptance",
                    self.kind.command()
                )));
            } else {
                self.accepted = true;
            }
            return progress;
        }
        if self.kind == RunKind::Compact {
            if event["type"] == "agent_settled" && self.accepted {
                self.compact_settled = true;
                let outcome = event["outcome"].as_str().unwrap_or("unknown");
                // Native can settle a cancelled provider request as `failed`
                // (e.g. Request aborted). A locally requested cancellation wins
                // over that failure, but only after authoritative settlement.
                let cancelled_failure = cancelled && outcome == "failed";
                if let Some(summary) = event["summary"]
                    .as_str()
                    .filter(|text| !cancelled_failure && !text.is_empty())
                    .or_else(|| (outcome == "declined").then_some("Nothing to compact yet."))
                {
                    progress
                        .deltas
                        .push(serde_json::json!({"kind":"text","text":summary}));
                }
                progress.outcome = Some(match outcome {
                    "compacted" | "declined" => Ok(RunOutcome::Completed),
                    "aborted" => Ok(RunOutcome::Cancelled),
                    "failed" if cancelled_failure => Ok(RunOutcome::Cancelled),
                    "failed" => Err(event["error"]
                        .as_str()
                        .filter(|text| !text.is_empty())
                        .unwrap_or("rpi compaction failed")
                        .into()),
                    outcome => Err(format!("rpi compaction did not complete: {outcome}")),
                });
            } else if matches!(event["type"].as_str(), Some("session_end" | "host_closed")) {
                progress.outcome =
                    Some(Err("rpi session ended before compaction completed".into()));
            }
            return progress;
        }
        if event["type"] == "agent_settled" && self.accepted {
            if let Some(error) = self.stream_error.take() {
                progress.outcome = Some(Err(error));
                return progress;
            }
            if let Some(text) = event["finalText"].as_str() {
                match self.messages.complete(text) {
                    Ok(delta) => progress.deltas.extend(delta),
                    Err(error) => {
                        progress.outcome = Some(Err(error));
                        return progress;
                    }
                }
            }
            progress.outcome = Some(match event["outcome"].as_str().unwrap_or("unknown") {
                "aborted" => Ok(RunOutcome::Cancelled),
                "failed" => Err(event["error"]
                    .as_str()
                    .filter(|error| !error.is_empty())
                    .map(str::to_owned)
                    .or_else(|| self.messages.error.clone())
                    .unwrap_or_else(|| {
                        "rpi run failed before completing the reply; check the agent stderr log"
                            .into()
                    })),
                "completed" if self.messages.stop_reason.as_deref() == Some("length") => {
                    Ok(RunOutcome::TokenLimit)
                }
                "completed" => Ok(RunOutcome::Completed),
                outcome => Err(format!("rpi run did not complete: {outcome}")),
            });
        } else if matches!(event["type"].as_str(), Some("session_end" | "host_closed")) {
            progress.outcome = Some(Err("rpi session ended before the prompt completed".into()));
        } else {
            match self.messages.event(event) {
                Ok(deltas) => progress.deltas = deltas,
                Err(error) => {
                    self.stream_error.get_or_insert(error);
                }
            }
        }
        progress
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    include!("run_compact_tests.rs");

    #[test]
    fn acceptance_is_correlated_and_settlement_repairs_the_tail() {
        let mut run = RunTracker::new(json!("request"));
        assert!(run
            .observe(
                &json!({"type":"response","id":"other","success":true,"data":{"accepted":true}}),
                false
            )
            .outcome
            .is_none());
        assert!(run
            .observe(
                &json!({"type":"agent_settled","outcome":"completed"}),
                false
            )
            .outcome
            .is_none());
        run.observe(
            &json!({"type":"response","id":"request","success":true,"data":{"accepted":true}}),
            false,
        );
        assert!(run
            .observe(&json!({"type":"agent_end"}), false)
            .outcome
            .is_none());
        let update = run.observe(
            &json!({"type":"message_update","eventType":"text_delta","delta":"你好🌊"}),
            false,
        );
        assert_eq!(
            update.deltas,
            json!([{"kind":"text","text":"你好🌊"}])
                .as_array()
                .unwrap()
                .to_vec()
        );
        let update = run.observe(
            &json!({"type":"agent_settled","outcome":"completed","finalText":"你好🌊，完成。"}),
            false,
        );
        assert_eq!(update.deltas[0]["text"], "，完成。");
        assert_eq!(update.outcome, Some(Ok(RunOutcome::Completed)));
    }

    #[test]
    fn incompatible_snapshots_drain_to_settlement_before_failure() {
        let mut run = RunTracker::new(json!(1));
        run.observe(
            &json!({"type":"response","id":1,"success":true,"data":{"accepted":true}}),
            false,
        );
        run.observe(
            &json!({"type":"message_update","eventType":"text_delta","delta":"first"}),
            false,
        );
        let update=run.observe(&json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"different"}]}}),false);
        assert!(update.outcome.is_none());
        assert!(run
            .observe(
                &json!({"type":"agent_settled","outcome":"completed"}),
                false
            )
            .outcome
            .unwrap()
            .is_err());
    }
}
