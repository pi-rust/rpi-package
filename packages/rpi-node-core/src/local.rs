//! RPC-only local runtime configuration and workspace ownership.
//!
//! This module never launches a process or configures an HTTP service. The native
//! host must retain `LocalRuntime` until all native children owned by the core
//! client have shut down. Dropping it earlier releases the workspace lock while
//! those children may still be using the data directory.

use crate::desktop::find_runtime;
use fs2::FileExt;
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    path::PathBuf,
};

const MISSING_RPI: &str =
    "未找到 rpi 执行宿主。请安装 rpi 或将其放在 RPI Work 可执行文件旁，然后重新启动。";

/// Resolved native runtime paths and an exclusive application-data lock.
///
/// This guard owns only the lock, not native children. Its owner must shut down
/// and await all client-owned children **before** dropping this value. The client
/// may append `rpc` to `data`; the shared lock remains at `data/node.lock`.
#[derive(Debug)]
pub struct LocalRuntime {
    pub rpi: PathBuf,
    pub data: PathBuf,
    _lock: File,
}

impl LocalRuntime {
    /// Resolve the application sibling/PATH runtime and platform data directory.
    /// `RPI_WORK_RPI_BIN`, when present, is authoritative even if missing.
    pub fn prepare() -> Result<Self, String> {
        Self::prepare_with(None, None)
    }

    /// Resolve explicit extension-launch paths without mutating process environment.
    #[allow(clippy::needless_pass_by_value)] // Own the launch configuration at the host boundary.
    pub fn prepare_with(
        rpi_override: Option<PathBuf>,
        data_override: Option<PathBuf>,
    ) -> Result<Self, String> {
        let application = std::env::current_exe()
            .map_err(|_| "无法定位应用目录。")?
            .parent()
            .ok_or("无法定位应用目录。")?
            .to_owned();
        let rpi = find_runtime(
            "rpi",
            rpi_override.or_else(|| std::env::var_os("RPI_WORK_RPI_BIN").map(PathBuf::from)),
            &application,
        )
        .ok_or(MISSING_RPI)?;
        let data = data_directory(|name| {
            if name == "RPI_WORK_DATA_DIR" {
                data_override
                    .as_ref()
                    .map(|p| p.as_os_str().to_owned())
                    .or_else(|| std::env::var_os(name))
            } else {
                std::env::var_os(name)
            }
        })?;
        Self::from_paths(rpi, data)
    }

    /// Validate an already-resolved runtime and lock the supplied data directory.
    /// No process is started. A missing runtime never falls back to PATH.
    pub fn from_paths(rpi: PathBuf, data: PathBuf) -> Result<Self, String> {
        if !rpi.is_file() {
            return Err(MISSING_RPI.into());
        }
        std::fs::create_dir_all(&data).map_err(|_| "无法创建本机节点数据目录。")?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(data.join("node.lock"))
            .map_err(|_| "无法打开本机节点锁。")?;
        lock.try_lock_exclusive()
            .map_err(|_| "本机节点已由另一个 RPI Work 宿主使用（桌面或无头进程）；请返回原窗口，或用 --connect <节点地址> 连接并独立登录。")?;
        Ok(Self {
            rpi,
            data,
            _lock: lock,
        })
    }
}

// Preserve the desktop's existing home requirement, including with a data override.
fn data_directory(
    mut environment: impl FnMut(&str) -> Option<OsString>,
) -> Result<PathBuf, String> {
    let home = environment(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or("无法定位用户目录。")?;
    if let Some(data) = environment("RPI_WORK_DATA_DIR") {
        return Ok(data.into());
    }
    #[cfg(target_os = "windows")]
    let root = environment("LOCALAPPDATA").map_or(home, PathBuf::from);
    #[cfg(target_os = "macos")]
    let root = home.join("Library/Application Support");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let root =
        environment("XDG_DATA_HOME").map_or_else(|| home.join(".local/share"), PathBuf::from);
    Ok(root.join("rpi-work/node"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::find_runtime_on_path;
    use std::path::Path;

    fn binary(directory: &Path) -> PathBuf {
        std::fs::create_dir_all(directory).unwrap();
        let path = directory.join(format!("rpi{}", std::env::consts::EXE_SUFFIX));
        // A non-executable fixture also proves preparation never launches it.
        std::fs::write(&path, "fixture").unwrap();
        path
    }

    #[test]
    fn runtime_precedence_override_then_sibling_then_path() {
        let temp = tempfile::tempdir().unwrap();
        let application = temp.path().join("app");
        let sibling = binary(&application);
        let explicit = binary(&temp.path().join("override"));
        let first = binary(&temp.path().join("path-first"));
        let second = binary(&temp.path().join("path-second"));
        let search =
            std::env::join_paths([first.parent().unwrap(), second.parent().unwrap()]).unwrap();
        let resolve = |override_path| {
            find_runtime_on_path("rpi", override_path, &application, Some(search.clone()))
        };
        assert_eq!(resolve(Some(explicit.clone())), Some(explicit));
        assert_eq!(resolve(None), Some(sibling.clone()));
        assert_eq!(resolve(Some(temp.path().join("missing"))), None);
        std::fs::remove_file(sibling).unwrap();
        assert_eq!(resolve(None), Some(first.clone()));
        std::fs::remove_file(first).unwrap();
        assert_eq!(resolve(None), Some(second.clone()));
        std::fs::remove_file(second).unwrap();
        assert_eq!(resolve(None), None);
    }

    #[test]
    fn missing_runtime_does_not_create_data() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let error =
            LocalRuntime::from_paths(temp.path().join("missing"), data.clone()).unwrap_err();
        assert_eq!(error, MISSING_RPI);
        assert!(!data.exists());
        assert!(LocalRuntime::from_paths(temp.path().to_owned(), data).is_err());
    }

    #[test]
    fn lock_is_exclusive_and_released_on_drop() {
        let temp = tempfile::tempdir().unwrap();
        let rpi = binary(temp.path());
        let data = temp.path().join("data");
        let runtime = LocalRuntime::from_paths(rpi.clone(), data.clone()).unwrap();
        assert_eq!(runtime.rpi, rpi);
        assert_eq!(runtime.data, data);
        assert!(data.join("node.lock").is_file());
        assert!(!data.join("rpc").exists());
        let error = LocalRuntime::from_paths(rpi.clone(), data.clone()).unwrap_err();
        assert!(error.contains("另一个 RPI Work 宿主"));
        // Compatibility mode and RPC use the very same lock file.
        let competing = OpenOptions::new()
            .read(true)
            .write(true)
            .open(data.join("node.lock"))
            .unwrap();
        assert!(competing.try_lock_exclusive().is_err());
        drop(competing);
        drop(runtime);
        let restarted = LocalRuntime::from_paths(rpi, data).unwrap();
        drop(restarted);
    }

    #[test]
    fn data_resolution_matches_desktop_without_global_environment_changes() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let platform_root = temp.path().join("platform-data");
        let custom = temp.path().join("custom");
        let home_variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let resolve = |override_data: bool, platform_data: bool| {
            data_directory(|name| match name {
                name if name == home_variable => Some(home.clone().into_os_string()),
                "RPI_WORK_DATA_DIR" if override_data => Some(custom.clone().into_os_string()),
                "LOCALAPPDATA" | "XDG_DATA_HOME" if platform_data => {
                    Some(platform_root.clone().into_os_string())
                }
                _ => None,
            })
            .unwrap()
        };
        #[cfg(target_os = "windows")]
        let fallback = home.clone();
        #[cfg(target_os = "macos")]
        let fallback = home.join("Library/Application Support");
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let fallback = home.join(".local/share");
        assert_eq!(resolve(false, false), fallback.join("rpi-work/node"));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(resolve(false, true), platform_root.join("rpi-work/node"));
        #[cfg(target_os = "macos")]
        assert_eq!(resolve(false, true), fallback.join("rpi-work/node"));
        assert_eq!(resolve(true, true), custom);
        assert_eq!(resolve(true, false), custom);
        let error = data_directory(|name| {
            (name == "RPI_WORK_DATA_DIR").then(|| custom.clone().into_os_string())
        })
        .unwrap_err();
        assert_eq!(error, "无法定位用户目录。");
    }
}
