//! Tiny durable session registry.
//!
//! The native host owns the transcripts; this file only remembers which session
//! ids the workspace has seen, so the sidebar survives a bridge restart and a
//! session can be reopened with `--session-id`.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionPreferences {
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "projectId", default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(rename = "parentSessionId", default)]
    pub parent_session_id: Option<String>,
    #[serde(rename = "forkEntryId", default)]
    pub fork_entry_id: Option<String>,
    #[serde(default)]
    pub relation: Option<String>,
    #[serde(rename = "createdAt", default)]
    pub created_at: u64,
    #[serde(rename = "updatedAt", default)]
    pub updated_at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    sessions: Vec<RegistryEntry>,
    #[serde(default)]
    preferences: HashMap<String, SessionPreferences>,
    #[serde(default, deserialize_with = "resource_records")]
    resources: HashMap<String, crate::resources::ResourceConfiguration>,
}

#[derive(Debug)]
pub struct Registry {
    path: PathBuf,
    entries: Vec<RegistryEntry>,
    preferences: HashMap<String, SessionPreferences>,
    resources: HashMap<String, crate::resources::ResourceConfiguration>,
}

/// Default registry location, outside the project tree.
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(".rpi-web").join("sessions.json")
}

impl Registry {
    pub fn load(path: PathBuf) -> Result<Self, String> {
        let data = match read_registry(&path) {
            Ok(raw) => serde_json::from_slice::<RegistryFile>(&raw)
                .map_err(|e| format!("cannot read session registry: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => RegistryFile::default(),
            Err(e) => return Err(format!("cannot read session registry: {e}")),
        };
        let mut ids = HashSet::new();
        if data.sessions.iter().any(|entry| {
            entry.session_id.trim().is_empty() || !ids.insert(entry.session_id.as_str())
        }) {
            return Err("session registry contains empty or duplicate session ids".into());
        }
        for (id, record) in &data.resources {
            if !ids.contains(id.as_str()) {
                return Err("resource record references unknown session".into());
            }
            record.validate_session(id).map_err(|e| e.to_string())?;
        }
        Ok(Self {
            path,
            entries: data.sessions,
            preferences: data.preferences,
            resources: data.resources,
        })
    }

    pub fn resource_configuration(
        &self,
        id: &str,
    ) -> Option<&crate::resources::ResourceConfiguration> {
        self.resources.get(id)
    }

    /// Persist configuration under the existing registry owner. Creation can
    /// register metadata and selection together, before the first native launch.
    pub fn set_resource_configuration(
        &mut self,
        entry: RegistryEntry,
        record: crate::resources::ResourceConfiguration,
    ) -> Result<(), String> {
        record
            .validate_session(&entry.session_id)
            .map_err(|e| e.to_string())?;
        let previous_entries = self.entries.clone();
        let previous_resources = self.resources.clone();
        if !self
            .entries
            .iter()
            .any(|e| e.session_id == entry.session_id)
        {
            self.entries.push(entry.clone());
        }
        self.resources.insert(entry.session_id, record);
        if let Err(error) = self.save_result() {
            self.entries = previous_entries;
            self.resources = previous_resources;
            return Err(error);
        }
        Ok(())
    }

    pub fn entries(&self) -> &[RegistryEntry] {
        &self.entries
    }

    pub fn get(&self, session_id: &str) -> Option<&RegistryEntry> {
        self.entries
            .iter()
            .find(|entry| entry.session_id == session_id)
    }

    pub fn preferences(&self, id: &str) -> SessionPreferences {
        self.preferences.get(id).cloned().unwrap_or_default()
    }

    pub fn update_preferences(
        &mut self,
        id: &str,
        pinned: Option<bool>,
        archived: Option<bool>,
        tags: Option<Vec<String>>,
    ) -> Result<SessionPreferences, String> {
        if self.get(id).is_none() {
            return Err("session not found".into());
        }
        let normalized_tags = tags.map(normalize_tags).transpose()?;
        let previous = self.preferences(id);
        let value = self.preferences.entry(id.into()).or_default();
        if let Some(pinned) = pinned {
            value.pinned = pinned;
        }
        if let Some(archived) = archived {
            value.archived = archived;
        }
        if let Some(tags) = normalized_tags {
            value.tags = tags;
        }
        let result = value.clone();
        if let Err(error) = self.save_result() {
            self.preferences.insert(id.into(), previous);
            return Err(error);
        }
        Ok(result)
    }

    pub fn upsert(&mut self, entry: RegistryEntry) -> Result<(), String> {
        let previous = self.entries.clone();
        match self
            .entries
            .iter_mut()
            .find(|existing| existing.session_id == entry.session_id)
        {
            Some(existing) => {
                if entry.project_id.is_some() {
                    existing.project_id = entry.project_id;
                }
                if entry.title.is_some() {
                    existing.title = entry.title;
                }
                if entry.parent_session_id.is_some() {
                    existing.parent_session_id = entry.parent_session_id;
                }
                if entry.fork_entry_id.is_some() {
                    existing.fork_entry_id = entry.fork_entry_id;
                }
                if entry.relation.is_some() {
                    existing.relation = entry.relation;
                }
                existing.updated_at = existing.updated_at.max(entry.updated_at);
            }
            None => self.entries.push(entry),
        }
        if let Err(error) = self.save_result() {
            self.entries = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn remove(&mut self, session_id: &str) -> Result<bool, String> {
        let previous = self.entries.clone();
        let resources = self.resources.clone();
        let preferences = self.preferences.clone();
        let before = self.entries.len();
        self.entries.retain(|entry| entry.session_id != session_id);
        let removed = self.entries.len() != before;
        if removed {
            self.preferences.remove(session_id);
            self.resources.remove(session_id);
            if let Err(error) = self.save_result() {
                self.resources = resources;
                self.entries = previous;
                self.preferences = preferences;
                return Err(error);
            }
        }
        Ok(removed)
    }

    pub fn rename(&mut self, session_id: &str, title: String) -> Result<bool, String> {
        let previous = self.entries.clone();
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.session_id == session_id)
        else {
            return Ok(false);
        };
        entry.title = Some(title);
        entry.updated_at = unix_ms();
        if let Err(error) = self.save_result() {
            self.entries = previous;
            return Err(error);
        }
        Ok(true)
    }

    fn save_result(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let file = RegistryFile {
            sessions: self.entries.clone(),
            preferences: self.preferences.clone(),
            resources: self.resources.clone(),
        };
        let raw = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        if raw.len() > MAX_REGISTRY_BYTES {
            return Err("session registry exceeds byte budget".into());
        }
        atomic_write_registry(&self.path, raw.as_bytes(), |_| Ok(())).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
pub(crate) fn atomic_write_resource_test(
    path: &std::path::Path,
    raw: &[u8],
    stage: u8,
) -> std::io::Result<()> {
    atomic_write_registry(path, raw, |at| {
        let matches = matches!(
            (stage, at),
            (0, RegistryWriteStage::BeforeWrite)
                | (1, RegistryWriteStage::BeforeRename)
                | (2, RegistryWriteStage::AfterRename)
        );
        if matches {
            Err(std::io::Error::other(
                "injected resource decision persistence failure",
            ))
        } else {
            Ok(())
        }
    })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegistryWriteStage {
    BeforeWrite,
    BeforeRename,
    AfterRename,
}
fn atomic_write_registry(
    path: &std::path::Path,
    raw: &[u8],
    mut checkpoint: impl FnMut(RegistryWriteStage) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::io::Write;
    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let temp = path.with_extension("json.tmp");
    // Never truncate a stale/foreign temp, follow its link or infer success from
    // it. A crash-left temp needs deliberate owner inspection/removal.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let _cleanup = Temp(temp.clone());
    checkpoint(RegistryWriteStage::BeforeWrite)?;
    file.write_all(raw)?;
    file.sync_all()?;
    drop(file);
    checkpoint(RegistryWriteStage::BeforeRename)?;
    std::fs::rename(&temp, path)?;
    checkpoint(RegistryWriteStage::AfterRename)?;
    #[cfg(unix)]
    std::fs::File::open(path.parent().unwrap_or(std::path::Path::new(".")))?.sync_all()?;
    // Windows std has no portable directory sync. File contents are flushed
    // before rename; acknowledged process-crash persistence is not a claim of
    // power-loss durability for the parent directory/rename on that platform.
    Ok(())
}

const MAX_REGISTRY_BYTES: usize = 128 * 1024 * 1024;
fn read_registry(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut raw = Vec::new();
    file.take(MAX_REGISTRY_BYTES as u64 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > MAX_REGISTRY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "session registry exceeds byte budget",
        ));
    }
    Ok(raw)
}
fn resource_records<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<HashMap<String, crate::resources::ResourceConfiguration>, D::Error> {
    struct Records;
    impl<'de> serde::de::Visitor<'de> for Records {
        type Value = HashMap<String, crate::resources::ResourceConfiguration>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("unique resource session records")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut records = HashMap::new();
            while let Some((id, record)) =
                map.next_entry::<String, crate::resources::ResourceConfiguration>()?
            {
                if records.insert(id, record).is_some() {
                    return Err(serde::de::Error::custom(
                        "duplicate resource session record",
                    ));
                }
            }
            Ok(records)
        }
    }
    deserializer.deserialize_map(Records)
}

const MAX_TAGS: usize = 12;
const MAX_TAG_LENGTH: usize = 32;

fn normalize_tags(tags: Vec<String>) -> Result<Vec<String>, String> {
    let mut normalized = Vec::new();
    for tag in tags {
        let tag = tag.trim();
        if tag.is_empty() {
            continue;
        }
        if tag.chars().count() > MAX_TAG_LENGTH {
            return Err(format!("tag is longer than {MAX_TAG_LENGTH} characters"));
        }
        if !normalized.iter().any(|item| item == tag) {
            normalized.push(tag.to_owned());
        }
        if normalized.len() > MAX_TAGS {
            return Err(format!("a session can have at most {MAX_TAGS} tags"));
        }
    }
    Ok(normalized)
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synced_atomic_registry_faults_preserve_prior_bytes_or_report_commit_uncertainty() {
        let root =
            std::env::temp_dir().join(format!("registry-sync-fault-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("registry.json");
        for stage in [
            RegistryWriteStage::BeforeWrite,
            RegistryWriteStage::BeforeRename,
            RegistryWriteStage::AfterRename,
        ] {
            std::fs::write(&path, b"prior durable record").unwrap();
            let result = atomic_write_registry(&path, b"next durable record", |at| {
                if at == stage {
                    Err(std::io::Error::other("injected registry I/O failure"))
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err());
            let expected: &[u8] = if stage == RegistryWriteStage::AfterRename {
                b"next durable record"
            } else {
                b"prior durable record"
            };
            assert_eq!(std::fs::read(&path).unwrap(), expected);
            assert!(!path.with_extension("json.tmp").exists());
        }
        atomic_write_registry(&path, b"acknowledged durable record", |_| Ok(())).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"acknowledged durable record"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn structurally_invalid_registries_are_rejected_without_being_rewritten() {
        let dir = std::env::temp_dir().join(format!("rpi-registry-shape-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("sessions.json");
        for raw in [
            r#"{}"#,
            r#"{"sessions":[{"sessionId":""}]}"#,
            r#"{"sessions":[{"sessionId":"same"},{"sessionId":"same"}]}"#,
        ] {
            std::fs::write(&file, raw).unwrap();
            assert!(Registry::load(file.clone()).is_err());
            assert_eq!(std::fs::read_to_string(&file).unwrap(), raw);
        }
        std::fs::write(&file, r#"{"sessions":[]}"#).unwrap();
        assert!(Registry::load(file).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn upsert_merges_and_removes() {
        let dir = std::env::temp_dir().join(format!("rpi-web-registry-{}", uuid::Uuid::new_v4()));
        let mut registry = Registry::load(dir.join("sessions.json")).unwrap();
        registry
            .upsert(RegistryEntry {
                session_id: "a".into(),
                project_id: Some("project-a".into()),
                title: None,
                parent_session_id: None,
                fork_entry_id: None,
                relation: None,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
        registry
            .upsert(RegistryEntry {
                session_id: "a".into(),
                project_id: None,
                title: Some("first prompt".into()),
                parent_session_id: None,
                fork_entry_id: None,
                relation: None,
                created_at: 1,
                updated_at: 5,
            })
            .unwrap();
        let entry = registry.get("a").unwrap();
        assert_eq!(entry.title.as_deref(), Some("first prompt"));
        assert_eq!(entry.updated_at, 5);
        assert_eq!(entry.project_id.as_deref(), Some("project-a"));
        registry
            .update_preferences(
                "a",
                Some(true),
                Some(true),
                Some(vec!["work".into(), "priority".into()]),
            )
            .unwrap();
        let reloaded = Registry::load(dir.join("sessions.json")).unwrap();
        assert!(reloaded.preferences("a").pinned && reloaded.preferences("a").archived);
        assert_eq!(reloaded.preferences("a").tags, ["work", "priority"]);
        registry
            .update_preferences("a", None, Some(false), None)
            .unwrap();
        assert!(registry.preferences("a").pinned && !registry.preferences("a").archived);
        assert!(registry.rename("a", "renamed".into()).unwrap());
        assert_eq!(registry.get("a").unwrap().title.as_deref(), Some("renamed"));
        assert!(registry.remove("a").unwrap());
        assert!(!registry.remove("a").unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reloads_persisted_entries() {
        let dir = std::env::temp_dir().join(format!("rpi-web-registry-{}", uuid::Uuid::new_v4()));
        let path = dir.join("sessions.json");
        let mut registry = Registry::load(path.clone()).unwrap();
        registry
            .upsert(RegistryEntry {
                session_id: "b".into(),
                project_id: Some("project-b".into()),
                title: Some("kept".into()),
                parent_session_id: None,
                fork_entry_id: None,
                relation: None,
                created_at: 2,
                updated_at: 2,
            })
            .unwrap();
        let reloaded = Registry::load(path).unwrap();
        assert_eq!(reloaded.entries().len(), 1);
        assert_eq!(reloaded.get("b").unwrap().title.as_deref(), Some("kept"));
        assert_eq!(
            reloaded.get("b").unwrap().project_id.as_deref(),
            Some("project-b")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn malformed_registry_is_reported_without_overwriting_it() {
        let dir = std::env::temp_dir().join(format!("rpi-web-registry-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sessions.json");
        std::fs::write(&file, "{damaged").unwrap();
        assert!(Registry::load(file.clone()).is_err());
        assert_eq!(std::fs::read_to_string(file).unwrap(), "{damaged");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn normalizes_and_limits_tags() {
        let dir = std::env::temp_dir().join(format!("rpi-web-tags-{}", uuid::Uuid::new_v4()));
        let mut registry = Registry::load(dir.join("sessions.json")).unwrap();
        registry
            .upsert(RegistryEntry {
                session_id: "tags".into(),
                project_id: None,
                title: None,
                parent_session_id: None,
                fork_entry_id: None,
                relation: None,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
        registry
            .update_preferences(
                "tags",
                None,
                None,
                Some(vec![
                    " work ".into(),
                    "work".into(),
                    "priority".into(),
                    "".into(),
                ]),
            )
            .unwrap();
        assert_eq!(registry.preferences("tags").tags, ["work", "priority"]);
        assert!(registry
            .update_preferences(
                "tags",
                None,
                None,
                Some(vec!["x".repeat(MAX_TAG_LENGTH + 1)]),
            )
            .is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_preferences_default_to_empty_tags() {
        let dir =
            std::env::temp_dir().join(format!("rpi-web-legacy-tags-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.json");
        std::fs::write(&path, r#"{"sessions":[{"sessionId":"legacy"}],"preferences":{"legacy":{"pinned":true,"archived":false}}}"#).unwrap();
        let registry = Registry::load(path).unwrap();
        assert!(registry.preferences("legacy").pinned);
        assert!(registry.preferences("legacy").tags.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_writes_roll_back_all_registry_mutations() {
        let dir = std::env::temp_dir().join(format!("rpi-web-registry-{}", uuid::Uuid::new_v4()));
        let path = dir.join("sessions.json");
        let mut registry = Registry::load(path.clone()).unwrap();
        let entry = RegistryEntry {
            session_id: "kept".into(),
            project_id: None,
            title: Some("Original".into()),
            parent_session_id: None,
            fork_entry_id: None,
            relation: None,
            created_at: 1,
            updated_at: 1,
        };
        registry.upsert(entry.clone()).unwrap();
        registry
            .update_preferences("kept", Some(true), None, None)
            .unwrap();
        std::fs::create_dir(path.with_extension("json.tmp")).unwrap();
        assert!(registry.rename("kept", "Unsaved".into()).is_err());
        assert!(registry.remove("kept").is_err());
        assert!(registry
            .update_preferences("kept", Some(false), Some(true), None)
            .is_err());
        let mut other = entry;
        other.session_id = "unsaved".into();
        assert!(registry.upsert(other).is_err());
        assert_eq!(registry.entries().len(), 1);
        assert_eq!(
            registry.get("kept").unwrap().title.as_deref(),
            Some("Original")
        );
        assert!(registry.preferences("kept").pinned);
        assert!(!registry.preferences("kept").archived);
        assert_eq!(Registry::load(path).unwrap().entries().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
