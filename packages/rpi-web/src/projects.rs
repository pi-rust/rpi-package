//! Persistent project catalog. Browser-created projects stay inside the managed root.
use crate::workspace::Project;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
struct SavedProject {
    name: String,
    path: PathBuf,
}

pub struct Projects {
    pub entries: Vec<Project>,
    pub root: PathBuf,
    file: PathBuf,
    saved: Vec<SavedProject>,
}

impl Projects {
    pub fn load(configured: Vec<Project>, root: PathBuf, file: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&root)
            .map_err(|e| format!("cannot create projects directory: {e}"))?;
        let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
        let saved: Vec<SavedProject> = match std::fs::read(&file) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("cannot read project catalog: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("cannot read project catalog: {e}")),
        };
        let mut entries = configured;
        for item in &saved {
            if let Ok(mut project) = Project::new(&item.path) {
                if project.root.starts_with(&root) && !entries.iter().any(|p| p.id == project.id) {
                    project.name = item.name.clone();
                    entries.push(project);
                }
            }
        }
        Ok(Self {
            entries,
            root,
            file,
            saved,
        })
    }

    pub async fn add(
        &mut self,
        name: &str,
        existing: Option<&str>,
        init_git: bool,
    ) -> Result<Project, String> {
        let name = project_name(name)?;
        let path = if let Some(existing) = existing {
            let path = Path::new(existing);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                self.root.join(path)
            };
            let canonical =
                std::fs::canonicalize(&path).map_err(|_| "directory does not exist".to_string())?;
            if !canonical.starts_with(&self.root) || canonical == self.root {
                return Err("choose a folder inside the projects directory".into());
            }
            canonical
        } else {
            let target = self.root.join(&name);
            std::fs::create_dir(&target).map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    "a folder with this name already exists; use Open folder".into()
                } else {
                    format!("cannot create project: {e}")
                }
            })?;
            target
        };
        let mut project = Project::new(&path).map_err(|e| e.to_string())?;
        if self.entries.iter().any(|p| p.id == project.id) {
            return Err("this project is already in the workspace".into());
        }
        if existing.is_none() && init_git {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                tokio::process::Command::new("git")
                    .args(["-c", "core.fsmonitor=false", "init", "--quiet"])
                    .arg(&path)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .status(),
            )
            .await;
            if !matches!(result,Ok(Ok(status)) if status.success()) {
                return Err("project folder was created, but Git initialization failed; open the folder or retry without Git".into());
            }
        }
        project.name = name;
        self.saved.push(SavedProject {
            name: project.name.clone(),
            path: project.root.clone(),
        });
        if let Err(error) = self.save() {
            self.saved.pop();
            return Err(error);
        }
        self.entries.push(project.clone());
        Ok(project)
    }

    fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let temp = self.file.with_extension("json.tmp");
        std::fs::write(
            &temp,
            serde_json::to_vec_pretty(&self.saved).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(temp, &self.file).map_err(|e| format!("cannot save project catalog: {e}"))
    }
}

fn project_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > 80
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        || name.ends_with(['.', ' '])
        || name.starts_with('.')
        || Path::new(name)
            .components()
            .any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err(
            "use 1–80 characters without path separators or reserved filename characters".into(),
        );
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ]
    .contains(&stem.as_str())
    {
        return Err("this name is reserved by the operating system".into());
    }
    Ok(name.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn catalog_write_failure_keeps_existing_projects_and_can_be_recovered() {
        let temp =
            std::env::temp_dir().join(format!("rpi-project-failure-{}", uuid::Uuid::new_v4()));
        let root = temp.join("projects");
        let file = temp.join("projects.json");
        let mut store = Projects::load(Vec::new(), root.clone(), file.clone()).unwrap();
        let first = store.add("first", None, false).await.unwrap();
        let original = std::fs::read(&file).unwrap();
        let blocked = file.with_extension("json.tmp");
        std::fs::create_dir(&blocked).unwrap();
        assert!(store.add("second", None, false).await.is_err());
        assert_eq!(store.entries.len(), 1);
        assert_eq!(store.saved.len(), 1);
        assert_eq!(std::fs::read(&file).unwrap(), original);
        assert!(root.join("second").is_dir());
        std::fs::remove_dir(blocked).unwrap();
        let recovered = store.add("second", Some("second"), false).await.unwrap();
        let loaded = Projects::load(Vec::new(), root, file.clone()).unwrap();
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.entries[0].id, first.id);
        assert_eq!(loaded.entries[1].id, recovered.id);
        std::fs::write(&file, b"broken JSON").unwrap();
        assert!(Projects::load(Vec::new(), temp.join("projects"), file).is_err());
        std::fs::remove_dir_all(temp).unwrap();
    }
    #[tokio::test]
    async fn new_projects_persist_and_cannot_escape_the_managed_root() {
        let temp = std::env::temp_dir().join(format!("rpi-projects-{}", uuid::Uuid::new_v4()));
        let root = temp.join("projects");
        let file = temp.join("projects.json");
        let mut store = Projects::load(Vec::new(), root.clone(), file.clone()).unwrap();
        let project = store.add("我的项目", None, true).await.unwrap();
        assert!(project.root.join(".git").is_dir());
        assert!(store.add("我的项目", None, false).await.is_err());
        assert!(store
            .add("escape", Some(temp.to_str().unwrap()), false)
            .await
            .is_err());
        for bad in ["../escape", ".", "a/b", "CON", "NUL.txt", "a\\b"] {
            assert!(store.add(bad, None, false).await.is_err());
        }
        let loaded = Projects::load(Vec::new(), root, file).unwrap();
        assert_eq!(loaded.entries[0].id, project.id);
        std::fs::remove_dir_all(temp).unwrap();
    }
}
