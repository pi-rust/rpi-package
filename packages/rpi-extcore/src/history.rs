//! Read native transcript branches without projecting a client protocol.
pub mod page;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};
/// Maximum first-line bytes inspected for native workspace metadata (including newline).
pub const MAX_METADATA_HEADER_BYTES: usize = 64 * 1024;

/// Read-only metadata from the authoritative native v4 transcript header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSessionMetadata {
    pub cwd: PathBuf,
}

/// Read only a bounded first line, validating the native format and exact identity.
/// No branch/messages are parsed or retained; callers may retry after a read error.
/// This is a metadata snapshot, not protection against concurrent local rewrites.
pub fn saved_session_metadata(path: &Path, id: &str) -> Result<SavedSessionMetadata, String> {
    let header = read_saved_header(path, id, MAX_METADATA_HEADER_BYTES)?;
    let cwd = header["cwd"]
        .as_str()
        .ok_or("invalid saved session cwd metadata")?;
    Ok(SavedSessionMetadata { cwd: cwd.into() })
}

fn read_saved_header(path: &Path, id: &str, limit: usize) -> Result<Value, String> {
    // Put the limit below buffering so even read-ahead cannot exceed it.
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file.take(limit as u64 + 1));
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    if line.len() > limit {
        return Err("saved session header exceeds byte budget".into());
    }
    let header: Value =
        serde_json::from_str(&line).map_err(|_| "invalid saved session header".to_string())?;
    if header["kind"] != "header" || header["version"] != 4 || header["id"] != id {
        return Err("invalid saved session header".into());
    }
    Ok(header)
}

pub fn find_saved_session(directory: &Path, id: &str) -> Result<PathBuf, String> {
    let mut found = None;
    let directories = std::fs::read_dir(directory)
        .map_err(|_| "unknown sessionId: no saved native session".to_string())?;
    for entry in directories {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(entry.path()).map_err(|e| e.to_string())? {
            let file = file.map_err(|e| e.to_string())?;
            if !file.file_type().map_err(|e| e.to_string())?.is_file()
                || !file
                    .file_name()
                    .to_string_lossy()
                    .ends_with(&format!("_{id}.jsonl"))
            {
                continue;
            }
            let path = file.path();
            // Preserve the existing discovery budget and compatibility with headers
            // without workspace metadata; stricter metadata consumers opt in above.
            read_saved_header(&path, id, page::MAX_PAGE_BYTES)?;
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
        path.push((id, entry));
        leaf = entry["parentId"].as_str();
    }
    let mut updates = Vec::new();
    for (id, entry) in path.into_iter().rev() {
        if entry["type"] == "message" {
            let mut message = entry["message"].clone();
            let fields = message
                .as_object_mut()
                .ok_or("invalid saved session message")?;
            // Identity belongs to the native entry, never to a provider field
            // or a value embedded inside message content.
            fields.insert("entryId".into(), json!(id));
            updates.push(message);
        }
    }
    Ok(updates)
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    use serde_json::json;

    struct HeaderFile(PathBuf);
    impl HeaderFile {
        fn new() -> Self {
            Self(
                std::env::temp_dir()
                    .join(format!("cwd-worker-header-{}.jsonl", uuid::Uuid::new_v4())),
            )
        }
        fn write(&self, value: &Value) {
            std::fs::write(
                &self.0,
                format!("{value}\ninvalid branch JSON is not read\n"),
            )
            .unwrap();
        }
    }
    impl Drop for HeaderFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn metadata_reads_only_validated_v4_header() {
        let file = HeaderFile::new();
        let header = json!({"kind":"header","version":4,"id":"expected","createdAt":0,"cwd":"workspace","metadata":{}});
        file.write(&header);
        assert_eq!(
            saved_session_metadata(&file.0, "expected").unwrap().cwd,
            PathBuf::from("workspace")
        );
        assert!(saved_session_metadata(&file.0, "other").is_err());
        for (key, value) in [
            ("kind", json!("entry")),
            ("version", json!(3)),
            ("cwd", json!(null)),
            ("cwd", json!(42)),
        ] {
            let mut bad = header.clone();
            bad[key] = value;
            file.write(&bad);
            assert!(
                saved_session_metadata(&file.0, "expected").is_err(),
                "{key}"
            );
        }
        let mut missing = header.clone();
        missing.as_object_mut().unwrap().remove("cwd");
        file.write(&missing);
        assert!(saved_session_metadata(&file.0, "expected").is_err());
        // Retrying after repair has no stale cache/cursor or transcript sidecar.
        file.write(&header);
        assert!(saved_session_metadata(&file.0, "expected").is_ok());
    }

    #[test]
    fn metadata_header_budget_is_limit_plus_one_and_retryable() {
        let file = HeaderFile::new();
        let mut header = json!({"kind":"header","version":4,"id":"expected","createdAt":0,"cwd":"","padding":""});
        let overhead = serde_json::to_vec(&header).unwrap().len() + 1;
        header["padding"] = json!("x".repeat(MAX_METADATA_HEADER_BYTES - overhead));
        file.write(&header);
        assert!(saved_session_metadata(&file.0, "expected").is_ok());
        header["padding"] = json!("x".repeat(MAX_METADATA_HEADER_BYTES - overhead + 1));
        file.write(&header);
        assert_eq!(
            saved_session_metadata(&file.0, "expected").unwrap_err(),
            "saved session header exceeds byte budget"
        );
    }
}
