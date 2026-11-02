//! Retain the existing native writer lock through the registry decision. This
//! is observational, noncreating and immediate, never a writer authority.
use super::{ResourceError, ResourceErrorKind, ResourceResult};
use rpi_mcp_core::SharedFileLock;
use std::path::{Path, PathBuf};
pub(crate) struct ObservationLock {
    _lease: SharedFileLock,
}
impl ObservationLock {
    pub(crate) fn acquire(root: &Path, history: &Path) -> ResourceResult<Self> {
        let path = PathBuf::from(format!("{}.lock", history.display()));
        let canonical = std::fs::canonicalize(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ResourceError::new(
                    ResourceErrorKind::Unsupported,
                    "native observation lock absent; deliberate original open/close required",
                )
            } else {
                ResourceError::unavailable(e)
            }
        })?;
        if canonical != path
            || !canonical.starts_with(root)
            || !std::fs::metadata(&canonical)
                .map_err(ResourceError::unavailable)?
                .is_file()
        {
            return Err(ResourceError::conflict(
                "native observation lock is not canonical and contained",
            ));
        }
        let lease = SharedFileLock::try_open(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::WouldBlock => {
                ResourceError::conflict("native saved session has an active writer")
            }
            std::io::ErrorKind::Unsupported => ResourceError::new(
                ResourceErrorKind::Unsupported,
                "platform has no compatible native observation lock",
            ),
            _ => ResourceError::unavailable(e),
        })?;
        Ok(Self { _lease: lease })
    }
}
