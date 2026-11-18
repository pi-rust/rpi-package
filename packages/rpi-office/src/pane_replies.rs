//! Pane-prompt ↔ reply correlation.
//!
//! Bridge mode runs ONE shared conversation, so a prompt typed in the add-in
//! sidebar must be correlated with the assistant reply that answers it. The
//! rpi host delivers a queued follow-up without re-firing a per-run hook, so
//! correlation happens at delivery time: the delivered user text is matched
//! against the prompts this bridge injected.
//!
//! Pure bookkeeping, no host calls, so the matching and TTL rules are
//! unit-testable.
#![forbid(unsafe_code)]

use crate::protocol::now_ms;

/// How long an injected prompt may wait for delivery before it is dropped.
pub const DEFAULT_PROMPT_TTL_MS: u64 = 5 * 60_000;

/// One pane-originated prompt injected into the shared conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanePromptEntry<TPane> {
    /// The pane that sent it; replies are routed only to this pane.
    pub pane: TPane,
    /// Correlation id echoed on the final `agent_message`.
    pub prompt_id: String,
    /// The exact text injected into the host (document tag included).
    pub sent_text: String,
}

struct Pending<TPane> {
    entry: PanePromptEntry<TPane>,
    enqueued_at: u64,
}

/// FIFO of injected pane prompts plus the prompt bound to the delivered turn.
pub struct PanePromptTracker<TPane> {
    pending: Vec<Pending<TPane>>,
    current: Option<PanePromptEntry<TPane>>,
    ttl_ms: u64,
}

impl<TPane: Clone> Default for PanePromptTracker<TPane> {
    fn default() -> Self {
        Self::new(DEFAULT_PROMPT_TTL_MS)
    }
}

impl<TPane: Clone> PanePromptTracker<TPane> {
    pub fn new(ttl_ms: u64) -> Self {
        Self {
            pending: Vec::new(),
            current: None,
            ttl_ms,
        }
    }

    /// Queue a prompt that was just injected into the session.
    pub fn enqueue(&mut self, entry: PanePromptEntry<TPane>) {
        self.enqueue_at(entry, now_ms());
    }

    pub fn enqueue_at(&mut self, entry: PanePromptEntry<TPane>, now: u64) {
        self.prune(now);
        self.pending.push(Pending {
            entry,
            enqueued_at: now,
        });
    }

    /// Bind a delivered user message to the oldest pending pane prompt.
    ///
    /// Returns the bound entry when `text` is the prompt this bridge injected
    /// (exact match, or prefix match if another extension transformed the
    /// tail), otherwise `None` — a TUI/RPC prompt, which must not receive a
    /// pane reply.
    pub fn bind_delivered_user_message(&mut self, text: &str) -> Option<PanePromptEntry<TPane>> {
        self.bind_delivered_user_message_at(text, now_ms())
    }

    pub fn bind_delivered_user_message_at(
        &mut self,
        text: &str,
        now: u64,
    ) -> Option<PanePromptEntry<TPane>> {
        self.prune(now);
        let head = self.pending.first()?;
        if head.entry.sent_text != text && !text.starts_with(&head.entry.sent_text) {
            return None;
        }
        let head = self.pending.remove(0);
        self.current = Some(head.entry.clone());
        Some(head.entry)
    }

    /// The pane prompt the current turn is answering, if any.
    pub fn active(&self) -> Option<&PanePromptEntry<TPane>> {
        self.current.as_ref()
    }

    /// Finish the active prompt and return it.
    pub fn complete(&mut self) -> Option<PanePromptEntry<TPane>> {
        self.current.take()
    }

    /// Drop every queued and active prompt (shutdown / abort).
    pub fn clear(&mut self) {
        self.pending.clear();
        self.current = None;
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    fn prune(&mut self, now: u64) {
        while let Some(head) = self.pending.first() {
            if now.saturating_sub(head.enqueued_at) <= self.ttl_ms {
                return;
            }
            self.pending.remove(0);
        }
    }
}

/// Leading line prepended to every pane prompt so the shared conversation shows
/// which document a message came from (and the model can reuse the id as a
/// `document` target).
pub fn format_document_tag(
    host: &str,
    document_name: Option<&str>,
    document_id: Option<&str>,
) -> String {
    let label = document_name.unwrap_or("untitled");
    let id = document_id
        .map(|id| format!(" id={id}"))
        .unwrap_or_default();
    format!("[office:{host} \"{label}\"{id}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(prompt_id: &str, text: &str) -> PanePromptEntry<&'static str> {
        PanePromptEntry {
            pane: "pane-1",
            prompt_id: prompt_id.into(),
            sent_text: text.into(),
        }
    }

    #[test]
    fn binds_only_the_prompt_this_bridge_injected() {
        let mut tracker = PanePromptTracker::new(1_000);
        tracker.enqueue_at(entry("prompt-1", "[office:excel \"Book\"]\nsum A1"), 10);
        // A TUI prompt is not ours.
        assert!(tracker
            .bind_delivered_user_message_at("what is in A1?", 20)
            .is_none());
        assert_eq!(tracker.pending_count(), 1);

        let bound = tracker
            .bind_delivered_user_message_at("[office:excel \"Book\"]\nsum A1", 30)
            .expect("bound");
        assert_eq!(bound.prompt_id, "prompt-1");
        assert_eq!(
            tracker.active().map(|entry| entry.prompt_id.as_str()),
            Some("prompt-1")
        );
        assert_eq!(tracker.pending_count(), 0);

        let completed = tracker.complete().expect("completed");
        assert_eq!(completed.prompt_id, "prompt-1");
        assert!(tracker.active().is_none());
    }

    #[test]
    fn accepts_a_transformed_tail_and_expires_undelivered_prompts() {
        let mut tracker = PanePromptTracker::new(1_000);
        tracker.enqueue_at(entry("prompt-1", "tagged prompt"), 0);
        assert!(tracker
            .bind_delivered_user_message_at("tagged prompt\n\n[extra context]", 10)
            .is_some());

        tracker.enqueue_at(entry("prompt-2", "stale"), 0);
        assert!(tracker
            .bind_delivered_user_message_at("stale", 5_000)
            .is_none());
        assert_eq!(tracker.pending_count(), 0);
    }

    #[test]
    fn fifo_order_and_clear() {
        let mut tracker = PanePromptTracker::new(60_000);
        tracker.enqueue_at(entry("prompt-1", "first"), 0);
        tracker.enqueue_at(entry("prompt-2", "second"), 1);
        let bound = tracker.bind_delivered_user_message_at("first", 2).unwrap();
        assert_eq!(bound.prompt_id, "prompt-1");
        tracker.complete();
        let bound = tracker.bind_delivered_user_message_at("second", 3).unwrap();
        assert_eq!(bound.prompt_id, "prompt-2");
        tracker.clear();
        assert!(tracker.active().is_none());
        assert_eq!(tracker.pending_count(), 0);
    }

    #[test]
    fn document_tag_matches_upstream_shape() {
        assert_eq!(
            format_document_tag("excel", Some("Book1.xlsx"), Some("url_sha256:abc")),
            "[office:excel \"Book1.xlsx\" id=url_sha256:abc]"
        );
        assert_eq!(
            format_document_tag("word", None, None),
            "[office:word \"untitled\"]"
        );
    }
}
