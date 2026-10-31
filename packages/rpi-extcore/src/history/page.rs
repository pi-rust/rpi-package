//! Bounded native history preparation, independent of client presentation.
use crate::host::{BufferedEvent, HostSession};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{broadcast, watch, OwnedMutexGuard},
    time::Instant,
};

pub const MAX_PAGE_MESSAGES: usize = 128;
pub const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_SPOOL_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_FALLBACK_BYTES: usize = 64 * 1024 * 1024;
pub const PREPARATION_BUDGET: Duration = Duration::from_secs(30);
const RPC_BUDGET: Duration = Duration::from_secs(15);

// Stop serialization at the bound rather than allocating an oversized copy
// just to discover that an untrusted native response is too large.
fn bounded_json<T: serde::Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, String> {
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_PAGE_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "history page exceeds byte budget",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded(Vec::new());
    serde_json::to_writer(&mut writer, value).map_err(|e| e.to_string())?;
    Ok(writer.0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchAnchor {
    pub leaf_id: Option<String>,
    pub total: u64,
}
#[derive(Debug)]
pub struct MessagePage {
    pub messages: Vec<Value>,
    pub anchor: BranchAnchor,
    pub first_id: Option<String>,
    pub last_id: Option<String>,
    pub has_more: bool,
}
fn optional_id(data: &Value, key: &str) -> Result<Option<String>, String> {
    match data.get(key) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(id)) if !id.is_empty() && id.len() <= 1024 => Ok(Some(id.clone())),
        _ => Err(format!("invalid history {key}")),
    }
}
pub fn validate_page(
    data: Value,
    before: Option<&str>,
    limit: usize,
    expected: Option<&BranchAnchor>,
) -> Result<MessagePage, String> {
    if !(1..=MAX_PAGE_MESSAGES).contains(&limit) {
        return Err("history page limit out of range".into());
    }
    bounded_json(&data)?;
    let anchor = BranchAnchor {
        leaf_id: optional_id(&data, "leafId")?,
        total: data["total"].as_u64().ok_or("invalid history total")?,
    };
    if anchor.total > 0 && anchor.leaf_id.is_none() {
        return Err("missing history branch leaf".into());
    }
    if expected.is_some_and(|a| a != &anchor) {
        return Err("history branch changed".into());
    }
    if optional_id(&data, "beforeId")?.as_deref() != before {
        return Err("history cursor echo mismatch".into());
    }
    let first_id = optional_id(&data, "firstId")?;
    let last_id = optional_id(&data, "lastId")?;
    let has_more = data["hasMore"].as_bool().ok_or("invalid history hasMore")?;
    let messages = data["messages"]
        .as_array()
        .ok_or("invalid history messages")?;
    if messages.len() > limit
        || messages.len() as u64 > anchor.total
        || (before.is_some() && messages.len() as u64 >= anchor.total)
        || (has_more && (messages.len() != limit || messages.len() as u64 >= anchor.total))
        || (before.is_none() && !has_more && messages.len() as u64 != anchor.total)
    {
        return Err("inconsistent history pagination".into());
    }
    let mut ids = HashSet::new();
    for message in messages {
        let id = message["entryId"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 1024)
            .ok_or("invalid history entryId")?;
        if Some(id) == before || !ids.insert(id) || !message["role"].is_string() {
            return Err("invalid or repeated history entry".into());
        }
    }
    if first_id.as_deref() != messages.first().and_then(|m| m["entryId"].as_str())
        || last_id.as_deref() != messages.last().and_then(|m| m["entryId"].as_str())
        || (has_more && messages.is_empty())
    {
        return Err("inconsistent history page bounds".into());
    }
    Ok(MessagePage {
        messages: messages.clone(),
        anchor,
        first_id,
        last_id,
        has_more,
    })
}

/// Holds the existing run lease through both preparation and replay. Dropping
/// the owner or its preparation future cancels work without a detached task.
pub struct HistoryRead {
    host: Arc<HostSession>,
    _lease: OwnedMutexGuard<()>,
    _events: broadcast::Receiver<BufferedEvent>,
    cancellation: watch::Receiver<u64>,
    boundary: u64,
    deadline: Instant,
}
impl HistoryRead {
    pub async fn begin(host: Arc<HostSession>) -> Result<Self, String> {
        let deadline = Instant::now() + PREPARATION_BUDGET;
        let lease = host.try_run_lease()?;
        let events = host.subscribe();
        let cancellation = host.history_cancellation();
        let boundary = tokio::time::timeout_at(deadline, host.history_boundary())
            .await
            .map_err(|_| "history preparation timed out")??;
        Ok(Self {
            host,
            _lease: lease,
            _events: events,
            cancellation,
            boundary,
            deadline,
        })
    }
    pub fn supports_pages(&self) -> bool {
        self.host.supports_command("get_messages")
    }
    fn check(&self) -> Result<Duration, String> {
        self.ensure_active()?;
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| "history preparation timed out".into())
    }
    pub fn ensure_active(&self) -> Result<(), String> {
        if self.cancellation.has_changed().unwrap_or(true) {
            return Err("history read cancelled".into());
        }
        Ok(())
    }
    async fn command(&self, kind: &str, params: Value) -> Result<Value, String> {
        let budget = self.check()?.min(RPC_BUDGET);
        let mut cancellation = self.cancellation.clone();
        tokio::select! {
            _ = cancellation.changed() => Err("history read cancelled".into()),
            result = self.host.command(kind,params,budget) => result,
        }
    }
    pub async fn page(
        &self,
        before: Option<&str>,
        expected: Option<&BranchAnchor>,
        limit: usize,
    ) -> Result<MessagePage, String> {
        if !(1..=MAX_PAGE_MESSAGES).contains(&limit) {
            return Err("history page limit out of range".into());
        }
        let data = self
            .command("get_messages", json!({"limit":limit,"beforeId":before}))
            .await?;
        let page = validate_page(data, before, limit, expected)?;
        self.check()?;
        Ok(page)
    }
    async fn verify_boundary(&self) -> Result<(), String> {
        self.check()?;
        if self.host.history_boundary().await? != self.boundary {
            return Err("history event boundary changed".into());
        }
        self.check()?;
        Ok(())
    }
    /// Validate the complete branch before exposing any page to a client.
    /// Capability absence alone selects fallback; bad native replies never do.
    pub async fn prepare(&self, saved: Option<&Path>) -> Result<HistorySpool, String> {
        tokio::time::timeout_at(self.deadline, async {
            let mut spool = HistorySpool::create()?;
            if self.supports_pages() {
                let mut before = None;
                let mut anchor = None;
                let mut seen = HashSet::new();
                loop {
                    let page = self
                        .page(before.as_deref(), anchor.as_ref(), MAX_PAGE_MESSAGES)
                        .await?;
                    for message in &page.messages {
                        if !seen.insert(message["entryId"].as_str().unwrap().to_owned()) {
                            return Err("repeated history entry across pages".into());
                        }
                    }
                    let count = seen.len() as u64;
                    if count > page.anchor.total || (page.has_more && count >= page.anchor.total) {
                        return Err("history cursor made no progress".into());
                    }
                    spool.push(&page.messages)?;
                    self.check()?;
                    if !page.has_more {
                        if count != page.anchor.total {
                            return Err("incomplete history traversal".into());
                        }
                        self.page(None, Some(&page.anchor), 1).await?;
                        break;
                    }
                    before = page.first_id;
                    anchor = Some(page.anchor);
                }
            } else {
                let path = saved.ok_or("saved transcript required for old host")?;
                let state = self.command("get_state", json!({})).await?;
                let leaf = optional_id(&state, "leafId")?;
                let messages = bounded_branch_messages(path, leaf.as_deref(), self).await?;
                // Stack newest windows first; pop preserves native oldest-first order.
                let mut end = messages.len();
                while end > 0 {
                    let mut start = end;
                    let mut bytes = 2;
                    while start > 0 && end - start < MAX_PAGE_MESSAGES {
                        let size = bounded_json(&messages[start - 1])?.len() + 1;
                        if bytes + size > MAX_PAGE_BYTES {
                            break;
                        }
                        bytes += size;
                        start -= 1;
                    }
                    if start == end {
                        return Err("history message exceeds page byte budget".into());
                    }
                    spool.push(&messages[start..end])?;
                    self.check()?;
                    end = start;
                }
                let final_state = self.command("get_state", json!({})).await?;
                if optional_id(&final_state, "leafId")? != leaf {
                    return Err("history branch changed".into());
                }
            }
            self.verify_boundary().await?;
            Ok(spool)
        })
        .await
        .map_err(|_| "history preparation timed out")?
    }
}

/// Limit-plus-one reads on the actual open file and each line, not a metadata
/// precheck followed by an unbounded helper. The existing native branch
/// validator remains authoritative for duplicate/missing entries and cycles.
async fn bounded_branch_messages(
    path: &Path,
    leaf: Option<&str>,
    read: &HistoryRead,
) -> Result<Vec<Value>, String> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file.take(MAX_FALLBACK_BYTES as u64 + 1));
    let mut total = 0;
    let mut entries = HashMap::new();
    let mut header = true;
    loop {
        read.check()?;
        let mut line = Vec::new();
        (&mut reader)
            .take(MAX_PAGE_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)
            .await
            .map_err(|e| e.to_string())?;
        if line.is_empty() {
            break;
        }
        total += line.len();
        if total > MAX_FALLBACK_BYTES || line.len() > MAX_PAGE_BYTES {
            return Err("saved history exceeds file or line budget".into());
        }
        if line.iter().all(u8::is_ascii_whitespace) && !header {
            continue;
        }
        let entry: Value =
            serde_json::from_slice(&line).map_err(|e| format!("invalid saved history: {e}"))?;
        if header {
            if entry["kind"] != "header" || entry["version"] != 4 {
                return Err("invalid saved history header".into());
            }
            header = false;
        } else if entry["kind"] == "entry" {
            let id = entry["id"]
                .as_str()
                .filter(|id| !id.is_empty() && id.len() <= 1024)
                .ok_or("history entry has no valid id")?
                .to_owned();
            if !matches!(
                entry.get("parentId"),
                Some(Value::Null) | Some(Value::String(_))
            ) {
                return Err("invalid saved history parentId".into());
            }
            if entry["type"] == "message" && !entry["message"]["role"].is_string() {
                return Err("invalid saved history message".into());
            }
            if entries.insert(id, entry).is_some() {
                return Err("duplicate history entry id".into());
            }
        }
    }
    if header {
        return Err("missing saved history header".into());
    }
    read.check()?;
    let result = super::branch_messages_from_entries(&entries, leaf)?;
    read.check()?;
    Ok(result)
}

/// Private, randomly named create_new disk stack; no offsets/transcript retained
/// in memory. The creator owns cleanup on errors, drop and task cancellation.
#[derive(Debug)]
pub struct HistorySpool {
    file: Option<std::fs::File>,
    path: PathBuf,
    end: u64,
}
impl Drop for HistorySpool {
    fn drop(&mut self) {
        self.file.take();
        let _ = std::fs::remove_file(&self.path);
    }
}
impl HistorySpool {
    pub fn create() -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!("rpi-history-{}.spool", uuid::Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(|e| e.to_string())?;
        Ok(Self {
            file: Some(file),
            path,
            end: 0,
        })
    }
    pub fn push(&mut self, messages: &[Value]) -> Result<(), String> {
        if messages.len() > MAX_PAGE_MESSAGES {
            return Err("history spool message budget exceeded".into());
        }
        let bytes = bounded_json(messages)?;
        if bytes.len() > MAX_PAGE_BYTES || self.end + bytes.len() as u64 + 8 > MAX_SPOOL_BYTES {
            return Err("history spool byte budget exceeded".into());
        }
        let file = self.file.as_mut().unwrap();
        file.seek(SeekFrom::Start(self.end))
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.write_all(&(bytes.len() as u64).to_le_bytes())
            .map_err(|e| e.to_string())?;
        self.end += bytes.len() as u64 + 8;
        Ok(())
    }
    /// Walk in replay order, retaining only one bounded page at a time. The
    /// logical cursor, file length and physical cursor survive callback errors.
    /// Adapter validation remains outside core; this is not a second transcript.
    pub fn validate(
        &mut self,
        mut check: impl FnMut(&Value) -> Result<(), String>,
    ) -> Result<(), String> {
        let position = self
            .file
            .as_mut()
            .unwrap()
            .stream_position()
            .map_err(|e| e.to_string())?;
        let deadline = std::time::Instant::now() + PREPARATION_BUDGET;
        let result = (|| {
            let mut end = self.end;
            while end != 0 {
                if std::time::Instant::now() >= deadline {
                    return Err("history validation deadline exceeded".into());
                }
                let (start, messages) = self.read_page_at(end)?;
                for message in &messages {
                    if std::time::Instant::now() >= deadline {
                        return Err("history validation deadline exceeded".into());
                    }
                    check(message)?;
                }
                end = start;
            }
            Ok(())
        })();
        let restore = self
            .file
            .as_mut()
            .unwrap()
            .seek(SeekFrom::Start(position))
            .map(|_| ())
            .map_err(|e| e.to_string());
        result.and(restore)
    }

    fn read_page_at(&mut self, end: u64) -> Result<(u64, Vec<Value>), String> {
        if !(8..=MAX_SPOOL_BYTES).contains(&end) {
            return Err("invalid history spool".into());
        }
        let file = self.file.as_mut().unwrap();
        file.seek(SeekFrom::Start(end - 8))
            .map_err(|e| e.to_string())?;
        let mut length = [0; 8];
        file.read_exact(&mut length).map_err(|e| e.to_string())?;
        let len = u64::from_le_bytes(length);
        if len > MAX_PAGE_BYTES as u64 || len + 8 > end {
            return Err("invalid history spool".into());
        }
        let start = end - len - 8;
        file.seek(SeekFrom::Start(start))
            .map_err(|e| e.to_string())?;
        let mut bytes = vec![0; len as usize];
        file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        let messages: Vec<Value> = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if messages.len() > MAX_PAGE_MESSAGES {
            return Err("invalid history spool".into());
        }
        Ok((start, messages))
    }

    pub fn pop(&mut self) -> Result<Option<Vec<Value>>, String> {
        if self.end == 0 {
            return Ok(None);
        }
        let (start, messages) = self.read_page_at(self.end)?;
        self.file
            .as_mut()
            .unwrap()
            .set_len(start)
            .map_err(|e| e.to_string())?;
        self.end = start;
        Ok(Some(messages))
    }
}

#[cfg(test)]
#[path = "page_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "spool_validation_tests.rs"]
mod validation_tests;
