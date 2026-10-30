//! Read-only, bounded indexing of authoritative native v4 run records.
use crate::reply::{failure, ReplyKind, ServiceError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::{self, File, Metadata},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::SystemTime,
};

const BYTE_BUDGET: usize = 4 * 1024 * 1024;
const RECORD_BUDGET: usize = 4096;
const MAX_LINE: usize = 32 * 1024 * 1024;
const RETAINED_SCRATCH: usize = 64 * 1024;
const MAX_TIMINGS: usize = 20_000;
const MAX_SESSIONS: usize = 16;
const MAX_LANES: usize = 128;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunTimingsQuery {
    #[serde(rename = "entryIds", default)]
    pub entry_ids: Vec<String>,
    pub cursor: Option<String>,
}
impl RunTimingsQuery {
    pub fn validate(&self) -> Result<(), ServiceError> {
        let mut seen = HashSet::new();
        if self.entry_ids.len() > 400
            || self
                .entry_ids
                .iter()
                .any(|id| id.is_empty() || id.len() > 256 || !seen.insert(id))
            || self.cursor.as_ref().is_some_and(|c| c.len() > 128)
        {
            return Err(failure(ReplyKind::InvalidInput, "entryIds must contain at most 400 unique, nonempty IDs of at most 256 bytes; cursor at most 128 bytes"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunTiming {
    pub run_id: String,
    pub lane: String,
    pub final_entry_id: String,
    pub started_at: i64,
    pub finished_at: i64,
    pub elapsed_ms: u64,
    pub outcome: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunTimingsPage {
    pub timings: Vec<RunTiming>,
    pub complete: bool,
    pub cursor: Option<String>,
    pub scanned_bytes: u64,
    pub total_bytes: u64,
}
struct Active {
    id: String,
    start: i64,
    last: Option<String>,
}
struct Scan {
    path: PathBuf,
    generation: String,
    identity: Identity,
    modified: Option<SystemTime>,
    length: u64,
    offset: u64,
    pending: Vec<u8>,
    anchor: Vec<u8>,
    header: bool,
    active: HashMap<String, Active>,
    timings: Vec<RunTiming>,
}
#[derive(PartialEq)]
struct Identity {
    created: Option<SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}
fn identity(m: &Metadata) -> Identity {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Identity {
        created: m.created().ok(),
        #[cfg(unix)]
        dev: m.dev(),
        #[cfg(unix)]
        ino: m.ino(),
    }
}
#[derive(Default)]
pub(crate) struct RunTimingCache {
    scans: VecDeque<(String, Scan)>,
}
fn io_error(e: std::io::Error) -> ServiceError {
    failure(
        ReplyKind::Unavailable,
        &format!("run timings unavailable: {e}"),
    )
}
fn limit(message: &str) -> ServiceError {
    failure(ReplyKind::LimitExceeded, message)
}
fn conflict() -> ServiceError {
    failure(
        ReplyKind::Conflict,
        "run timing generation changed; discard timings and retry without cursor",
    )
}
fn short(v: &Value) -> Option<&str> {
    v.as_str().filter(|s| !s.is_empty() && s.len() <= 256)
}

impl RunTimingCache {
    pub(crate) fn query(
        &mut self,
        id: &str,
        root: &Path,
        query: &RunTimingsQuery,
    ) -> Result<RunTimingsPage, ServiceError> {
        query.validate()?;
        let existing = self.scans.iter().position(|(key, _)| key == id);
        let mut scan = if let Some(index) = existing {
            // A stale client must not evict or promote a healthy newer generation.
            if query
                .cursor
                .as_ref()
                .is_some_and(|c| c != &self.scans[index].1.generation)
            {
                return Err(conflict());
            }
            self.scans.remove(index).unwrap().1
        } else {
            if query.cursor.is_some() {
                return Err(conflict());
            }
            let path = discover(root, id)?;
            let m = fs::metadata(&path).map_err(io_error)?;
            Scan {
                path,
                generation: format!("{id}:{}", uuid::Uuid::new_v4()),
                identity: identity(&m),
                modified: m.modified().ok(),
                length: m.len(),
                offset: 0,
                pending: Vec::new(),
                anchor: Vec::new(),
                header: false,
                active: HashMap::new(),
                timings: Vec::new(),
            }
        };
        // Once the cursor matches, errors invalidate the affected cached scan.
        // File changes must still invalidate it, unlike a stale-client rejection.
        let canonical_root = fs::canonicalize(root).map_err(io_error)?;
        let canonical = fs::canonicalize(&scan.path).map_err(io_error)?;
        if !canonical.starts_with(&canonical_root) {
            return Err(failure(
                ReplyKind::AccessDenied,
                "transcript escapes session root",
            ));
        }
        let mut file = File::open(&canonical).map_err(io_error)?;
        let m = file.metadata().map_err(io_error)?;
        if !m.is_file()
            || identity(&m) != scan.identity
            || m.len() < scan.length
            || (m.len() == scan.length && m.modified().ok() != scan.modified)
        {
            return Err(conflict());
        }
        if !scan.anchor.is_empty() {
            file.seek(SeekFrom::Start(scan.offset - scan.anchor.len() as u64))
                .map_err(io_error)?;
            let mut anchor = vec![0; scan.anchor.len()];
            file.read_exact(&mut anchor).map_err(io_error)?;
            if anchor != scan.anchor {
                return Err(conflict());
            }
        }
        // Revalidate the small header on each continuation, without an unbounded read_line.
        if scan.header {
            validate_header(&mut file, id)?;
        }
        file.seek(SeekFrom::Start(scan.offset)).map_err(io_error)?;
        let mut bytes = 0;
        let mut records = 0;
        // Read no farther than the metadata snapshot. A concurrent append is handled next call.
        let total = m.len();
        let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
        use std::io::BufRead;
        while scan.offset < total && bytes < BYTE_BUDGET && records < RECORD_BUDGET {
            let available = reader.fill_buf().map_err(io_error)?;
            if available.is_empty() {
                return Err(conflict());
            }
            let n = available
                .len()
                .min(BYTE_BUDGET - bytes)
                .min((total - scan.offset) as usize);
            let chunk = &available[..n];
            let take = chunk.iter().position(|b| *b == b'\n').map_or(n, |p| p + 1);
            if scan.pending.len() + take > if scan.header { MAX_LINE } else { 64 * 1024 } {
                return Err(limit("native JSONL line exceeds supported bound"));
            }
            scan.pending.extend_from_slice(&chunk[..take]);
            reader.consume(take);
            bytes += take;
            scan.offset += take as u64;
            if scan.pending.last() == Some(&b'\n') {
                let value: Value = serde_json::from_slice(&scan.pending)
                    .map_err(|_| failure(ReplyKind::Unavailable, "invalid native JSONL record"))?;
                if !scan.header {
                    check_header(&value, id)?;
                    scan.header = true;
                } else {
                    scan.record(&value)?;
                }
                if scan.pending.capacity() > RETAINED_SCRATCH {
                    scan.pending = Vec::new();
                } else {
                    scan.pending.clear();
                }
                records += 1;
            }
        }
        let mut file = reader.into_inner();
        let size = scan.offset.min(4096) as usize;
        file.seek(SeekFrom::Start(scan.offset - size as u64))
            .map_err(io_error)?;
        scan.anchor.resize(size, 0);
        file.read_exact(&mut scan.anchor).map_err(io_error)?;
        let after = file.metadata().map_err(io_error)?;
        if identity(&after) != scan.identity || after.len() < total {
            return Err(conflict());
        }
        scan.modified = m.modified().ok();
        scan.length = total;
        let selected: HashSet<_> = query.entry_ids.iter().map(String::as_str).collect();
        let page = RunTimingsPage {
            timings: scan
                .timings
                .iter()
                .filter(|t| selected.contains(t.final_entry_id.as_str()))
                .cloned()
                .collect(),
            complete: scan.header && scan.offset == total && scan.pending.is_empty(),
            cursor: Some(scan.generation.clone()),
            scanned_bytes: scan.offset,
            total_bytes: total,
        };
        self.scans.push_back((id.to_owned(), scan));
        while self.scans.len() > MAX_SESSIONS {
            self.scans.pop_front();
        }
        Ok(page)
    }
}
impl Scan {
    fn record(&mut self, v: &Value) -> Result<(), ServiceError> {
        let lane = match v.get("lane") {
            Some(Value::String(s)) if s.len() <= 256 => s.as_str(),
            // Compatibility for older fixtures without a lane. Native fork
            // imports explicitly encode lane:null and must not join an active run.
            None => "main",
            _ => return Ok(()),
        };
        match (v["kind"].as_str(), v["type"].as_str()) {
            (Some("record"), Some("operation_started")) => {
                // Any intervening operation invalidates an earlier active run on this lane.
                self.active.remove(lane);
                if v["intent"]["kind"] == "run" {
                    if let (Some(id), Some(start)) = (short(&v["id"]), v["timestamp"].as_i64()) {
                        if self.active.len() >= MAX_LANES {
                            return Err(limit("too many active run lanes"));
                        }
                        self.active.insert(
                            lane.to_owned(),
                            Active {
                                id: id.to_owned(),
                                start,
                                last: None,
                            },
                        );
                    }
                }
            }
            (Some("entry"), Some("message")) => {
                if let Some(active) = self.active.get_mut(lane) {
                    if visible(&v["message"]) {
                        if let Some(id) = short(&v["id"]) {
                            active.last = Some(id.to_owned());
                        }
                    }
                }
            }
            (Some("record"), Some("operation_finished"))
                if self
                    .active
                    .get(lane)
                    .is_some_and(|a| v["runId"].as_str() == Some(&a.id)) =>
            {
                let active = self.active.remove(lane).unwrap();
                if let (Some(last), Some(end), Some(outcome)) =
                    (active.last, v["timestamp"].as_i64(), short(&v["outcome"]))
                {
                    if let Some(elapsed) = end
                        .checked_sub(active.start)
                        .and_then(|d| u64::try_from(d).ok())
                    {
                        if self.timings.len() >= MAX_TIMINGS {
                            return Err(limit("run timing metadata capacity exceeded"));
                        }
                        self.timings.push(RunTiming {
                            run_id: active.id,
                            lane: lane.to_owned(),
                            final_entry_id: last,
                            started_at: active.start,
                            finished_at: end,
                            elapsed_ms: elapsed,
                            outcome: outcome.to_owned(),
                        });
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
fn visible(m: &Value) -> bool {
    if !matches!(
        m["role"].as_str(),
        Some("assistant" | "user" | "toolResult")
    ) {
        return false;
    }
    match &m["content"] {
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(blocks) => blocks.iter().any(|b| match b["type"].as_str() {
            Some("text") => b["text"].as_str().is_some_and(|s| !s.trim().is_empty()),
            Some("thinking") => b["thinking"]
                .as_str()
                .or_else(|| b["text"].as_str())
                .is_some_and(|s| !s.trim().is_empty()),
            Some("image_url") => b["image_url"]["url"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty()),
            Some("image" | "toolCall" | "tool_call") => true,
            _ => false,
        }),
        _ => false,
    }
}
fn check_header(v: &Value, id: &str) -> Result<(), ServiceError> {
    if v["kind"] != "header" || v["version"] != 4 || v["id"] != id {
        return Err(failure(
            ReplyKind::Unavailable,
            "expected exact native v4 session header",
        ));
    }
    Ok(())
}
fn validate_header(file: &mut File, id: &str) -> Result<(), ServiceError> {
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let mut line = Vec::new();
    use std::io::BufRead;
    std::io::BufReader::new(file.take(64 * 1024 + 1))
        .read_until(b'\n', &mut line)
        .map_err(io_error)?;
    if line.len() > 64 * 1024 || line.last() != Some(&b'\n') {
        return Err(failure(
            ReplyKind::Unavailable,
            "invalid or oversized native header",
        ));
    }
    let v = serde_json::from_slice(&line)
        .map_err(|_| failure(ReplyKind::Unavailable, "invalid native header"))?;
    check_header(&v, id)
}
fn discover(root: &Path, id: &str) -> Result<PathBuf, ServiceError> {
    let root = fs::canonicalize(root).map_err(io_error)?;
    let mut dirs = VecDeque::from([(root.clone(), 0)]);
    let mut count = 0;
    let mut found = None;
    while let Some((dir, depth)) = dirs.pop_front() {
        for entry in fs::read_dir(dir).map_err(io_error)? {
            count += 1;
            if count > 10_000 {
                return Err(limit("session file discovery exceeds 10000 entries"));
            }
            let entry = entry.map_err(io_error)?;
            let kind = entry.file_type().map_err(io_error)?;
            if kind.is_dir() && depth == 0 {
                dirs.push_back((entry.path(), 1));
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !(name == format!("{id}.jsonl") || name.ends_with(&format!("_{id}.jsonl"))) {
                continue;
            }
            let path = fs::canonicalize(entry.path()).map_err(io_error)?;
            if !path.starts_with(&root) {
                return Err(failure(
                    ReplyKind::AccessDenied,
                    "transcript escapes session root",
                ));
            }
            if !path.is_file() {
                continue;
            }
            validate_header(&mut File::open(&path).map_err(io_error)?, id)?;
            if found.replace(path).is_some() {
                return Err(failure(
                    ReplyKind::Conflict,
                    "multiple transcripts for session",
                ));
            }
        }
    }
    found.ok_or_else(|| {
        failure(
            ReplyKind::Unavailable,
            "native transcript unavailable in configured session root",
        )
    })
}

/// Mirror native precedence; do not silently fall back when user settings may
/// redirect storage. JSON-with-comments settings are explicitly unsupported.
pub(crate) fn session_root(
    project: &Path,
    configured: Option<PathBuf>,
) -> Result<PathBuf, ServiceError> {
    let selected = if let Some(path) = configured {
        Some(path)
    } else {
        let agent = std::env::var_os("RPI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .or_else(|| std::env::var_os("HOME"))
                    .map(|h| PathBuf::from(h).join(".rpi/agent"))
            });
        let mut selected = None;
        if let Some(agent) = agent {
            match File::open(agent.join("settings.json")) {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    file.take(1024 * 1024 + 1)
                        .read_to_end(&mut bytes)
                        .map_err(io_error)?;
                    if bytes.len() > 1024 * 1024 {
                        return Err(limit("native settings too large"));
                    }
                    let settings: Value = serde_json::from_slice(&bytes).map_err(|_| failure(ReplyKind::Unavailable, "cannot safely resolve native settings; configure session_dir explicitly"))?;
                    if !settings["sessionDir"].is_null() {
                        selected = Some(PathBuf::from(
                            settings["sessionDir"].as_str().ok_or_else(|| {
                                failure(
                                    ReplyKind::Unavailable,
                                    "unsupported native sessionDir setting",
                                )
                            })?,
                        ));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_error(e)),
            }
        }
        selected.or_else(|| std::env::var_os("RPI_CODING_AGENT_SESSION_DIR").map(PathBuf::from))
    };
    let path = selected.unwrap_or_else(|| project.join(".rpi/sessions"));
    Ok(if path.is_relative() {
        project.join(path)
    } else {
        path
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    struct Fixture {
        root: PathBuf,
        path: PathBuf,
        id: String,
    }
    impl Fixture {
        fn new(nested: bool) -> Self {
            let root =
                std::env::temp_dir().join(format!("rpi-run-timing-{}", uuid::Uuid::new_v4()));
            let dir = if nested {
                root.join("project")
            } else {
                root.clone()
            };
            fs::create_dir_all(&dir).unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            let path = dir.join(format!("2026_{id}.jsonl"));
            fs::write(
                &path,
                format!("{}\n", json!({"kind":"header","version":4,"id":id})),
            )
            .unwrap();
            Self { root, path, id }
        }
        fn append(&self, value: Value) {
            writeln!(
                fs::OpenOptions::new()
                    .append(true)
                    .open(&self.path)
                    .unwrap(),
                "{value}"
            )
            .unwrap();
        }
        fn start(&self, id: &str, lane: &str, time: i64, kind: &str) {
            self.append(json!({"kind":"record","type":"operation_started","id":id,"lane":lane,"timestamp":time,"sourceLeafId":"old","intent":{"kind":kind,"initialMessages":[{"id":"reserved"}]}}));
        }
        fn message(&self, id: &str, lane: &str) {
            self.append(json!({"kind":"entry","type":"message","id":id,"lane":lane,"timestamp":1,"seq":42,"parentId":"old","message":{"role":"assistant","content":[{"type":"text","text":"answer"}]}}));
        }
        fn finish(&self, id: &str, lane: &str, time: i64) {
            self.append(json!({"kind":"record","type":"operation_finished","id":"finish","runId":id,"lane":lane,"timestamp":time,"outcome":"completed"}));
        }
        fn query(
            &self,
            cache: &mut RunTimingCache,
            cursor: Option<String>,
            ids: &[&str],
        ) -> Result<RunTimingsPage, ServiceError> {
            cache.query(
                &self.id,
                &self.root,
                &RunTimingsQuery {
                    entry_ids: ids.iter().map(|s| s.to_string()).collect(),
                    cursor,
                },
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn native_multirun_multilane_and_exact_timestamps() {
        for nested in [false, true] {
            let f = Fixture::new(nested);
            f.start("r1", "main", 1001, "run");
            f.start("r2", "side", 2000, "run");
            for n in 0..600 {
                f.message(&format!("m{n}"), "main");
            }
            f.message("side-answer", "side");
            f.finish("r2", "side", 4507);
            f.finish("r1", "main", 3509);
            f.start("r3", "main", 9000, "run");
            f.message("later", "main");
            f.finish("r3", "main", 9100);
            let page = f
                .query(
                    &mut RunTimingCache::default(),
                    None,
                    &["m599", "side-answer", "later", "reserved"],
                )
                .unwrap();
            assert!(page.complete);
            assert_eq!(page.timings.len(), 3);
            let main = page.timings.iter().find(|t| t.run_id == "r1").unwrap();
            assert_eq!(
                (main.started_at, main.finished_at, main.elapsed_ms),
                (1001, 3509, 2508)
            );
            assert_eq!(main.final_entry_id, "m599");
            assert_eq!(page.timings[0].lane, "side");
        }
    }
    #[test]
    fn no_fabrication_for_missing_reversed_noop_compaction_or_suspended() {
        let f = Fixture::new(false);
        f.message("old", "main");
        f.finish("missing", "main", 9);
        f.start("noop", "main", 1, "run");
        f.finish("noop", "main", 10);
        f.start("compact", "main", 1, "compaction");
        f.message("compact", "main");
        f.finish("compact", "main", 10);
        f.start("nav", "main", 1, "navigation");
        f.message("nav", "main");
        f.finish("nav", "main", 10);
        f.start("reverse", "main", 20, "run");
        f.message("reverse", "main");
        f.finish("reverse", "main", 10);
        f.start("suspend", "main", 1, "run");
        f.message("suspend", "main");
        let page = f
            .query(
                &mut RunTimingCache::default(),
                None,
                &["old", "compact", "nav", "reverse", "suspend"],
            )
            .unwrap();
        assert!(page.complete);
        assert!(page.timings.is_empty());
    }
    #[test]
    fn bounded_continuation_changes_selection_and_partial_tail_append() {
        let f = Fixture::new(false);
        f.start("r", "main", 100, "run");
        for _ in 0..5000 {
            f.append(json!({"kind":"record","type":"step_attempt"}));
        }
        f.message("answer", "main");
        let mut file = fs::OpenOptions::new().append(true).open(&f.path).unwrap();
        write!(
            file,
            "{{\"kind\":\"record\",\"type\":\"operation_finished\",\"runId\":\"r\","
        )
        .unwrap();
        let mut cache = RunTimingCache::default();
        let first = f.query(&mut cache, None, &["different"]).unwrap();
        assert!(!first.complete);
        assert!(first.scanned_bytes < first.total_bytes);
        let partial = f
            .query(&mut cache, first.cursor.clone(), &["answer"])
            .unwrap();
        assert!(!partial.complete);
        assert_eq!(partial.scanned_bytes, partial.total_bytes);
        assert!(partial.timings.is_empty());
        writeln!(
            file,
            "\"timestamp\":450,\"lane\":\"main\",\"outcome\":\"completed\"}}"
        )
        .unwrap();
        let final_page = f.query(&mut cache, partial.cursor, &["answer"]).unwrap();
        assert!(final_page.complete);
        assert_eq!(final_page.timings[0].elapsed_ms, 350);
        assert_eq!(final_page.cursor, first.cursor);
    }
    #[test]
    fn large_image_line_is_incremental_and_visible() {
        let f = Fixture::new(false);
        f.start("r", "main", 1, "run");
        f.append(json!({"kind":"entry","type":"message","id":"image","lane":"main","message":{"role":"user","content":[{"type":"image","data":"x".repeat(5 * 1024 * 1024)}]}}));
        f.finish("r", "main", 5);
        let mut cache = RunTimingCache::default();
        let page = f.query(&mut cache, None, &["image"]).unwrap();
        assert!(!page.complete);
        assert!(page.scanned_bytes <= BYTE_BUDGET as u64);
        let page = f.query(&mut cache, page.cursor, &["image"]).unwrap();
        assert!(page.complete);
        assert_eq!(page.timings[0].final_entry_id, "image");
    }
    #[test]
    fn truncate_replacement_and_invalid_cursor_invalidate() {
        for replace in [false, true] {
            let f = Fixture::new(false);
            f.start("r", "main", 1, "run");
            f.message("a", "main");
            f.finish("r", "main", 9);
            let mut cache = RunTimingCache::default();
            let page = f.query(&mut cache, None, &["a"]).unwrap();
            assert_eq!(page.timings.len(), 1);
            if replace {
                fs::rename(&f.path, f.path.with_extension("old")).unwrap();
            }
            fs::write(
                &f.path,
                format!("{}\n", json!({"kind":"header","version":4,"id":f.id})),
            )
            .unwrap();
            assert_eq!(
                f.query(&mut cache, page.cursor.clone(), &["a"])
                    .unwrap_err()
                    .0,
                ReplyKind::Conflict
            );
            assert_eq!(
                f.query(&mut cache, page.cursor, &["a"]).unwrap_err().0,
                ReplyKind::Conflict
            );
            assert!(f
                .query(&mut cache, None, &["a"])
                .unwrap()
                .timings
                .is_empty());
            assert_eq!(
                f.query(&mut cache, Some("bogus".into()), &["a"])
                    .unwrap_err()
                    .0,
                ReplyKind::Conflict
            );
        }
    }
    #[test]
    fn stale_generation_preserves_healthy_scan_and_lru() {
        let f = Fixture::new(false);
        f.start("r1", "main", 1, "run");
        f.message("a", "main");
        f.finish("r1", "main", 2);
        let mut cache = RunTimingCache::default();
        let g1 = f.query(&mut cache, None, &["a"]).unwrap().cursor;
        fs::write(
            &f.path,
            format!("{}\n", json!({"kind":"header","version":4,"id":f.id})),
        )
        .unwrap();
        assert_eq!(
            f.query(&mut cache, g1.clone(), &[]).unwrap_err().0,
            ReplyKind::Conflict
        );
        assert!(cache.scans.is_empty());
        let g2 = f.query(&mut cache, None, &[]).unwrap().cursor;
        assert_ne!(g1, g2);
        let other = Fixture::new(false);
        other.query(&mut cache, None, &[]).unwrap();
        let order: Vec<_> = cache.scans.iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(
            f.query(&mut cache, g1, &[]).unwrap_err().0,
            ReplyKind::Conflict
        );
        assert_eq!(
            cache
                .scans
                .iter()
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>(),
            order
        );
        f.start("r2", "main", 10, "run");
        f.message("b", "main");
        f.finish("r2", "main", 30);
        let page = f.query(&mut cache, g2.clone(), &["a", "b"]).unwrap();
        assert!(page.complete);
        assert_eq!(page.cursor, g2);
        assert_eq!(page.timings.len(), 1);
        assert_eq!(page.timings[0].final_entry_id, "b");
        assert_eq!(page.timings[0].elapsed_ms, 20);
    }

    #[test]
    fn thinking_and_valid_image_url_bind_the_last_visible_entry() {
        for (block, expected) in [
            (json!({"type":"thinking","thinking":"reasoning"}), "b"),
            (json!({"type":"thinking","text":"fallback reasoning"}), "b"),
            (
                json!({"type":"thinking","thinking":null,"text":"fallback"}),
                "b",
            ),
            (
                json!({"type":"thinking","thinking":" ","text":"not used"}),
                "a",
            ),
            (json!({"type":"thinking","text":" \n"}), "a"),
            (
                json!({"type":"image_url","image_url":{"url":"https://example.com/image.png"}}),
                "b",
            ),
            (
                json!({"type":"image_url","image_url":{"url":"data:image/png;base64,AA=="}}),
                "b",
            ),
            (json!({"type":"image_url","image_url":{"url":" "}}), "a"),
            (json!({"type":"image_url","image_url":{"url":7}}), "a"),
            (
                json!({"type":"image_url","image_url":"https://example.com/image.png"}),
                "a",
            ),
            (json!({"type":"image_url"}), "a"),
        ] {
            let f = Fixture::new(false);
            f.start("r", "main", 1, "run");
            f.message("a", "main");
            f.append(json!({"kind":"entry","type":"message","id":"b","lane":"main","message":{"role":"assistant","content":[block]}}));
            f.finish("r", "main", 5);
            let page = f
                .query(&mut RunTimingCache::default(), None, &["a", "b"])
                .unwrap();
            assert!(page.complete);
            assert_eq!(page.timings.len(), 1);
            assert_eq!(page.timings[0].final_entry_id, expected);
        }
    }

    #[test]
    fn completed_large_lines_release_scratch_but_partial_lines_survive() {
        let f = Fixture::new(false);
        f.start("r", "main", 1, "run");
        f.append(json!({"kind":"entry","type":"message","id":"large","lane":"main","message":{"role":"assistant","content":"x".repeat(96 * 1024)}}));
        let mut cache = RunTimingCache::default();
        let page = f.query(&mut cache, None, &[]).unwrap();
        assert!(page.complete);
        let scan = &cache.scans[0].1;
        assert!(scan.pending.is_empty());
        assert!(scan.pending.capacity() <= RETAINED_SCRATCH);

        let partial = format!(
            "{{\"kind\":\"entry\",\"type\":\"message\",\"id\":\"partial\",\"lane\":\"main\",\"message\":{{\"role\":\"assistant\",\"content\":\"{}",
            "y".repeat(96 * 1024)
        );
        let mut file = fs::OpenOptions::new().append(true).open(&f.path).unwrap();
        file.write_all(partial.as_bytes()).unwrap();
        let page = f.query(&mut cache, page.cursor, &[]).unwrap();
        assert!(!page.complete);
        assert_eq!(page.scanned_bytes, page.total_bytes);
        assert_eq!(cache.scans[0].1.pending, partial.as_bytes());
        assert!(cache.scans[0].1.pending.capacity() > RETAINED_SCRATCH);
        writeln!(file, "\"}}}}").unwrap();
        f.finish("r", "main", 5);
        let page = f
            .query(&mut cache, page.cursor, &["large", "partial"])
            .unwrap();
        assert!(page.complete);
        assert_eq!(page.timings.len(), 1);
        assert_eq!(page.timings[0].final_entry_id, "partial");
        assert!(cache.scans[0].1.pending.is_empty());
        assert!(cache.scans[0].1.pending.capacity() <= RETAINED_SCRATCH);
    }

    #[test]
    fn null_lane_imports_do_not_bind_or_replace_active_run_entries() {
        for with_output in [false, true] {
            let f = Fixture::new(false);
            f.start("r", "main", 1, "run");
            if with_output {
                f.message("a", "main");
            }
            f.append(json!({"kind":"entry","type":"message","id":"imported","lane":null,"message":{"role":"assistant","content":"imported answer"}}));
            // Records require string lanes in the native codec. A null-lane
            // record must neither invent a run nor disrupt the real main run.
            f.append(json!({"kind":"record","type":"operation_started","id":"fake","lane":null,"timestamp":2,"intent":{"kind":"run"}}));
            f.append(json!({"kind":"record","type":"operation_finished","runId":"fake","lane":null,"timestamp":3,"outcome":"completed"}));
            f.finish("r", "main", 5);
            let page = f
                .query(&mut RunTimingCache::default(), None, &["a", "imported"])
                .unwrap();
            assert!(page.complete);
            assert_eq!(page.timings.len(), usize::from(with_output));
            if with_output {
                assert_eq!(page.timings[0].run_id, "r");
                assert_eq!(page.timings[0].final_entry_id, "a");
            }
        }
    }

    #[test]
    fn missing_lane_remains_main_for_backward_fixtures() {
        let f = Fixture::new(false);
        f.append(json!({"kind":"record","type":"operation_started","id":"r","timestamp":1,"intent":{"kind":"run"}}));
        f.append(json!({"kind":"entry","type":"message","id":"a","message":{"role":"assistant","content":"answer"}}));
        f.append(json!({"kind":"record","type":"operation_finished","runId":"r","timestamp":5,"outcome":"completed"}));
        let page = f
            .query(&mut RunTimingCache::default(), None, &["a"])
            .unwrap();
        assert!(page.complete);
        assert_eq!(page.timings.len(), 1);
        assert_eq!(page.timings[0].lane, "main");
        assert_eq!(page.timings[0].final_entry_id, "a");
    }

    #[test]
    fn capped_input_and_header() {
        assert!(serde_json::from_value::<RunTimingsQuery>(json!({"other":[]})).is_err());
        for ids in [
            vec!["".into()],
            vec!["a".into(), "a".into()],
            vec!["x".repeat(257)],
            (0..401).map(|n| n.to_string()).collect(),
        ] {
            assert_eq!(
                RunTimingsQuery {
                    entry_ids: ids,
                    cursor: None
                }
                .validate()
                .unwrap_err()
                .0,
                ReplyKind::InvalidInput
            );
        }
        let f = Fixture::new(false);
        fs::write(
            &f.path,
            format!("{}\n", json!({"kind":"header","version":3,"id":f.id})),
        )
        .unwrap();
        assert_eq!(
            f.query(&mut RunTimingCache::default(), None, &[])
                .unwrap_err()
                .0,
            ReplyKind::Unavailable
        );
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn metadata_lane_lru_and_line_caps_are_explicit() {
        let root = std::env::temp_dir().join(format!("rpi-timing-cap-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let mut cache = RunTimingCache::default();
        let mut first = None;
        for _ in 0..=MAX_SESSIONS {
            let id = uuid::Uuid::new_v4().to_string();
            fs::write(
                root.join(format!("{id}.jsonl")),
                format!("{}\n", json!({"kind":"header","version":4,"id":id})),
            )
            .unwrap();
            let page = cache
                .query(&id, &root, &RunTimingsQuery::default())
                .unwrap();
            first.get_or_insert((id, page.cursor));
        }
        assert_eq!(cache.scans.len(), MAX_SESSIONS);
        let (id, cursor) = first.unwrap();
        assert_eq!(
            cache
                .query(
                    &id,
                    &root,
                    &RunTimingsQuery {
                        cursor,
                        ..Default::default()
                    }
                )
                .unwrap_err()
                .0,
            ReplyKind::Conflict
        );
        let scan = &mut cache.scans.back_mut().unwrap().1;
        let timing = RunTiming {
            run_id: "r".into(),
            lane: "main".into(),
            final_entry_id: "m".into(),
            started_at: 0,
            finished_at: 1,
            elapsed_ms: 1,
            outcome: "completed".into(),
        };
        scan.timings = vec![timing; MAX_TIMINGS];
        scan.active.insert(
            "main".into(),
            Active {
                id: "r".into(),
                start: 0,
                last: Some("m".into()),
            },
        );
        assert_eq!(scan.record(&json!({"kind":"record","type":"operation_finished","runId":"r","lane":"main","timestamp":1,"outcome":"completed"})).unwrap_err().0, ReplyKind::LimitExceeded);
        scan.active.clear();
        for n in 0..=MAX_LANES {
            let result = scan.record(&json!({"kind":"record","type":"operation_started","intent":{"kind":"run"},"id":"r","lane":n.to_string(),"timestamp":0}));
            if n == MAX_LANES {
                assert_eq!(result.unwrap_err().0, ReplyKind::LimitExceeded);
            } else {
                result.unwrap();
            }
        }
        // Exercise the actual line limit without retaining more than one long
        // scratch line; no incomplete scan may claim completion.
        let id = uuid::Uuid::new_v4().to_string();
        let path = root.join(format!("{id}.jsonl"));
        let mut file = File::create(&path).unwrap();
        use std::io::Write;
        writeln!(file, "{}", json!({"kind":"header","version":4,"id":id})).unwrap();
        let block = vec![b' '; 1024 * 1024];
        for _ in 0..33 {
            file.write_all(&block).unwrap();
        }
        drop(file);
        let mut cursor = None;
        loop {
            match cache.query(
                &id,
                &root,
                &RunTimingsQuery {
                    cursor: cursor.clone(),
                    ..Default::default()
                },
            ) {
                Ok(page) => {
                    assert!(!page.complete);
                    cursor = page.cursor;
                }
                Err(error) => {
                    assert_eq!(error.0, ReplyKind::LimitExceeded);
                    break;
                }
            }
        }
        fs::remove_dir_all(root).unwrap();
    }
}
