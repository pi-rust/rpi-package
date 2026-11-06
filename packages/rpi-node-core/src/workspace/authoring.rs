//! Project-owned source authoring. No alternate session/install/agent engine.
use super::resources::{
    conflict, digest, immutable, invalid, plain_directory, resource_error, safe_relative,
};
use super::{
    Duration, LocalError, LocalWorkspace, Path, PathBuf, Service, SessionSummary, Value,
    WorkspaceError, decode, json, service, unsupported,
};
use rpi_extcore::resources::{
    MainPackage, ResourceScope, ResourceSelection, ResourceTools,
    local::{LocalResourceOperation, LocalResourceRequest, ResourceKind},
};
use rpi_node_api::api::{
    AuthoringDraft, AuthoringRelease, AuthoringRequest, AuthoringState, AuthoringTest,
    AuthoringValidation, NativeKind,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Read};
mod transaction;
const MAX_SOURCE: usize = 1024 * 1024;
const MAX_STORE: usize = 8 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u32,
    state: AuthoringState,
}
fn read_bounded(path: &Path, max: usize) -> Result<Vec<u8>, WorkspaceError> {
    plain_directory(
        path.parent()
            .ok_or_else(|| invalid("missing authoring parent"))?,
        false,
    )?;
    let meta = std::fs::symlink_metadata(path).map_err(LocalError::from)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(invalid("authoring reparse point rejected"));
        }
    }
    if !meta.is_file() || meta.len() > max as u64 {
        return Err(invalid(
            "authoring input must be a bounded regular file; symlinks rejected",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(LocalError::from)?
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(LocalError::from)?;
    if bytes.len() > max {
        return Err(invalid("authoring input exceeds budget"));
    }
    Ok(bytes)
}
fn sources_digest(sources: &BTreeMap<String, String>) -> Result<String, WorkspaceError> {
    check_sources(sources)?;
    Ok(digest(
        &serde_json::to_vec(sources).map_err(LocalError::from)?,
    ))
}
fn check_sources(sources: &BTreeMap<String, String>) -> Result<(), WorkspaceError> {
    if sources.is_empty()
        || sources.len() > 64
        || sources.values().map(String::len).sum::<usize>() > MAX_SOURCE
    {
        return Err(invalid("source budget: 1..64 files, aggregate 1 MiB UTF-8"));
    }
    for (path, text) in sources {
        safe_relative(path)?;
        if text.len() > 256 * 1024 || path.split('/').any(|s| s.starts_with('.')) {
            return Err(invalid("source file exceeds 256 KiB or uses hidden path"));
        }
    }
    Ok(())
}
fn read_sources(root: &Path) -> Result<BTreeMap<String, String>, WorkspaceError> {
    fn walk(
        root: &Path,
        path: &Path,
        files: &mut BTreeMap<String, String>,
        depth: usize,
        count: &mut usize,
    ) -> Result<(), WorkspaceError> {
        if depth > 12 {
            return Err(invalid("source nesting exceeds limit"));
        }
        for entry in std::fs::read_dir(path).map_err(LocalError::from)? {
            let entry = entry.map_err(LocalError::from)?;
            *count += 1;
            if *count > 128 {
                return Err(invalid("source directory entry budget exceeded"));
            }
            let meta = std::fs::symlink_metadata(entry.path()).map_err(LocalError::from)?;
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if meta.file_attributes() & 0x400 != 0 {
                    return Err(invalid("source reparse point rejected"));
                }
            }
            if meta.is_dir() && !meta.file_type().is_symlink() {
                walk(root, &entry.path(), files, depth + 1, count)?;
            } else if meta.is_file() && !meta.file_type().is_symlink() {
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|_| invalid("source escape"))?
                    .to_string_lossy()
                    .replace('\\', "/");
                let text = String::from_utf8(read_bounded(&entry.path(), 256 * 1024)?).map_err(|_| invalid("authoring editor only supports UTF-8 source; binary builds are separate"))?;
                files.insert(relative, text);
                if files.len() > 64 || files.values().map(String::len).sum::<usize>() > MAX_SOURCE {
                    return Err(invalid("source budget exceeded"));
                }
            } else {
                return Err(invalid("source symlink/special file rejected"));
            }
        }
        Ok(())
    }
    plain_directory(root, false)?;
    let mut files = BTreeMap::new();
    walk(root, root, &mut files, 0, &mut 0)?;
    check_sources(&files)?;
    Ok(files)
}
fn write_sources(root: &Path, files: &BTreeMap<String, String>) -> Result<(), WorkspaceError> {
    check_sources(files)?;
    for (path, text) in files {
        immutable(&root.join(safe_relative(path)?), text.as_bytes())?;
    }
    Ok(())
}
fn save_store(path: &Path, state: &AuthoringState) -> Result<(), WorkspaceError> {
    let bytes = serde_json::to_vec_pretty(&Store {
        version: 1,
        state: state.clone(),
    })
    .map_err(LocalError::from)?;
    if bytes.len() > MAX_STORE {
        return Err(invalid(
            "authoring metadata capacity exceeded; no records evicted",
        ));
    }
    let temporary = path.with_extension(format!("{}.tmp", nonce()));
    immutable(&temporary, &bytes)?;
    replace_metadata(&temporary, path, &bytes)
}
/// Retry ONLY Windows sharing/access-denied publication, with both candidate
/// and prior destination bytes checked before each attempt. Never unlink the
/// destination, write the final path, or retry against a changed identity.
fn replace_metadata(stage: &Path, target: &Path, bytes: &[u8]) -> Result<(), WorkspaceError> {
    let observe = || -> Result<Option<Vec<u8>>, WorkspaceError> {
        match std::fs::symlink_metadata(target) {
            Ok(_) => read_bounded(target, MAX_STORE).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(LocalError::from(e).into()),
        }
    };
    let previous = observe()?;
    // Local Windows scanners may deny delete sharing for more than 310 ms.
    // Ten checks cap the cooperative worker wait at 5.11 seconds; no async
    // owner thread is blocked, and every retry revalidates BOTH byte identities.
    for attempt in 0..10 {
        if read_bounded(stage, MAX_STORE)? != bytes || observe()? != previous {
            return Err(conflict(
                "metadata identity changed during uncertain publication; original/candidate and pending recovery retained",
            ));
        }
        match std::fs::rename(stage, target) {
            Ok(()) => return Ok(()),
            Err(error) => {
                eprintln!(
                    "authoring metadata rename failed: attempt={} kind={:?} os={:?}",
                    attempt + 1,
                    error.kind(),
                    error.raw_os_error()
                );
                let retryable = cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33));
                if !retryable || attempt == 9 {
                    return Err(LocalError::from(error).into());
                }
                std::thread::sleep(Duration::from_millis(10 << attempt));
            }
        }
    }
    unreachable!("bounded metadata publication")
}
fn nonce() -> String {
    format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}
fn identifier(id: &str) -> Result<(), WorkspaceError> {
    if id.is_empty()
        || id.len() > 64
        || id.starts_with('.')
        || id.starts_with('-')
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        return Err(invalid("resource ID must be 1..64 safe ASCII characters"));
    }
    Ok(())
}
fn templates(id: &str, kind: NativeKind) -> BTreeMap<String, String> {
    let pairs = match kind {
        NativeKind::Package => vec![("prompts/system.md", format!("# {id}\nFollow the user's task precisely. Explain observed results honestly.\n")), ("skills/author-help/SKILL.md", "---\nname: author-help\ndescription: Guidance for this locally authored Package.\n---\nInspect project files before editing.\n".into()), ("examples/test.md", "Ask the agent to explain its selected Package instructions and run a small permitted task.\n".into())],
        NativeKind::Skill => vec![("SKILL.md", format!("---\nname: {id}\ndescription: Locally authored project skill.\n---\nInspect the current project and report evidence for the requested task.\n")), ("examples/test.md", "Ask the model to read this skill and follow its instructions.\n".into())],
        NativeKind::Extension => vec![("src/lib.rs", "// Native extension source. Build separately using the actual compatible native SDK.\n// Work validation does not compile, load or promise ABI compatibility.\n".into()), ("README.md", "A native extension requires a separately authorized build. Rust text is not an installed DLL.\n".into())],
    };
    pairs.into_iter().map(|(p, t)| (p.into(), t)).collect()
}
fn skill_name(sources: &BTreeMap<String, String>) -> Result<String, WorkspaceError> {
    let text = sources
        .get("SKILL.md")
        .ok_or_else(|| invalid("Skill requires SKILL.md"))?;
    let header = text
        .strip_prefix("---\n")
        .and_then(|s| s.split_once("\n---\n").map(|(header, _)| header))
        .ok_or_else(|| invalid("Skill requires bounded name/description frontmatter"))?;
    let names: Vec<_> = header
        .lines()
        .filter_map(|line| line.strip_prefix("name:"))
        .collect();
    let name = names
        .first()
        .filter(|_| names.len() == 1)
        .map(|s| s.trim().trim_matches(['\'', '"']))
        .ok_or_else(|| invalid("Skill frontmatter requires one explicit canonical name"))?;
    if name.is_empty()
        || name.len() > 64
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
        || !name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        return Err(invalid(
            "Skill name must be lowercase kebab-case, at most 64 bytes, and match its installed directory name",
        ));
    }
    Ok(name.into())
}
fn validation(draft: &AuthoringDraft) -> AuthoringValidation {
    let mut diagnostics = Vec::new();
    let valid = match draft.kind {
        NativeKind::Package => draft
            .sources
            .get("prompts/system.md")
            .is_some_and(|s| !s.trim().is_empty()),
        NativeKind::Skill => {
            if let Err(error) = skill_name(&draft.sources) {
                diagnostics.push(error.to_string());
                false
            } else {
                draft
                    .sources
                    .get("SKILL.md")
                    .is_some_and(|s| s.contains("\ndescription:"))
            }
        }
        NativeKind::Extension => {
            diagnostics.push("Static source only. No compiler/build script/DLL was executed; ABI unverified. Supply a separately built native artifact through the resource manager to execute an extension.".into());
            false
        }
    };
    if !valid {
        diagnostics.push("Canonical Package requires prompts/system.md; Skill requires SKILL.md with name/description frontmatter; extension text alone is not executable.".into());
    }
    diagnostics.push("Static validation only; not a passed execution test.".into());
    AuthoringValidation {
        revision: draft.revision,
        digest: draft.digest.clone(),
        valid,
        diagnostics,
    }
}
impl LocalWorkspace {
    fn authoring_root(&self, project: &str) -> Result<PathBuf, WorkspaceError> {
        let root = std::fs::canonicalize(self.project_root(project)?).map_err(LocalError::from)?;
        let path = root.join(".rpi/work-authoring");
        plain_directory(&path, true)?;
        let canonical = std::fs::canonicalize(&path).map_err(LocalError::from)?;
        if !canonical.starts_with(root) {
            return Err(invalid("authoring root escapes project"));
        }
        Ok(canonical)
    }
    fn load_authoring(root: &Path) -> Result<AuthoringState, WorkspaceError> {
        if root.join("pending.json").exists() {
            return Err(conflict(
                "Uncertain authoring publication. Recoverable old/new bytes and metadata retained under pending.json and recovery/. Inspect before explicit repair; no autosave/reset.",
            ));
        }
        let path = root.join("store.json");
        if !path.try_exists().map_err(LocalError::from)? {
            return Ok(AuthoringState::default());
        }
        let mut store: Store =
            serde_json::from_slice(&read_bounded(&path, MAX_STORE)?).map_err(LocalError::from)?;
        if store.version != 1 || store.state.drafts.len() > 64 {
            return Err(invalid(
                "unsupported/corrupt authoring metadata; original bytes retained",
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        for draft in &store.state.drafts {
            identifier(&draft.id)?;
            if !ids.insert(&draft.id)
                || sources_digest(&draft.sources)? != draft.digest
                || draft.revision == 0
                || draft.path
                    != root
                        .join("drafts")
                        .join(&draft.id)
                        .join("source")
                        .to_string_lossy()
            {
                return Err(invalid(
                    "corrupt authoring identity/digest/path; refusing mutation",
                ));
            }
        }
        let journals = root.join("test-journal");
        if journals.try_exists().map_err(LocalError::from)? {
            for (index, entry) in std::fs::read_dir(journals)
                .map_err(LocalError::from)?
                .enumerate()
            {
                if index >= 256 {
                    return Err(invalid(
                        "test journal capacity exhausted; retain and explicitly archive old evidence",
                    ));
                }
                let entry = entry.map_err(LocalError::from)?;
                if !entry
                    .path()
                    .join("completed.json")
                    .try_exists()
                    .map_err(LocalError::from)?
                {
                    let intent =
                        String::from_utf8(read_bounded(&entry.path().join("intent.json"), 65536)?)
                            .map_err(|_| invalid("corrupt test intent"))?;
                    let session = entry.path().join("session.json");
                    let session = if session.try_exists().map_err(LocalError::from)? {
                        String::from_utf8(read_bounded(&session, 65536)?)
                            .map_err(|_| invalid("corrupt test session receipt"))?
                    } else {
                        "creation not observed; inspect core session catalog, do not infer success or auto replay".into()
                    };
                    store.state.diagnostics.push(format!(
                        "Recoverable incomplete test journal: {intent}; session receipt: {session}"
                    ));
                }
            }
        }
        Ok(store.state)
    }
    fn sync_authoring(root: &Path, state: &mut AuthoringState) -> Result<bool, WorkspaceError> {
        let mut changed = false;
        for draft in &mut state.drafts {
            let source = root.join("drafts").join(&draft.id).join("source");
            let files = read_sources(&source)?;
            let actual = sources_digest(&files)?;
            if actual != draft.digest {
                immutable(
                    &root
                        .join("recovery")
                        .join(format!("{}-{}.json", draft.id, draft.digest)),
                    &serde_json::to_vec(&draft.sources).map_err(LocalError::from)?,
                )?;
                draft.revision = draft
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| invalid("revision exhausted"))?;
                draft.digest = actual;
                draft.sources = files;
                draft.validation = None;
                state.diagnostics.push(format!("External/model edit detected in {}; validation and current test eligibility invalidated. Historical tests unchanged.", draft.id));
                changed = true;
            }
        }
        Ok(changed)
    }
    fn check_revision(
        draft: &AuthoringDraft,
        revision: u64,
        expected: &str,
    ) -> Result<(), WorkspaceError> {
        if draft.revision != revision || draft.digest != expected {
            return Err(conflict(
                "draft revision/digest changed on node; refresh explicitly, preserving your unsaved editor buffer",
            ));
        }
        Ok(())
    }
    /// A fence from the existing core owner, plus fail-closed refusal of project ACP owners.
    async fn authoring_admission(
        &self,
        service: &Service,
        project: &str,
    ) -> Result<rpi_extcore::service::ResourceProjectFence, WorkspaceError> {
        if self
            .inner
            .agents
            .summaries()?
            .iter()
            .any(|s| s.project_id.as_deref() == Some(project))
        {
            return Err(unsupported(
                "safe project authoring with ACP owners (close/remove ACP sessions first)",
                "acp",
            ));
        }
        service
            .resource_project_fence(project)
            .await
            .map_err(resource_error)
    }
    /// The worker owns the cloned owner, serial admission, lifecycle lease and
    /// project fence until it actually exits, even if the HTTP/UI future drops.
    /// `Handle::block_on` runs native async work without blocking the owner executor;
    /// all bounded source/metadata filesystem work remains on this blocking worker.
    pub async fn authoring(
        &self,
        project: &str,
        request: AuthoringRequest,
    ) -> Result<AuthoringState, WorkspaceError> {
        let owner = self.clone();
        let project = project.to_owned();
        let runtime = tokio::runtime::Handle::current();
        let result = tokio::task::spawn_blocking(move || {
            runtime.block_on(owner.authoring_worker(&project, request))
        })
        .await
        .map_err(|_| {
            LocalError::Initialization(
                "authoring worker failed; inspect retained recovery records".into(),
            )
        })?;
        result.map_err(|error| match error {
            WorkspaceError::Local(LocalError::Io(io)) => LocalError::AuthoringIo(io).into(),
            other => other,
        })
    }
    async fn authoring_worker(
        &self,
        project: &str,
        request: AuthoringRequest,
    ) -> Result<AuthoringState, WorkspaceError> {
        let _lifecycle = self.inner.lifecycle.read().await;
        let _serial = self
            .inner
            .authoring
            .try_lock()
            .map_err(|_| conflict("another authoring operation is in progress"))?;
        transaction::run(self, project, request).await
    }
}
#[cfg(test)]
mod tests {
    use super::{
        BTreeMap, Duration, LocalWorkspace, NativeKind, check_sources, replace_metadata,
        skill_name, templates,
    };
    #[test]
    fn skill_metadata_name_is_canonical_and_not_randomized_for_tests() {
        assert_eq!(
            skill_name(&templates("real-skill", NativeKind::Skill)).unwrap(),
            "real-skill"
        );
        for id in ["Upper", "bad.name", "bad--name", "-name", "name-", ""] {
            assert!(
                skill_name(&templates(id, NativeKind::Skill)).is_err(),
                "{id}"
            );
        }
        let mut files = templates("real-skill", NativeKind::Skill);
        files.insert(
            "SKILL.md".into(),
            "---\nname: real-skill\nname: other\ndescription: ambiguous\n---\nbody\n".into(),
        );
        assert!(skill_name(&files).is_err());
    }
    #[test]
    fn unsafe_source_paths_and_oversize_rejected() {
        for p in ["../x", "/x", "a/../x", "C:/x", "a\\b", "a/.hidden"] {
            assert!(
                check_sources(&BTreeMap::from([(p.into(), "x".into())])).is_err(),
                "{p}"
            );
        }
    }
    #[cfg(windows)]
    #[test]
    fn sharing_violation_retry_is_bounded_and_preserves_old_and_candidate_on_failure() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("store.json");
        let stage = root.path().join("candidate.tmp");
        std::fs::write(&target, b"old").unwrap();
        std::fs::write(&stage, b"new").unwrap();
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&target)
            .unwrap();
        assert!(replace_metadata(&stage, &target, b"new").is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert_eq!(std::fs::read(&stage).unwrap(), b"new");
        drop(locked);
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&target)
            .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(45));
            drop(locked);
        });
        replace_metadata(&stage, &target, b"new").unwrap();
        release.join().unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(!stage.exists());
    }
    #[cfg(windows)]
    #[test]
    fn sharing_retry_refuses_changed_destination_identity() {
        use std::{io::Write, os::windows::fs::OpenOptionsExt};
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("store.json");
        let stage = root.path().join("candidate.tmp");
        std::fs::write(&target, b"old").unwrap();
        std::fs::write(&stage, b"new").unwrap();
        let mut locked = std::fs::OpenOptions::new()
            .write(true)
            .share_mode(3)
            .open(&target)
            .unwrap();
        let changed = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(15));
            locked.write_all(b"external").unwrap();
            locked.sync_all().unwrap();
            std::thread::sleep(Duration::from_millis(40));
        });
        assert!(replace_metadata(&stage, &target, b"new").is_err());
        changed.join().unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"external");
        assert_eq!(std::fs::read(&stage).unwrap(), b"new");
    }
    #[test]
    fn corrupt_metadata_and_uncertain_writes_preserve_original_bytes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("store.json");
        std::fs::write(&path, b"broken original").unwrap();
        assert!(LocalWorkspace::load_authoring(root.path()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"broken original");
        std::fs::write(root.path().join("pending.json"), b"uncertain").unwrap();
        assert!(LocalWorkspace::load_authoring(root.path()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"broken original");
    }
}
