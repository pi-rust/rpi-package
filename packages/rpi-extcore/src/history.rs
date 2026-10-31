//! Read native transcript branches without projecting a client protocol.
pub mod page;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};
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
            let mut reader = BufReader::new(std::fs::File::open(&path).map_err(|e| e.to_string())?);
            let mut line = String::new();
            (&mut reader)
                .take(page::MAX_PAGE_BYTES as u64 + 1)
                .read_line(&mut line)
                .map_err(|e| e.to_string())?;
            if line.len() > page::MAX_PAGE_BYTES {
                return Err("saved session header exceeds byte budget".into());
            }
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
