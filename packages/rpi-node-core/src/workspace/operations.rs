//! Owner-scoped, bounded receipts for remote mutations; no socket or Agent owner.
//!
//! Only the authenticated adapter may supply `principal`. Operation references are
//! not credentials. Reserve does not execute anything. Execute must first obtain
//! the unique permit returned by `claim`; unknown IDs can never create a record.
use rpi_node_api::api::realtime::{
    MAX_ID_BYTES, MAX_MESSAGE_BYTES, OperationOutcome, OperationRef, OperationStatus,
    RealtimeOperationKind,
};
use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{self, Write},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

const MAX_ENTRIES: usize = 1024;
const MAX_RESERVED_PER_PRINCIPAL: usize = 32;
const MAX_RESULT_BYTES: usize = 16 * 1024 * 1024;
// Leave room for the transport envelope around a cached result.
const MAX_OUTCOME_BYTES: usize = MAX_MESSAGE_BYTES - 4096;
const RESERVATION_TTL: Duration = Duration::from_secs(60);
const RESULT_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OperationError {
    #[error("invalid operation binding")]
    InvalidInput,
    #[error("operation access denied")]
    AccessDenied,
    #[error("operation payload conflicts with its first execution")]
    Conflict,
    #[error("operation is unknown or expired; do not execute again")]
    UnknownOperation,
    #[error("operation registry capacity exceeded")]
    LimitExceeded,
    #[error("operation registry unavailable")]
    Unavailable,
}

#[derive(Clone, Copy)]
struct Limits {
    entries: usize,
    result_bytes: usize,
}
struct Entry {
    principal: String,
    client_instance_id: String,
    session_id: String,
    kind: RealtimeOperationKind,
    order: u64,
    digest: Option<[u8; 32]>,
    status: OperationStatus,
    expires_at: Option<Instant>,
    result_bytes: usize,
}
struct State {
    entries: HashMap<String, Entry>,
    next: u64,
    result_bytes: usize,
    closed: bool,
}
struct Inner {
    epoch: String,
    limits: Limits,
    state: Mutex<State>,
}

/// Shared by all adapters attached to one `LocalWorkspace`, not by unrelated owners.
/// There is deliberately no Debug implementation which might reveal cached data.
#[derive(Clone)]
pub struct RemoteOperations(Arc<Inner>);

pub enum OperationClaim {
    Execute(OperationPermit),
    Existing(OperationStatus),
}

/// The only successful transition from Reserved to Dispatching. Dropping it leaves
/// an uncertain receipt, never a new reservation. Do not hold it in the WS task:
/// transfer it to the existing owner executor before publishing to the Agent.
pub struct OperationPermit {
    registry: RemoteOperations,
    reference: OperationRef,
    client_instance_id: String,
    session_id: String,
    kind: RealtimeOperationKind,
    payload: Value,
    finished: bool,
}

impl RemoteOperations {
    pub(crate) fn new() -> Result<Self, OperationError> {
        Self::with_limits(Limits {
            entries: MAX_ENTRIES,
            result_bytes: MAX_RESULT_BYTES,
        })
    }
    fn with_limits(limits: Limits) -> Result<Self, OperationError> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|_| OperationError::Unavailable)?;
        Ok(Self(Arc::new(Inner {
            epoch: format!("{:032x}", u128::from_le_bytes(bytes)),
            limits,
            state: Mutex::new(State {
                entries: HashMap::new(),
                next: 0,
                result_bytes: 0,
                closed: false,
            }),
        })))
    }
    #[must_use]
    pub fn owner_epoch(&self) -> &str {
        &self.0.epoch
    }
    fn lock(&self) -> Result<MutexGuard<'_, State>, OperationError> {
        self.0.state.lock().map_err(|_| OperationError::Unavailable)
    }
    pub fn reserve(
        &self,
        principal: &str,
        client_instance_id: &str,
        session_id: &str,
        kind: RealtimeOperationKind,
    ) -> Result<OperationRef, OperationError> {
        check_id(principal)?;
        check_id(client_instance_id)?;
        check_id(session_id)?;
        let now = Instant::now();
        let mut state = self.lock()?;
        state.purge(now);
        if state.closed {
            return Err(OperationError::Unavailable);
        }
        if state
            .entries
            .values()
            .filter(|entry| {
                entry.principal == principal && entry.status == OperationStatus::Reserved
            })
            .count()
            >= MAX_RESERVED_PER_PRINCIPAL
        {
            return Err(OperationError::LimitExceeded);
        }
        while state.entries.len() >= self.0.limits.entries {
            if !state.evict_final(None) {
                return Err(OperationError::LimitExceeded);
            }
        }
        // Never reuse an ID within this epoch, including after cache eviction.
        // A nonce also prevents callers from enumerating another client's IDs.
        let next = state
            .next
            .checked_add(1)
            .ok_or(OperationError::LimitExceeded)?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| OperationError::Unavailable)?;
        let id = format!("{next:016x}-{:032x}", u128::from_le_bytes(nonce));
        state.next = next;
        state.entries.insert(
            id.clone(),
            Entry {
                principal: principal.into(),
                client_instance_id: client_instance_id.into(),
                session_id: session_id.into(),
                kind,
                order: next,
                digest: None,
                status: OperationStatus::Reserved,
                expires_at: Some(now + RESERVATION_TTL),
                result_bytes: 0,
            },
        );
        Ok(OperationRef {
            owner_epoch: self.0.epoch.clone(),
            operation_id: id,
        })
    }
    /// Atomically bind the first payload. A duplicate receives only a receipt,
    /// never a permit. Payload is owned by the permit, not retained in the ledger.
    pub fn claim(
        &self,
        principal: &str,
        reference: &OperationRef,
        payload: Value,
    ) -> Result<OperationClaim, OperationError> {
        check_id(principal)?;
        reference
            .validate()
            .map_err(|_| OperationError::InvalidInput)?;
        let digest = payload_digest(&payload)?;
        let mut state = self.lock()?;
        state.purge(Instant::now());
        if reference.owner_epoch != self.0.epoch {
            return Err(OperationError::UnknownOperation);
        }
        let closed = state.closed;
        let entry = state
            .entries
            .get_mut(&reference.operation_id)
            .ok_or(OperationError::UnknownOperation)?;
        if entry.principal != principal {
            return Err(OperationError::AccessDenied);
        }
        if let Some(previous) = entry.digest {
            if previous != digest {
                return Err(OperationError::Conflict);
            }
            return Ok(OperationClaim::Existing(entry.status.clone()));
        }
        if closed || entry.status != OperationStatus::Reserved {
            return Err(OperationError::UnknownOperation);
        }
        entry.digest = Some(digest);
        entry.status = OperationStatus::Dispatching;
        // In-flight entries must never expire or be evicted while work can run.
        entry.expires_at = None;
        Ok(OperationClaim::Execute(OperationPermit {
            registry: self.clone(),
            reference: reference.clone(),
            client_instance_id: entry.client_instance_id.clone(),
            session_id: entry.session_id.clone(),
            kind: entry.kind,
            payload,
            finished: false,
        }))
    }
    pub fn status(
        &self,
        principal: &str,
        reference: &OperationRef,
    ) -> Result<OperationStatus, OperationError> {
        check_id(principal)?;
        reference
            .validate()
            .map_err(|_| OperationError::InvalidInput)?;
        let mut state = self.lock()?;
        state.purge(Instant::now());
        if reference.owner_epoch != self.0.epoch {
            return Ok(OperationStatus::OutcomeUnknown);
        }
        let Some(entry) = state.entries.get(&reference.operation_id) else {
            return Ok(OperationStatus::OutcomeUnknown);
        };
        if entry.principal != principal {
            return Err(OperationError::AccessDenied);
        }
        Ok(entry.status.clone())
    }
    /// Closing the actual owner is permanent; listener/socket changes don't call
    /// this. Already-dispatching permits may still record their eventual result.
    pub(crate) fn close(&self) {
        if let Ok(mut state) = self.lock() {
            state.closed = true;
            for entry in state.entries.values_mut() {
                if entry.status == OperationStatus::Reserved {
                    entry.status = OperationStatus::OutcomeUnknown;
                }
            }
        }
    }
}
impl OperationPermit {
    #[must_use]
    pub fn reference(&self) -> &OperationRef {
        &self.reference
    }
    #[must_use]
    pub fn client_instance_id(&self) -> &str {
        &self.client_instance_id
    }
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    #[must_use]
    pub fn kind(&self) -> RealtimeOperationKind {
        self.kind
    }
    #[must_use]
    pub fn payload(&self) -> &Value {
        &self.payload
    }

    /// Record a known RPC result, NOT Agent settlement. Unknown execution errors
    /// must instead drop the permit. No lock is held while invoking the owner.
    pub fn finish(mut self, outcome: OperationOutcome) -> Result<OperationStatus, OperationError> {
        if matches!(&outcome, OperationOutcome::Failed { error }
            if error.code == rpi_node_api::api::realtime::RealtimeErrorCode::OutcomeUnknown)
        {
            // Do not let an adapter turn uncertainty into a known rejection.
            // Drop retains the digest and fences every retry of this operation.
            drop(self);
            return Ok(OperationStatus::OutcomeUnknown);
        }
        let bytes = measured_bytes(&outcome, MAX_OUTCOME_BYTES);
        let mut state = self.registry.lock()?;
        let id = &self.reference.operation_id;
        let Some(entry) = state.entries.get(id) else {
            return Err(OperationError::UnknownOperation);
        };
        if entry.status != OperationStatus::Dispatching {
            return Err(OperationError::Conflict);
        }
        let bytes = bytes
            .ok()
            .filter(|bytes| *bytes <= self.registry.0.limits.result_bytes);
        if let Some(bytes) = bytes {
            while state.result_bytes + bytes > self.registry.0.limits.result_bytes {
                if !state.evict_final(Some(id)) {
                    break;
                }
            }
        }
        let status = if bytes
            .is_some_and(|bytes| state.result_bytes + bytes <= self.registry.0.limits.result_bytes)
        {
            OperationStatus::KnownResult { outcome }
        } else {
            OperationStatus::OutcomeUnknown
        };
        let bytes = if matches!(status, OperationStatus::KnownResult { .. }) {
            bytes.unwrap_or(0)
        } else {
            0
        };
        let entry = state
            .entries
            .get_mut(id)
            .ok_or(OperationError::UnknownOperation)?;
        entry.status = status.clone();
        entry.result_bytes = bytes;
        entry.expires_at = Some(Instant::now() + RESULT_TTL);
        state.result_bytes += bytes;
        self.finished = true;
        Ok(status)
    }
}
impl Drop for OperationPermit {
    fn drop(&mut self) {
        if !self.finished {
            if let Ok(mut state) = self.registry.lock() {
                if let Some(entry) = state.entries.get_mut(&self.reference.operation_id) {
                    if entry.status == OperationStatus::Dispatching {
                        entry.status = OperationStatus::OutcomeUnknown;
                        entry.expires_at = Some(Instant::now() + RESULT_TTL);
                    }
                }
            }
        }
    }
}
impl State {
    fn remove(&mut self, id: &str) {
        if let Some(entry) = self.entries.remove(id) {
            self.result_bytes -= entry.result_bytes;
        }
    }
    fn purge(&mut self, now: Instant) {
        let expired: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.expires_at.is_some_and(|deadline| deadline <= now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.remove(&id);
        }
    }
    fn evict_final(&mut self, except: Option<&str>) -> bool {
        let oldest = self
            .entries
            .iter()
            .filter(|(id, entry)| {
                except != Some(id.as_str())
                    && matches!(
                        entry.status,
                        OperationStatus::KnownResult { .. } | OperationStatus::OutcomeUnknown
                    )
            })
            .min_by_key(|(_, entry)| entry.order)
            .map(|(id, _)| id.clone());
        if let Some(id) = oldest {
            self.remove(&id);
            true
        } else {
            false
        }
    }
}
fn check_id(value: &str) -> Result<(), OperationError> {
    if value.trim().is_empty() || value.len() > MAX_ID_BYTES || value.chars().any(char::is_control)
    {
        Err(OperationError::InvalidInput)
    } else {
        Ok(())
    }
}

// Stable object ordering even when serde_json's preserve_order feature is enabled.
// Stream directly into a bounded hash writer; do not copy large attachment values.
struct Canonical<'a>(&'a Value);
impl Serialize for Canonical<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Object(map) => {
                let mut keys: Vec<_> = map.keys().collect();
                keys.sort_unstable();
                let mut output = serializer.serialize_map(Some(keys.len()))?;
                for key in keys {
                    output.serialize_entry(key, &Canonical(&map[key]))?;
                }
                output.end()
            }
            Value::Array(array) => serializer.collect_seq(array.iter().map(Canonical)),
            value => value.serialize(serializer),
        }
    }
}
struct BudgetWriter {
    hash: Sha256,
    bytes: usize,
    limit: usize,
}
impl Write for BudgetWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes) {
            return Err(io::Error::other("operation byte budget exceeded"));
        }
        self.hash.update(bytes);
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn payload_digest(payload: &Value) -> Result<[u8; 32], OperationError> {
    let mut writer = BudgetWriter {
        hash: Sha256::new(),
        bytes: 0,
        limit: MAX_MESSAGE_BYTES,
    };
    serde_json::to_writer(&mut writer, &Canonical(payload))
        .map_err(|_| OperationError::LimitExceeded)?;
    Ok(writer.hash.finalize().into())
}
fn measured_bytes(value: &impl Serialize, limit: usize) -> Result<usize, OperationError> {
    let mut writer = BudgetWriter {
        hash: Sha256::new(),
        bytes: 0,
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| OperationError::LimitExceeded)?;
    Ok(writer.bytes)
}

#[cfg(test)]
mod tests;
