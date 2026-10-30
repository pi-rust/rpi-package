//! Persist a native fork without exposing a live staging child to adapters.
//!
//! Native `--fork` allocates the authoritative UUID; `SessionOptions::session_id`
//! is not a requested fork identity. The caller can reopen `ForkedSession::file`
//! using `SessionOptions::saved_session` after this helper returns.
use crate::host::{HostSession, SessionOptions};
use std::path::{Path, PathBuf};

/// A closed, persisted native session in `root/<id>/<native cwd directory>/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkedSession {
    pub id: String,
    pub file: PathBuf,
}

/// Fork the exact file in `options.fork_source` into a caller-selected root.
///
/// The source must be a file path, not a native session search fragment. Project,
/// additional directories, model, provider and permission settings are forwarded
/// unchanged. Session selection, session directory and temporary timing storage
/// are overridden for staging. No prompt is submitted and the source is never
/// opened for writing by this helper.
///
/// The helper owns shutdown and cleanup even if its caller drops the future.
/// Existing destination directories (including empty ones) are never replaced.
pub async fn fork_saved_session(
    root: &Path,
    options: SessionOptions,
) -> Result<ForkedSession, String> {
    let root = root.to_path_buf();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    // Keep the resource owner alive independently of a cancelled adapter request.
    tokio::spawn(async move {
        match prepare(&root, options).await {
            Ok((fork, mut published)) => {
                if sender.send(Ok(fork)).is_ok() {
                    published.keep = true;
                }
            }
            Err(error) => {
                let _ = sender.send(Err(error));
            }
        }
    });
    receiver
        .await
        .map_err(|_| "native fork worker stopped".to_string())?
}

struct Directory {
    path: PathBuf,
    keep: bool,
}

impl Directory {
    fn create(path: PathBuf) -> Result<Self, String> {
        std::fs::create_dir(&path).map_err(|error| {
            format!("cannot reserve fork directory {}: {error}", path.display())
        })?;
        Ok(Self { path, keep: false })
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

async fn prepare(
    root: &Path,
    mut options: SessionOptions,
) -> Result<(ForkedSession, Directory), String> {
    if !root.is_absolute() {
        return Err("native fork root must be absolute".into());
    }
    let source = options
        .fork_source
        .as_deref()
        .ok_or("native fork requires a source file")?;
    let source = std::fs::canonicalize(source)
        .map_err(|error| format!("cannot resolve native fork source: {error}"))?;
    if !source.is_file() {
        return Err("native fork source must be a file".into());
    }
    std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let root = std::fs::canonicalize(root).map_err(|error| error.to_string())?;
    let stage = Directory::create(root.join(format!(".fork-{}", uuid::Uuid::new_v4())))?;
    let sessions = stage.path.join("sessions");
    // HostSession's fork timing lookup derives a path from fork_source. Keep
    // even that lookup (and any resulting timing writes) inside our stage,
    // rather than beside the original transcript. Native still owns the fork
    // operation and preserves the original header's parent session identity.
    let staged_source = stage.path.join("source.jsonl");
    std::fs::copy(&source, &staged_source)
        .map_err(|error| format!("cannot stage native fork source: {error}"))?;
    options.saved_session = None;
    options.fork_source = Some(staged_source.to_string_lossy().into_owned());
    options.session_dir = Some(sessions.clone());
    options.timings_dir = stage.path.join("timings");
    options.session_id = uuid::Uuid::new_v4().to_string();
    let host = HostSession::spawn_options(options).await?;
    let state = host.state().await;
    // Release native session locks and all writers before inspecting/moving files,
    // including when get_state fails.
    host.shutdown().await;
    let state = state?;
    let id = state["sessionId"]
        .as_str()
        .ok_or("native fork returned no session id")?;
    if uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.to_string())
        .as_deref()
        != Ok(id)
    {
        return Err("native fork returned a noncanonical UUID".into());
    }
    let file = crate::history::find_saved_session(&sessions, id)?;
    let relative = file
        .strip_prefix(&sessions)
        .map_err(|error| error.to_string())?;
    // Exclusive reservation rejects collisions without replacing an empty tree.
    // Both directories are on the same filesystem; retain native cwd/file names.
    let published = Directory::create(root.join(id))?;
    for entry in std::fs::read_dir(&sessions).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        std::fs::rename(entry.path(), published.path.join(entry.file_name()))
            .map_err(|error| format!("cannot publish native fork: {error}"))?;
    }
    let fork = ForkedSession {
        id: id.to_owned(),
        file: published.path.join(relative),
    };
    Ok((fork, published))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temporary() -> Directory {
        Directory::create(
            std::env::temp_dir().join(format!("rpi-fork-test-{}", uuid::Uuid::new_v4())),
        )
        .unwrap()
    }

    #[test]
    fn reservation_rejects_existing_directories_and_cleanup_is_scoped() {
        let root = temporary();
        let destination = root.path.join("destination");
        std::fs::create_dir(&destination).unwrap();
        assert!(Directory::create(destination.clone()).is_err());
        assert!(destination.is_dir());
        {
            let stage = Directory::create(root.path.join("stage")).unwrap();
            std::fs::write(stage.path.join("partial"), "partial fork").unwrap();
        }
        assert!(!root.path.join("stage").exists());
        assert!(destination.is_dir());
    }

    /// Run with RPI_FORK_TEST_BIN pointing to a real native rpi executable.
    /// Does not call a model or alter installed executables.
    #[tokio::test]
    #[ignore = "requires RPI_FORK_TEST_BIN native executable"]
    async fn native_fork_is_authoritative_persisted_and_reopenable() {
        let executable = std::env::var_os("RPI_FORK_TEST_BIN")
            .expect("set RPI_FORK_TEST_BIN to a native rpi executable");
        let root = temporary();
        let source_id = uuid::Uuid::new_v4().to_string();
        let source = root.path.join("source.jsonl");
        let bytes = format!(
            "{}\n",
            json!({
                "kind":"header", "version":4, "id":source_id,
                "createdAt":0, "cwd":root.path
            })
        );
        std::fs::write(&source, &bytes).unwrap();
        let supplied_id = uuid::Uuid::new_v4().to_string();
        let options = SessionOptions {
            executable: executable.into(),
            project: root.path.clone(),
            session_id: supplied_id.clone(),
            session_dir: None,
            saved_session: None,
            fork_source: Some(source.to_string_lossy().into_owned()),
            timings_dir: root.path.join("unused-timings"),
            model: None,
            provider: None,
            additional_directories: vec![],
            permission_gate: true,
            bypass: false,
        };
        let sessions = root.path.join("persisted");
        let fork = fork_saved_session(&sessions, options.clone())
            .await
            .unwrap();
        assert_ne!(fork.id, supplied_id);
        assert_ne!(fork.id, source_id);
        assert_eq!(std::fs::read(&source).unwrap(), bytes.as_bytes());
        assert!(!options.timings_dir.exists());
        assert_eq!(
            std::fs::canonicalize(
                crate::history::find_saved_session(&sessions.join(&fork.id), &fork.id).unwrap()
            )
            .unwrap(),
            fork.file
        );
        let text = std::fs::read_to_string(&fork.file).unwrap();
        let header: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(header["id"], fork.id);
        assert_eq!(header["parentSessionId"], source_id);
        let mut reopen = options.clone();
        reopen.fork_source = None;
        reopen.session_id = fork.id.clone();
        reopen.saved_session = Some(fork.file.clone());
        reopen.session_dir = Some(sessions.join(&fork.id));
        reopen.timings_dir = root.path.join("reopen-timings");
        let host = HostSession::spawn_options(reopen).await.unwrap();
        let state = host.state().await;
        host.shutdown().await;
        assert_eq!(state.unwrap()["sessionId"], fork.id);
        assert_eq!(std::fs::read(&source).unwrap(), bytes.as_bytes());

        // Native startup failure must also discard staging, not just success.
        let invalid = root.path.join("invalid.jsonl");
        std::fs::write(&invalid, "not a native transcript\n").unwrap();
        let mut failed = options;
        failed.fork_source = Some(invalid.to_string_lossy().into_owned());
        assert!(fork_saved_session(&sessions, failed).await.is_err());
        let names: Vec<_> = std::fs::read_dir(&sessions)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from(&fork.id)]);
    }
}
