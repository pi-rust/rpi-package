//! Bounded native-owned saved history; no executable host or transcript mirror.
use super::*;
#[cfg(test)]
mod tests;
use std::{io::Write, path::PathBuf, time::Duration};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceHistoryCursor {
    pub version: u32,
    pub source: String,
    pub leaf_id: Option<String>,
    pub after_seq: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceHistoryRequest {
    pub limit: usize,
    pub cursor: Option<ResourceHistoryCursor>,
}
impl Default for ResourceHistoryRequest {
    fn default() -> Self {
        Self {
            limit: 64,
            cursor: None,
        }
    }
}
impl ResourceHistoryRequest {
    pub fn validate(&self) -> ResourceResult<()> {
        if !(1..=128).contains(&self.limit)
            || self.cursor.as_ref().is_some_and(|c| {
                c.version != 1
                    || c.after_seq == 0
                    || c.after_seq > 9_007_199_254_740_991
                    || !fingerprint(&c.source)
                    || c.leaf_id.as_ref().is_some_and(|s| {
                        s.is_empty() || s.len() > 512 || s.chars().any(char::is_control)
                    })
            })
        {
            return Err(ResourceError::new(
                ResourceErrorKind::InvalidInput,
                "invalid saved history limit/cursor",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceSavedBinding {
    pub revision: u64,
    pub fingerprint: Option<String>,
    pub operation_id: Option<String>,
}
impl ResourceSavedBinding {
    pub(crate) fn expected(&self) -> ExpectedBinding {
        ExpectedBinding {
            revision: self.revision,
            fingerprint: self.fingerprint.clone(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceSavedPending {
    pub execution_queue_or_recovery: bool,
    pub progress: bool,
    pub idle_proof_supported: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceSavedHistory {
    pub session_id: String,
    pub cwd: PathBuf,
    pub leaf_id: Option<String>,
    pub source: String,
    pub messages: Vec<Value>,
    pub cursor: Option<ResourceHistoryCursor>,
    pub filtered: bool,
    pub custom_entries: Vec<Value>,
    pub binding: ResourceSavedBinding,
    pub pending: ResourceSavedPending,
    #[serde(default)]
    pub executable: bool,
    #[serde(default)]
    pub blocked_operation_id: Option<String>,
}
impl ResourceSavedHistory {
    pub(crate) fn ensure_idle(&self) -> ResourceResult<()> {
        if !self.pending.idle_proof_supported {
            return Err(ResourceError::new(ResourceErrorKind::Unsupported,"native saved session has no canonical durable idle proof; deliberate original open/close required"));
        }
        if self.pending.execution_queue_or_recovery || self.pending.progress {
            return Err(ResourceError::conflict(
                "native saved execution/queue/recovery/progress is pending",
            ));
        }
        let durable = rpi_durable_core::DurableState::try_from_branch(&self.custom_entries)
            .map_err(ResourceError::unavailable)?;
        if durable.run.as_ref().is_some_and(|run| {
            matches!(
                run.status,
                rpi_durable_core::RunStatus::Queued
                    | rpi_durable_core::RunStatus::Running
                    | rpi_durable_core::RunStatus::Interrupted
            )
        }) || durable.queue.iter().any(|item| {
            matches!(
                item.status,
                rpi_durable_core::QueueStatus::Queued | rpi_durable_core::QueueStatus::Claimed
            )
        }) {
            return Err(ResourceError::conflict(
                "canonical saved durable work is pending",
            ));
        }
        Ok(())
    }
}
fn fingerprint(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    #[serde(rename = "type")]
    kind: String,
    success: bool,
    data: Option<ResourceSavedHistory>,
    error: Option<Failure>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Failure {
    code: String,
    message: String,
}
fn parse(
    ok: bool,
    bytes: &[u8],
    id: &str,
    query: &ResourceHistoryRequest,
) -> ResourceResult<ResourceSavedHistory> {
    let envelope: Envelope = serde_json::from_slice(bytes)
        .map_err(|_| ResourceError::unavailable("invalid native saved reader envelope"))?;
    if envelope.version != 1 || envelope.kind != "saved_session_read" || envelope.success != ok {
        return Err(ResourceError::unavailable(
            "native saved reader outcome mismatch",
        ));
    }
    if !ok {
        if envelope.data.is_some() {
            return Err(ResourceError::unavailable(
                "native saved failure contains success data",
            ));
        }
        let error = envelope
            .error
            .ok_or_else(|| ResourceError::unavailable("native saved failure missing diagnostic"))?;
        let kind = match error.code.as_str() {
            "busy" | "branch_changed" => ResourceErrorKind::Conflict,
            "unsupported" => ResourceErrorKind::Unsupported,
            "limit_exceeded" => ResourceErrorKind::LimitExceeded,
            "invalid_input" => ResourceErrorKind::InvalidInput,
            _ => ResourceErrorKind::Unavailable,
        };
        return Err(ResourceError::new(kind, error.message));
    }
    if envelope.error.is_some() {
        return Err(ResourceError::unavailable(
            "native saved success contains an error",
        ));
    }
    let data = envelope
        .data
        .ok_or_else(|| ResourceError::unavailable("native saved success missing data"))?;
    if data.session_id != id
        || !data.cwd.is_absolute()
        || !data.filtered
        || data.executable
        || data.blocked_operation_id.is_some()
        || data.messages.len() > query.limit
        || data.custom_entries.len() > 1024
        || !fingerprint(&data.source)
        || (data.binding.revision == 0) != data.binding.fingerprint.is_none()
        || data
            .binding
            .fingerprint
            .as_ref()
            .is_some_and(|s| !fingerprint(s))
        || (data.binding.revision == 0 && data.binding.operation_id.is_some())
    {
        return Err(ResourceError::unavailable(
            "invalid native saved identity/binding/page",
        ));
    }
    if let Some(operation) = &data.binding.operation_id {
        validate_operation_id(operation)
            .map_err(|_| ResourceError::unavailable("invalid saved native operation identity"))?;
    }
    for entry in &data.custom_entries {
        if entry["type"] != "custom" || entry["customType"] != crate::durable::ENTRY_TYPE {
            return Err(ResourceError::unavailable(
                "saved reader returned an unrequested custom type",
            ));
        }
    }
    for message in &data.messages {
        if !message.is_object()
            || !message["entryId"]
                .as_str()
                .is_some_and(|s| !s.is_empty() && s.len() <= 512)
        {
            return Err(ResourceError::unavailable("invalid saved message identity"));
        }
    }
    if let Some(cursor) = &data.cursor {
        ResourceHistoryRequest {
            limit: query.limit,
            cursor: Some(cursor.clone()),
        }
        .validate()
        .map_err(|_| ResourceError::unavailable("invalid native saved cursor"))?;
        if cursor.source != data.source
            || cursor.leaf_id != data.leaf_id
            || data.messages.is_empty()
        {
            return Err(ResourceError::unavailable(
                "native saved cursor is not anchored to page",
            ));
        }
    }
    Ok(data)
}
struct RequestFile(PathBuf);
impl RequestFile {
    fn create(bytes: &[u8]) -> ResourceResult<Self> {
        let path =
            std::env::temp_dir().join(format!("rpi-resource-read-{}.json", uuid::Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(ResourceError::unavailable)?;
        let owner = Self(path);
        file.write_all(bytes).map_err(ResourceError::unavailable)?;
        Ok(owner)
    }
}
impl Drop for RequestFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
pub(crate) struct SavedReadLease {
    pub data: ResourceSavedHistory,
    _lock: super::lock::ObservationLock,
}
pub(crate) async fn read(
    bin: PathBuf,
    cwd: PathBuf,
    root: PathBuf,
    path: PathBuf,
    id: String,
    query: ResourceHistoryRequest,
) -> ResourceResult<SavedReadLease> {
    query.validate()?;
    tokio::task::spawn_blocking(move || {
        local::negotiate_sync(&bin,&cwd)?.require_saved_read()?;
        let lock=super::lock::ObservationLock::acquire(&root,&path)?;
        let bytes=serde_json::to_vec(&serde_json::json!({"version":1,"sessionId":id,"sessionRoot":root,"sessionPath":path,"limit":query.limit,"customTypes":[crate::durable::ENTRY_TYPE],"cursor":query.cursor})).map_err(ResourceError::unavailable)?;
        if bytes.len()>MAX_CONFIG_BYTES {return Err(ResourceError::new(ResourceErrorKind::LimitExceeded,"saved history request exceeds 64 KiB"));}
        let file=RequestFile::create(&bytes)?;
        let (ok,bytes,_)=local::invoke(&bin,&cwd,&["--resource-session-read".into(),file.0.as_os_str().into()],8*1024*1024,Duration::from_secs(15))?;
        let data=parse(ok,&bytes,&id,&query)?;
        let expected=std::fs::canonicalize(&cwd).map_err(ResourceError::unavailable)?;
        let recorded=std::fs::canonicalize(&data.cwd).map_err(ResourceError::unavailable)?;
        if expected!=recorded {return Err(ResourceError::conflict("saved native session belongs to a different project"));}
        Ok(SavedReadLease {data,_lock:lock})
    }).await.map_err(|_|ResourceError::unavailable("saved history native owner failed"))?
}
