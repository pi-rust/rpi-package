//! Capability-gated, read-only native Code Mode child inspection.
//!
//! Native owns branch membership and origin verification. This service validates
//! untrusted pages without scanning transcripts, hydrating bodies for summaries,
//! executing tools, replaying runs or creating child processes. Scan exhaustion
//! is not lifecycle settlement. ForkCopy preserves historical origin IDs and
//! does not establish a destination run checkpoint. Live child events require
//! the separately negotiated native event profile; this module only reads pages.
use super::Service;
use crate::{
    host::HostSession,
    reply::{failure, ReplyKind, ServiceError, ServiceResult},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::{self, Write},
    time::Duration,
};

const SUMMARY_COMMAND: &str = "get_tool_program_children";
const PHASE_COMMAND: &str = "get_tool_program_child_phases";
const BODY_COMMAND: &str = "get_tool_program_child_body";
const MAX_RESPONSE_BYTES: usize = 131072;
const CHUNK_BYTES: usize = 32768;
const MAX_CHUNKS: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    OldestFirst,
    NewestFirst,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildCursor {
    pub session_id: String,
    pub lane: String,
    pub leaf_id: String,
    pub direction: Direction,
    /// None for begin pages, Some for the selected child's phase scan.
    pub child_id: Option<String>,
    pub after_entry_id: String,
    pub after_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildPageQuery {
    pub leaf_id: Option<String>,
    pub cursor: Option<ChildCursor>,
    pub direction: Direction,
    pub limit: usize,
    /// Complete encoded page, including envelope and cursor; no first-item exception.
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildPhaseQuery {
    pub child_id: String,
    pub leaf_id: Option<String>,
    pub cursor: Option<ChildCursor>,
    pub direction: Direction,
    pub limit: usize,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildOrigin {
    pub run_id: String,
    pub assistant_entry_id: String,
    pub tool_index: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChildState {
    Unknown,
}
/// How the original child origin was verified in this destination session.
/// This is provenance over trusted native transcripts and fork metadata, not
/// protection against forgery by a writer with raw storage access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChildOriginVerification {
    NativeCheckpoint,
    /// Historical copied begin; no destination execution/completion is implied.
    ForkCopy,
}
/// Authoritative begin metadata only. State is deliberately not inferred from
/// a bounded window; missing final/close never means successful completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildSummary {
    pub version: u8,
    pub child_id: String,
    pub seq: u64,
    pub origin: ChildOrigin,
    pub origin_verification: ChildOriginVerification,
    pub root_call_id: String,
    pub root_tool_name: String,
    pub tool_name: String,
    pub ordinal: u32,
    pub parent_child_id: Option<String>,
    pub state: ChildState,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChildPhaseKind {
    Requested,
    Prepared,
    Update,
    Final,
    Closed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildPhase {
    pub entry_id: String,
    pub seq: u64,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildPage {
    pub leaf_id: Option<String>,
    pub children: Vec<ChildSummary>,
    pub next_cursor: Option<ChildCursor>,
    pub scan_exhausted: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildPhasePage {
    pub leaf_id: String,
    pub child_id: String,
    pub phases: Vec<ChildPhase>,
    pub next_cursor: Option<ChildCursor>,
    /// Candidates consumed, including other children. At most 128 per call.
    pub scanned: usize,
    /// Exhaustion of this pinned scan, NOT completion of the child lifecycle.
    pub scan_exhausted: bool,
    pub state: ChildState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildBodyCursor {
    /// Body chunks are always traversed in ascending index order.
    pub direction: Direction,
    pub child_id: String,
    pub phase_entry_id: String,
    pub body: BodyCursor,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildBodyQuery {
    pub child_id: String,
    pub phase_entry_id: String,
    pub descriptor_id: String,
    pub leaf_id: Option<String>,
    pub cursor: Option<ChildBodyCursor>,
    pub limit: usize,
    /// Decoded body bytes, as in BodyPageQuery (1..=65536).
    pub max_bytes: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildBodyPage {
    pub child_id: String,
    pub phase_entry_id: String,
    pub body: BodyPage,
    pub next_cursor: Option<ChildBodyCursor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyKind {
    Requested,
    Prepared,
    Update,
    Final,
}

/// The terminal descriptor. `descriptorId` is generated natively, not a path.
/// Its presence is a commit, NOT proof that every chunk remains available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BodyDescriptor {
    pub descriptor_id: String,
    pub version: u8,
    pub body_id: String,
    pub child_id: String,
    pub kind: BodyKind,
    pub codec: String,
    pub encoding: String,
    pub byte_length: usize,
    pub chunk_bytes: usize,
    pub chunks: usize,
    pub sha256: String,
}

/// Explicit source binding, not an authenticated token or authorization boundary.
/// Rebased/foreign contexts are rejected; callers may select an index only within
/// this validated body. No source-session fallback is performed after a fork.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BodyCursor {
    pub session_id: String,
    pub lane: String,
    pub leaf_id: String,
    pub descriptor_id: String,
    pub next_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BodyPageChunk {
    pub index: usize,
    pub byte_offset: usize,
    pub byte_length: usize,
    pub data: String,
    pub sha256: String,
}

/// Each returned chunk's checksum is verified. Whole-body integrity requires the
/// consumer to stream all decoded pages and compare `descriptor.sha256`.
/// There is deliberately no `complete`/`available` flag for unread chunks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BodyPage {
    pub descriptor: BodyDescriptor,
    pub leaf_id: String,
    pub byte_offset: usize,
    pub byte_length: usize,
    pub total_bytes: usize,
    pub chunks: Vec<BodyPageChunk>,
    pub next_cursor: Option<BodyCursor>,
}

fn invalid() -> ServiceError {
    failure(
        ReplyKind::InvalidInput,
        "invalid bounded native child query",
    )
}
fn malformed() -> ServiceError {
    failure(
        ReplyKind::HostUnavailable,
        "native host returned an invalid bounded child page",
    )
}
fn ensure(condition: bool) -> ServiceResult<()> {
    if condition {
        Ok(())
    } else {
        Err(malformed())
    }
}
pub(super) fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control)
}
fn uuid4(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok_and(|u| u.get_version_num() == 4 && u.to_string() == s)
}
pub(super) fn native_id(s: &str, prefix: &str) -> bool {
    s.strip_prefix(prefix).is_some_and(uuid4)
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn ordered(direction: Direction, before: u64, after: u64) -> bool {
    match direction {
        Direction::OldestFirst => before < after,
        Direction::NewestFirst => before > after,
    }
}
fn pinned<'a>(leaf: Option<&'a str>, cursor: Option<&'a ChildCursor>) -> Option<&'a str> {
    leaf.or_else(|| cursor.map(|c| c.leaf_id.as_str()))
}
fn query_base(
    id: &str,
    limit: usize,
    bytes: usize,
    max_count: usize,
    max_bytes: usize,
    leaf: Option<&str>,
) -> ServiceResult<()> {
    if !valid_id(id)
        || !(1..=max_count).contains(&limit)
        || !(1..=max_bytes).contains(&bytes)
        || leaf.is_some_and(|s| !valid_id(s))
    {
        return Err(invalid());
    }
    Ok(())
}
fn query_cursor(
    id: &str,
    leaf: Option<&str>,
    dir: Direction,
    child: Option<&str>,
    c: Option<&ChildCursor>,
) -> ServiceResult<()> {
    if let Some(c) = c {
        if ![&c.session_id, &c.lane, &c.leaf_id, &c.after_entry_id]
            .iter()
            .all(|s| valid_id(s))
            || c.session_id != id
            || c.direction != dir
            || c.child_id.as_deref() != child
            || leaf.is_some_and(|s| s != c.leaf_id)
            || !native_id(
                &c.after_entry_id,
                if child.is_some() {
                    "tpphase-"
                } else {
                    "tpchild-"
                },
            )
        {
            return Err(invalid());
        }
    }
    Ok(())
}
fn body_cursor(
    id: &str,
    leaf: Option<&str>,
    child: &str,
    phase: &str,
    descriptor: &str,
    c: &ChildBodyCursor,
) -> bool {
    [
        &c.body.session_id,
        &c.body.lane,
        &c.body.leaf_id,
        &c.body.descriptor_id,
    ]
    .iter()
    .all(|s| valid_id(s))
        && c.direction == Direction::OldestFirst
        && c.child_id == child
        && c.phase_entry_id == phase
        && c.body.session_id == id
        && c.body.descriptor_id == descriptor
        && leaf.map_or(true, |s| s == c.body.leaf_id)
        && c.body.next_index < MAX_CHUNKS
}
fn profile() -> Value {
    json!({"version":1,"summaryCommand":SUMMARY_COMMAND,"phaseCommand":PHASE_COMMAND,
        "bodyCommand":BODY_COMMAND,"maxCount":32,"maxPhaseCandidates":128,
        "maxResponseBytes":131072,"responseBytesScope":"pageBytes","maxBodyDecodedBytes":65536,
        "summaryState":"unknown","recovery":"readOnly"})
}
pub(super) fn supported(session: &HostSession) -> bool {
    session.ready()["toolProgramChildren"] == profile()
        && [SUMMARY_COMMAND, PHASE_COMMAND, BODY_COMMAND]
            .iter()
            .all(|s| session.supports_command(s))
}
// Bound serialization of the DATA PAGE, not the duplicated data/result RPC frame.
pub(super) struct Counter {
    pub(super) bytes: usize,
    pub(super) max: usize,
}
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.max.saturating_sub(self.bytes) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "page byte budget exceeded",
            ));
        }
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
async fn read<Q: Serialize, P: DeserializeOwned>(
    state: &Service,
    id: &str,
    command: &str,
    query: &Q,
    max_bytes: usize,
) -> ServiceResult<P> {
    state.check_session_admission(id).await?;
    let session = state
        .live_session(id)
        .await
        .ok_or_else(|| failure(ReplyKind::NotFound, "session not found"))?;
    if !supported(&session) {
        return Err((
            ReplyKind::Unsupported,
            json!({"code":"tool_program_queries_unsupported",
            "error":"native host does not advertise the exact read-only child query profile"}),
        ));
    }
    let mut request = serde_json::to_value(query).map_err(|_| invalid())?;
    request["type"] = json!(command);
    request["id"] = json!(uuid::Uuid::new_v4().to_string());
    // Use the existing owner/correlation/admission/timeout path; no prompt lease,
    // execution, retry, new process, transcript fallback or background task.
    let response = session
        .send_timeout(request, Duration::from_secs(30))
        .await
        .map_err(|_| failure(ReplyKind::HostUnavailable, "native child query failed"))?;
    if response["success"] != true {
        ensure(response["success"] == false)?;
        // Native RPC currently drops structured SessionError codes. Never infer
        // caller fault from diagnostic text or expose backend paths/secrets.
        ensure(
            response["error"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty() && s.len() <= 4096),
        )?;
        return Err(failure(
            ReplyKind::HostUnavailable,
            "native child query rejected",
        ));
    }
    let data = response.get("data").ok_or_else(malformed)?;
    serde_json::to_writer(
        &mut Counter {
            bytes: 0,
            max: max_bytes.min(MAX_RESPONSE_BYTES),
        },
        data,
    )
    .map_err(|_| malformed())?;
    serde_json::from_value(data.clone()).map_err(|_| malformed())
}
fn leaf_matches(leaf: Option<&str>, expected: Option<&str>) -> bool {
    leaf.map_or(true, valid_id) && expected.map_or(true, |s| Some(s) == leaf)
}
fn next_cursor(
    id: &str,
    leaf: &str,
    dir: Direction,
    child: Option<&str>,
    previous: Option<&ChildCursor>,
    next: &ChildCursor,
) -> ServiceResult<()> {
    query_cursor(id, Some(leaf), dir, child, Some(next)).map_err(|_| malformed())?;
    if let Some(prev) = previous {
        ensure(
            prev.lane == next.lane
                && ordered(dir, prev.after_seq, next.after_seq)
                && prev.after_entry_id != next.after_entry_id,
        )?;
    }
    Ok(())
}
fn descriptor(d: &BodyDescriptor, child: &str) -> ServiceResult<()> {
    ensure(
        d.version == 1
            && uuid4(&d.body_id)
            && d.descriptor_id == format!("tpbody-{}-commit", d.body_id)
            && native_id(child, "tpchild-")
            && d.child_id == child
            && d.codec == "native-json-v1"
            && d.encoding == "base64"
            && d.chunk_bytes == CHUNK_BYTES
            && (1..=CHUNK_BYTES * MAX_CHUNKS).contains(&d.byte_length)
            && (1..=MAX_CHUNKS).contains(&d.chunks)
            && d.chunks == d.byte_length.div_ceil(CHUNK_BYTES)
            && hash(&d.sha256),
    )
}
pub(super) fn summary(s: &ChildSummary) -> ServiceResult<()> {
    ensure(
        s.version == 1
            && native_id(&s.child_id, "tpchild-")
            && [
                &s.origin.run_id,
                &s.origin.assistant_entry_id,
                &s.root_call_id,
                &s.root_tool_name,
                &s.tool_name,
            ]
            .iter()
            .all(|s| valid_id(s))
            && s.parent_child_id
                .as_ref()
                .map_or(true, |p| native_id(p, "tpchild-") && p != &s.child_id),
    )
}
fn validate_summaries(id: &str, q: &ChildPageQuery, p: &ChildPage) -> ServiceResult<()> {
    ensure(
        p.children.len() <= q.limit
            && (!p.scan_exhausted || p.children.len() < q.limit)
            && p.children.len() <= 32
            && leaf_matches(
                p.leaf_id.as_deref(),
                pinned(q.leaf_id.as_deref(), q.cursor.as_ref()),
            )
            && (p.children.is_empty() || p.leaf_id.is_some())
            && p.scan_exhausted == p.next_cursor.is_none()
            && (p.scan_exhausted || !p.children.is_empty()),
    )?;
    let mut ids = HashSet::new();
    let mut last_seq = q.cursor.as_ref().map(|c| c.after_seq);
    for s in &p.children {
        summary(s)?;
        ensure(
            ids.insert(&s.child_id)
                && last_seq.map_or(true, |seq| ordered(q.direction, seq, s.seq))
                && q.cursor
                    .as_ref()
                    .map_or(true, |c| c.after_entry_id != s.child_id),
        )?;
        last_seq = Some(s.seq);
    }
    // Parent metadata can be checked when that parent is on this page. Foreign
    // ancestry/assistant-slot/checkpoint verification remains native-owned.
    for s in &p.children {
        if let Some(parent) = p
            .children
            .iter()
            .find(|v| Some(&v.child_id) == s.parent_child_id.as_ref())
        {
            ensure(
                parent.seq < s.seq
                    && parent.origin.run_id == s.origin.run_id
                    && parent.origin.assistant_entry_id == s.origin.assistant_entry_id
                    && parent.origin.tool_index == s.origin.tool_index
                    && parent.root_call_id == s.root_call_id
                    && parent.root_tool_name == s.root_tool_name,
            )?;
        }
    }
    if let Some(c) = &p.next_cursor {
        next_cursor(
            id,
            p.leaf_id.as_deref().ok_or_else(malformed)?,
            q.direction,
            None,
            q.cursor.as_ref(),
            c,
        )?;
        let last = p.children.last().ok_or_else(malformed)?;
        ensure(c.after_entry_id == last.child_id && c.after_seq == last.seq)?;
    }
    Ok(())
}
pub(super) fn phase(p: &ChildPhase, child: &str) -> ServiceResult<()> {
    let closed = p.phase == ChildPhaseKind::Closed;
    let final_phase = p.phase == ChildPhaseKind::Final;
    ensure(
        p.version == 1
            && p.child_id == child
            && native_id(&p.entry_id, "tpphase-")
            && ((p.phase == ChildPhaseKind::Requested) == (p.index == 0))
            && p.failed.is_some() == closed
            && p.interrupted.is_some() == closed
            && p.is_error.is_some() == final_phase
            && p.cancelled.is_some() == final_phase
            && p.descriptor.is_none() == closed,
    )?;
    if let Some(d) = &p.descriptor {
        descriptor(d, child)?;
        ensure(matches!(
            (p.phase, d.kind),
            (ChildPhaseKind::Requested, BodyKind::Requested)
                | (ChildPhaseKind::Prepared, BodyKind::Prepared)
                | (ChildPhaseKind::Update, BodyKind::Update)
                | (ChildPhaseKind::Final, BodyKind::Final)
        ))?;
    }
    Ok(())
}
fn validate_phases(id: &str, q: &ChildPhaseQuery, p: &ChildPhasePage) -> ServiceResult<()> {
    ensure(
        p.child_id == q.child_id
            && valid_id(&p.leaf_id)
            && leaf_matches(
                Some(&p.leaf_id),
                pinned(q.leaf_id.as_deref(), q.cursor.as_ref()),
            )
            && p.phases.len() <= q.limit
            && p.phases.len() <= 32
            && p.scanned <= 128
            && p.scanned >= p.phases.len()
            && p.scan_exhausted == p.next_cursor.is_none()
            && (p.scan_exhausted || p.scanned > 0)
            && (!p.scan_exhausted || p.scanned < 128),
    )?;
    let mut ids = HashSet::new();
    let mut last_seq = q.cursor.as_ref().map(|c| c.after_seq);
    let mut last_index = None;
    for v in &p.phases {
        phase(v, &q.child_id)?;
        ensure(
            ids.insert(&v.entry_id)
                && q.cursor
                    .as_ref()
                    .map_or(true, |c| c.after_entry_id != v.entry_id)
                && last_seq.map_or(true, |seq| ordered(q.direction, seq, v.seq))
                && last_index.map_or(true, |index| ordered(q.direction, index, v.index)),
        )?;
        last_seq = Some(v.seq);
        last_index = Some(v.index);
    }
    // Cursor is the last scanned candidate, potentially a different child.
    // v1 carries no phase index, so only sequence order spans page boundaries.
    if let Some(c) = &p.next_cursor {
        next_cursor(
            id,
            &p.leaf_id,
            q.direction,
            Some(&q.child_id),
            q.cursor.as_ref(),
            c,
        )?;
        if let Some(last) = p.phases.last() {
            ensure(
                (c.after_seq == last.seq && c.after_entry_id == last.entry_id)
                    || (p.scanned > p.phases.len() && ordered(q.direction, last.seq, c.after_seq)),
            )?;
        }
        if p.scanned == p.phases.len() {
            let last = p.phases.last().ok_or_else(malformed)?;
            ensure(c.after_seq == last.seq && c.after_entry_id == last.entry_id)?;
        }
        ensure(
            p.phases
                .iter()
                .all(|v| v.entry_id != c.after_entry_id || v.seq == c.after_seq),
        )?;
    }
    Ok(())
}
fn validate_body(id: &str, q: &ChildBodyQuery, p: &ChildBodyPage) -> ServiceResult<()> {
    let b = &p.body;
    let d = &b.descriptor;
    let pinned = q
        .leaf_id
        .as_deref()
        .or_else(|| q.cursor.as_ref().map(|c| c.body.leaf_id.as_str()));
    descriptor(d, &q.child_id)?;
    ensure(
        p.child_id == q.child_id
            && p.phase_entry_id == q.phase_entry_id
            && d.descriptor_id == q.descriptor_id
            && valid_id(&b.leaf_id)
            && leaf_matches(Some(&b.leaf_id), pinned)
            && b.next_cursor.is_none()
            && b.total_bytes == d.byte_length
            && !b.chunks.is_empty()
            && b.chunks.len() <= q.limit
            && b.chunks.len() <= 8
            && b.byte_length <= q.max_bytes
            && b.byte_length <= 65536,
    )?;
    let start = q.cursor.as_ref().map_or(0, |c| c.body.next_index);
    ensure(start < d.chunks && b.byte_offset == start * CHUNK_BYTES)?;
    let mut length = 0;
    for (offset, c) in b.chunks.iter().enumerate() {
        let index = start + offset;
        ensure(index < d.chunks)?;
        let expected = (d.byte_length - index * CHUNK_BYTES).min(CHUNK_BYTES);
        ensure(length + expected <= q.max_bytes && length + expected <= 65536)?;
        ensure(
            c.index == index
                && c.byte_offset == index * CHUNK_BYTES
                && c.byte_length == expected
                && hash(&c.sha256)
                && c.data.len() == expected.div_ceil(3) * 4,
        )?;
        let bytes = STANDARD.decode(&c.data).map_err(|_| malformed())?;
        ensure(
            bytes.len() == expected
                && STANDARD.encode(&bytes) == c.data
                && format!("{:x}", Sha256::digest(&bytes)) == c.sha256,
        )?;
        length += expected;
    }
    ensure(length == b.byte_length)?;
    let next = start + b.chunks.len();
    ensure(p.next_cursor.is_some() == (next < d.chunks))?;
    if let Some(c) = &p.next_cursor {
        ensure(
            body_cursor(
                id,
                Some(&b.leaf_id),
                &q.child_id,
                &q.phase_entry_id,
                &q.descriptor_id,
                c,
            ) && c.body.next_index == next
                && q.cursor
                    .as_ref()
                    .map_or(true, |prev| prev.body.lane == c.body.lane),
        )?;
    }
    // Consumers must stream ALL pages to verify descriptor.sha256. Neither
    // descriptor presence nor a page verifies unread chunks/whole-body integrity.
    Ok(())
}
/// Read authoritative begins only, preserving Unknown and historical origins.
/// This never opens a session or fetches bodies to render summaries.
pub async fn child_summaries(
    state: &Service,
    id: String,
    query: ChildPageQuery,
) -> ServiceResult<ChildPage> {
    query_base(
        &id,
        query.limit,
        query.max_bytes,
        32,
        MAX_RESPONSE_BYTES,
        query.leaf_id.as_deref(),
    )?;
    query_cursor(
        &id,
        query.leaf_id.as_deref(),
        query.direction,
        None,
        query.cursor.as_ref(),
    )?;
    let page = read(state, &id, SUMMARY_COMMAND, &query, query.max_bytes).await?;
    validate_summaries(&id, &query, &page)?;
    Ok(page)
}
/// Read a bounded candidate scan. Empty continuation and scan exhaustion do not
/// establish completion; the only summary state is Unknown.
pub async fn child_phases(
    state: &Service,
    id: String,
    query: ChildPhaseQuery,
) -> ServiceResult<ChildPhasePage> {
    query_base(
        &id,
        query.limit,
        query.max_bytes,
        32,
        MAX_RESPONSE_BYTES,
        query.leaf_id.as_deref(),
    )?;
    if !native_id(&query.child_id, "tpchild-") {
        return Err(invalid());
    }
    query_cursor(
        &id,
        query.leaf_id.as_deref(),
        query.direction,
        Some(&query.child_id),
        query.cursor.as_ref(),
    )?;
    let page = read(state, &id, PHASE_COMMAND, &query, query.max_bytes).await?;
    validate_phases(&id, &query, &page)?;
    Ok(page)
}
/// Read exact phase-authorized chunks. Verifies returned chunks, not unread
/// chunks or the whole body. Chunk indexes are always ascending.
pub async fn child_body(
    state: &Service,
    id: String,
    query: ChildBodyQuery,
) -> ServiceResult<ChildBodyPage> {
    query_base(
        &id,
        query.limit,
        query.max_bytes,
        8,
        65536,
        query.leaf_id.as_deref(),
    )?;
    if !native_id(&query.child_id, "tpchild-")
        || !native_id(&query.phase_entry_id, "tpphase-")
        || !valid_id(&query.descriptor_id)
        || query.cursor.as_ref().is_some_and(|c| {
            !body_cursor(
                &id,
                query.leaf_id.as_deref(),
                &query.child_id,
                &query.phase_entry_id,
                &query.descriptor_id,
                c,
            )
        })
    {
        return Err(invalid());
    }
    let page = read(state, &id, BODY_COMMAND, &query, MAX_RESPONSE_BYTES).await?;
    validate_body(&id, &query, &page)?;
    Ok(page)
}

#[cfg(test)]
mod tests;
