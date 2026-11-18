//! The project store behind `/optchat profile`: OptChat's cross-session memory.
//!
//! Upstream keeps one durable store per profile, so a new chat opens with the
//! old one's memory. A native plugin has no store action, so this is a JSONL
//! file next to the project, following `rpi-goal`'s `.rpi/goals.json` precedent:
//! one entry per line, read with a size and line bound, written with a single
//! append. The session branch stays the source of truth for the current chat; the
//! store is where it can outlive the session, and `/optchat profile attach` copies
//! it back in as imported notes.

use crate::memory::Entry;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Refuse to read a store larger than this; a memory file that big is a bug.
const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Most entries one attach imports.
const MAX_ENTRIES: usize = 5_000;

/// The store directory for this project.
pub fn directory() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".rpi")
        .join("optchat")
}

/// A profile name safe to use as a file name: no separators, no traversal.
pub fn normalize(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return None;
    }
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch);
        } else if ch.is_whitespace() {
            out.push('-');
        } else {
            return None;
        }
    }
    // `.`/`..` and a leading dot would escape or hide the file.
    if out.starts_with('.') || out.chars().all(|ch| ch == '.') {
        return None;
    }
    Some(out)
}

pub fn path(profile: &str) -> Option<PathBuf> {
    path_in(&directory(), profile)
}

fn path_in(base: &Path, profile: &str) -> Option<PathBuf> {
    normalize(profile).map(|name| base.join(format!("{name}.jsonl")))
}

/// Entries the profile holds, oldest first, bounded and tolerant: a malformed
/// line is skipped rather than failing the attach.
pub fn read(profile: &str) -> Result<Vec<Entry>, String> {
    read_in(&directory(), profile)
}

fn read_in(base: &Path, profile: &str) -> Result<Vec<Entry>, String> {
    let Some(path) = path_in(base, profile) else {
        return Err(format!("`{profile}` is not a usable profile name"));
    };
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    if metadata.len() > MAX_BYTES {
        return Err(format!(
            "{} is larger than {} MiB; split it first",
            path.display(),
            MAX_BYTES / (1024 * 1024)
        ));
    }
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("{} is not readable UTF-8: {error}", path.display()))?;
    let mut entries: Vec<Entry> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Entry>(line).ok())
        .collect();
    entries.sort_by_key(|entry| entry.i);
    entries.dedup_by_key(|entry| entry.i);
    if entries.len() > MAX_ENTRIES {
        entries.drain(..entries.len() - MAX_ENTRIES);
    }
    Ok(entries)
}

/// Append one entry. Best effort: a memory write must never fail a turn, so the
/// caller only logs the error.
pub fn append(profile: &str, entry: &Entry) -> Result<(), String> {
    append_in(&directory(), profile, entry)
}

fn append_in(base: &Path, profile: &str, entry: &Entry) -> Result<(), String> {
    let Some(path) = path_in(base, profile) else {
        return Err(format!("`{profile}` is not a usable profile name"));
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let line = serde_json::to_string(entry).map_err(|error| format!("cannot encode: {error}"))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    writeln!(file, "{line}").map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// How many entries a profile holds, for `/optchat status`.
pub fn count(profile: &str) -> usize {
    read(profile).map(|entries| entries.len()).unwrap_or(0)
}

/// Whether the store already holds something for this profile.
#[cfg(test)]
pub fn exists(profile: &str) -> bool {
    path(profile)
        .map(|path| Path::new(&path).is_file())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{iso8601, Kind};

    fn entry(i: u64, text: &str) -> Entry {
        Entry {
            i,
            kind: Kind::Note,
            text: text.to_string(),
            date: iso8601(1_700_000_000_000 + i as i64),
            receipt: None,
            origin: None,
        }
    }

    #[test]
    fn names_are_restricted_to_a_single_safe_component() {
        assert_eq!(normalize("work").as_deref(), Some("work"));
        assert_eq!(normalize("my work").as_deref(), Some("my-work"));
        assert_eq!(normalize(" a_b-c.1 ").as_deref(), Some("a_b-c.1"));
        assert_eq!(normalize(""), None);
        assert_eq!(normalize("   "), None);
        assert_eq!(normalize("../escape"), None);
        assert_eq!(normalize("a/b"), None);
        assert_eq!(normalize(".hidden"), None);
        assert_eq!(normalize(".."), None);
        assert_eq!(normalize(&"x".repeat(65)), None);
    }

    #[test]
    fn a_round_trip_skips_broken_lines() {
        let dir = std::env::temp_dir().join(format!("optchat-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".rpi").join("optchat")).expect("dir");
        let base = dir.join(".rpi").join("optchat");
        let path = path_in(&base, "round-trip").expect("path");
        fs::write(
            &path,
            format!(
                "{}\nnot json\n{}\n",
                serde_json::to_string(&entry(1, "second")).unwrap(),
                serde_json::to_string(&entry(0, "first")).unwrap()
            ),
        )
        .expect("write");
        let entries = read_in(&base, "round-trip").expect("read");
        assert_eq!(entries.len(), 2);
        // Sorted by id, so the older message comes back first.
        assert_eq!(entries[0].text, "first");
        assert_eq!(entries[1].text, "second");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn appending_creates_the_directory_and_keeps_order() {
        let dir = std::env::temp_dir().join(format!("optchat-append-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let base = dir.join(".rpi").join("optchat");
        append_in(&base, "work", &entry(0, "first")).expect("first");
        append_in(&base, "work", &entry(1, "second")).expect("second");
        let entries = read_in(&base, "work").expect("read");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text, "first");
        assert_eq!(entries[1].text, "second");
        assert!(path_in(&base, "work").expect("path").is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_store_reads_as_empty_and_a_bad_name_fails() {
        let dir = std::env::temp_dir().join(format!("optchat-empty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let base = dir.join(".rpi").join("optchat");
        assert!(read_in(&base, "absent").expect("empty").is_empty());
        assert!(read_in(&base, "../bad").is_err());
        assert!(append_in(&base, "../bad", &entry(0, "x")).is_err());
        assert!(!path_in(&base, "absent").expect("path").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
