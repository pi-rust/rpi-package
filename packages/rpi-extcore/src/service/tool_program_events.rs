//! Strict, protocol-neutral validation of native post-ACK child metadata events.
//!
//! The negotiated profile promises native journal commits, not reliable delivery.
//! Parsing neither independently verifies checkpoints nor persists/replays events,
//! reads bodies, executes children, or infers child/parent lifecycle settlement.
use super::tool_program_queries as query;
use crate::{
    host::HostSession,
    reply::{failure, ReplyKind, ServiceError, ServiceResult},
};
pub use query::{BodyDescriptor, ChildPhaseKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const BEGIN: &str = "tool_program_child_begin";
const PHASE: &str = "tool_program_child_phase";
const RESYNC: &str = "tool_program_child_resync_required";
const BEGIN_CUSTOM: &str = "rpi.tool_program.child_begin.v1";
const PHASE_CUSTOM: &str = "rpi.tool_program.child_phase.v1";
const MAX_EVENT_BYTES: usize = 32768;
const MAX_DATA_BYTES: usize = 16384;

#[derive(Clone, Debug)]
pub struct ChildEventContext {
    pub session_id: String,
    pub lane: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ChildEvent {
    #[serde(rename = "tool_program_child_begin")]
    Begin(ChildCommit<ChildBegin>),
    #[serde(rename = "tool_program_child_phase")]
    Phase(ChildCommit<ChildPhaseData>),
    #[serde(rename = "tool_program_child_resync_required")]
    ResyncRequired(ChildResync),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildCommit<T> {
    pub schema_version: u8,
    pub source_session_id: String,
    pub lane: String,
    pub entry_id: String,
    pub parent_entry_id: Option<String>,
    pub seq: u64,
    pub custom_type: String,
    pub data: T,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildBegin {
    pub version: u8,
    pub child_id: String,
    pub origin: query::ChildOrigin,
    pub root_call_id: String,
    pub root_tool_name: String,
    pub tool_name: String,
    pub ordinal: u32,
    pub parent_child_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildPhaseData {
    pub version: u8,
    pub child_id: String,
    pub index: u64,
    pub phase: ChildPhaseKind,
    pub descriptor: Option<BodyDescriptor>,
    pub is_error: Option<bool>,
    pub cancelled: Option<bool>,
    pub failed: Option<bool>,
    pub interrupted: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildResync {
    pub reason: String,
    pub skipped: u64,
}

impl ChildCommit<ChildBegin> {
    /// NativeCheckpoint reflects the negotiated native post-ACK event promise;
    /// this parser does not independently read or verify the native checkpoint.
    /// A begin is never proof of completion, including after final/closed events.
    pub fn summary(&self) -> query::ChildSummary {
        query::ChildSummary {
            version: self.data.version,
            child_id: self.data.child_id.clone(),
            seq: self.seq,
            origin: self.data.origin.clone(),
            origin_verification: query::ChildOriginVerification::NativeCheckpoint,
            root_call_id: self.data.root_call_id.clone(),
            root_tool_name: self.data.root_tool_name.clone(),
            tool_name: self.data.tool_name.clone(),
            ordinal: self.data.ordinal,
            parent_child_id: self.data.parent_child_id.clone(),
            state: query::ChildState::Unknown,
        }
    }
}

impl ChildCommit<ChildPhaseData> {
    /// Metadata projection only; descriptor presence does not verify any body.
    pub fn phase(&self) -> query::ChildPhase {
        query::ChildPhase {
            entry_id: self.entry_id.clone(),
            seq: self.seq,
            version: self.data.version,
            child_id: self.data.child_id.clone(),
            index: self.data.index,
            phase: self.data.phase,
            descriptor: self.data.descriptor.clone(),
            is_error: self.data.is_error,
            cancelled: self.data.cancelled,
            failed: self.data.failed,
            interrupted: self.data.interrupted,
        }
    }
}

pub fn profile() -> Value {
    json!({"version":1,"beginEvent":BEGIN,"phaseEvent":PHASE,"resyncEvent":RESYNC,
        "sequence":"nativeEntrySeq-v1","delivery":"bestEffortResync",
        "maxEventBytes":MAX_EVENT_BYTES})
}

fn supported_ready(ready: &Value) -> bool {
    ready.get("toolProgramChildEvents") == Some(&profile())
        && ready
            .get("supportedEvents")
            .and_then(Value::as_array)
            .is_some_and(|events| {
                [BEGIN, PHASE, RESYNC]
                    .iter()
                    .all(|name| events.iter().any(|event| event.as_str() == Some(name)))
            })
}

/// Independent of the read-only child query profile and commands.
pub fn supported(session: &HostSession) -> bool {
    supported_ready(&session.ready())
}

fn malformed() -> ServiceError {
    failure(
        ReplyKind::HostUnavailable,
        "native host returned an invalid child event",
    )
}

fn ensure(condition: bool) -> ServiceResult<()> {
    if condition {
        Ok(())
    } else {
        Err(malformed())
    }
}

fn bounded(value: &Value, max: usize) -> ServiceResult<()> {
    serde_json::to_writer(&mut query::Counter { bytes: 0, max }, value).map_err(|_| malformed())
}

fn commit<T>(value: &ChildCommit<T>, context: &ChildEventContext) -> ServiceResult<()> {
    ensure(
        value.schema_version == 1
            && value.source_session_id == context.session_id
            && value.lane == context.lane
            && query::valid_id(&value.entry_id)
            && value
                .parent_entry_id
                .as_ref()
                .is_some_and(|parent| query::valid_id(parent) && parent != &value.entry_id)
            && value.seq > 0,
    )
}

/// Ignore unrelated events even for old hosts. Recognized child events require
/// the exact independent event capability, with no query/history fallback.
/// Bounds cover the COMPLETE original JSON value (including unknown fields),
/// before cloning/deserialization, and then the raw metadata data object.
/// Malformed payload diagnostics are never returned to callers.
pub fn parse(
    ready: &Value,
    context: &ChildEventContext,
    value: &Value,
) -> ServiceResult<Option<ChildEvent>> {
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some(BEGIN | PHASE | RESYNC)
    ) {
        return Ok(None);
    }
    if !supported_ready(ready) {
        return Err(failure(
            ReplyKind::Unsupported,
            "native host does not advertise the exact child event profile",
        ));
    }
    bounded(value, MAX_EVENT_BYTES)?;
    ensure(query::valid_id(&context.session_id) && query::valid_id(&context.lane))?;
    if let Some(data) = value.get("data") {
        bounded(data, MAX_DATA_BYTES)?;
    }
    let event: ChildEvent = serde_json::from_value(value.clone()).map_err(|_| malformed())?;
    match &event {
        ChildEvent::Begin(begin) => {
            commit(begin, context)?;
            ensure(begin.custom_type == BEGIN_CUSTOM && begin.entry_id == begin.data.child_id)?;
            query::summary(&begin.summary()).map_err(|_| malformed())?;
        }
        ChildEvent::Phase(phase) => {
            commit(phase, context)?;
            ensure(
                phase.custom_type == PHASE_CUSTOM
                    && query::native_id(&phase.data.child_id, "tpchild-"),
            )?;
            query::phase(&phase.phase(), &phase.data.child_id).map_err(|_| malformed())?;
        }
        ChildEvent::ResyncRequired(resync) => {
            ensure(
                resync.skipped > 0
                    && matches!(
                        resync.reason.as_str(),
                        "commit_stream_lagged"
                            | "unknown_native_journal_custom_type"
                            | "native_journal_event_too_large"
                    ),
            )?;
        }
    }
    Ok(Some(event))
}

#[cfg(test)]
mod tests;
