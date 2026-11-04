//! Caller-owned bounded ongoing child metadata; no worker, refresh or execution.
use super::{
    tool_program_access::ChildAccess, tool_program_events as events, tool_program_queries as query,
    tool_program_snapshot as snapshot, Service,
};
use crate::{
    host::{BufferedEvent, HostSession},
    reply::{failure, ReplyKind, ServiceResult},
};
use events::{ChildEvent, ChildEventContext};
use query::ChildPhase;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snapshot::{
    ChildObservation, ChildSnapshot, ChildSnapshotQuery, Identities, ReplayDisposition,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::sync::broadcast;

const RECENT_IDENTITIES: usize = 128;

/// Sanitized terminal causes. None contains untrusted native diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChildResyncReason {
    SnapshotUncertain,
    HostReplaced,
    HostClosed,
    HostError,
    ReceiverClosed,
    Lagged,
    CoreSequenceGap,
    InvalidChildEvent,
    NativeResync,
    OutsideWindow,
    IdentityConflict,
    UnknownOldIdentity,
    PhaseDiscontinuity,
    BranchChanged,
    BranchUnavailable,
}

/// Neutral DTO; core `seq` and the event's native `seq` are different domains.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ChildStreamItem {
    Update {
        seq: u64,
        event: Box<ChildEvent>,
        observation: Option<Box<ChildObservation>>,
        disposition: ReplayDisposition,
    },
    ResyncRequired {
        seq: u64,
        reason: ChildResyncReason,
    },
}

/// Drop releases only this receiver/references, never shuts down the shared host.
/// `next` is cancellation safe, including the host-identity check after receive.
pub struct ChildSubscription {
    access: ChildAccess,
    context: ChildEventContext,
    ready: Value,
    snapshot: ChildSnapshot,
    receiver: Option<broadcast::Receiver<BufferedEvent>>,
    pending: Option<BufferedEvent>,
    initial_resync: Option<ChildResyncReason>,
    cursor: u64,
    native_high: u64,
    working: HashMap<String, Vec<ChildPhase>>,
    identities: Identities,
    // Canonical metadata identities and complete event envelopes are distinct:
    // envelopes also detect changed parentEntryId and other commit fields.
    envelopes: VecDeque<(String, Value)>,
}

/// Subscribe BEFORE invoking the existing bounded snapshot. Both capabilities
/// and the exact host Arc are checked independently at attachment and completion.
pub async fn child_subscription(
    state: &Service,
    id: String,
    request: ChildSnapshotQuery,
) -> ServiceResult<ChildSubscription> {
    let access = ChildAccess::for_service(state, id).await?;
    subscription_for_access(access, request).await
}

/// Caller-owned live access using only the existing host Arc.
pub async fn child_subscription_for_host(
    host: Arc<HostSession>,
    request: ChildSnapshotQuery,
) -> ServiceResult<ChildSubscription> {
    let access = ChildAccess::for_host(host).await?;
    subscription_for_access(access, request).await
}

async fn subscription_for_access(
    access: ChildAccess,
    request: ChildSnapshotQuery,
) -> ServiceResult<ChildSubscription> {
    let host = &access.host;
    if !query::supported(host) || !events::supported(host) {
        return Err(failure(
            ReplyKind::Unsupported,
            "native host does not advertise child snapshot queries and events",
        ));
    }
    access.check_read().await?;
    let receiver = host.subscribe();
    let initial = snapshot::snapshot_for_access(&access, request).await?;
    access.check_read().await?;
    let mut subscription = ChildSubscription {
        ready: host.ready(),
        context: ChildEventContext {
            session_id: access.id.clone(),
            lane: initial.lane.clone(),
        },
        access,
        cursor: initial.last_seq,
        initial_resync: initial
            .resync_required
            .then_some(ChildResyncReason::SnapshotUncertain),
        snapshot: initial,
        receiver: Some(receiver),
        pending: None,
        native_high: 0,
        working: HashMap::new(),
        identities: Identities::default(),
        envelopes: VecDeque::new(),
    };
    if subscription.initial_resync.is_none() && subscription.seed().is_err() {
        subscription.initial_resync = Some(ChildResyncReason::IdentityConflict);
    }
    Ok(subscription)
}

impl ChildSubscription {
    /// Immutable original snapshot, including pinned native pages and cursors.
    pub fn snapshot(&self) -> &ChildSnapshot {
        &self.snapshot
    }

    fn seed(&mut self) -> ServiceResult<()> {
        for child in &self.snapshot.page.children {
            self.identities.begin(child)?;
            self.native_high = self.native_high.max(child.seq);
            self.working.insert(child.child_id.clone(), Vec::new());
        }
        for page in &self.snapshot.phases {
            let phases = self
                .working
                .get_mut(&page.child_id)
                .expect("selected phase page");
            for phase in page.phases.iter().rev() {
                self.identities.phase(phase)?;
                self.native_high = self.native_high.max(phase.seq);
                snapshot::merge(phases, phase.clone())?;
            }
        }
        for replay in &self.snapshot.events {
            let (id, seq) = match &replay.event {
                ChildEvent::Begin(commit) => {
                    self.identities.begin(&commit.summary())?;
                    (&commit.entry_id, commit.seq)
                }
                ChildEvent::Phase(commit) => {
                    let phase = commit.phase();
                    self.identities.phase(&phase)?;
                    snapshot::merge(
                        self.working
                            .get_mut(&phase.child_id)
                            .expect("selected replay"),
                        phase,
                    )?;
                    (&commit.entry_id, commit.seq)
                }
                ChildEvent::ResyncRequired(_) => continue,
            };
            self.native_high = self.native_high.max(seq);
            self.envelopes.push_back((
                id.clone(),
                serde_json::to_value(&replay.event).map_err(|_| {
                    failure(ReplyKind::Internal, "child event serialization failed")
                })?,
            ));
            if self.envelopes.len() > RECENT_IDENTITIES {
                self.envelopes.pop_front();
            }
        }
        // Preserve actual terminal facts even when a short query/replay overlay
        // omitted older records. This does not invent missing phases.
        for observation in &self.snapshot.observations {
            let phases = self
                .working
                .get_mut(&observation.child_id)
                .expect("selected observation");
            for phase in [&observation.final_phase, &observation.closed_phase]
                .into_iter()
                .flatten()
            {
                self.identities.phase(phase)?;
                snapshot::merge(phases, phase.clone())?;
            }
        }
        self.identities.retain_recent(RECENT_IDENTITIES);
        Ok(())
    }

    fn stop(&mut self, reason: ChildResyncReason) -> Option<ChildStreamItem> {
        self.receiver = None;
        self.pending = None;
        Some(ChildStreamItem::ResyncRequired {
            seq: self.cursor,
            reason,
        })
    }

    /// All uncertain/closed conditions emit one terminal resync, then None.
    /// Unrelated events advance the core cursor but their bodies are not returned.
    pub async fn next(&mut self) -> Option<ChildStreamItem> {
        self.receiver.as_ref()?;
        if let Some(reason) = self.initial_resync.take() {
            return self.stop(reason);
        }
        loop {
            if self.access.check_binding().await.is_err() {
                return self.stop(ChildResyncReason::HostReplaced);
            }
            if self.pending.is_none() {
                match self.receiver.as_mut()?.recv().await {
                    Ok(event) => self.pending = Some(event),
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        return self.stop(ChildResyncReason::Lagged)
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return self.stop(ChildResyncReason::ReceiverClosed)
                    }
                }
            }
            // Keep pending across this await: cancellation cannot discard a frame.
            if self.access.check_binding().await.is_err() {
                return self.stop(ChildResyncReason::HostReplaced);
            }
            let buffered = self.pending.as_ref().expect("received event");
            if buffered.seq <= self.snapshot.last_seq {
                self.pending.take();
                continue;
            }
            if self.cursor.checked_add(1) != Some(buffered.seq) {
                return self.stop(ChildResyncReason::CoreSequenceGap);
            }
            match buffered.value["type"].as_str() {
                Some("host_closed") => return self.stop(ChildResyncReason::HostClosed),
                Some("host_error") => return self.stop(ChildResyncReason::HostError),
                _ => {}
            }
            let event = match events::parse(&self.ready, &self.context, &buffered.value) {
                Ok(Some(event)) => event,
                Ok(None)
                    if buffered.value["type"]
                        .as_str()
                        .is_some_and(|s| s.starts_with("tool_program_child_")) =>
                {
                    return self.stop(ChildResyncReason::InvalidChildEvent)
                }
                Ok(None) => {
                    self.cursor = buffered.seq;
                    self.pending.take();
                    continue;
                }
                Err(_) => return self.stop(ChildResyncReason::InvalidChildEvent),
            };
            if let Err(reason) = self.preflight(&event) {
                return self.stop(reason);
            }
            // No cursor, pending frame, identities or working facts change across
            // this await. Dropping next drops the query correlation; a fresh next
            // starts the entire bounded proof again on the same incoming frame.
            match tokio::time::timeout(Duration::from_secs(30), self.branch_proof(&event)).await {
                Ok(Ok(())) => {}
                Ok(Err(reason)) => return self.stop(reason),
                Err(_) => return self.stop(ChildResyncReason::BranchUnavailable),
            }
            self.cursor = self.pending.take().expect("proved event").seq;
            return match self.apply(&event) {
                Ok((disposition, observation)) => Some(ChildStreamItem::Update {
                    seq: self.cursor,
                    event: Box::new(event),
                    observation: observation.map(Box::new),
                    disposition,
                }),
                Err(reason) => self.stop(reason),
            };
        }
    }

    // Reject known contradictions/out-of-window events without authoritative
    // reads, but never use local identity/deduplication as branch proof.
    fn preflight(&self, event: &ChildEvent) -> Result<(), ChildResyncReason> {
        let (id, seq, child_id) = match event {
            ChildEvent::Begin(c) => (&c.entry_id, c.seq, &c.data.child_id),
            ChildEvent::Phase(c) => (&c.entry_id, c.seq, &c.data.child_id),
            ChildEvent::ResyncRequired(_) => return Err(ChildResyncReason::NativeResync),
        };
        if !self.working.contains_key(child_id) {
            return Err(ChildResyncReason::OutsideWindow);
        }
        let envelope =
            serde_json::to_value(event).map_err(|_| ChildResyncReason::InvalidChildEvent)?;
        if self
            .envelopes
            .iter()
            .any(|(key, old)| key == id && old != &envelope)
        {
            return Err(ChildResyncReason::IdentityConflict);
        }
        if !self.identities.contains(id) && seq <= self.native_high {
            return Err(ChildResyncReason::UnknownOldIdentity);
        }
        if let ChildEvent::Phase(c) = event {
            let incoming = c.phase();
            if self.working.values().flatten().any(|old| {
                (old.entry_id == incoming.entry_id || old.seq == incoming.seq) && old != &incoming
            }) {
                return Err(ChildResyncReason::IdentityConflict);
            }
        }
        Ok(())
    }

    async fn fresh_head(&self) -> Result<query::ChildPage, ChildResyncReason> {
        query::child_summaries_owned(
            query::QueryOwner::Bound(&self.access),
            self.access.id.clone(),
            query::ChildPageQuery {
                leaf_id: None,
                cursor: None,
                direction: query::Direction::NewestFirst,
                limit: 1,
                max_bytes: 32768,
            },
        )
        .await
        .map_err(|(kind, _)| Self::proof_error(kind))
    }

    fn proof_error(kind: ReplyKind) -> ChildResyncReason {
        if kind == ReplyKind::Conflict {
            ChildResyncReason::HostReplaced
        } else {
            ChildResyncReason::BranchUnavailable
        }
    }

    /// Frozen v1 has no generation token or reliable navigation event. Prove
    /// every retained fact (including terminal facts) and the incoming phase on
    /// one fresh authoritative branch before EACH delivery, even duplicates.
    /// At most 8 phase pages + 2 heads, 32 KiB/page, one page at a time; native
    /// scans at most 128 candidates/page and verifies the selected begin ancestry.
    /// Missing facts, even in an exhausted/sparse scan, require explicit resync;
    /// no continuation or history fallback. The final fresh head/leaf must be
    /// EXACTLY stable: ordinary concurrent appends may conservatively resync.
    async fn branch_proof(&self, event: &ChildEvent) -> Result<(), ChildResyncReason> {
        let head = self.fresh_head().await?;
        let original = self
            .snapshot
            .page
            .children
            .first()
            .ok_or(ChildResyncReason::OutsideWindow)?;
        let mut newest = head
            .children
            .first()
            .cloned()
            .ok_or(ChildResyncReason::BranchChanged)?;
        newest.origin_verification = original.origin_verification;
        if &newest != original {
            return Err(ChildResyncReason::BranchChanged);
        }
        let leaf = head
            .leaf_id
            .as_ref()
            .ok_or(ChildResyncReason::BranchChanged)?;
        let incoming = match event {
            ChildEvent::Phase(c) => Some(c.phase()),
            _ => None,
        };
        for (child_id, facts) in &self.working {
            let page = query::child_phases_owned(
                query::QueryOwner::Bound(&self.access),
                self.access.id.clone(),
                query::ChildPhaseQuery {
                    child_id: child_id.clone(),
                    leaf_id: Some(leaf.clone()),
                    cursor: None,
                    direction: query::Direction::NewestFirst,
                    limit: 32,
                    max_bytes: 32768,
                },
            )
            .await
            .map_err(|(kind, _)| Self::proof_error(kind))?;
            if facts.iter().any(|fact| !page.phases.contains(fact))
                || incoming.as_ref().is_some_and(|phase| {
                    &phase.child_id == child_id && !page.phases.contains(phase)
                })
            {
                return Err(ChildResyncReason::BranchChanged);
            }
        }
        let check = self.fresh_head().await?;
        if check.leaf_id != head.leaf_id || check.children != head.children {
            return Err(ChildResyncReason::BranchChanged);
        }
        Ok(())
    }

    fn apply(
        &mut self,
        event: &ChildEvent,
    ) -> Result<(ReplayDisposition, Option<ChildObservation>), ChildResyncReason> {
        let (id, seq, child_id) = match event {
            ChildEvent::Begin(c) => (&c.entry_id, c.seq, &c.data.child_id),
            ChildEvent::Phase(c) => (&c.entry_id, c.seq, &c.data.child_id),
            ChildEvent::ResyncRequired(_) => return Err(ChildResyncReason::NativeResync),
        };
        if !self.working.contains_key(child_id) {
            return Err(ChildResyncReason::OutsideWindow);
        }
        let envelope =
            serde_json::to_value(event).map_err(|_| ChildResyncReason::InvalidChildEvent)?;
        if let Some((_, old)) = self.envelopes.iter().find(|(key, _)| key == id) {
            return if old == &envelope {
                Ok((ReplayDisposition::Duplicate, None))
            } else {
                Err(ChildResyncReason::IdentityConflict)
            };
        }
        let known = self.identities.contains(id);
        if !known && seq <= self.native_high {
            return Err(ChildResyncReason::UnknownOldIdentity);
        }
        let disposition = match event {
            ChildEvent::Begin(c) => {
                // The selected begins remain pinned even after recent-ID eviction.
                let original = self
                    .snapshot
                    .page
                    .children
                    .iter()
                    .find(|c| &c.child_id == child_id)
                    .expect("selected begin");
                let mut incoming = c.summary();
                incoming.origin_verification = original.origin_verification;
                if &incoming != original {
                    return Err(ChildResyncReason::IdentityConflict);
                }
                self.identities
                    .begin(&incoming)
                    .map_err(|_| ChildResyncReason::IdentityConflict)?;
                ReplayDisposition::Duplicate
            }
            ChildEvent::Phase(c) => {
                let begin = self
                    .snapshot
                    .page
                    .children
                    .iter()
                    .find(|c| &c.child_id == child_id)
                    .expect("selected begin");
                if seq <= begin.seq {
                    return Err(ChildResyncReason::IdentityConflict);
                }
                let phase = c.phase();
                self.identities
                    .phase(&phase)
                    .map_err(|_| ChildResyncReason::IdentityConflict)?;
                if known {
                    ReplayDisposition::Duplicate
                } else {
                    snapshot::merge(
                        self.working.get_mut(child_id).expect("selected child"),
                        phase,
                    )
                    .map_err(|_| ChildResyncReason::IdentityConflict)?
                }
            }
            ChildEvent::ResyncRequired(_) => unreachable!(),
        };
        if matches!(
            disposition,
            ReplayDisposition::ResyncRequired | ReplayDisposition::OutsideWindow
        ) {
            return Err(ChildResyncReason::PhaseDiscontinuity);
        }
        self.native_high = self.native_high.max(seq);
        self.identities.retain_recent(RECENT_IDENTITIES);
        self.envelopes.push_back((id.clone(), envelope));
        if self.envelopes.len() > RECENT_IDENTITIES {
            self.envelopes.pop_front();
        }
        let observation = (disposition == ReplayDisposition::Applied)
            .then(|| snapshot::observe(child_id, &self.working[child_id]));
        Ok((disposition, observation))
    }
}

#[cfg(test)]
mod tests;
