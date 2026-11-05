//! Bounded child metadata plus a gap-checked snapshot/live handoff.
//!
//! The native query pages stay pinned and unchanged. Observations reconcile only
//! the selected children; new/out-of-window children explicitly require refresh.
//! No bodies, transcript fallback, child execution, parent settlement, cache or
//! background worker is introduced. A historical fork's outcome describes its
//! copied records, never execution in the destination.
use super::{
    tool_program_access::ChildAccess, tool_program_events as events, tool_program_queries as query,
    Service,
};
use crate::{
    host::{HostSession, ReplayError},
    reply::{failure, ReplyKind, ServiceError, ServiceResult},
};
use events::{ChildEvent, ChildEventContext};
use query::{ChildPage, ChildPageQuery, ChildPhase, ChildPhaseKind, ChildPhasePage, Direction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc, time::Duration};

pub const MAX_CHILD_SNAPSHOT_BYTES: usize = 131072;
const MAX_CHILDREN: usize = 8;
const MAX_PHASES: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildSnapshotQuery {
    /// Fresh newest-first window only. Use child_summaries for historical paging.
    pub children: ChildPageQuery,
    /// One bounded newest-first phase scan per selected child; no hidden loop.
    pub phase_limit: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChildOutcome {
    Unknown,
    FinalObserved,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildObservation {
    pub child_id: String,
    pub latest_phase: Option<ChildPhaseKind>,
    pub final_phase: Option<ChildPhase>,
    pub closed_phase: Option<ChildPhase>,
    pub outcome: ChildOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReplayDisposition {
    Applied,
    Duplicate,
    OutsideWindow,
    ResyncRequired,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildReplay {
    /// Core replay sequence; event.seq is the separate native entry sequence.
    pub seq: u64,
    pub event: ChildEvent,
    pub disposition: ReplayDisposition,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildSnapshot {
    pub session_id: String,
    /// Native RPC's primary lane. This API does not select arbitrary lanes.
    pub lane: String,
    pub page: ChildPage,
    pub phases: Vec<ChildPhasePage>,
    pub observations: Vec<ChildObservation>,
    pub start_seq: u64,
    /// Resume the existing replay/subscription API strictly after this sequence.
    pub last_seq: u64,
    pub events: Vec<ChildReplay>,
    pub resync_required: bool,
}

pub(super) fn binding_conflict() -> ServiceError {
    conflict()
}
fn conflict() -> ServiceError {
    (
        ReplyKind::Conflict,
        json!({"code":"child_snapshot_resync_required",
        "error":"child snapshot changed or exceeded its replay budget; reload"}),
    )
}
fn malformed() -> ServiceError {
    failure(
        ReplyKind::HostUnavailable,
        "native child metadata is inconsistent",
    )
}
fn bounded<T: Serialize>(value: &T) -> ServiceResult<()> {
    serde_json::to_writer(
        &mut query::Counter {
            bytes: 0,
            max: MAX_CHILD_SNAPSHOT_BYTES,
        },
        value,
    )
    .map_err(|_| conflict())
}
pub(super) async fn same_host(
    state: &Service,
    id: &str,
    host: &Arc<HostSession>,
) -> ServiceResult<()> {
    state.check_session_admission(id).await?;
    if state
        .live_session(id)
        .await
        .is_some_and(|current| Arc::ptr_eq(&current, host))
    {
        Ok(())
    } else {
        Err(conflict())
    }
}

#[derive(Default)]
pub(super) struct Identities {
    entries: HashMap<String, (u64, Value)>,
    sequences: HashMap<u64, String>,
}
impl Identities {
    fn record<T: Serialize>(&mut self, id: &str, seq: u64, value: &T) -> ServiceResult<()> {
        let value = serde_json::to_value(value).map_err(|_| malformed())?;
        if self.sequences.get(&seq).is_some_and(|old| old != id)
            || self
                .entries
                .get(id)
                .is_some_and(|(old_seq, old)| *old_seq != seq || old != &value)
        {
            return Err(malformed());
        }
        self.sequences.insert(seq, id.into());
        self.entries.insert(id.into(), (seq, value));
        Ok(())
    }
    pub(super) fn contains(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }
    /// Live subscriptions retain only recent canonical facts. Unknown older
    /// identities must then require resync rather than being accepted anew.
    pub(super) fn retain_recent(&mut self, max: usize) {
        while self.entries.len() > max {
            let seq = *self.sequences.keys().min().expect("identity sequence");
            let id = self.sequences.remove(&seq).expect("identity ID");
            self.entries.remove(&id);
        }
    }
    pub(super) fn begin(&mut self, child: &query::ChildSummary) -> ServiceResult<()> {
        let mut canonical = child.clone();
        // Provenance is validated separately, not part of the shared native
        // begin payload when comparing a fork query with a commit envelope.
        canonical.origin_verification = query::ChildOriginVerification::NativeCheckpoint;
        self.record(&child.child_id, child.seq, &canonical)
    }
    pub(super) fn phase(&mut self, phase: &ChildPhase) -> ServiceResult<()> {
        self.record(&phase.entry_id, phase.seq, phase)
    }
}

pub(super) fn observe(child_id: &str, phases: &[ChildPhase]) -> ChildObservation {
    let latest = phases.iter().max_by_key(|p| p.seq);
    let final_phase = phases
        .iter()
        .find(|p| p.phase == ChildPhaseKind::Final)
        .cloned();
    let closed_phase = phases
        .iter()
        .find(|p| p.phase == ChildPhaseKind::Closed)
        .cloned();
    let outcome = match (&final_phase, &closed_phase) {
        (Some(final_phase), Some(closed))
            if closed.index == final_phase.index.saturating_add(1)
                && closed.seq > final_phase.seq =>
        {
            if closed.interrupted == Some(true) {
                ChildOutcome::Interrupted
            } else if closed.failed == Some(true) {
                ChildOutcome::Failed
            } else if final_phase.cancelled == Some(true) {
                ChildOutcome::Cancelled
            } else if final_phase.is_error == Some(true) {
                ChildOutcome::Failed
            } else {
                ChildOutcome::Succeeded
            }
        }
        (Some(_), _) => ChildOutcome::FinalObserved,
        _ => ChildOutcome::Unknown,
    };
    ChildObservation {
        child_id: child_id.into(),
        latest_phase: latest.map(|p| p.phase),
        final_phase,
        closed_phase,
        outcome,
    }
}

// Check identities and lifecycle ordering across query/replay overlap, without
// claiming that missing old phases or scan exhaustion establish completion.
pub(super) fn merge(
    phases: &mut Vec<ChildPhase>,
    incoming: ChildPhase,
) -> ServiceResult<ReplayDisposition> {
    if let Some(old) = phases.iter().find(|p| p.entry_id == incoming.entry_id) {
        return if old == &incoming {
            Ok(ReplayDisposition::Duplicate)
        } else {
            Err(malformed())
        };
    }
    let newest = phases.iter().max_by_key(|p| p.seq);
    let disposition = if newest.is_some_and(|p| incoming.seq < p.seq) {
        ReplayDisposition::OutsideWindow
    } else if newest.is_some_and(|p| incoming.index != p.index.saturating_add(1)) {
        ReplayDisposition::ResyncRequired
    } else {
        ReplayDisposition::Applied
    };
    phases.push(incoming);
    phases.sort_by_key(|p| p.seq);
    for pair in phases.windows(2) {
        if pair[0].seq >= pair[1].seq
            || pair[0].index >= pair[1].index
            || pair[0].phase == ChildPhaseKind::Closed
            || (pair[0].phase == ChildPhaseKind::Final && pair[1].phase != ChildPhaseKind::Closed)
        {
            return Err(malformed());
        }
    }
    for kind in [
        ChildPhaseKind::Requested,
        ChildPhaseKind::Prepared,
        ChildPhaseKind::Final,
        ChildPhaseKind::Closed,
    ] {
        if phases.iter().filter(|p| p.phase == kind).count() > 1 {
            return Err(malformed());
        }
    }
    if phases.len() > MAX_PHASES {
        phases.remove(0);
    }
    Ok(disposition)
}

/// Read a bounded fresh primary-lane child window and reconcile its live overlap.
/// Subscribe before the first boundary/read. The caller owns this future; drop
/// cancels correlated reads, and the entire operation has a 30-second budget.
/// Existing ordinary transcript snapshots and old-host behavior are unchanged.
pub async fn child_snapshot(
    state: &Service,
    id: String,
    request: ChildSnapshotQuery,
) -> ServiceResult<ChildSnapshot> {
    validate_request(&id, &request)?;
    tokio::time::timeout(Duration::from_secs(30), async {
        let access = ChildAccess::for_service(state, id).await?;
        snapshot_inner(&access, request).await
    })
    .await
    .map_err(|_| failure(ReplyKind::Timeout, "native child snapshot timed out"))?
}

/// Snapshot an existing host Arc; no Service, registry or additional native RPC.
pub async fn child_snapshot_for_host(
    host: Arc<HostSession>,
    request: ChildSnapshotQuery,
) -> ServiceResult<ChildSnapshot> {
    let access = ChildAccess::for_host(host).await?;
    snapshot_for_access(&access, request).await
}

pub(super) fn validate_request(id: &str, request: &ChildSnapshotQuery) -> ServiceResult<()> {
    let q = &request.children;
    if !query::valid_id(id)
        || !(1..=MAX_CHILDREN).contains(&q.limit)
        || !(1..=MAX_CHILD_SNAPSHOT_BYTES).contains(&q.max_bytes)
        || q.direction != Direction::NewestFirst
        || q.leaf_id.is_some()
        || q.cursor.is_some()
        || !(1..=MAX_PHASES).contains(&request.phase_limit)
    {
        return Err(failure(
            ReplyKind::InvalidInput,
            "invalid bounded fresh child snapshot query",
        ));
    }
    Ok(())
}

pub(super) async fn snapshot_for_access(
    access: &ChildAccess,
    request: ChildSnapshotQuery,
) -> ServiceResult<ChildSnapshot> {
    validate_request(&access.id, &request)?;
    tokio::time::timeout(Duration::from_secs(30), snapshot_inner(access, request))
        .await
        .map_err(|_| failure(ReplyKind::Timeout, "native child snapshot timed out"))?
}

async fn snapshot_inner(
    access: &ChildAccess,
    request: ChildSnapshotQuery,
) -> ServiceResult<ChildSnapshot> {
    access.check_read().await?;
    let host = &access.host;
    let id = &access.id;
    if !query::supported(host) || !events::supported(host) {
        return Err(failure(
            ReplyKind::Unsupported,
            "native host does not advertise child snapshot queries and events",
        ));
    }
    let ready = host.ready();
    let context = ChildEventContext {
        session_id: id.clone(),
        lane: "main".into(),
    };
    let _receiver = host.subscribe();
    let start = host.history_boundary().await.map_err(|_| conflict())?;
    let page = query::child_summaries_owned(
        query::QueryOwner::Bound(access),
        id.clone(),
        request.children,
    )
    .await?;
    let mut snapshot = ChildSnapshot {
        session_id: id.clone(),
        lane: context.lane.clone(),
        page,
        phases: Vec::new(),
        observations: Vec::new(),
        start_seq: start,
        last_seq: start,
        events: Vec::new(),
        resync_required: false,
    };
    let mut identities = Identities::default();
    for child in &snapshot.page.children {
        identities.begin(child)?;
    }
    let mut working = HashMap::<String, Vec<ChildPhase>>::new();
    for child in &snapshot.page.children {
        access.check_read().await?;
        let page = query::child_phases_owned(
            query::QueryOwner::Bound(access),
            id.clone(),
            query::ChildPhaseQuery {
                child_id: child.child_id.clone(),
                leaf_id: snapshot.page.leaf_id.clone(),
                cursor: None,
                direction: Direction::NewestFirst,
                limit: request.phase_limit,
                max_bytes: 32768,
            },
        )
        .await?;
        let mut phases = Vec::new();
        for phase in page.phases.iter().rev() {
            if phase.seq <= child.seq {
                return Err(malformed());
            }
            identities.phase(phase)?;
            merge(&mut phases, phase.clone())?;
        }
        working.insert(child.child_id.clone(), phases);
        snapshot.phases.push(page);
        bounded(&snapshot)?;
    }
    access.check_read().await?;
    // Lightweight native child query revalidates the pinned branch membership;
    // never use get_state/history/message scans or a source-session fallback.
    let check = query::child_summaries_owned(
        query::QueryOwner::Bound(access),
        id.clone(),
        ChildPageQuery {
            leaf_id: snapshot.page.leaf_id.clone(),
            cursor: None,
            direction: Direction::NewestFirst,
            limit: 1,
            max_bytes: 32768,
        },
    )
    .await?;
    if check.leaf_id != snapshot.page.leaf_id {
        snapshot.resync_required = true;
    } else if check.children.first() != snapshot.page.children.first() {
        return Err(malformed());
    }
    for child in &check.children {
        identities.begin(child)?;
    }
    access.check_read().await?;
    let mut event_bytes = query::Counter {
        bytes: 0,
        max: MAX_CHILD_SNAPSHOT_BYTES,
    };
    let mut seen = HashMap::<String, (u64, Value)>::new();
    let mut last_native_seq = None;
    let mapped = host
        .map_history(start, |buffered| {
            if matches!(
                buffered.value["type"].as_str(),
                Some("host_closed" | "host_error")
            ) {
                snapshot.resync_required = true;
            }
            let Some(event) = events::parse(&ready, &context, &buffered.value)? else {
                return Ok(None);
            };
            serde_json::to_writer(&mut event_bytes, &event).map_err(|_| conflict())?;
            match &event {
                ChildEvent::Begin(commit) => identities.begin(&commit.summary())?,
                ChildEvent::Phase(commit) => identities.phase(&commit.phase())?,
                ChildEvent::ResyncRequired(_) => {}
            }
            let mut repeated = false;
            let identity = match &event {
                ChildEvent::Begin(c) => Some((&c.entry_id, c.seq)),
                ChildEvent::Phase(c) => Some((&c.entry_id, c.seq)),
                ChildEvent::ResyncRequired(_) => None,
            };
            if let Some((entry_id, seq)) = identity {
                let value = serde_json::to_value(&event).map_err(|_| malformed())?;
                if let Some((old_seq, old)) = seen.get(entry_id) {
                    if old_seq != &seq || old != &value {
                        return Err(malformed());
                    }
                    repeated = true;
                } else {
                    if last_native_seq.is_some_and(|last| seq <= last) {
                        return Err(malformed());
                    }
                    seen.insert(entry_id.clone(), (seq, value));
                    last_native_seq = Some(seq);
                }
            }
            let disposition = if repeated {
                ReplayDisposition::Duplicate
            } else {
                match &event {
                    ChildEvent::Begin(commit) => {
                        if let Some(child) = snapshot
                            .page
                            .children
                            .iter()
                            .find(|c| c.child_id == commit.data.child_id)
                        {
                            let mut incoming = commit.summary();
                            incoming.origin_verification = child.origin_verification;
                            if child != &incoming {
                                return Err(malformed());
                            }
                            ReplayDisposition::Duplicate
                        } else {
                            ReplayDisposition::ResyncRequired
                        }
                    }
                    ChildEvent::Phase(commit) => {
                        if let Some(phases) = working.get_mut(&commit.data.child_id) {
                            let begin = snapshot
                                .page
                                .children
                                .iter()
                                .find(|c| c.child_id == commit.data.child_id)
                                .ok_or_else(malformed)?;
                            if commit.seq <= begin.seq {
                                return Err(malformed());
                            }
                            merge(phases, commit.phase())?
                        } else {
                            ReplayDisposition::ResyncRequired
                        }
                    }
                    ChildEvent::ResyncRequired(_) => ReplayDisposition::ResyncRequired,
                }
            };
            if disposition == ReplayDisposition::ResyncRequired {
                snapshot.resync_required = true;
            }
            Ok(Some(ChildReplay {
                seq: buffered.seq,
                event,
                disposition,
            }))
        })
        .await;
    let (last_seq, replay) = match mapped {
        Ok(value) => value,
        Err(ReplayError::Gap) => return Err(conflict()),
        Err(ReplayError::Projection(error)) => return Err(error),
    };
    access.check_read().await?;
    snapshot.last_seq = last_seq;
    snapshot.events = replay;
    snapshot.observations = snapshot
        .page
        .children
        .iter()
        .map(|child| {
            observe(
                &child.child_id,
                working.get(&child.child_id).map_or(&[], Vec::as_slice),
            )
        })
        .collect();
    bounded(&snapshot)?;
    Ok(snapshot)
}

#[cfg(test)]
mod native_tests;
#[cfg(test)]
mod tests;
