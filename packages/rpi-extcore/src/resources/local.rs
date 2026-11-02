//! Trusted local native CLI facade. No caller argv, environment, builds or ABI loads.
use super::*;
#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
use std::{
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceKind {
    Package,
    Skill,
    Extension,
}
impl ResourceKind {
    fn argument(self) -> &'static str {
        match self {
            Self::Package => "package",
            Self::Skill => "skill",
            Self::Extension => "extension",
        }
    }
}
#[derive(Clone, Debug)]
pub enum LocalResourceOperation {
    List,
    Inspect {
        kind: ResourceKind,
        name: String,
    },
    Install {
        kind: ResourceKind,
        name: String,
        source: PathBuf,
        version: Option<String>,
    },
    Update {
        kind: ResourceKind,
        name: String,
        source: PathBuf,
        version: Option<String>,
    },
    Reinstall {
        kind: ResourceKind,
        name: String,
        source: PathBuf,
        version: Option<String>,
    },
    Remove {
        kind: ResourceKind,
        name: String,
    },
    Recover,
}
#[derive(Clone, Debug)]
pub struct LocalResourceRequest {
    pub project_id: String,
    pub scope: ResourceScope,
    pub operation: LocalResourceOperation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalResourceResult {
    pub success: bool,
    pub observation: Value,
    pub diagnostics: String,
}
#[derive(Clone, Debug)]
pub struct ResourceCapabilities {
    pub profile: Value,
}
fn bounded_read(mut pipe: impl Read, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    pipe.by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("native CLI output limit exceeded".into());
    }
    Ok(bytes)
}
struct CaptureSpool {
    path: PathBuf,
    file: std::fs::File,
}
impl CaptureSpool {
    fn create() -> ResourceResult<Self> {
        let path =
            std::env::temp_dir().join(format!("rpi-resource-capture-{}.tmp", uuid::Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .map_err(|e| ResourceError::unavailable(e.to_string()))?;
        Ok(Self { path, file })
    }
    fn check(&self, limit: usize) -> ResourceResult<()> {
        if self
            .file
            .metadata()
            .map_err(|e| ResourceError::unavailable(e.to_string()))?
            .len()
            > limit as u64
        {
            return Err(ResourceError::new(
                ResourceErrorKind::LimitExceeded,
                "native CLI output limit exceeded",
            ));
        }
        Ok(())
    }
    fn read(&mut self, limit: usize) -> ResourceResult<Vec<u8>> {
        use std::io::{Seek, SeekFrom};
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| ResourceError::unavailable(e.to_string()))?;
        bounded_read(&mut self.file, limit).map_err(ResourceError::unavailable)
    }
}
impl Drop for CaptureSpool {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
pub(super) fn invoke(
    bin: &Path,
    cwd: &Path,
    args: &[OsString],
    limit: usize,
    budget: Duration,
) -> ResourceResult<(bool, Vec<u8>, Vec<u8>)> {
    let mut stdout = CaptureSpool::create()?;
    let mut stderr = CaptureSpool::create()?;
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(
            stdout
                .file
                .try_clone()
                .map_err(|e| ResourceError::unavailable(e.to_string()))?,
        )
        .stderr(
            stderr
                .file
                .try_clone()
                .map_err(|e| ResourceError::unavailable(e.to_string()))?,
        );
    let mut tree = rpi_mcp_core::ProcessTree::spawn(&mut cmd)
        .map_err(|e| ResourceError::unavailable(format!("native CLI spawn: {e}")))?;
    let deadline = Instant::now() + budget;
    let status = loop {
        stdout.check(limit)?;
        stderr.check(16 * 1024)?;
        if let Some(status) = tree
            .child_mut()
            .try_wait()
            .map_err(|e| ResourceError::unavailable(e.to_string()))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            return Err(ResourceError::unavailable("native CLI deadline exceeded"));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    tree.stop();
    // Regular files have finite reads; no pipe readers/joins can be retained by
    // a descendant. Recheck after contained-tree exit before bounded readback.
    stdout.check(limit)?;
    stderr.check(16 * 1024)?;
    Ok((
        status.success(),
        stdout.read(limit)?,
        stderr.read(16 * 1024)?,
    ))
}
pub(super) fn negotiate_sync(bin: &Path, cwd: &Path) -> ResourceResult<ResourceCapabilities> {
    negotiate_with(|args, limit| invoke(bin, cwd, args, limit, Duration::from_secs(5)))
}
fn negotiate_with(
    mut run: impl FnMut(&[OsString], usize) -> ResourceResult<(bool, Vec<u8>, Vec<u8>)>,
) -> ResourceResult<ResourceCapabilities> {
    let (ok, help, _) = run(&["--help".into()], 64 * 1024)?;
    if !ok
        || !String::from_utf8_lossy(&help)
            .split_whitespace()
            .any(|s| s == "--resource-capabilities")
    {
        return Err(ResourceError::new(
            ResourceErrorKind::Unsupported,
            "native CLI lacks safe resource capability query",
        ));
    }
    let (ok, raw, _) = run(&["--resource-capabilities".into()], 16 * 1024)?;
    let profile: Value = serde_json::from_slice(&raw).map_err(|_| {
        ResourceError::new(
            ResourceErrorKind::Unsupported,
            "invalid native resource profile",
        )
    })?;
    if !ok
        || profile["version"] != 1
        || profile["type"] != "resource_capabilities"
        || profile["loadedFacts"] != false
    {
        return Err(ResourceError::new(
            ResourceErrorKind::Unsupported,
            "unsupported native resource profile",
        ));
    }
    Ok(ResourceCapabilities { profile })
}
impl ResourceCapabilities {
    pub fn require_saved_read(&self) -> ResourceResult<()> {
        let p = &self.profile["savedSessionRead"];
        if !p["versions"]
            .as_array()
            .is_some_and(|a| a.contains(&serde_json::json!(1)))
            || p["flag"] != "--resource-session-read"
            || p["maxRequestBytes"] != MAX_CONFIG_BYTES
            || p["maxResponseBytes"] != 8 * 1024 * 1024
            || p["maxReadBytes"] != 64 * 1024 * 1024
            || p["maxMessages"] != 128
            || p["maxCustomEntries"] != 1024
            || p["maxCustomBytes"] != 2 * 1024 * 1024
            || p["deadlineMs"] != 10000
            || p["nonexecuting"] != true
            || p["noncreatingObservationLock"] != true
            || p["strict"] != true
        {
            return Err(ResourceError::new(
                ResourceErrorKind::Unsupported,
                "native lacks bounded nonexecuting savedSessionRead v1",
            ));
        }
        Ok(())
    }
    fn require_management(
        &self,
        request: &LocalResourceRequest,
        args: &[OsString],
    ) -> ResourceResult<()> {
        let p = &self.profile["management"];
        let supports = |key: &str, value: &str| {
            p[key]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(value)))
        };
        let op = args[1].to_string_lossy();
        let kind = if matches!(
            request.operation,
            LocalResourceOperation::List | LocalResourceOperation::Recover
        ) {
            None
        } else {
            Some(args[2].to_string_lossy())
        };
        if p["version"] != 1
            || p["command"] != "resource"
            || !supports("operations", &op)
            || !supports("scopes", request.scope.argument())
            || kind.is_some_and(|k| !supports("kinds", &k))
            || p["explicitScopeRequired"] != true
            || p["build"] != false
            || p["network"] != false
        {
            return Err(ResourceError::new(
                ResourceErrorKind::Unsupported,
                "native management kind/scope/operation unsupported",
            ));
        }
        Ok(())
    }
    pub fn require_exact(&self, transition: bool) -> ResourceResult<()> {
        let p = &self.profile;
        if !p["exactSelection"]["versions"]
            .as_array()
            .is_some_and(|a| a.contains(&serde_json::json!(1)))
            || p["exactSelection"]["startupFlag"] != "--resource-config"
            || p["exactSelection"]["stateCommand"] != "get_resource_state"
            || p["exactSelection"]["mode"] != "rpc"
            || p["exactSelection"]["maxConfigBytes"] != MAX_CONFIG_BYTES
            || p["exactSelection"]["maxSnapshotBytes"] != MAX_STATE_BYTES
            || !["default", "only", "none"].iter().all(|mode| {
                p["exactSelection"]["toolModes"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(mode)))
            })
            || (transition
                && (p["transition"]["version"] != 1
                    || p["transition"]["startupFlag"] != "--resource-transition"
                    || p["transition"]["maxRequestBytes"] != MAX_CONFIG_BYTES))
        {
            return Err(ResourceError::new(
                ResourceErrorKind::Unsupported,
                "native exact selection/transition unsupported",
            ));
        }
        Ok(())
    }
}
pub async fn capabilities(bin: PathBuf, cwd: PathBuf) -> ResourceResult<ResourceCapabilities> {
    tokio::task::spawn_blocking(move || negotiate_sync(&bin, &cwd))
        .await
        .map_err(|_| ResourceError::unavailable("resource probe worker failed"))?
}
fn arguments(request: &LocalResourceRequest) -> ResourceResult<Vec<OsString>> {
    use LocalResourceOperation::*;
    let (op, identity, source) = match &request.operation {
        List => ("list", None, None),
        Recover => ("recover", None, None),
        Inspect { kind, name } => ("inspect", Some((*kind, name)), None),
        Remove { kind, name } => ("remove", Some((*kind, name)), None),
        Install {
            kind,
            name,
            source,
            version,
        } => ("install", Some((*kind, name)), Some((source, version))),
        Update {
            kind,
            name,
            source,
            version,
        } => ("update", Some((*kind, name)), Some((source, version))),
        Reinstall {
            kind,
            name,
            source,
            version,
        } => ("reinstall", Some((*kind, name)), Some((source, version))),
    };
    let mut args: Vec<OsString> = vec!["resource".into(), op.into()];
    if let Some((kind, name)) = identity {
        if name.is_empty()
            || name.len() > 96
            || name.starts_with('.')
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            || name.starts_with('-')
        {
            return Err(ResourceError::new(
                ResourceErrorKind::InvalidInput,
                "invalid native resource name",
            ));
        }
        args.extend([kind.argument().into(), name.into()]);
    }
    if let Some((source, version)) = source {
        if !source.is_absolute() {
            return Err(ResourceError::new(
                ResourceErrorKind::InvalidInput,
                "authorized resource source must be absolute",
            ));
        }
        args.extend(["--path".into(), source.as_os_str().into()]);
        if let Some(version) = version {
            if version.len() > 128
                || version.chars().any(char::is_control)
                || version.starts_with('-')
            {
                return Err(ResourceError::new(
                    ResourceErrorKind::InvalidInput,
                    "invalid resource version",
                ));
            }
            args.extend(["--version".into(), version.into()]);
        }
    }
    args.extend(["--scope".into(), request.scope.argument().into()]);
    Ok(args)
}
fn parse_envelope(
    ok: bool,
    raw: &[u8],
    stderr: &[u8],
    request: &LocalResourceRequest,
    args: &[OsString],
) -> ResourceResult<LocalResourceResult> {
    let observation: Value = serde_json::from_slice(raw)
        .map_err(|_| ResourceError::unavailable("invalid resource CLI JSON"))?;
    let success = observation["success"]
        .as_bool()
        .ok_or_else(|| ResourceError::unavailable("missing resource CLI outcome"))?;
    if observation["version"] != 1 || success != ok {
        return Err(ResourceError::unavailable("invalid resource CLI envelope"));
    }
    let op = args[1].to_string_lossy();
    let identity = !matches!(
        request.operation,
        LocalResourceOperation::List | LocalResourceOperation::Recover
    );
    for (key, expected) in [
        ("operation", Some(op.as_ref())),
        ("scope", Some(request.scope.argument())),
        ("kind", identity.then(|| args[2].to_str().unwrap_or(""))),
        ("name", identity.then(|| args[3].to_str().unwrap_or(""))),
    ] {
        let actual = &observation[key];
        // Native parser failures may have no parsed context. Never infer an
        // applied receipt from these valid bounded failure diagnostics.
        if !success && actual.is_null() {
            continue;
        }
        if actual.as_str() != expected {
            return Err(ResourceError::unavailable(
                "resource CLI request context differs",
            ));
        }
    }
    if !success && !observation["error"].is_string() {
        return Err(ResourceError::unavailable(
            "missing native failure diagnostic",
        ));
    }
    Ok(LocalResourceResult {
        success,
        observation,
        diagnostics: String::from_utf8_lossy(stderr).into_owned(),
    })
}

pub(crate) async fn execute(
    bin: PathBuf,
    cwd: PathBuf,
    request: LocalResourceRequest,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> ResourceResult<LocalResourceResult> {
    let args = arguments(&request)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let caps = negotiate_sync(&bin, &cwd)?;
        caps.require_management(&request, &args)?;
        let (ok, raw, stderr) =
            invoke(&bin, &cwd, &args, MAX_STATE_BYTES, Duration::from_secs(30))?;
        parse_envelope(ok, &raw, &stderr, &request, &args)
    })
    .await
    .map_err(|_| ResourceError::unavailable("resource CLI worker failed"))?
}
