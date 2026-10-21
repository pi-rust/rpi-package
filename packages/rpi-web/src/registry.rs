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
}

#[derive(Debug)]
pub struct Registry {
    path: PathBuf,
    entries: Vec<RegistryEntry>,
    preferences: HashMap<String, SessionPreferences>,
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
        let data = match std::fs::read(&path) {
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
        Ok(Self {
            path,
            entries: data.sessions,
            preferences: data.preferences,
        })
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
    ) -> Result<SessionPreferences, String> {
        if self.get(id).is_none() {
            return Err("session not found".into());
        }
        let previous = self.preferences(id);
        let value = self.preferences.entry(id.into()).or_default();
        if let Some(pinned) = pinned {
            value.pinned = pinned;
        }
        if let Some(archived) = archived {
            value.archived = archived;
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
        let preferences = self.preferences.clone();
        let before = self.entries.len();
        self.entries.retain(|entry| entry.session_id != session_id);
        let removed = self.entries.len() != before;
        if removed {
            self.preferences.remove(session_id);
            if let Err(error) = self.save_result() {
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
        };
        let raw = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, raw).map_err(|e| e.to_string())?;
        std::fs::rename(&temp, &self.path).map_err(|e| e.to_string())
    }
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
            .update_preferences("a", Some(true), Some(true))
            .unwrap();
        let reloaded = Registry::load(dir.join("sessions.json")).unwrap();
        assert!(reloaded.preferences("a").pinned && reloaded.preferences("a").archived);
        registry.update_preferences("a", None, Some(false)).unwrap();
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
            .update_preferences("kept", Some(true), None)
            .unwrap();
        std::fs::create_dir(path.with_extension("json.tmp")).unwrap();
        assert!(registry.rename("kept", "Unsaved".into()).is_err());
        assert!(registry.remove("kept").is_err());
        assert!(registry
            .update_preferences("kept", Some(false), Some(true))
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
