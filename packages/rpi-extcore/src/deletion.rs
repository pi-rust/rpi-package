//! Exact, retryable native-history retirement. The caller must durably persist the
//! plan after confirmed native close. For a new deletion, `prepare_locked` owns
//! the native lock across durable persistence and `execute_prepared`; only an
//! existing durable intent may use `execute`. No discovery result alone authorizes deletion.
//!
//! This is defensive against accidental replacement, not a hostile-filesystem
//! sandbox: metadata/open/unlink are separate syscalls. Windows uses timestamps
//! and length rather than a native file index. A registry is trusted storage, not
//! a signed capability: its configured root bindings must remain trusted too.
use crate::reply::{ReplyKind, ServiceError, ServiceResult};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashSet, VecDeque},
    fs::{self, File, Metadata, OpenOptions},
    io::Read,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

const MAX_DIR_ENTRIES: usize = 10_000;
const SCAN_TIME: Duration = Duration::from_secs(5);
const HEADER_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeletionPlan {
    version: u8,
    id: String,
    root: DirectoryAnchor,
    timings: DirectoryAnchor,
    // Relative native path; all other paths are derived, never accepted verbatim.
    primary: Option<PathBuf>,
    files: Vec<PlannedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryAnchor {
    path: PathBuf,
    identity: Option<DirectoryIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryIdentity {
    created: Option<SystemTime>,
    dev: Option<u64>,
    ino: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fingerprint {
    len: u64,
    modified: Option<SystemTime>,
    identity: DirectoryIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FileKind {
    Primary,
    PrimaryTmp,
    Progress,
    ProgressTmp,
    Timing,
    TimingTmp,
}
const KINDS: [FileKind; 6] = [
    FileKind::Primary,
    FileKind::PrimaryTmp,
    FileKind::Progress,
    FileKind::ProgressTmp,
    FileKind::Timing,
    FileKind::TimingTmp,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlannedFile {
    kind: FileKind,
    fingerprint: Fingerprint,
}

fn error(kind: ReplyKind, code: &str, message: &str) -> ServiceError {
    (kind, json!({"code":code,"error":message}))
}
fn unsafe_path() -> ServiceError {
    error(
        ReplyKind::AccessDenied,
        "deletion_unsafe_path",
        "Unsafe history deletion path",
    )
}
fn unavailable() -> ServiceError {
    error(
        ReplyKind::Unavailable,
        "deletion_io",
        "History deletion I/O failed",
    )
}
fn changed() -> ServiceError {
    error(
        ReplyKind::Conflict,
        "deletion_identity_changed",
        "History deletion identity changed",
    )
}
fn unknown() -> ServiceError {
    error(
        ReplyKind::Conflict,
        "deletion_unknown_history",
        "Unrecognized native history layout or format",
    )
}
fn bound() -> ServiceError {
    error(
        ReplyKind::LimitExceeded,
        "deletion_scan_limit",
        "History discovery budget exceeded",
    )
}
fn canonical_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.hyphenated().to_string() == id)
}
fn identity(metadata: &Metadata) -> DirectoryIdentity {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    DirectoryIdentity {
        created: metadata.created().ok(),
        #[cfg(unix)]
        dev: Some(metadata.dev()),
        #[cfg(not(unix))]
        dev: None,
        #[cfg(unix)]
        ino: Some(metadata.ino()),
        #[cfg(not(unix))]
        ino: None,
    }
}
fn fingerprint(metadata: &Metadata) -> Fingerprint {
    Fingerprint {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        identity: identity(metadata),
    }
}
fn reparse(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}
fn metadata(path: &Path) -> ServiceResult<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(value) if reparse(&value) => Err(unsafe_path()),
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(unavailable()),
    }
}
// Check every existing component, including the supplied root and its ancestors.
// Missing descendants are not created. Parent traversal is never interpreted.
fn check_components(path: &Path) -> ServiceResult<()> {
    if !path.is_absolute() {
        return Err(unsafe_path());
    }
    let mut current = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir | Component::CurDir => return Err(unsafe_path()),
            Component::Normal(name) if name == "." || name == ".." => return Err(unsafe_path()),
            Component::Prefix(_) => {
                current.push(part.as_os_str());
                continue; // A Windows drive/UNC prefix alone is not an openable path.
            }
            _ => current.push(part.as_os_str()),
        }
        metadata(&current)?;
    }
    Ok(())
}
fn anchor(path: &Path) -> ServiceResult<DirectoryAnchor> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|_| unavailable())?
            .join(path)
    };
    check_components(&absolute)?;
    // canonicalize the nearest existing ancestor, without creating user roots.
    let mut existing = absolute.as_path();
    let mut tail = Vec::new();
    while metadata(existing)?.is_none() {
        tail.push(existing.file_name().ok_or_else(unsafe_path)?.to_owned());
        existing = existing.parent().ok_or_else(unsafe_path)?;
    }
    let mut canonical = fs::canonicalize(existing).map_err(|_| unavailable())?;
    for name in tail.into_iter().rev() {
        canonical.push(name);
    }
    let identity = match metadata(&canonical)? {
        Some(m) if m.is_dir() => Some(identity(&m)),
        Some(_) => return Err(unsafe_path()),
        None => None,
    };
    Ok(DirectoryAnchor {
        path: canonical,
        identity,
    })
}
impl DirectoryAnchor {
    fn check(&self) -> ServiceResult<()> {
        check_components(&self.path)?;
        match (metadata(&self.path)?, &self.identity) {
            (Some(m), Some(expected)) if m.is_dir() && identity(&m) == *expected => {
                if fs::canonicalize(&self.path).map_err(|_| unavailable())? != self.path {
                    return Err(unsafe_path());
                }
                Ok(())
            }
            (None, None) => Ok(()),
            // A removed or newly created root invalidates this persisted intent.
            _ => Err(changed()),
        }
    }
}
fn append(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}
fn primary_name(name: &str, id: &str) -> bool {
    let suffix = format!("_{id}.jsonl");
    name.len() > suffix.len() && name.ends_with(&suffix)
}
fn orphan_name(name: &str, id: &str) -> bool {
    [".tmp", ".progress.jsonl", ".progress.jsonl.tmp"]
        .iter()
        .any(|suffix| {
            name.strip_suffix(suffix)
                .is_some_and(|main| primary_name(main, id))
        })
}
fn valid_relative(path: &Path, id: &str) -> bool {
    let parts: Vec<_> = path.components().collect();
    (1..=3).contains(&parts.len())
        && parts.iter().all(|p| match p {
            Component::Normal(s) => s.to_str().is_some_and(|s| {
                !s.is_empty()
                    && s.len() <= 1024
                    && !s.contains([':', '\\', '/'])
                    && !s.chars().any(char::is_control)
            }),
            _ => false,
        })
        && (parts.len() != 3 || parts[0].as_os_str() == id)
        && path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| primary_name(s, id))
}

impl DeletionPlan {
    /// Structural validation of an untrusted serialized plan. Roots themselves
    /// are authority supplied by the service, not authenticated by this format.
    pub fn validate(&self, id: &str) -> Result<(), String> {
        let valid = self.version == 1
            && canonical_id(id)
            && self.id == id
            && [&self.root, &self.timings].iter().all(|a| {
                a.path.is_absolute()
                    && a.path.components().all(|p| match p {
                        Component::ParentDir | Component::CurDir => false,
                        Component::Normal(name) => name != "." && name != "..",
                        _ => true,
                    })
                    && a.path.file_name().is_some()
            })
            && self
                .primary
                .as_ref()
                .map_or(true, |p| valid_relative(p, id))
            && self.files.len() <= KINDS.len();
        let kinds: HashSet<_> = self.files.iter().map(|f| f.kind).collect();
        let native = kinds
            .iter()
            .any(|k| !matches!(k, FileKind::Timing | FileKind::TimingTmp));
        if !valid
            || kinds.len() != self.files.len()
            || (self.primary.is_some() != kinds.contains(&FileKind::Primary))
            || (native && self.primary.is_none())
            || (self.root.identity.is_none() && self.primary.is_some())
            || (self.timings.identity.is_none()
                && kinds
                    .iter()
                    .any(|k| matches!(k, FileKind::Timing | FileKind::TimingTmp)))
        {
            return Err("Invalid history deletion manifest".into());
        }
        Ok(())
    }
    fn path(&self, kind: FileKind) -> Option<(PathBuf, &DirectoryAnchor)> {
        match kind {
            FileKind::Timing | FileKind::TimingTmp => {
                let p = self.timings.path.join(format!("{}.json", self.id));
                Some((
                    if kind == FileKind::TimingTmp {
                        append(&p, ".tmp")
                    } else {
                        p
                    },
                    &self.timings,
                ))
            }
            _ => {
                let p = self.root.path.join(self.primary.as_ref()?);
                let suffix = match kind {
                    FileKind::Primary => "",
                    FileKind::PrimaryTmp => ".tmp",
                    FileKind::Progress => ".progress.jsonl",
                    FileKind::ProgressTmp => ".progress.jsonl.tmp",
                    _ => unreachable!(),
                };
                Some((append(&p, suffix), &self.root))
            }
        }
    }
}

// Follow neither links nor directory trees outside the two supported layouts:
// root/encoded-cwd/H and root/UUID/encoded-cwd/H (also accept root/H).
fn discover(
    root: &DirectoryAnchor,
    id: &str,
    retry_orphans: &[PathBuf],
) -> ServiceResult<Option<PathBuf>> {
    if root.identity.is_none() {
        return Ok(None);
    }
    let start = Instant::now();
    let mut count = 0;
    let mut dirs = VecDeque::from([(root.path.clone(), 0)]);
    let mut found = None;
    let mut orphans = Vec::new();
    while let Some((dir, depth)) = dirs.pop_front() {
        if start.elapsed() >= SCAN_TIME {
            return Err(bound());
        }
        for entry in fs::read_dir(&dir).map_err(|_| unavailable())? {
            count += 1;
            if count > MAX_DIR_ENTRIES || start.elapsed() >= SCAN_TIME {
                return Err(bound());
            }
            let entry = entry.map_err(|_| unavailable())?;
            let path = entry.path();
            let m = metadata(&path)?.ok_or_else(changed)?;
            if m.is_dir() {
                if depth == 0
                    || (depth == 1
                        && dir
                            .file_name()
                            .and_then(|s| s.to_str())
                            .is_some_and(canonical_id))
                {
                    dirs.push_back((path, depth + 1));
                } else {
                    // Unknown nested directory cannot safely prove absence.
                    return Err(unknown());
                }
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if primary_name(name, id) {
                let relative = path
                    .strip_prefix(&root.path)
                    .map_err(|_| unsafe_path())?
                    .to_owned();
                if !valid_relative(&relative, id) {
                    return Err(unknown());
                }
                inspect(&path, root)?.ok_or_else(changed)?;
                if found.replace(relative).is_some() {
                    return Err(error(
                        ReplyKind::Conflict,
                        "deletion_duplicate_history",
                        "Multiple native histories for session",
                    ));
                }
            } else if orphan_name(name, id) {
                inspect(&path, root)?.ok_or_else(changed)?;
                orphans.push(path);
            }
        }
    }
    // A sidecar for a different/missing primary is unknown, never silently swept.
    if orphans.iter().any(|p| {
        !retry_orphans.contains(p)
            && found.as_ref().map_or(true, |h| {
                let h = root.path.join(h);
                ![".tmp", ".progress.jsonl", ".progress.jsonl.tmp"]
                    .iter()
                    .any(|s| append(&h, s) == *p)
            })
    }) {
        return Err(unknown());
    }
    Ok(found)
}

fn inspect(path: &Path, root: &DirectoryAnchor) -> ServiceResult<Option<Fingerprint>> {
    if !path.starts_with(&root.path) {
        return Err(unsafe_path());
    }
    check_components(path)?;
    match metadata(path)? {
        None => Ok(None),
        Some(m) if m.is_file() => {
            if !fs::canonicalize(path)
                .map_err(|_| unavailable())?
                .starts_with(&root.path)
            {
                return Err(unsafe_path());
            }
            Ok(Some(fingerprint(&m)))
        }
        Some(_) => Err(unsafe_path()),
    }
}
fn header(path: &Path, id: &str, expected: &Fingerprint) -> ServiceResult<()> {
    let mut file = File::open(path).map_err(|_| unavailable())?;
    if fingerprint(&file.metadata().map_err(|_| unavailable())?) != *expected {
        return Err(changed());
    }
    // Deliberately byte-wise and unbuffered: never read a transcript body byte.
    // At most 64 KiB reads; header contents are neither retained nor reported.
    let mut line = Vec::new();
    let mut byte = [0];
    while line.len() < HEADER_LIMIT {
        if file.read(&mut byte).map_err(|_| unavailable())? == 0 {
            break;
        }
        line.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }
    if line.last() != Some(&b'\n') {
        return Err(unknown());
    }
    let value: Value = serde_json::from_slice(&line).map_err(|_| unknown())?;
    if value["kind"] != "header"
        || value["version"] != 4
        || value["id"] != id
        || value
            .get("progressFormat")
            .is_some_and(|p| p != "assistant-progress-v1")
    {
        return Err(unknown());
    }
    if fingerprint(&file.metadata().map_err(|_| unavailable())?) != *expected {
        return Err(changed());
    }
    Ok(())
}

/// Snapshot a new manifest without reading body data. A unique main history is
/// required: absence cannot prove native never used a different configured root.
/// This fail-closed rule also rejects legacy registry-only/historyless entries;
/// it must not delete their timings or forget them. Only persisted intents may
/// execute with already-missing captured files. Service callers use prepare_locked.
pub fn prepare(root: &Path, id: &str, timings_dir: &Path) -> ServiceResult<DeletionPlan> {
    if !canonical_id(id) {
        return Err(error(
            ReplyKind::InvalidInput,
            "deletion_invalid_id",
            "Expected canonical session UUID",
        ));
    }
    let root = anchor(root)?;
    let timings = anchor(timings_dir)?;
    let primary = Some(discover(&root, id, &[])?.ok_or_else(history_missing)?);
    let mut plan = DeletionPlan {
        version: 1,
        id: id.into(),
        root,
        timings,
        primary,
        files: Vec::new(),
    };
    for kind in KINDS {
        if let Some((path, root)) = plan.path(kind) {
            if let Some(fingerprint) = inspect(&path, root)? {
                if kind == FileKind::Primary {
                    header(&path, id, &fingerprint)?;
                }
                plan.files.push(PlannedFile { kind, fingerprint });
            } else if kind == FileKind::Primary {
                return Err(changed());
            }
        }
    }
    plan.root.check()?;
    plan.timings.check()?;
    plan.validate(id).map_err(|_| unsafe_path())?;
    Ok(plan)
}

fn history_missing() -> ServiceError {
    error(
        ReplyKind::Conflict,
        "deletion_history_missing",
        "Native history is missing from the configured root; deletion cannot be verified",
    )
}

/// Owned by the retained async deletion task across registry persistence, then
/// moved into blocking execution. Never probe and release the lock before intent.
/// File (and hence this owner) is Send + Sync; drop releases on every error.
#[derive(Debug)]
pub struct PreparedDeletion {
    plan: DeletionPlan,
    lock: NativeLock,
}
impl PreparedDeletion {
    pub fn plan(&self) -> &DeletionPlan {
        &self.plan
    }
}

pub fn prepare_locked(
    root: &Path,
    id: &str,
    timings_dir: &Path,
) -> ServiceResult<PreparedDeletion> {
    if !canonical_id(id) {
        return Err(error(
            ReplyKind::InvalidInput,
            "deletion_invalid_id",
            "Expected canonical session UUID",
        ));
    }
    // Bounded discovery identifies the native lock path, not durable fingerprints.
    let discovered_root = anchor(root)?;
    let primary = discover(&discovered_root, id, &[])?.ok_or_else(history_missing)?;
    let lock = NativeLock::acquire(&discovered_root.path.join(&primary), &discovered_root)?;
    discovered_root.check()?;
    // Capture every fingerprint anew only while excluding native writers.
    let plan = prepare(root, id, timings_dir)?;
    if plan.primary.as_ref() != Some(&primary) || plan.root.path != discovered_root.path {
        return Err(changed());
    }
    lock.check(&plan.root)?;
    Ok(PreparedDeletion { plan, lock })
}

pub fn execute_prepared(prepared: PreparedDeletion) -> ServiceResult<()> {
    execute_locked(&prepared.plan, Some(&prepared.lock))
}

#[derive(Debug)]
struct NativeLock {
    file: File,
    path: PathBuf,
    identity: DirectoryIdentity,
}
impl NativeLock {
    fn acquire(primary: &Path, root: &DirectoryAnchor) -> ServiceResult<Self> {
        let path = append(primary, ".lock");
        // Stable native lock is intentionally never unlinked, including failures.
        let before = inspect(&path, root)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|_| unavailable())?;
        let opened = fingerprint(&file.metadata().map_err(|_| unavailable())?);
        if before
            .as_ref()
            .is_some_and(|f| f.identity != opened.identity)
        {
            return Err(changed());
        }
        if !file.metadata().map_err(|_| unavailable())?.is_file() {
            return Err(unsafe_path());
        }
        FileExt::try_lock_exclusive(&file).map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.raw_os_error() == fs2::lock_contended_error().raw_os_error()
            {
                error(
                    ReplyKind::Conflict,
                    "session_in_use",
                    "Native session is in use",
                )
            } else {
                unavailable()
            }
        })?;
        let lock = Self {
            file,
            path,
            identity: opened.identity,
        };
        lock.check(root)?;
        Ok(lock)
    }
    fn check(&self, root: &DirectoryAnchor) -> ServiceResult<()> {
        let current = inspect(&self.path, root)?.ok_or_else(changed)?;
        if current.identity != self.identity
            || identity(&self.file.metadata().map_err(|_| unavailable())?) != self.identity
        {
            return Err(changed());
        }
        Ok(())
    }
}
impl Drop for NativeLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Execute only the original durable manifest; missing captured files are an
/// idempotent retry. Never refresh fingerprints or replace the persisted plan.
pub fn execute(plan: &DeletionPlan) -> ServiceResult<()> {
    plan.validate(&plan.id).map_err(|_| {
        error(
            ReplyKind::InvalidInput,
            "deletion_invalid_plan",
            "Invalid history deletion manifest",
        )
    })?;
    plan.root.check()?;
    plan.timings.check()?;
    let lock = plan
        .primary
        .as_ref()
        .map(|p| NativeLock::acquire(&plan.root.path.join(p), &plan.root))
        .transpose()?;
    execute_locked(plan, lock.as_ref())
}

// Initial execution already owns the lock: reacquiring a second handle here
// would conflict with ourselves (and releasing first would reopen the race).
fn execute_locked(plan: &DeletionPlan, lock: Option<&NativeLock>) -> ServiceResult<()> {
    plan.root.check()?;
    plan.timings.check()?;
    if let Some(lock) = lock {
        lock.check(&plan.root)?;
    }
    // Verify discovery too: a second primary or unknown orphan is not authority.
    // During retry our primary may be gone but its captured companions remain.
    verify_candidates(plan)?;
    preflight(plan)?;
    for kind in KINDS {
        let Some(captured) = plan.files.iter().find(|f| f.kind == kind) else {
            continue;
        };
        plan.root.check()?;
        plan.timings.check()?;
        if let Some(lock) = &lock {
            lock.check(&plan.root)?;
        }
        let (path, root) = plan.path(kind).ok_or_else(unsafe_path)?;
        if let Some(actual) = inspect(&path, root)? {
            if actual != captured.fingerprint {
                return Err(changed());
            }
            #[cfg(test)]
            fail_remove(kind)?;
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(unavailable()),
            }
        }
    }
    // Do not report success if a companion appeared while captured files were
    // being removed. This detects ordinary races, not hostile concurrent writes.
    verify_candidates(plan)?;
    for kind in KINDS {
        if let Some((path, root)) = plan.path(kind) {
            if inspect(&path, root)?.is_some() {
                return Err(changed());
            }
        }
    }
    plan.root.check()?;
    plan.timings.check()?;
    if let Some(lock) = &lock {
        lock.check(&plan.root)?;
    }
    Ok(())
}
fn preflight(plan: &DeletionPlan) -> ServiceResult<()> {
    for kind in KINDS {
        let Some((path, root)) = plan.path(kind) else {
            continue;
        };
        let current = inspect(&path, root)?;
        let captured = plan.files.iter().find(|f| f.kind == kind);
        match (current, captured) {
            (Some(actual), Some(expected)) if actual == expected.fingerprint => {
                if kind == FileKind::Primary {
                    header(&path, &plan.id, &actual)?;
                }
            }
            (None, _) => {}
            // Includes newly appeared sidecars: never delete or claim them clean.
            _ => return Err(changed()),
        }
    }
    Ok(())
}
// Discovery normally rejects orphan companions. For persisted retries only, allow
// precisely the captured sidecars of the removed primary, never another path.
fn verify_candidates(plan: &DeletionPlan) -> ServiceResult<()> {
    let allowed: Vec<_> = plan
        .files
        .iter()
        .filter_map(|file| {
            if matches!(
                file.kind,
                FileKind::PrimaryTmp | FileKind::Progress | FileKind::ProgressTmp
            ) {
                plan.path(file.kind).map(|(path, _)| path)
            } else {
                None
            }
        })
        .collect();
    let found = discover(&plan.root, &plan.id, &allowed)?;
    if found.is_some() && found != plan.primary {
        return Err(changed());
    }
    Ok(())
}

#[cfg(test)]
thread_local! { static FAIL_REMOVE: std::cell::Cell<Option<FileKind>> = const { std::cell::Cell::new(None) }; }
#[cfg(test)]
fn fail_remove(kind: FileKind) -> ServiceResult<()> {
    FAIL_REMOVE.with(|fail| {
        if fail.get() == Some(kind) {
            fail.set(None);
            Err(unavailable())
        } else {
            Ok(())
        }
    })
}
#[cfg(test)]
#[path = "deletion_tests.rs"]
mod tests;
