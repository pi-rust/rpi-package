//! Owner-resolved bounded location followed by the negotiated canonical native
//! reader. This module never decodes a transcript branch itself.
use crate::resources::*;
#[cfg(test)]
mod tests;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
fn locate(root: &Path, id: &str) -> ResourceResult<PathBuf> {
    let root = std::fs::canonicalize(root).map_err(ResourceError::unavailable)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut entries = 0usize;
    let mut directories = 0usize;
    let mut found = None;
    let mut inspect = |entry: std::fs::DirEntry| -> ResourceResult<()> {
        entries += 1;
        if entries > 32768 || Instant::now() >= deadline {
            return Err(ResourceError::new(
                ResourceErrorKind::LimitExceeded,
                "saved session discovery budget exceeded",
            ));
        }
        if !entry
            .file_name()
            .to_string_lossy()
            .ends_with(&format!("_{id}.jsonl"))
        {
            return Ok(());
        }
        if !entry
            .file_type()
            .map_err(ResourceError::unavailable)?
            .is_file()
        {
            return Err(ResourceError::conflict(
                "saved session candidate is not a regular native file",
            ));
        }
        let path = std::fs::canonicalize(entry.path()).map_err(ResourceError::unavailable)?;
        if !path.starts_with(&root) || path == root {
            return Err(ResourceError::conflict(
                "saved native session escaped its owner root",
            ));
        }
        super::read_saved_header(&path, id, super::MAX_METADATA_HEADER_BYTES)
            .map_err(ResourceError::unavailable)?;
        if found.replace(path).is_some() {
            return Err(ResourceError::conflict(
                "ambiguous saved native session identity",
            ));
        }
        Ok(())
    };
    for entry in std::fs::read_dir(&root).map_err(ResourceError::unavailable)? {
        let entry = entry.map_err(ResourceError::unavailable)?;
        let kind = entry.file_type().map_err(ResourceError::unavailable)?;
        if kind.is_dir() {
            directories += 1;
            if directories > 256 || Instant::now() >= deadline {
                return Err(ResourceError::new(
                    ResourceErrorKind::LimitExceeded,
                    "saved directory discovery budget exceeded",
                ));
            }
            let path = std::fs::canonicalize(entry.path()).map_err(ResourceError::unavailable)?;
            if !path.starts_with(&root) || path == root {
                return Err(ResourceError::conflict(
                    "saved directory escaped native root",
                ));
            }
            for file in std::fs::read_dir(path).map_err(ResourceError::unavailable)? {
                inspect(file.map_err(ResourceError::unavailable)?)?;
            }
        } else {
            inspect(entry)?;
        }
    }
    found.ok_or_else(|| ResourceError::conflict("no owner-resolved saved native session"))
}
pub(crate) async fn read(
    bin: PathBuf,
    project: PathBuf,
    configured: Option<PathBuf>,
    id: String,
    query: ResourceHistoryRequest,
) -> ResourceResult<crate::resources::saved::SavedReadLease> {
    query.validate()?;
    let resolve_id = id.clone();
    let resolve_project = project.clone();
    let (root, path) = tokio::task::spawn_blocking(move || {
        let root = crate::run_timing::session_root(&resolve_project, configured).map_err(
            |(_, body)| {
                ResourceError::unavailable(
                    body["error"]
                        .as_str()
                        .unwrap_or("cannot resolve native session root"),
                )
            },
        )?;
        let root = std::fs::canonicalize(root).map_err(ResourceError::unavailable)?;
        let path = locate(&root, &resolve_id)?;
        Ok::<_, ResourceError>((root, path))
    })
    .await
    .map_err(|_| ResourceError::unavailable("saved location owner failed"))??;
    crate::resources::saved::read(bin, project, root, path, id, query).await
}
