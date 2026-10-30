//! Read native transcript branches without projecting a client protocol.
pub mod page;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fmt,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::UNIX_EPOCH,
};

pub fn find_saved_session(directory: &Path, id: &str) -> Result<PathBuf, String> {
    let directories = std::fs::read_dir(directory)
        .map_err(|_| "unknown sessionId: no saved native session".to_string())?;
    let mut found = None;
    for entry in directories {
        let entry = entry.map_err(|e| e.to_string())?;
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if !file_type.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(entry.path()).map_err(|e| e.to_string())? {
            let file = file.map_err(|e| e.to_string())?;
            let file_type = file.file_type().map_err(|e| e.to_string())?;
            if !file_type.is_file() || filename_session_id(&file.path()) != Some(id) {
                continue;
            }
            let path = file.path();
            let mut reader = BufReader::new(std::fs::File::open(&path).map_err(|e| e.to_string())?);
            let mut line = String::new();
            reader.read_line(&mut line).map_err(|e| e.to_string())?;
            let header: Value =
                serde_json::from_str(&line).map_err(|e| format!("invalid session header: {e}"))?;
            if header["kind"] != "header" || header["version"] != 4 || header["id"] != id {
                return Err("invalid saved session header".into());
            }
            if found.replace(path).is_some() {
                return Err("multiple saved files for sessionId".into());
            }
        }
    }
    found.ok_or_else(|| "unknown sessionId: no saved native session".into())
}

pub fn branch_messages(path: &Path, leaf: Option<&str>) -> Result<Vec<Value>, String> {
    let reader = BufReader::new(std::fs::File::open(path).map_err(|e| e.to_string())?);
    let mut entries = HashMap::new();
    for line in reader.lines().skip(1) {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: Value =
            serde_json::from_str(&line).map_err(|e| format!("invalid session history: {e}"))?;
        if entry["kind"] == "entry" {
            let id = entry["id"]
                .as_str()
                .ok_or("history entry has no id")?
                .to_owned();
            if entries.insert(id, entry).is_some() {
                return Err("duplicate history entry id".into());
            }
        }
    }
    branch_messages_from_entries(&entries, leaf)
}

pub fn branch_messages_from_entries<'a>(
    entries: &'a HashMap<String, Value>,
    mut leaf: Option<&'a str>,
) -> Result<Vec<Value>, String> {
    let mut path = Vec::new();
    let mut visited = HashSet::new();
    while let Some(id) = leaf {
        if !visited.insert(id) {
            return Err("cycle in saved session history".into());
        }
        let entry = entries
            .get(id)
            .ok_or("saved session history is missing a branch entry")?;
        path.push(entry);
        leaf = entry["parentId"].as_str();
    }
    let mut updates = Vec::new();
    for entry in path.into_iter().rev() {
        if entry["type"] == "message" {
            updates.push(entry["message"].clone());
        }
    }
    Ok(updates)
}

/// Header-only metadata for one persisted native session. The transcript file
/// remains the source of truth; this value is only a bounded discovery result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSessionMetadata {
    pub id: String,
    pub cwd: String,
    pub title: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSessionPage {
    pub sessions: Vec<SavedSessionMetadata>,
    pub next_cursor: Option<String>,
}

#[derive(Debug)]
pub enum SavedSessionListError {
    InvalidCursor,
    Io(String),
    InvalidSession(String),
}

impl fmt::Display for SavedSessionListError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCursor => formatter.write_str("invalid session list cursor"),
            Self::Io(error) | Self::InvalidSession(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for SavedSessionListError {}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionCursor {
    version: u8,
    cwd: Option<String>,
    created_at: i64,
    id: String,
}

const DISCOVERY_HEAD_BYTES: usize = 64 * 1024;
const DISCOVERY_TAIL_BYTES: usize = 8 * 1024;

/// Discover persisted sessions without loading their transcripts. The native
/// layout is `root/session-id/cwd-directory/timestamp_session-id.jsonl`.
/// Only the best `limit + 1` filtered records are retained while scanning.
/// The cursor uses the immutable `(createdAt desc, id asc)` ordering key.
pub fn list_saved_sessions(
    root: &Path,
    cwd: Option<&Path>,
    cursor: Option<&str>,
    limit: usize,
) -> Result<SavedSessionPage, SavedSessionListError> {
    list_saved_sessions_with_cancel(root, cwd, cursor, limit, None)
}

/// Cancellable form used by adapters that run discovery off the async runtime.
/// The scanner checks the flag between directories and files, so cancellation
/// does not leave a detached blocking worker walking a large store.
pub fn list_saved_sessions_cancelable(
    root: &Path,
    cwd: Option<&Path>,
    cursor: Option<&str>,
    limit: usize,
    cancelled: &AtomicBool,
) -> Result<SavedSessionPage, SavedSessionListError> {
    list_saved_sessions_with_cancel(root, cwd, cursor, limit, Some(cancelled))
}

fn list_saved_sessions_with_cancel(
    root: &Path,
    cwd: Option<&Path>,
    cursor: Option<&str>,
    limit: usize,
    cancelled: Option<&AtomicBool>,
) -> Result<SavedSessionPage, SavedSessionListError> {
    if !root.is_absolute() {
        return Err(SavedSessionListError::Io(
            "session storage root must be absolute".into(),
        ));
    }
    let filter = cwd
        .map(normalize_cwd)
        .transpose()
        .map_err(SavedSessionListError::Io)?;
    let after = cursor
        .map(|value| decode_cursor(value, filter.as_deref()))
        .transpose()?;
    let page_size = limit.clamp(1, 100);
    let mut sessions = Vec::with_capacity(page_size + 1);

    match std::fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => {
            scan_saved_sessions(
                root,
                page_size + 1,
                filter.as_deref(),
                after.as_ref(),
                &mut sessions,
                cancelled,
            )?;
        }
        Ok(_) => {
            return Err(SavedSessionListError::Io(
                "session storage root is not a directory".into(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SavedSessionPage {
                sessions: Vec::new(),
                next_cursor: None,
            });
        }
        Err(error) => {
            return Err(SavedSessionListError::Io(format!(
                "list saved sessions: {error}"
            )))
        }
    }

    sort_sessions(&mut sessions);
    let has_more = sessions.len() > page_size;
    sessions.truncate(page_size);
    let next_cursor = if has_more {
        let last = sessions.last().expect("non-empty paginated session list");
        Some(encode_cursor(&SessionCursor {
            version: 1,
            cwd: filter.clone(),
            created_at: last.created_at,
            id: last.id.clone(),
        }))
    } else {
        None
    };
    Ok(SavedSessionPage {
        sessions,
        next_cursor,
    })
}

fn scan_saved_sessions(
    root: &Path,
    capacity: usize,
    filter: Option<&str>,
    after: Option<&SessionCursor>,
    sessions: &mut Vec<SavedSessionMetadata>,
    cancelled: Option<&AtomicBool>,
) -> Result<(), SavedSessionListError> {
    if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Ok(());
    }
    let entries = std::fs::read_dir(root)
        .map_err(|error| SavedSessionListError::Io(format!("list saved sessions: {error}")))?;
    for entry in entries {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Ok(());
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => return Err(SavedSessionListError::Io(error.to_string())),
        };
        let session_directory = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(SavedSessionListError::Io(error.to_string())),
        };
        // Do not follow symlinked directories outside the configured store.
        if !file_type.is_dir() {
            continue;
        }
        let Some(session_id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if uuid::Uuid::parse_str(&session_id).map(|uuid| uuid.to_string() == session_id) != Ok(true)
        {
            continue;
        }
        let mut matching_files = 0_u8;
        let cwd_entries = match std::fs::read_dir(&session_directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(SavedSessionListError::Io(error.to_string())),
        };
        for cwd_entry in cwd_entries {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                return Ok(());
            }
            let cwd_entry = match cwd_entry {
                Ok(entry) => entry,
                Err(error) => return Err(SavedSessionListError::Io(error.to_string())),
            };
            let cwd_type = match cwd_entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(SavedSessionListError::Io(error.to_string())),
            };
            if !cwd_type.is_dir() {
                continue;
            }
            let files = match std::fs::read_dir(cwd_entry.path()) {
                Ok(files) => files,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(SavedSessionListError::Io(error.to_string())),
            };
            for file in files {
                if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                    return Ok(());
                }
                let file = match file {
                    Ok(file) => file,
                    Err(error) => return Err(SavedSessionListError::Io(error.to_string())),
                };
                let file_type = match file.file_type() {
                    Ok(file_type) => file_type,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(SavedSessionListError::Io(error.to_string())),
                };
                let file_path = file.path();
                if !file_type.is_file()
                    || file_path.extension().and_then(|ext| ext.to_str()) != Some("jsonl")
                {
                    continue;
                }
                // Unrelated .jsonl files are ignored. A UUID-suffixed file is
                // a native candidate and must agree with its directory/header.
                let Some(file_id) = filename_session_id(&file_path) else {
                    continue;
                };
                if file_id != session_id {
                    if uuid::Uuid::parse_str(file_id).is_ok() {
                        return Err(SavedSessionListError::InvalidSession(
                            "saved session filename does not match its directory".into(),
                        ));
                    }
                    continue;
                }
                let metadata = match read_saved_metadata(&file_path, &session_id) {
                    Ok(Some(metadata)) => metadata,
                    Ok(None) => continue, // deleted concurrently
                    Err(error) => return Err(error),
                };
                matching_files = matching_files.saturating_add(1);
                if matching_files > 1 {
                    return Err(SavedSessionListError::InvalidSession(
                        "multiple saved files for sessionId".into(),
                    ));
                }
                if !matches_page(
                    metadata.created_at,
                    &metadata.id,
                    &metadata.cwd,
                    filter,
                    after,
                ) {
                    continue;
                }
                sessions.push(metadata);
                sort_sessions(sessions);
                if sessions.len() > capacity {
                    sessions.pop();
                }
            }
        }
    }
    Ok(())
}

fn sort_sessions(sessions: &mut [SavedSessionMetadata]) {
    sessions.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn matches_page(
    created_at: i64,
    id: &str,
    cwd: &str,
    filter: Option<&str>,
    after: Option<&SessionCursor>,
) -> bool {
    if let Some(filter) = filter {
        if normalize_cwd_value(cwd) != filter {
            return false;
        }
    }
    after.map_or(true, |cursor| {
        created_at < cursor.created_at
            || (created_at == cursor.created_at && id > cursor.id.as_str())
    })
}

fn normalize_cwd(path: &Path) -> Result<String, String> {
    if !path.is_absolute() {
        return Err("cwd must be an absolute path".into());
    }
    Ok(normalize_cwd_value(&path.to_string_lossy()))
}

fn normalize_cwd_value(value: &str) -> String {
    let path = std::fs::canonicalize(value).unwrap_or_else(|_| PathBuf::from(value));
    let mut normalized = path.to_string_lossy().replace('\\', "/");
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    if cfg!(windows) {
        normalized.make_ascii_lowercase();
    }
    normalized
}

fn decode_cursor(value: &str, cwd: Option<&str>) -> Result<SessionCursor, SavedSessionListError> {
    if value.is_empty()
        || value.len() % 2 != 0
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(SavedSessionListError::InvalidCursor);
    }
    let bytes = (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| SavedSessionListError::InvalidCursor)?;
    let cursor: SessionCursor =
        serde_json::from_slice(&bytes).map_err(|_| SavedSessionListError::InvalidCursor)?;
    if cursor.version != 1
        || cursor.id.trim().is_empty()
        || cursor.cwd.as_deref() != cwd
        || cursor.created_at < 0
    {
        return Err(SavedSessionListError::InvalidCursor);
    }
    Ok(cursor)
}

fn encode_cursor(cursor: &SessionCursor) -> String {
    serde_json::to_vec(cursor)
        .expect("session cursor serializes")
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn filename_session_id(path: &Path) -> Option<&str> {
    let stem = path.file_stem()?.to_str()?;
    stem.rsplit_once('_').map(|(_, id)| id)
}

fn read_saved_metadata(
    path: &Path,
    expected_id: &str,
) -> Result<Option<SavedSessionMetadata>, SavedSessionListError> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SavedSessionListError::Io(error.to_string())),
    };
    let modified_at = file
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default();
    let mut head = vec![0_u8; DISCOVERY_HEAD_BYTES];
    let head_len = file
        .read(&mut head)
        .map_err(|error| SavedSessionListError::Io(error.to_string()))?;
    let head_bytes = &head[..head_len];
    let Some(newline) = head_bytes.iter().position(|byte| *byte == b'\n') else {
        return Err(SavedSessionListError::InvalidSession(
            "saved session header exceeds discovery budget".into(),
        ));
    };
    let header_line = String::from_utf8_lossy(&head_bytes[..newline]);
    let header: Value = serde_json::from_str(&header_line).map_err(|error| {
        SavedSessionListError::InvalidSession(format!("invalid saved session header: {error}"))
    })?;
    let id = header.get("id").and_then(Value::as_str);
    let cwd = header.get("cwd").and_then(Value::as_str);
    let created_at = header.get("createdAt").and_then(Value::as_i64);
    if header.get("kind").and_then(Value::as_str) != Some("header")
        || header.get("version").and_then(Value::as_u64) != Some(4)
        || id != Some(expected_id)
        || cwd.map_or(true, |cwd| cwd.trim().is_empty())
        || created_at.map_or(true, |_| false)
        || !Path::new(cwd.unwrap_or_default()).is_absolute()
    {
        return Err(SavedSessionListError::InvalidSession(
            "invalid saved session header".into(),
        ));
    }
    let mut title = scan_name_fact(&head_bytes[newline + 1..]);
    if title.is_none() {
        title = header
            .get("metadata")
            .and_then(Value::as_object)
            .and_then(|metadata| metadata.get("title").or_else(|| metadata.get("name")))
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    let size = match file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SavedSessionListError::Io(error.to_string())),
    };
    if size > DISCOVERY_HEAD_BYTES as u64 {
        let start = size.saturating_sub(DISCOVERY_TAIL_BYTES as u64);
        file.seek(SeekFrom::Start(start))
            .map_err(|error| SavedSessionListError::Io(error.to_string()))?;
        let mut tail = vec![0_u8; (size - start) as usize];
        let tail_len = file
            .read(&mut tail)
            .map_err(|error| SavedSessionListError::Io(error.to_string()))?;
        if let Some(tail_title) = scan_name_fact(&tail[..tail_len]) {
            title = Some(tail_title);
        }
    }
    let created_at = created_at.unwrap_or_default();
    Ok(Some(SavedSessionMetadata {
        id: expected_id.to_owned(),
        cwd: cwd.unwrap_or_default().to_owned(),
        title,
        created_at,
        updated_at: modified_at.max(created_at),
        path: path.to_path_buf(),
    }))
}

/// `Some(None)` represents an explicit empty/latest name fact, which clears an
/// older header title. `None` means no name fact was present in this bounded
/// slice.
fn scan_name_fact(bytes: &[u8]) -> Option<String> {
    let mut found = None;
    for line in String::from_utf8_lossy(bytes).lines() {
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if value.get("kind").and_then(Value::as_str) == Some("fact")
            && value.get("fact").and_then(Value::as_str) == Some("name")
        {
            found = value
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| Some(String::new()));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(root: &Path, id: &str, cwd: &Path, created: i64, title: &str) {
        let directory = root.join(id).join("cwd-encoded");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("{created}_{id}.jsonl"));
        std::fs::write(
            path,
            format!(
                "{}\n{}\n",
                serde_json::json!({
                    "kind":"header", "version":4, "id":id,
                    "createdAt":created, "cwd":cwd,
                }),
                serde_json::json!({"kind":"fact", "fact":"name", "name":title}),
            ),
        )
        .unwrap();
    }

    #[test]
    fn discovery_validates_nested_layout_filters_and_pages() {
        let root = std::env::temp_dir().join(format!("rpi-history-{}", uuid::Uuid::new_v4()));
        let first = root.join("one");
        let second = root.join("two");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        session(&root, &a, &first, 1, "A");
        session(&root, &b, &second, 2, "B");
        let page = list_saved_sessions(&root, None, None, 1).unwrap();
        assert_eq!(page.sessions.len(), 1);
        assert_eq!(page.sessions[0].title.as_deref(), Some("B"));
        let next = page.next_cursor.unwrap();
        let page = list_saved_sessions(&root, None, Some(&next), 10).unwrap();
        assert_eq!(
            page.sessions
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec![a.as_str()]
        );
        assert!(matches!(
            list_saved_sessions(&root, Some(&first), Some(&next), 10),
            Err(SavedSessionListError::InvalidCursor)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tied_timestamps_page_without_duplicates_or_omissions() {
        let root = std::env::temp_dir().join(format!("rpi-history-ties-{}", uuid::Uuid::new_v4()));
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        let mut ids = Vec::new();
        for _ in 0..101 {
            let id = uuid::Uuid::new_v4().to_string();
            session(&root, &id, &cwd, 1000, "title");
            ids.push(id);
        }
        ids.sort();
        let mut cursor = None;
        let mut observed = Vec::new();
        loop {
            let page = list_saved_sessions(&root, None, cursor.as_deref(), 7).unwrap();
            observed.extend(page.sessions.into_iter().map(|session| session.id));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(observed, ids);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancellation_stops_discovery_before_candidate_validation() {
        let root =
            std::env::temp_dir().join(format!("rpi-history-cancel-{}", uuid::Uuid::new_v4()));
        let id = uuid::Uuid::new_v4().to_string();
        let directory = root.join(&id).join("cwd");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(format!("1_{id}.jsonl")), "not-json\n").unwrap();
        let cancelled = AtomicBool::new(true);
        let page = list_saved_sessions_cancelable(&root, None, None, 50, &cancelled).unwrap();
        assert!(page.sessions.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_candidate_header_is_rejected_and_missing_root_is_empty() {
        let root = std::env::temp_dir().join(format!("rpi-history-bad-{}", uuid::Uuid::new_v4()));
        assert!(list_saved_sessions(&root, None, None, 50)
            .unwrap()
            .sessions
            .is_empty());
        let id = uuid::Uuid::new_v4().to_string();
        let directory = root.join(&id).join("cwd");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(format!("1_{id}.jsonl")), "not-json\n").unwrap();
        assert!(matches!(
            list_saved_sessions(&root, None, None, 50),
            Err(SavedSessionListError::InvalidSession(_))
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
}
