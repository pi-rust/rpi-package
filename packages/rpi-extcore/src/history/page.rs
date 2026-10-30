//! Validated bounded native transcript pages. Native history remains authoritative.
use crate::{events::Subscription, host::HostSession};
use serde_json::{json, Value};
use std::{collections::HashSet, time::Duration};

pub const MAX_PAGE_MESSAGES: usize = 128;
pub const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchAnchor {
    pub leaf_id: Option<String>,
    pub total: u64,
}

#[derive(Debug)]
pub struct MessagePage {
    pub messages: Vec<Value>,
    pub anchor: BranchAnchor,
    pub first_id: Option<String>,
    pub has_more: bool,
}

/// Validate the native contract rather than silently treating malformed data as
/// an empty transcript. `beforeId` is exclusive and identifies a native entry,
/// not an ACP message ID. A branch change invalidates the entire traversal.
pub fn validate_page(
    data: Value,
    before: Option<&str>,
    limit: usize,
    expected: Option<&BranchAnchor>,
) -> Result<MessagePage, String> {
    if !(1..=MAX_PAGE_MESSAGES).contains(&limit) {
        return Err("history page limit out of range".into());
    }
    if serde_json::to_vec(&data).map_err(|e| e.to_string())?.len() > MAX_PAGE_BYTES {
        return Err("native history page exceeds byte budget".into());
    }
    let optional_id = |key: &str| -> Result<Option<String>, String> {
        match data.get(key) {
            Some(Value::Null) => Ok(None),
            Some(Value::String(id)) if !id.is_empty() && id.len() <= 1024 => Ok(Some(id.clone())),
            _ => Err(format!("invalid native history {key}")),
        }
    };
    let anchor = BranchAnchor {
        leaf_id: optional_id("leafId")?,
        total: data["total"]
            .as_u64()
            .ok_or("invalid native history total")?,
    };
    if expected.is_some_and(|expected| expected != &anchor) {
        return Err("history branch changed; restart history read".into());
    }
    if optional_id("beforeId")?.as_deref() != before {
        return Err("native history cursor mismatch".into());
    }
    let first_id = optional_id("firstId")?;
    let last_id = optional_id("lastId")?;
    let has_more = data["hasMore"]
        .as_bool()
        .ok_or("invalid native history hasMore")?;
    let messages = data["messages"]
        .as_array()
        .ok_or("invalid native history messages")?;
    if messages.len() > limit
        || messages.len() as u64 > anchor.total
        || (before.is_some() && messages.len() as u64 >= anchor.total)
        || (has_more && (messages.len() != limit || messages.len() as u64 >= anchor.total))
    {
        return Err("native history page exceeds message budget or has inconsistent pagination".into());
    }
    let mut ids = HashSet::new();
    for message in messages {
        let id = message["entryId"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 1024)
            .ok_or("native history message has no entryId")?;
        if Some(id) == before || !ids.insert(id) || !message["role"].is_string() {
            return Err("invalid or repeated native history entry".into());
        }
    }
    if first_id.as_deref() != messages.first().and_then(|m| m["entryId"].as_str())
        || last_id.as_deref() != messages.last().and_then(|m| m["entryId"].as_str())
        || (has_more && messages.is_empty())
        || (before.is_none() && !has_more && messages.len() as u64 != anchor.total)
    {
        return Err("inconsistent native history page bounds".into());
    }
    Ok(MessagePage {
        messages: messages.clone(),
        anchor,
        first_id,
        has_more,
    })
}

/// Owns the run lease for the entire read/replay, not just individual RPCs.
/// Capture the atomic event boundary after subscribing; callers must never
/// invent an independently sampled sequence number.
pub struct HistoryRead {
    host: std::sync::Arc<HostSession>,
    _lease: tokio::sync::OwnedMutexGuard<()>,
    pub subscription: Subscription,
    pub instance_id: uuid::Uuid,
}
impl HistoryRead {
    pub async fn begin(host: std::sync::Arc<HostSession>) -> Result<Self, String> {
        let lease = host.try_run_lease()?;
        let subscription = Subscription::new(&host, None).await;
        Ok(Self {
            instance_id: host.instance_id(),
            host,
            _lease: lease,
            subscription,
        })
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
            .host
            .command(
                "get_messages",
                json!({"limit":limit,"beforeId":before}),
                Duration::from_secs(15),
            )
            .await?;
        validate_page(data, before, limit, expected)
    }
    pub async fn verify(&self, anchor: &BranchAnchor) -> Result<(), String> {
        self.page(None, Some(anchor), 1).await?;
        let initial = self.subscription.replay.last().map_or(0, |event| event.seq);
        let (_, latest) = self.host.replay_boundary().await;
        if latest != initial {
            return Err("history event boundary changed; restart history read".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn data() -> Value {
        json!({"messages":[{"entryId":"b","role":"user"}],"total":2,"leafId":"b","firstId":"b","lastId":"b","beforeId":null,"hasMore":true})
    }
    #[test]
    fn validates_bounds_anchor_and_progress() {
        let page = validate_page(data(), None, 1, None).unwrap();
        assert!(page.has_more);
        let mut bad = data();
        bad["messages"] = json!([]);
        assert!(validate_page(bad, None, 1, None).is_err());
        let mut bad = data();
        bad["firstId"] = json!("wrong");
        assert!(validate_page(bad, None, 1, None).is_err());
        let mut bad = data();
        bad["leafId"] = json!("new-branch");
        assert!(validate_page(bad, None, 1, Some(&page.anchor)).is_err());
        assert!(validate_page(data(), Some("b"), 1, None).is_err());
        let mut bad = data();
        bad["hasMore"] = json!(false);
        assert!(validate_page(bad, None, 1, None).is_err());
    }
    #[test]
    fn exclusive_pages_are_full_until_the_oldest_window() {
        let mut page = data();
        page["total"] = json!(4);
        page["beforeId"] = json!("c");
        assert!(validate_page(page.clone(), Some("c"), 1, None).is_ok());
        assert!(validate_page(page.clone(), Some("c"), 2, None).is_err());
        page["messages"][0]["entryId"] = json!("c");
        page["firstId"] = json!("c");
        page["lastId"] = json!("c");
        assert!(validate_page(page, Some("c"), 1, None).is_err());
        let mut page = data();
        page["total"] = json!(1);
        assert!(validate_page(page.clone(), None, 1, None).is_err());
        page["hasMore"] = json!(false);
        page["beforeId"] = json!("c");
        assert!(validate_page(page, Some("c"), 1, None).is_err());
        let empty = json!({"messages":[],"total":0,"leafId":null,"firstId":null,"lastId":null,"beforeId":null,"hasMore":false});
        assert!(validate_page(empty, None, 1, None).is_ok());
    }

    #[test]
    fn rejects_duplicate_oversized_and_missing_fields() {
        let mut bad = data();
        bad["messages"] = json!([{"entryId":"b","role":"user"},{"entryId":"b","role":"user"}]);
        assert!(validate_page(bad, None, 2, None).is_err());
        let mut bad = data();
        bad.as_object_mut().unwrap().remove("leafId");
        assert!(validate_page(bad, None, 1, None).is_err());
        let mut bad = data();
        bad["messages"][0]["content"] = json!("x".repeat(MAX_PAGE_BYTES));
        assert!(validate_page(bad, None, 1, None).is_err());
    }
}

/// Disk-backed page stack: native pages arrive newest first; popping yields
/// oldest-first pages without retaining the transcript or a growing offset
/// index in memory. Dropping it (including task cancellation) removes the file.
pub struct HistorySpool {
    file: Option<std::fs::File>,
    path: std::path::PathBuf,
    end: u64,
}
impl Drop for HistorySpool {
    fn drop(&mut self) {
        self.file.take();
        let _ = std::fs::remove_file(&self.path);
    }
}
impl HistorySpool {
    pub async fn create() -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!("rpi-history-{}.spool", uuid::Uuid::new_v4()));
        // Open synchronously so cancellation cannot strand an asynchronously
        // created file before its owner/Drop guard has been constructed.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            file: Some(file),
            path,
            end: 0,
        })
    }
    pub async fn push(&mut self, messages: &[Value]) -> Result<(), String> {
        use std::io::{Seek, Write};
        if messages.len() > MAX_PAGE_MESSAGES {
            return Err("history spool exceeds message budget".into());
        }
        let bytes = serde_json::to_vec(messages).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_PAGE_BYTES || self.end + bytes.len() as u64 + 8 > 512 * 1024 * 1024 {
            return Err("history spool exceeds disk budget".into());
        }
        let file = self.file.as_mut().expect("owned spool");
        file.seek(std::io::SeekFrom::Start(self.end))
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.write_all(&(bytes.len() as u64).to_le_bytes())
            .map_err(|e| e.to_string())?;
        self.end += bytes.len() as u64 + 8;
        Ok(())
    }
    pub async fn pop(&mut self) -> Result<Option<Vec<Value>>, String> {
        use std::io::{Read, Seek};
        if self.end == 0 {
            return Ok(None);
        }
        let file = self.file.as_mut().expect("owned spool");
        file.seek(std::io::SeekFrom::Start(self.end - 8))
            .map_err(|e| e.to_string())?;
        let mut length = [0; 8];
        file.read_exact(&mut length).map_err(|e| e.to_string())?;
        let len = u64::from_le_bytes(length);
        if len > MAX_PAGE_BYTES as u64 || len + 8 > self.end {
            return Err("invalid history spool".into());
        }
        let start = self.end - len - 8;
        file.seek(std::io::SeekFrom::Start(start))
            .map_err(|e| e.to_string())?;
        let mut bytes = vec![0; len as usize];
        file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        let messages: Vec<Value> = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if messages.len() > MAX_PAGE_MESSAGES {
            return Err("invalid history spool message budget".into());
        }
        file.set_len(start).map_err(|e| e.to_string())?;
        self.end = start;
        Ok(Some(messages))
    }
}

/// Collect and validate a stable branch before emitting any history updates.
/// Each RPC is bounded. The owning adapter supplies the whole-operation timeout
/// and cancellation by dropping this future and its run lease.
pub async fn spool(read: &HistoryRead) -> Result<HistorySpool, String> {
    let mut spool = HistorySpool::create().await?;
    let mut before = None;
    let mut anchor = None;
    let mut count = 0_u64;
    loop {
        let page = read
            .page(before.as_deref(), anchor.as_ref(), MAX_PAGE_MESSAGES)
            .await?;
        count += page.messages.len() as u64;
        if count > page.anchor.total || (page.has_more && count >= page.anchor.total) {
            return Err("history pagination made no progress".into());
        }
        spool.push(&page.messages).await?;
        if !page.has_more {
            if count != page.anchor.total {
                return Err("incomplete native history traversal".into());
            }
            read.verify(&page.anchor).await?;
            return Ok(spool);
        }
        before = page.first_id;
        anchor = Some(page.anchor);
    }
}

#[cfg(test)]
mod traversal_tests {
    use super::*;
    #[tokio::test]
    async fn page_stack_is_oldest_first_and_drop_cleans_the_file() {
        let mut spool = HistorySpool::create().await.unwrap();
        let path = spool.path.clone();
        spool
            .push(&[json!({"entryId":"3"}), json!({"entryId":"4"})])
            .await
            .unwrap();
        spool
            .push(&[json!({"entryId":"1"}), json!({"entryId":"2"})])
            .await
            .unwrap();
        assert_eq!(
            spool.pop().await.unwrap().unwrap(),
            vec![json!({"entryId":"1"}), json!({"entryId":"2"})]
        );
        assert_eq!(
            spool.pop().await.unwrap().unwrap(),
            vec![json!({"entryId":"3"}), json!({"entryId":"4"})]
        );
        assert!(spool.pop().await.unwrap().is_none());
        drop(spool);
        assert!(!path.exists());
    }
    #[tokio::test]
    async fn spool_reuse_truncates_disk_and_enforces_budgets() {
        let mut spool = HistorySpool::create().await.unwrap();
        let a = vec![json!({"entryId":"a"})];
        spool.push(&a).await.unwrap();
        spool.push(&[json!({"entryId":"b"})]).await.unwrap();
        spool.pop().await.unwrap();
        assert_eq!(spool.file.as_ref().unwrap().metadata().unwrap().len(), spool.end);
        let c = vec![json!({"entryId":"c"})];
        spool.push(&c).await.unwrap();
        assert_eq!(spool.pop().await.unwrap().unwrap(), c);
        assert_eq!(spool.pop().await.unwrap().unwrap(), a);
        assert_eq!(spool.file.as_ref().unwrap().metadata().unwrap().len(), 0);
        assert!(spool.push(&vec![Value::Null; MAX_PAGE_MESSAGES + 1]).await.is_err());
        assert!(spool.push(&[json!("x".repeat(MAX_PAGE_BYTES))]).await.is_err());
        // Exercise the cap without writing a half-gigabyte fixture.
        spool.end = 512 * 1024 * 1024;
        assert!(spool.push(&a).await.is_err());
    }

    #[tokio::test]
    async fn lease_covers_all_pages_and_releases_on_cancel() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let (writer, peer) = tokio::io::duplex(4096);
        let (host, replies) = HostSession::test_connection(writer, 16);
        let fixture = tokio::spawn(async move {
            let mut lines = BufReader::new(peer).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let end = request["beforeId"]
                    .as_str()
                    .map(|s| s.parse::<usize>().unwrap())
                    .unwrap_or(513);
                let limit = request["limit"].as_u64().unwrap() as usize;
                assert!((1..=MAX_PAGE_MESSAGES).contains(&limit));
                let start = end.saturating_sub(limit);
                let data = json!({"messages":(start..end).map(|i| json!({"entryId":i.to_string(),"role":"user","content":i.to_string()})).collect::<Vec<_>>(),"total":513,"leafId":"512","firstId":(start<end).then(||start.to_string()),"lastId":(start<end).then(||(end-1).to_string()),"beforeId":request["beforeId"],"hasMore":start>0});
                replies
                    .send(json!({"type":"response","id":request["id"],"success":true,"data":data}))
                    .unwrap();
            }
        });
        let read = HistoryRead::begin(host.clone()).await.unwrap();
        assert!(host.try_run_lease().is_err());
        let mut spool = spool(&read).await.unwrap();
        let mut ids = Vec::new();
        while let Some(page) = spool.pop().await.unwrap() {
            ids.extend(page.into_iter().map(|message| {
                message["entryId"]
                    .as_str()
                    .unwrap()
                    .parse::<usize>()
                    .unwrap()
            }));
        }
        assert_eq!(ids, (0..513).collect::<Vec<_>>());
        assert!(host.try_run_lease().is_err());
        drop(read);
        assert!(host.try_run_lease().is_ok());
        // Dropping a read future/owner is cancellation: no lock remains held.
        let read = HistoryRead::begin(host.clone()).await.unwrap();
        drop(read);
        assert!(host.try_run_lease().is_ok());
        fixture.abort();
    }
}
