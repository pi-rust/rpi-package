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
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Refuse to read a store larger than this; a memory file that big is a bug.
const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Most entries one attach imports.
const MAX_ENTRIES: usize = 5_000;

/// Where the store lives, when the project directory is not writable or the
/// caller wants profiles somewhere else.
const DIRECTORY_ENV: &str = "RPI_OPTCHAT_STORE_DIR";

/// The store directory for this project.
pub fn directory() -> PathBuf {
    if let Some(dir) = std::env::var_os(DIRECTORY_ENV).filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
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

pub(crate) fn read_in(base: &Path, profile: &str) -> Result<Vec<Entry>, String> {
    let Some(path) = path_in(base, profile) else {
        return Err(format!("`{profile}` is not a usable profile name"));
    };
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    // A reader takes a shared lock so it cannot observe a half-written JSONL
    // line from another process (append takes the exclusive lock).
    fs2::FileExt::lock_shared(&file)
        .map_err(|error| format!("cannot lock {}: {error}", path.display()))?;
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            let _ = fs2::FileExt::unlock(&file);
            return Err(format!("cannot stat {}: {error}", path.display()));
        }
    };
    if metadata.len() > MAX_BYTES {
        let _ = fs2::FileExt::unlock(&file);
        return Err(format!(
            "{} is larger than {} MiB; split it first",
            path.display(),
            MAX_BYTES / (1024 * 1024)
        ));
    }
    let mut text = String::new();
    let read = file.read_to_string(&mut text);
    let _ = fs2::FileExt::unlock(&file);
    read.map_err(|error| format!("{} is not readable UTF-8: {error}", path.display()))?;
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
    fs2::FileExt::lock_exclusive(&file)
        .map_err(|error| format!("cannot lock {}: {error}", path.display()))?;
    let result = writeln!(file, "{line}")
        .map_err(|error| format!("cannot write {}: {error}", path.display()));
    let _ = fs2::FileExt::unlock(&file);
    result
}

/// Append into an explicit base directory; tests only.
#[cfg(test)]
pub(crate) fn append_in_for_tests(base: &Path, profile: &str, entry: &Entry) -> Result<(), String> {
    append_in(base, profile, entry)
}

/// How many entries a profile holds, for `/optchat status`.
pub fn count(profile: &str) -> usize {
    read(profile).map(|entries| entries.len()).unwrap_or(0)
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
    fn concurrent_appends_keep_complete_jsonl_lines() {
        let dir = std::env::temp_dir().join(format!("optchat-concurrent-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let base = dir.join(".rpi").join("optchat");
        let mut workers = Vec::new();
        for worker in 0..4u64 {
            let base = base.clone();
            workers.push(std::thread::spawn(move || {
                for offset in 0..25u64 {
                    let i = worker * 25 + offset;
                    append_in(
                        &base,
                        "shared",
                        &entry(i, &format!("worker-{worker}-{offset}")),
                    )
                    .expect("append");
                }
            }));
        }
        for worker in workers {
            worker.join().expect("worker");
        }
        let entries = read_in(&base, "shared").expect("read");
        assert_eq!(entries.len(), 100);
        assert_eq!(entries.first().map(|entry| entry.i), Some(0));
        assert_eq!(entries.last().map(|entry| entry.i), Some(99));
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
