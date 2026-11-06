use super::{Alias, LocalError};
use rpi_extcore::{registry::Registry, workspace};
use std::path::Path;

pub(super) fn load_aliases(data: &Path) -> Result<Vec<Alias>, LocalError> {
    let target = data.join("rpc/projects.json");
    let mut aliases: Vec<Alias> = match std::fs::read(&target) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    let mut changed = false;
    for path in [data.join("projects.json"), data.join("node/projects.json")] {
        let saved: Vec<Alias> = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for mut alias in saved {
            // Unavailable legacy paths stay untouched in the source catalog.
            let Ok(project) = workspace::Project::new(&alias.path) else {
                continue;
            };
            if aliases
                .iter()
                .any(|a| workspace::Project::new(&a.path).is_ok_and(|p| p.id == project.id))
            {
                continue;
            }
            alias.path = project.root;
            aliases.push(alias);
            changed = true;
        }
    }
    if changed {
        let temporary = target.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(&aliases)?)?;
        std::fs::rename(temporary, target)?;
    }
    Ok(aliases)
}

pub(super) fn migrate_registries(data: &Path, aliases: &[Alias]) -> Result<(), LocalError> {
    for source in [data.join("sessions.json"), data.join("node/sessions.json")] {
        // Registry::load treats only NotFound as empty; do not hide other I/O
        // errors (including a directory masquerading as a registry).
        let legacy = Registry::load(source).map_err(LocalError::Initialization)?;
        for alias in aliases {
            let Ok(project) = workspace::Project::new(&alias.path) else {
                continue;
            };
            let directory = data.join("rpc/projects").join(&project.id);
            let entries: Vec<_> = legacy
                .entries()
                .iter()
                .filter(|entry| entry.project_id.as_deref() == Some(&project.id))
                .collect();
            if entries.is_empty() {
                continue;
            }
            std::fs::create_dir_all(&directory)?;
            let target = directory.join("sessions.json");
            let registry = Registry::load(target.clone()).map_err(LocalError::Initialization)?;
            let entries: Vec<_> = entries
                .into_iter()
                .filter(|entry| registry.get(&entry.session_id).is_none())
                .collect();
            if entries.is_empty() {
                continue;
            }
            // Stage both entries and preferences: a failed preference update must
            // not leave an entry that subsequent migrations mistake for complete.
            let staged = directory.join("sessions.import.json");
            let result = (|| {
                let bytes = match std::fs::read(&target) {
                    Ok(bytes) => bytes,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        b"{\"sessions\":[]}".to_vec()
                    }
                    Err(e) => return Err(LocalError::from(e)),
                };
                std::fs::write(&staged, bytes)?;
                let mut registry =
                    Registry::load(staged.clone()).map_err(LocalError::Initialization)?;
                for entry in entries {
                    registry
                        .upsert(entry.clone())
                        .map_err(LocalError::Initialization)?;
                    let prefs = legacy.preferences(&entry.session_id);
                    registry
                        .update_preferences(
                            &entry.session_id,
                            Some(prefs.pinned),
                            Some(prefs.archived),
                            Some(prefs.tags),
                        )
                        .map_err(LocalError::Initialization)?;
                }
                std::fs::rename(&staged, &target)?;
                Ok(())
            })();
            let _ = std::fs::remove_file(&staged);
            let _ = std::fs::remove_file(staged.with_extension("json.tmp"));
            result?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "rpi-migration-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(path.join("rpc")).unwrap();
            Self(path)
        }
        fn write(&self, path: &str, bytes: impl AsRef<[u8]>) {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        fn alias(&self) -> Alias {
            let path = self.0.join("project");
            std::fs::create_dir_all(&path).unwrap();
            Alias {
                name: "project".into(),
                path,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn corrupt_catalogues_are_never_rewritten() {
        let f = Fixture::new();
        let alias = f.alias();
        let valid = serde_json::to_vec(&vec![alias]).unwrap();
        for path in ["rpc/projects.json", "projects.json", "node/projects.json"] {
            f.write("rpc/projects.json", &valid);
            f.write("projects.json", b"[]");
            f.write("node/projects.json", b"[]");
            f.write(path, b"{broken");
            let before = std::fs::read(f.0.join("rpc/projects.json")).unwrap();
            assert!(load_aliases(&f.0).is_err());
            assert_eq!(std::fs::read(f.0.join(path)).unwrap(), b"{broken");
            assert_eq!(
                std::fs::read(f.0.join("rpc/projects.json")).unwrap(),
                before
            );
        }
    }

    #[test]
    fn catalogue_import_deduplicates_and_preserves_unavailable_sources() {
        let f = Fixture::new();
        let alias = f.alias();
        let original = json!([
            {"name":"first", "path":alias.path},
            {"name":"unavailable", "path":f.0.join("missing")}
        ])
        .to_string();
        f.write("projects.json", &original);
        f.write(
            "node/projects.json",
            json!([{"name":"duplicate", "path":alias.path.join(".")}]).to_string(),
        );
        let aliases = load_aliases(&f.0).unwrap();
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].name, "first");
        assert_eq!(
            std::fs::read_to_string(f.0.join("projects.json")).unwrap(),
            original
        );
        assert_eq!(load_aliases(&f.0).unwrap().len(), 1);
    }

    #[test]
    fn data_and_node_registries_import_only_matching_projects_and_keep_existing() {
        let f = Fixture::new();
        let alias = f.alias();
        let id = workspace::Project::new(&alias.path).unwrap().id;
        let target = format!("rpc/projects/{id}/sessions.json");
        f.write(
            &target,
            json!({"sessions":[{"sessionId":"existing", "title":"native"}],
            "preferences":{"existing":{"pinned":true}}})
            .to_string(),
        );
        for (path, session) in [("sessions.json", "data"), ("node/sessions.json", "node")] {
            f.write(
                path,
                json!({"sessions":[
                {"sessionId":session,"projectId":id,"parentSessionId":"parent","relation":"fork"},
                {"sessionId":"existing","projectId":id,"title":"legacy"},
                {"sessionId":"foreign","projectId":"other"}, {"sessionId":"unscoped"}
            ],"preferences":{session:{"pinned":true,"archived":true,"tags":["kept"]}}})
                .to_string(),
            );
        }
        let sources: Vec<_> = ["sessions.json", "node/sessions.json"]
            .map(|p| std::fs::read(f.0.join(p)).unwrap())
            .into();
        migrate_registries(&f.0, std::slice::from_ref(&alias)).unwrap();
        let first = std::fs::read(f.0.join(&target)).unwrap();
        migrate_registries(&f.0, &[alias]).unwrap();
        assert_eq!(std::fs::read(f.0.join(&target)).unwrap(), first);
        let registry = Registry::load(f.0.join(target)).unwrap();
        assert_eq!(registry.entries().len(), 3);
        assert_eq!(
            registry.get("existing").unwrap().title.as_deref(),
            Some("native")
        );
        assert!(registry.preferences("existing").pinned);
        for session in ["data", "node"] {
            assert_eq!(
                registry.get(session).unwrap().parent_session_id.as_deref(),
                Some("parent")
            );
            let prefs = registry.preferences(session);
            assert!(prefs.pinned && prefs.archived);
            assert_eq!(prefs.tags, ["kept"]);
        }
        for (path, bytes) in ["sessions.json", "node/sessions.json"].iter().zip(sources) {
            assert_eq!(std::fs::read(f.0.join(path)).unwrap(), bytes);
        }
    }

    #[test]
    fn invalid_preferences_do_not_publish_partial_imports_and_can_be_retried() {
        let f = Fixture::new();
        let alias = f.alias();
        let id = workspace::Project::new(&alias.path).unwrap().id;
        let target = format!("rpc/projects/{id}/sessions.json");
        f.write(&target, b"{\"sessions\":[]}");
        let original = std::fs::read(f.0.join(&target)).unwrap();
        f.write(
            "sessions.json",
            json!({"sessions":[{"sessionId":"new","projectId":id}],
            "preferences":{"new":{"tags":["x".repeat(33)]}}})
            .to_string(),
        );
        assert!(migrate_registries(&f.0, std::slice::from_ref(&alias)).is_err());
        assert_eq!(std::fs::read(f.0.join(&target)).unwrap(), original);
        f.write(
            "sessions.json",
            json!({"sessions":[{"sessionId":"new","projectId":id}],
            "preferences":{"new":{"pinned":true}}})
            .to_string(),
        );
        migrate_registries(&f.0, &[alias]).unwrap();
        assert!(
            Registry::load(f.0.join(target))
                .unwrap()
                .preferences("new")
                .pinned
        );
    }

    #[test]
    fn corrupt_registries_and_directory_sources_fail_without_overwriting() {
        let f = Fixture::new();
        let alias = f.alias();
        let id = workspace::Project::new(&alias.path).unwrap().id;
        let target = format!("rpc/projects/{id}/sessions.json");
        f.write(
            "sessions.json",
            json!({"sessions":[{"sessionId":"new","projectId":id}]}).to_string(),
        );
        f.write(&target, b"{broken");
        assert!(migrate_registries(&f.0, std::slice::from_ref(&alias)).is_err());
        assert_eq!(std::fs::read(f.0.join(&target)).unwrap(), b"{broken");
        f.write(&target, b"{\"sessions\":[]}");
        for path in ["sessions.json", "node/sessions.json"] {
            f.write(path, b"{broken");
            assert!(migrate_registries(&f.0, std::slice::from_ref(&alias)).is_err());
            assert_eq!(std::fs::read(f.0.join(path)).unwrap(), b"{broken");
            f.write(path, b"{\"sessions\":[]}");
        }
        std::fs::remove_file(f.0.join("sessions.json")).unwrap();
        std::fs::create_dir(f.0.join("sessions.json")).unwrap();
        assert!(migrate_registries(&f.0, &[alias]).is_err());
    }
}
