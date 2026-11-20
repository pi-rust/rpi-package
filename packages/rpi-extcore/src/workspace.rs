//! Read-only access to explicitly configured projects. Clients must use explicitly configured workspace roots.
use crate::reply::ReplyKind;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

const IMAGE_LIMIT: u64 = 20 * 1024 * 1024;
const FILE_LIMIT: u64 = 1024 * 1024;
const OUTPUT_LIMIT: u64 = 2 * 1024 * 1024;
const UPLOAD_LIMIT: usize = 20 * 1024 * 1024;
pub type Reply = crate::reply::ServiceResult;

#[derive(Clone, Debug, Serialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub archived: bool,
    #[serde(skip)]
    pub root: PathBuf,
}

impl Project {
    pub fn new(path: &Path) -> std::io::Result<Self> {
        let root = std::fs::canonicalize(path)?;
        if !root.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "project must be a directory",
            ));
        }
        let display = display_root(&root);
        let key = if cfg!(windows) {
            display.to_lowercase()
        } else {
            display.clone()
        };
        Ok(Self {
            id: format!("{:x}", Sha256::digest(key.as_bytes()))[..16].into(),
            name: root
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
            path: display,
            archived: false,
            root,
        })
    }
}

pub use crate::reply::failure;

fn hidden(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        ".git" | ".rpi" | "node_modules" | "target" | ".deploy"
    ) || name.to_ascii_lowercase().starts_with(".env")
}

/// Check components before resolving, then check the canonical target/ancestor.
/// The ancestor check also covers deleted and untracked paths used by Git diff.
fn resolve(root: &Path, relative: &str) -> Result<PathBuf, (ReplyKind, Value)> {
    if relative.contains(['\\', '\0', ':']) {
        return Err(failure(ReplyKind::InvalidInput, "invalid project path"));
    }
    let path = Path::new(relative);
    for part in path.components() {
        match part {
            Component::Normal(name) if !hidden(&name.to_string_lossy()) => {}
            _ => {
                return Err(failure(
                    ReplyKind::AccessDenied,
                    "path is outside the browsable project",
                ))
            }
        }
    }
    let joined = root.join(path);
    let mut ancestor = joined.as_path();
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(actual) => {
                if !actual.starts_with(root) {
                    return Err(failure(ReplyKind::AccessDenied, "path leaves the project"));
                }
                let inside = actual.strip_prefix(root).unwrap();
                if inside.components().any(|part| matches!(part, Component::Normal(name) if hidden(&name.to_string_lossy()))) {
                    return Err(failure(ReplyKind::AccessDenied, "path is outside the browsable project"));
                }
                // Use the checked canonical ancestor, rather than following the alias again.
                let remainder = joined.strip_prefix(ancestor).unwrap();
                return Ok(if remainder.as_os_str().is_empty() {
                    actual
                } else {
                    actual.join(remainder)
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| failure(ReplyKind::NotFound, "path not found"))?;
            }
            Err(_) => return Err(failure(ReplyKind::AccessDenied, "path is not accessible")),
        }
    }
}

/// Display spelling of a browsable root: canonical, without the Windows verbatim prefix.
fn display_root(path: &Path) -> String {
    path.to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string()
}

/// Linked worktrees of this project's repository, excluding the project root.
///
/// Agents work in sibling checkouts (`git worktree add ../name`), and every
/// write there used to be unpresentable: links pointed outside the one
/// configured root. Git lists every checkout of the repository, so the main
/// checkout -- and the project's own root -- are filtered by canonical
/// comparison instead of by string.
///
/// Never cached: a worktree is usually created while a run is in progress, and
/// a stale list would hide exactly the files the run was asked to produce.
/// Errors fail closed (no extra root), matching the read-only boundary.
pub async fn worktrees(project: &Project) -> Vec<PathBuf> {
    let Ok((true, output)) = git(project, &["worktree", "list", "--porcelain"]).await else {
        return Vec::new();
    };
    let mut roots: Vec<PathBuf> = Vec::new();
    for line in output.lines() {
        let Some(raw) = line.strip_prefix("worktree ") else {
            continue;
        };
        let Some(canonical) = std::fs::canonicalize(raw.trim())
            .ok()
            .filter(|p| p.is_dir())
        else {
            continue;
        };
        // The main checkout is listed too. It is already browsable when it is
        // the project itself, and when the project is a subdirectory of it the
        // enclosing checkout must NOT become a root: that would widen the
        // boundary past the configured project. Sibling and nested checkouts
        // are the extra roots this exists for.
        if canonical != project.root
            && !project.root.starts_with(&canonical)
            && !roots.contains(&canonical)
        {
            roots.push(canonical);
        }
    }
    roots
}

/// The `/projects` spelling of [`worktrees`], ready for a client to join and
/// hand back as the `root` of a file or image request.
pub async fn worktree_paths(project: &Project) -> Vec<String> {
    worktrees(project)
        .await
        .iter()
        .map(|path| display_root(path))
        .collect()
}

/// The root a client asked to read: empty is the project root, anything else
/// must be one of this project's linked worktrees. Canonical comparison keeps
/// the boundary even when the client sends `..`, a symlink, or another
/// spelling of the same directory.
pub async fn browse_root(project: &Project, root: &str) -> Result<PathBuf, (ReplyKind, Value)> {
    if root.is_empty() {
        return Ok(project.root.clone());
    }
    if root.contains('\0') {
        return Err(failure(ReplyKind::InvalidInput, "invalid project path"));
    }
    let requested = std::fs::canonicalize(root).map_err(|_| {
        failure(
            ReplyKind::AccessDenied,
            "path is outside the browsable project",
        )
    })?;
    if requested == project.root || worktrees(project).await.contains(&requested) {
        return Ok(requested);
    }
    Err(failure(
        ReplyKind::AccessDenied,
        "path is outside the browsable project",
    ))
}

pub async fn files(project: &Project, relative: &str) -> Reply {
    files_at(project, "", relative).await
}

/// List one directory of the project root or of a linked worktree.
pub async fn files_at(project: &Project, root: &str, relative: &str) -> Reply {
    let path = resolve(&browse_root(project, root).await?, relative)?;
    let mut directory = tokio::fs::read_dir(path)
        .await
        .map_err(|_| failure(ReplyKind::NotFound, "directory not found"))?;
    let mut entries = Vec::new();
    while let Some(entry) = directory
        .next_entry()
        .await
        .map_err(|_| failure(ReplyKind::AccessDenied, "cannot read directory"))?
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if hidden(&name) {
            continue;
        }
        let kind = entry
            .file_type()
            .await
            .map_err(|_| failure(ReplyKind::AccessDenied, "cannot read file type"))?;
        if kind.is_symlink() || !(kind.is_file() || kind.is_dir()) {
            continue;
        }
        let metadata = entry.metadata().await.ok();
        entries.push(json!({"name":name,"path":if relative.is_empty() { name.clone() } else { format!("{relative}/{name}") },
            "kind":if kind.is_dir() {"directory"} else {"file"},"size":metadata.map(|m|m.len()).unwrap_or(0)}));
        if entries.len() > 2000 {
            return Err(failure(
                ReplyKind::LimitExceeded,
                "directory has too many entries; open a smaller directory",
            ));
        }
    }
    entries.sort_by(|a, b| {
        (
            a["kind"] != "directory",
            a["name"].as_str().unwrap_or("").to_lowercase(),
        )
            .cmp(&(
                b["kind"] != "directory",
                b["name"].as_str().unwrap_or("").to_lowercase(),
            ))
    });
    Ok(json!({"path":relative,"entries":entries}))
}

/// Direct child directories of a root, for the "open an existing folder" picker.
pub async fn child_directories(
    root: &Path,
    relative: &str,
) -> Result<Vec<(String, String)>, (ReplyKind, Value)> {
    let path = resolve(root, relative)?;
    let missing = || {
        failure(
            ReplyKind::NotFound,
            &format!("directory not found under {}: {relative}", root.display()),
        )
    };
    let mut directory = tokio::fs::read_dir(path).await.map_err(|_| missing())?;
    let mut folders = Vec::new();
    while let Some(entry) = directory
        .next_entry()
        .await
        .map_err(|_| failure(ReplyKind::AccessDenied, "cannot read projects directory"))?
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if hidden(&name) {
            continue;
        }
        let kind = entry
            .file_type()
            .await
            .map_err(|_| failure(ReplyKind::AccessDenied, "cannot read file type"))?;
        if kind.is_symlink() || !kind.is_dir() {
            continue;
        }
        let path = if relative.is_empty() {
            name.clone()
        } else {
            format!("{relative}/{name}")
        };
        folders.push((name, path));
        if folders.len() > 2000 {
            return Err(failure(
                ReplyKind::LimitExceeded,
                "projects directory has too many folders",
            ));
        }
    }
    folders.sort_by_key(|a| a.0.to_lowercase());
    Ok(folders)
}

pub async fn upload(project: &Project, filename: &str, bytes: &[u8]) -> Reply {
    if bytes.len() > UPLOAD_LIMIT {
        return Err(failure(
            ReplyKind::LimitExceeded,
            "uploads are limited to 20 MiB",
        ));
    }
    if filename.contains(['/', '\\', '\0']) {
        return Err(failure(ReplyKind::InvalidInput, "invalid upload filename"));
    }
    let name = Path::new(filename)
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "." && *value != "..")
        .ok_or_else(|| failure(ReplyKind::InvalidInput, "invalid upload filename"))?;
    let directory = project.root.join(".rpi").join("uploads");
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|_| failure(ReplyKind::AccessDenied, "cannot create upload directory"))?;
    let stored_name = format!("{}-{name}", Uuid::new_v4());
    let path = directory.join(&stored_name);
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|_| failure(ReplyKind::AccessDenied, "cannot save upload"))?;
    Ok(json!({
        "name": name,
        "path": format!(".rpi/uploads/{stored_name}"),
        "size": bytes.len(),
    }))
}

/// Bounded raster content, independent of any transport or JSON encoding.
#[derive(Debug)]
pub struct ProjectImage {
    pub bytes: Vec<u8>,
    pub media_type: &'static str,
}

/// Bounded document content a browser can display or hand to a desktop app.
#[derive(Debug)]
pub struct ProjectDocument {
    pub bytes: Vec<u8>,
    pub media_type: &'static str,
}

/// Documents a browser can preview natively (PDF) or at least download with a
/// truthful media type. Text-like and raster formats keep their own routes.
const DOCUMENT_LIMIT: u64 = 40 * 1024 * 1024;

/// Bounded, opaque bytes for a browser download.
#[derive(Debug)]
pub struct ProjectDownload {
    pub bytes: Vec<u8>,
    pub file_name: String,
}

/// Read any regular project file as an opaque attachment.
///
/// Unlike the inline document route this accepts every extension, because the
/// transport hands it to the operating system with `Content-Disposition:
/// attachment` and `application/octet-stream`; nothing is rendered in the page.
pub async fn download_at(
    project: &Project,
    root: &str,
    relative: &str,
) -> Result<ProjectDownload, (ReplyKind, Value)> {
    let path = resolve(&browse_root(project, root).await?, relative)?;
    fn check(metadata: &std::fs::Metadata) -> Result<(), (ReplyKind, Value)> {
        if !metadata.is_file() {
            return Err(failure(ReplyKind::InvalidInput, "select a regular file"));
        }
        if metadata.len() > DOCUMENT_LIMIT {
            return Err(failure(
                ReplyKind::LimitExceeded,
                "downloads are limited to 40 MiB",
            ));
        }
        Ok(())
    }
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|_| failure(ReplyKind::NotFound, "file not found"))?;
    check(&metadata)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download")
        .to_string();
    let handle = tokio::fs::File::open(path)
        .await
        .map_err(|_| failure(ReplyKind::NotFound, "file not found"))?;
    check(
        &handle
            .metadata()
            .await
            .map_err(|_| failure(ReplyKind::AccessDenied, "cannot inspect file"))?,
    )?;
    let mut bytes = Vec::new();
    handle
        .take(DOCUMENT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| failure(ReplyKind::AccessDenied, "cannot read file"))?;
    if bytes.len() as u64 > DOCUMENT_LIMIT {
        return Err(failure(
            ReplyKind::LimitExceeded,
            "downloads are limited to 40 MiB",
        ));
    }
    Ok(ProjectDownload { bytes, file_name })
}

/// Read a project-relative document from the project root or a linked worktree.
///
/// The extension whitelist keeps this route from becoming a generic binary
/// download: only formats the UI knows how to name are served.
pub async fn document_at(
    project: &Project,
    root: &str,
    relative: &str,
) -> Result<ProjectDocument, (ReplyKind, Value)> {
    let path = resolve(&browse_root(project, root).await?, relative)?;
    let extension = Path::new(relative)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let media_type = match extension.as_str() {
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => {
            "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        }
        _ => {
            return Err(failure(
                ReplyKind::UnsupportedContent,
                "unsupported document format",
            ))
        }
    };
    fn check(metadata: &std::fs::Metadata) -> Result<(), (ReplyKind, Value)> {
        if !metadata.is_file() {
            return Err(failure(ReplyKind::InvalidInput, "select a regular file"));
        }
        if metadata.len() > DOCUMENT_LIMIT {
            return Err(failure(
                ReplyKind::LimitExceeded,
                "documents are limited to 40 MiB",
            ));
        }
        Ok(())
    }
    // Same handle discipline as image reads: reject devices/FIFOs, then recheck.
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|_| failure(ReplyKind::NotFound, "document not found"))?;
    check(&metadata)?;
    let handle = tokio::fs::File::open(path)
        .await
        .map_err(|_| failure(ReplyKind::NotFound, "document not found"))?;
    check(
        &handle
            .metadata()
            .await
            .map_err(|_| failure(ReplyKind::AccessDenied, "cannot inspect document"))?,
    )?;
    let mut bytes = Vec::new();
    handle
        .take(DOCUMENT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| failure(ReplyKind::AccessDenied, "cannot read document"))?;
    if bytes.len() as u64 > DOCUMENT_LIMIT {
        return Err(failure(
            ReplyKind::LimitExceeded,
            "documents are limited to 40 MiB",
        ));
    }
    // A renamed file must not be served under a media type its bytes contradict.
    let matches = match &extension[..] {
        "pdf" => bytes.starts_with(b"%PDF-"),
        "doc" | "xls" | "ppt" => bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]),
        _ => bytes.starts_with(b"PK\x03\x04"),
    };
    if !matches {
        return Err(failure(
            ReplyKind::UnsupportedContent,
            "document signature does not match extension",
        ));
    }
    Ok(ProjectDocument { bytes, media_type })
}

/// Read a project-relative raster image. Errors use the existing neutral reply contract.
pub async fn image(project: &Project, relative: &str) -> Result<ProjectImage, (ReplyKind, Value)> {
    image_at(project, "", relative).await
}

/// Read a raster image from the project root or one of its linked worktrees.
pub async fn image_at(
    project: &Project,
    root: &str,
    relative: &str,
) -> Result<ProjectImage, (ReplyKind, Value)> {
    let path = resolve(&browse_root(project, root).await?, relative)?;
    let extension = Path::new(relative)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let media_type = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => {
            return Err(failure(
                ReplyKind::UnsupportedContent,
                "unsupported image format",
            ))
        }
    };
    fn check(metadata: &std::fs::Metadata) -> Result<(), (ReplyKind, Value)> {
        if !metadata.is_file() {
            return Err(failure(ReplyKind::InvalidInput, "select a regular file"));
        }
        if metadata.len() > IMAGE_LIMIT {
            return Err(failure(
                ReplyKind::LimitExceeded,
                "images are limited to 20 MiB",
            ));
        }
        Ok(())
    }
    // Reject devices/FIFOs and oversized files before opening, then recheck the handle.
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|_| failure(ReplyKind::NotFound, "image not found"))?;
    check(&metadata)?;
    let handle = tokio::fs::File::open(path)
        .await
        .map_err(|_| failure(ReplyKind::NotFound, "image not found"))?;
    check(
        &handle
            .metadata()
            .await
            .map_err(|_| failure(ReplyKind::AccessDenied, "cannot inspect image"))?,
    )?;
    let mut bytes = Vec::new();
    handle
        .take(IMAGE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| failure(ReplyKind::AccessDenied, "cannot read image"))?;
    if bytes.len() as u64 > IMAGE_LIMIT {
        return Err(failure(
            ReplyKind::LimitExceeded,
            "images are limited to 20 MiB",
        ));
    }
    let matches = match media_type {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        "image/bmp" => bytes.starts_with(b"BM"),
        _ => false,
    };
    if !matches {
        return Err(failure(
            ReplyKind::UnsupportedContent,
            "image signature does not match extension",
        ));
    }
    Ok(ProjectImage { bytes, media_type })
}

pub async fn file(project: &Project, relative: &str) -> Reply {
    file_at(project, "", relative).await
}

/// Read a text file from the project root or one of its linked worktrees.
pub async fn file_at(project: &Project, root: &str, relative: &str) -> Reply {
    let path = resolve(&browse_root(project, root).await?, relative)?;
    // Reject devices/FIFOs before opening: opening one may otherwise block indefinitely.
    match tokio::fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err(failure(ReplyKind::InvalidInput, "select a regular file")),
        Err(_) => return Err(failure(ReplyKind::NotFound, "file not found")),
    }
    let handle = tokio::fs::File::open(path)
        .await
        .map_err(|_| failure(ReplyKind::NotFound, "file not found"))?;
    if !handle
        .metadata()
        .await
        .map(|m| m.is_file())
        .unwrap_or(false)
    {
        return Err(failure(ReplyKind::InvalidInput, "select a regular file"));
    }
    let mut bytes = Vec::new();
    handle
        .take(FILE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| failure(ReplyKind::AccessDenied, "cannot read file"))?;
    if bytes.len() as u64 > FILE_LIMIT {
        return Err(failure(
            ReplyKind::LimitExceeded,
            "preview supports files up to 1 MiB",
        ));
    }
    if bytes.contains(&0) {
        return Err(failure(
            ReplyKind::UnsupportedContent,
            "binary files cannot be previewed",
        ));
    }
    let content = String::from_utf8(bytes).map_err(|_| {
        failure(
            ReplyKind::UnsupportedContent,
            "preview supports UTF-8 text files",
        )
    })?;
    Ok(json!({"path":relative,"content":content}))
}

async fn git(project: &Project, args: &[&str]) -> Result<(bool, String), (ReplyKind, Value)> {
    let mut child = crate::process::background_command("git")
        .args([
            "--no-pager",
            "--literal-pathspecs",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.quotePath=false",
        ])
        .args(args)
        .current_dir(&project.root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| {
            failure(
                ReplyKind::Unavailable,
                "Git is not available on this server",
            )
        })?;
    let mut stdout = child.stdout.take().unwrap().take(OUTPUT_LIMIT + 1);
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await?;
        if bytes.len() as u64 > OUTPUT_LIMIT {
            return Ok::<_, std::io::Error>(None);
        }
        let status = child.wait().await?;
        Ok(Some((
            status.success(),
            String::from_utf8_lossy(&bytes).into_owned(),
        )))
    })
    .await
    .map_err(|_| failure(ReplyKind::Timeout, "Git request timed out"))?
    .map_err(|_| failure(ReplyKind::Internal, "Git request failed"))?;
    result.ok_or_else(|| {
        failure(
            ReplyKind::LimitExceeded,
            "Git output is too large to preview",
        )
    })
}

fn parse_status(output: &str, prefix: &str) -> Vec<Value> {
    let mut files = Vec::new();
    let mut records = output.split('\0').filter(|s| !s.is_empty());
    while let Some(record) = records.next() {
        if record.len() < 4 || !record.is_char_boundary(3) {
            continue;
        }
        let status = &record[..2];
        let path = &record[3..];
        let original = if status.contains(['R', 'C']) {
            records.next()
        } else {
            None
        };
        let Some(path) = path.strip_prefix(prefix) else {
            continue;
        };
        let original = original.and_then(|value| value.strip_prefix(prefix));
        if Path::new(path)
            .components()
            .any(|p| matches!(p, Component::Normal(n) if hidden(&n.to_string_lossy())))
        {
            continue;
        }
        files.push(json!({"path":path,"status":status,"originalPath":original,
            "staged":status.as_bytes()[0] != b' ' && status != "??", "unstaged":status.as_bytes()[1] != b' ' || status == "??"}));
    }
    files
}

pub async fn status(project: &Project) -> Reply {
    let (valid, repository_root) = git(project, &["rev-parse", "--show-toplevel"]).await?;
    if !valid {
        return Ok(json!({"isRepository":false,"files":[],"branch":null}));
    }
    let (ok, output) = git(
        project,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
        ],
    )
    .await?;
    if !ok {
        return Err(failure(ReplyKind::Internal, "cannot read Git status"));
    }
    let (named_branch, mut branch) =
        git(project, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await?;
    if !named_branch {
        branch = git(project, &["rev-parse", "--short", "HEAD"]).await?.1;
    }
    let (_, prefix) = git(project, &["rev-parse", "--show-prefix"]).await?;
    let (_, directories) = git(
        project,
        &["rev-parse", "--absolute-git-dir", "--git-common-dir"],
    )
    .await?;
    let mut directories = directories.lines();
    let is_worktree =
        directories
            .next()
            .zip(directories.next())
            .and_then(|(git_dir, common_dir)| {
                let git_dir = project.root.join(git_dir).canonicalize().ok()?;
                let common_dir = project.root.join(common_dir).canonicalize().ok()?;
                Some(git_dir != common_dir)
            });
    Ok(
        json!({"isRepository":true,"branch":branch.trim(),"isDetached":!named_branch,
            "repositoryRoot":repository_root.trim(),"isWorktree":is_worktree,
            "worktreePath":if is_worktree == Some(true) { Some(repository_root.trim()) } else { None },
            "files":parse_status(&output, prefix.trim_end_matches(['\r', '\n']))}),
    )
}

pub async fn diff(project: &Project, relative: &str, staged: bool) -> Reply {
    if relative.is_empty() {
        return Err(failure(ReplyKind::InvalidInput, "select a changed file"));
    }
    let path = resolve(&project.root, relative)?;
    match tokio::fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err(failure(ReplyKind::InvalidInput, "select a changed file")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A deleted file is valid, but a deleted directory is still a recursive pathspec.
            let (_, tracked) =
                git(project, &["ls-files", "--full-name", "-z", "--", relative]).await?;
            let (_, prefix) = git(project, &["rev-parse", "--show-prefix"]).await?;
            let expected = format!("{}{relative}", prefix.trim_end_matches(['\r', '\n']));
            if !tracked.split('\0').any(|name| name == expected) {
                return Err(failure(ReplyKind::InvalidInput, "select a changed file"));
            }
        }
        Err(_) => return Err(failure(ReplyKind::AccessDenied, "file is not accessible")),
    }
    let (valid, _) = git(project, &["rev-parse", "--show-toplevel"]).await?;
    if !valid {
        return Err(failure(
            ReplyKind::InvalidInput,
            "project is not a Git repository",
        ));
    }
    let mut args = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--unified=3",
    ];
    if staged {
        args.push("--cached");
    }
    args.extend(["--", relative]);
    let (ok, mut patch) = git(project, &args).await?;
    if !ok {
        return Err(failure(ReplyKind::Internal, "cannot read Git diff"));
    }
    if patch.is_empty() && !staged {
        let (_, output) = git(project, &["status", "--porcelain=v1", "-z", "--", relative]).await?;
        if output.starts_with("?? ") {
            let value = file(project, relative).await?;
            let content = value["content"].as_str().unwrap_or("");
            patch = format!(
                "--- /dev/null\n+++ b/{relative}\n@@ -0,0 +1,{} @@\n",
                content.lines().count()
            );
            for line in content.lines() {
                patch.push('+');
                patch.push_str(line);
                patch.push('\n');
            }
        }
    }
    Ok(json!({"path":relative,"staged":staged,"patch":patch}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> PathBuf {
        let path = std::env::temp_dir().join(format!("rpi-workspace-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    const PNG: &[u8] = include_bytes!("../tests/fixtures/project-image.png");

    #[tokio::test]
    async fn folder_browsing_lists_directories_below_a_canonical_root() {
        // `Projects::load` canonicalizes the configured root before any client
        // read, so containment compares canonical paths on every platform.
        let root = std::fs::canonicalize(fixture()).unwrap();
        for name in ["alpha", "beta", ".git", "node_modules"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        std::fs::create_dir_all(root.join("beta/nested/deep")).unwrap();
        std::fs::write(root.join("file.txt"), b"not a folder").unwrap();
        let top = child_directories(&root, "").await.unwrap();
        assert_eq!(
            top,
            vec![
                ("alpha".into(), "alpha".into()),
                ("beta".into(), "beta".into())
            ]
        );
        let nested = child_directories(&root, "beta/nested").await.unwrap();
        assert_eq!(nested, vec![("deep".into(), "beta/nested/deep".into())]);
        // Missing folders name the browsable root so a misconfigured
        // `--projects-root` (for example a literal `~`) is diagnosable.
        let (kind, value) = child_directories(&root, "missing").await.unwrap_err();
        assert_eq!(kind, ReplyKind::NotFound);
        assert!(
            value["error"]
                .as_str()
                .unwrap()
                .contains(&root.display().to_string()),
            "{value}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn folder_browsing_accepts_a_symlinked_projects_root() {
        // macOS mounts /tmp and /var as symlinks; the containment check must
        // compare canonical paths, not the configured spelling.
        let root = fixture();
        let link = root.with_extension("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        std::fs::create_dir_all(root.join("repo")).unwrap();
        let canonical = std::fs::canonicalize(&link).unwrap();
        let folders = child_directories(&canonical, "").await.unwrap();
        assert_eq!(folders, vec![("repo".into(), "repo".into())]);
        assert!(
            child_directories(&link, "").await.is_err(),
            "an uncationicalized root must fail closed rather than escape"
        );
        std::fs::remove_file(&link).unwrap();
    }

    #[tokio::test]
    async fn documents_are_whitelisted_and_signature_checked() {
        let root = fixture();
        let project = Project::new(&root).unwrap();
        for (name, bytes, media_type) in [
            ("report.pdf", &b"%PDF-1.7\n"[..], "application/pdf"),
            (
                "spec.docx",
                &b"PK\x03\x04docx"[..],
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            ),
            (
                "sheet.xlsx",
                &b"PK\x03\x04xlsx"[..],
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            ),
            // Legacy Office formats are OLE compound files.
            ("old.doc", &[0xD0, 0xCF, 0x11, 0xE0][..], "application/msword"),
        ] {
            std::fs::write(root.join(name), bytes).unwrap();
            let document = document_at(&project, "", name).await.unwrap();
            assert_eq!(document.media_type, media_type, "{name}");
            assert_eq!(document.bytes, bytes, "{name}");
        }
        // Only whitelisted formats are served; a text file keeps its own route.
        std::fs::write(root.join("notes.txt"), b"plain").unwrap();
        let (kind, _) = document_at(&project, "", "notes.txt").await.unwrap_err();
        assert_eq!(kind, ReplyKind::UnsupportedContent);
        // A renamed file must not be served under a media type its bytes deny.
        std::fs::write(root.join("fake.pdf"), b"plain text").unwrap();
        let (kind, _) = document_at(&project, "", "fake.pdf").await.unwrap_err();
        assert_eq!(kind, ReplyKind::UnsupportedContent);
        std::fs::write(root.join("fake.docx"), b"%PDF-1.7").unwrap();
        let (kind, _) = document_at(&project, "", "fake.docx").await.unwrap_err();
        assert_eq!(kind, ReplyKind::UnsupportedContent);
        let (kind, _) = document_at(&project, "", "missing.pdf").await.unwrap_err();
        assert_eq!(kind, ReplyKind::NotFound);
        // The shared path guards still apply.
        let (kind, _) = document_at(&project, "", ".rpi/sessions/a.pdf")
            .await
            .unwrap_err();
        assert_eq!(kind, ReplyKind::AccessDenied);
        let (kind, _) = document_at(&project, "", "../escape.pdf")
            .await
            .unwrap_err();
        assert_eq!(kind, ReplyKind::AccessDenied);
    }

    #[tokio::test]
    async fn linked_worktree_directories_are_listable() {
        let root = fixture();
        let project = Project::new(&root).unwrap();
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
            vec!["commit", "--allow-empty", "-m", "initial"],
        ] {
            assert!(git(&project, &args).await.unwrap().0);
        }
        let linked = root.with_extension("list-worktree");
        assert!(
            git(
                &project,
                &["worktree", "add", "-b", "listing", linked.to_str().unwrap()]
            )
            .await
            .unwrap()
            .0
        );
        std::fs::create_dir(linked.join("nested")).unwrap();
        std::fs::write(linked.join("nested/only.txt"), b"worktree").unwrap();
        let canonical = std::fs::canonicalize(&linked).unwrap();
        let branch = display_root(&canonical);

        // The project root lists its own directory only.
        let listed = files_at(&project, "", "").await.unwrap();
        let names: Vec<String> = listed["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"].as_str().unwrap().to_string())
            .collect();
        // The worktree's directories are not part of the project root listing.
        assert!(!names.contains(&"nested".to_string()), "{names:?}");

        // The linked checkout lists its own files under the same project.
        let listed = files_at(&project, &branch, "nested").await.unwrap();
        assert_eq!(
            listed["entries"][0]["path"].as_str().unwrap(),
            "nested/only.txt"
        );

        // Anything else stays outside the browsable project.
        assert!(files_at(&project, "/", "").await.is_err());
    }

    #[tokio::test]
    async fn downloads_are_opaque_bounded_project_files() {
        let root = fixture();
        let project = Project::new(&root).unwrap();
        // Any extension downloads; it never claims a renderable media type.
        std::fs::write(root.join("report.zip"), b"PK\x03\x04binary").unwrap();
        let download = download_at(&project, "", "report.zip").await.unwrap();
        assert_eq!(download.file_name, "report.zip");
        assert_eq!(download.bytes, b"PK\x03\x04binary");
        std::fs::write(root.join("photo.PNG"), PNG).unwrap();
        assert_eq!(
            download_at(&project, "", "photo.PNG").await.unwrap().bytes,
            PNG
        );
        let (kind, _) = download_at(&project, "", "missing.zip").await.unwrap_err();
        assert_eq!(kind, ReplyKind::NotFound);
        // The same browsable boundary guards downloads.
        for blocked in [".git/config", ".rpi/sessions/a.jsonl", "../escape.zip"] {
            let (kind, _) = download_at(&project, "", blocked).await.unwrap_err();
            assert_eq!(kind, ReplyKind::AccessDenied, "{blocked}");
        }
        std::fs::create_dir_all(root.join("folder")).unwrap();
        let (kind, _) = download_at(&project, "", "folder").await.unwrap_err();
        assert_eq!(kind, ReplyKind::InvalidInput);
    }

    #[tokio::test]
    async fn images_are_scoped_bounded_regular_rasters() {
        let root = fixture();
        let project = Project::new(&root).unwrap();
        std::fs::write(root.join("actual.PNG"), PNG).unwrap();
        let actual = image(&project, "actual.PNG").await.unwrap();
        assert_eq!(actual.media_type, "image/png");
        assert_eq!(actual.bytes, PNG);
        for (name, bytes, media_type) in [
            ("photo.jpg", &b"\xff\xd8\xff\xe0"[..], "image/jpeg"),
            ("photo.jpeg", &b"\xff\xd8\xff\xe1"[..], "image/jpeg"),
            ("old.gif", &b"GIF87a"[..], "image/gif"),
            ("new.gif", &b"GIF89a"[..], "image/gif"),
            ("photo.webp", &b"RIFF\0\0\0\0WEBP"[..], "image/webp"),
            ("photo.bmp", &b"BM"[..], "image/bmp"),
        ] {
            // These exercise signature recognition, not image decoding.
            std::fs::write(root.join(name), bytes).unwrap();
            assert_eq!(image(&project, name).await.unwrap().media_type, media_type);
        }
        for name in [
            "fake.png",
            "fake.jpg",
            "fake.gif",
            "fake.webp",
            "fake.bmp",
            "fake.svg",
            "fake.html",
        ] {
            std::fs::write(root.join(name), b"<html>not an image</html>").unwrap();
            assert_eq!(
                image(&project, name).await.unwrap_err().0,
                ReplyKind::UnsupportedContent,
                "{name}"
            );
        }
        std::fs::write(root.join("mismatch.jpg"), PNG).unwrap();
        std::fs::write(root.join("empty.png"), []).unwrap();
        std::fs::write(root.join("bad.webp"), b"RIFFxxxxNOTP").unwrap();
        for name in ["mismatch.jpg", "empty.png", "bad.webp"] {
            assert_eq!(
                image(&project, name).await.unwrap_err().0,
                ReplyKind::UnsupportedContent
            );
        }
        for name in [
            "../outside.png",
            "/actual.PNG",
            "C:/actual.PNG",
            "a\\b.png",
            ".git/image.png",
            ".rpi/image.png",
            "node_modules/image.png",
            "target/image.png",
            ".deploy/image.png",
            ".env/image.png",
        ] {
            assert!(image(&project, name).await.is_err(), "{name}");
        }
        assert_eq!(
            image(&project, "missing.png").await.unwrap_err().0,
            ReplyKind::NotFound
        );
        std::fs::create_dir(root.join("folder.png")).unwrap();
        assert_eq!(
            image(&project, "folder.png").await.unwrap_err().0,
            ReplyKind::InvalidInput
        );
        let large = std::fs::File::create(root.join("large.png")).unwrap();
        large.set_len(IMAGE_LIMIT + 1).unwrap();
        drop(large);
        assert_eq!(
            image(&project, "large.png").await.unwrap_err().0,
            ReplyKind::LimitExceeded
        );
        let mut boundary = PNG.to_vec();
        boundary.resize(IMAGE_LIMIT as usize, 0);
        std::fs::write(root.join("boundary.png"), &boundary).unwrap();
        assert_eq!(
            image(&project, "boundary.png").await.unwrap().bytes.len(),
            IMAGE_LIMIT as usize
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn image_junctions_cannot_escape_or_alias_hidden_directories() {
        let root = fixture();
        let outside = fixture();
        std::fs::write(outside.join("image.png"), PNG).unwrap();
        std::fs::create_dir(root.join(".deploy")).unwrap();
        std::fs::write(root.join(".deploy/image.png"), PNG).unwrap();
        let project = Project::new(&root).unwrap();
        for (name, target) in [("escape", outside.clone()), ("alias", root.join(".deploy"))] {
            let link = root.join(name);
            // Directory junctions do not require Windows symlink privileges.
            assert!(std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(&target)
                .stdout(Stdio::null())
                .status()
                .unwrap()
                .success());
            assert_eq!(
                image(&project, &format!("{name}/image.png"))
                    .await
                    .unwrap_err()
                    .0,
                ReplyKind::AccessDenied
            );
            std::fs::remove_dir(link).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn image_fifos_and_symlink_escapes_are_rejected() {
        let root = fixture();
        let outside = fixture();
        let project = Project::new(&root).unwrap();
        assert!(std::process::Command::new("mkfifo")
            .arg(root.join("pipe.png"))
            .status()
            .unwrap()
            .success());
        let result = tokio::time::timeout(Duration::from_millis(200), image(&project, "pipe.png"))
            .await
            .unwrap();
        assert_eq!(result.unwrap_err().0, ReplyKind::InvalidInput);
        std::fs::write(outside.join("image.png"), PNG).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        std::fs::create_dir(root.join(".deploy")).unwrap();
        std::fs::write(root.join(".deploy/image.png"), PNG).unwrap();
        std::os::unix::fs::symlink(root.join(".deploy"), root.join("alias")).unwrap();
        for name in ["escape/image.png", "alias/image.png"] {
            assert_eq!(
                image(&project, name).await.unwrap_err().0,
                ReplyKind::AccessDenied
            );
        }
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[tokio::test]
    async fn oversized_git_output_is_rejected_and_next_request_still_works() {
        let root = fixture();
        let project = Project::new(&root).unwrap();
        assert!(git(&project, &["init"]).await.unwrap().0);
        std::fs::write(root.join("large.txt"), "before\n").unwrap();
        assert!(git(&project, &["add", "large.txt"]).await.unwrap().0);
        std::fs::write(
            root.join("large.txt"),
            vec![b'x'; OUTPUT_LIMIT as usize + 100],
        )
        .unwrap();
        assert_eq!(
            diff(&project, "large.txt", false).await.unwrap_err().0,
            ReplyKind::LimitExceeded
        );
        std::fs::write(root.join("large.txt"), "after\n").unwrap();
        assert!(diff(&project, "large.txt", false).await.unwrap()["patch"]
            .as_str()
            .unwrap()
            .contains("+after"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fifo_preview_is_rejected_without_waiting_for_a_writer() {
        let root = fixture();
        assert!(std::process::Command::new("mkfifo")
            .arg(root.join("pipe"))
            .status()
            .unwrap()
            .success());
        let project = Project::new(&root).unwrap();
        let result = tokio::time::timeout(Duration::from_millis(200), file(&project, "pipe"))
            .await
            .unwrap();
        assert_eq!(result.unwrap_err().0, ReplyKind::InvalidInput);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn previews_are_scoped_bounded_and_text_only() {
        let root = fixture();
        let p = Project::new(&root).unwrap();
        std::fs::write(root.join("hello.rs"), "你好\nfn main() {}\n").unwrap();
        std::fs::write(root.join("binary"), [0, 1, 2]).unwrap();
        std::fs::write(root.join("large"), vec![b'a'; FILE_LIMIT as usize + 1]).unwrap();
        assert!(file(&p, "hello.rs").await.unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("你好"));
        for name in [
            "../outside",
            "/etc/passwd",
            "C:/Windows",
            ".git/config",
            ".env",
            "a\\b",
        ] {
            assert!(file(&p, name).await.is_err(), "{name}");
        }
        assert_eq!(
            file(&p, "binary").await.unwrap_err().0,
            ReplyKind::UnsupportedContent
        );
        assert_eq!(
            file(&p, "large").await.unwrap_err().0,
            ReplyKind::LimitExceeded
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn porcelain_keeps_rename_and_space_paths() {
        let files = parse_status(
            "R  new name.rs\0old name.rs\0 M src/main.rs\0?? added.rs\0?? .env\0",
            "",
        );
        assert_eq!(files.len(), 3);
        assert_eq!(files[0]["originalPath"], "old name.rs");
        assert_eq!(files[0]["staged"], true);
        assert_eq!(files[1]["unstaged"], true);
    }

    #[tokio::test]
    async fn git_path_requests_cannot_expand_into_hidden_files_or_directories() {
        let root = fixture();
        let p = Project::new(&root).unwrap();
        git(&p, &["init"]).await.unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join(".env"), "PRIVATE_BEFORE\n").unwrap();
        std::fs::write(root.join("src/.env"), "PRIVATE_BEFORE\n").unwrap();
        git(&p, &["add", "--force", ".env", "src/.env"])
            .await
            .unwrap();
        std::fs::write(root.join(".env"), "PRIVATE_AFTER\n").unwrap();
        std::fs::write(root.join("src/.env"), "PRIVATE_AFTER\n").unwrap();
        for requested in ["*", "src"] {
            if let Ok(value) = diff(&p, requested, false).await {
                assert!(
                    !value["patch"].as_str().unwrap().contains("PRIVATE_"),
                    "{requested} expanded into a private file"
                );
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_cannot_alias_hidden_files_inside_the_project() {
        let root = fixture();
        std::fs::write(root.join(".env"), "PRIVATE_CONTENT").unwrap();
        std::os::unix::fs::symlink(root.join(".env"), root.join("alias.txt")).unwrap();
        std::fs::create_dir(root.join(".deploy")).unwrap();
        std::fs::write(root.join(".deploy/access"), "PRIVATE_CONTENT").unwrap();
        std::os::unix::fs::symlink(root.join(".deploy"), root.join("config")).unwrap();
        let p = Project::new(&root).unwrap();
        assert!(file(&p, "alias.txt").await.is_err());
        assert!(files(&p, "config").await.is_err());
        assert!(file(&p, "config/access").await.is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn uploads_are_saved_under_project_metadata_without_overwriting_names() {
        let root = fixture();
        let project = Project::new(&root).unwrap();
        let first = upload(&project, "photo.png", b"one").await.unwrap();
        let second = upload(&project, "photo.png", b"two").await.unwrap();
        assert_ne!(first["path"], second["path"]);
        assert_eq!(first["name"], "photo.png");
        assert_eq!(
            std::fs::read(root.join(first["path"].as_str().unwrap())).unwrap(),
            b"one"
        );
        assert_eq!(
            std::fs::read(root.join(second["path"].as_str().unwrap())).unwrap(),
            b"two"
        );
        assert!(upload(&project, "../secret.txt", b"nope").await.is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn nested_projects_get_paths_relative_to_the_project() {
        let root = fixture();
        let parent = Project::new(&root).unwrap();
        git(&parent, &["init"]).await.unwrap();
        std::fs::create_dir(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/new.txt"), "new\n").unwrap();
        let p = Project::new(&root.join("nested")).unwrap();
        let value = status(&p).await.unwrap();
        assert_eq!(value["files"][0]["path"], "new.txt");
        assert!(diff(&p, "new.txt", false).await.unwrap()["patch"]
            .as_str()
            .unwrap()
            .contains("+new"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn git_treats_metacharacters_as_literal_file_names() {
        let root = fixture();
        let p = Project::new(&root).unwrap();
        git(&p, &["init"]).await.unwrap();
        let status = status(&p).await.unwrap();
        assert_ne!(status["branch"], "HEAD");
        for name in ["sample[1].txt", "sample1.txt"] {
            std::fs::write(root.join(name), "before\n").unwrap();
        }
        git(&p, &["add", "."]).await.unwrap();
        std::fs::write(root.join("sample[1].txt"), "literal file\n").unwrap();
        std::fs::write(root.join("sample1.txt"), "other file\n").unwrap();
        let value = diff(&p, "sample[1].txt", false).await.unwrap();
        assert!(value["patch"].as_str().unwrap().contains("+literal file"));
        assert!(!value["patch"].as_str().unwrap().contains("+other file"));
        std::fs::remove_file(root.join("sample[1].txt")).unwrap();
        assert!(diff(&p, "sample[1].txt", false).await.unwrap()["patch"]
            .as_str()
            .unwrap()
            .contains("-before"));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn git_status_identifies_linked_worktrees_and_detached_heads() {
        let root = fixture();
        let project = Project::new(&root).unwrap();
        assert_eq!(status(&project).await.unwrap()["isRepository"], false);
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
            vec!["commit", "--allow-empty", "-m", "initial"],
        ] {
            assert!(git(&project, &args).await.unwrap().0);
        }
        assert_eq!(status(&project).await.unwrap()["isWorktree"], false);
        let linked = root.join("linked");
        let linked_path = linked.to_str().unwrap();
        assert!(
            git(
                &project,
                &["worktree", "add", "-b", "test-worktree", linked_path]
            )
            .await
            .unwrap()
            .0
        );
        std::fs::create_dir(linked.join("src")).unwrap();
        let nested = Project::new(&linked.join("src")).unwrap();
        let info = status(&nested).await.unwrap();
        assert_eq!(info["isWorktree"], true);
        assert_eq!(info["branch"], "test-worktree");
        assert_eq!(
            Path::new(info["worktreePath"].as_str().unwrap())
                .canonicalize()
                .unwrap(),
            linked.canonicalize().unwrap()
        );
        assert!(git(&nested, &["checkout", "--detach"]).await.unwrap().0);
        let info = status(&nested).await.unwrap();
        assert_eq!(info["isDetached"], true);
        assert_ne!(info["branch"], "HEAD");
        assert!(
            git(&project, &["worktree", "remove", "--force", linked_path])
                .await
                .unwrap()
                .0
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The reported failure: an agent creates a linked worktree as a sibling
    /// (`git worktree add ../feature`) and writes its output files there. The
    /// web preview must read that checkout, and only that checkout, without
    /// widening the project root.
    #[tokio::test]
    async fn linked_worktrees_are_browsable_preview_roots() {
        let root = fixture();
        let project = Project::new(&root).unwrap();
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
            vec!["commit", "--allow-empty", "-m", "initial"],
        ] {
            assert!(git(&project, &args).await.unwrap().0);
        }
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "main checkout\n").unwrap();
        // Sibling worktree, the layout every `git worktree add ../name` produces.
        let linked = root.with_extension("worktree");
        assert!(
            git(
                &project,
                &["worktree", "add", "-b", "feature", linked.to_str().unwrap()]
            )
            .await
            .unwrap()
            .0
        );
        std::fs::create_dir(linked.join("src")).unwrap();
        std::fs::write(linked.join("src/main.rs"), "worktree checkout\n").unwrap();

        let canonical = std::fs::canonicalize(&linked).unwrap();
        assert_eq!(worktrees(&project).await, vec![canonical.clone()]);
        assert_eq!(
            worktree_paths(&project).await,
            vec![display_root(&canonical)]
        );
        let branch = display_root(&canonical);

        // A project rooted in a subdirectory keeps its own boundary: the
        // enclosing checkout is not an extra root, the sibling worktree is.
        let nested_project = Project::new(&root.join("src")).unwrap();
        assert_eq!(worktrees(&nested_project).await, vec![canonical.clone()]);
        assert!(browse_root(&nested_project, &root.to_string_lossy())
            .await
            .is_err());

        // The same relative path resolves independently per root: the project
        // root keeps its own content and the worktree keeps its output.
        assert_eq!(
            file(&project, "src/main.rs").await.unwrap()["content"],
            "main checkout\n"
        );
        assert_eq!(
            file_at(&project, &branch, "src/main.rs").await.unwrap()["content"],
            "worktree checkout\n"
        );
        // The worktree is still held to the same relative-path boundary.
        for relative in [
            "../src/main.rs",
            "/etc/passwd",
            ".git",
            ".rpi/sessions/a.jsonl",
        ] {
            assert!(
                file_at(&project, &branch, relative).await.is_err(),
                "{relative}"
            );
        }
        // A root that is not this project's linked worktree is refused.
        for denied in [
            linked.join("src").to_string_lossy().into_owned(),
            linked.join("missing").to_string_lossy().into_owned(),
            std::env::temp_dir().to_string_lossy().into_owned(),
        ] {
            assert!(
                file_at(&project, &denied, "src/main.rs").await.is_err(),
                "{denied}"
            );
            assert!(browse_root(&project, &denied).await.is_err(), "{denied}");
        }
        assert!(file_at(&project, &branch, "missing.rs").await.is_err());
        assert_eq!(browse_root(&project, "").await.unwrap(), project.root);
        assert_eq!(browse_root(&project, &branch).await.unwrap(), canonical);

        assert!(
            git(
                &project,
                &["worktree", "remove", "--force", linked.to_str().unwrap()]
            )
            .await
            .unwrap()
            .0
        );
        assert!(worktrees(&project).await.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn git_previews_staged_unstaged_and_untracked_changes() {
        let root = fixture();
        let p = Project::new(&root).unwrap();
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
        ] {
            assert!(git(&p, &args).await.unwrap().0);
        }
        std::fs::write(root.join("sample.txt"), "before\n").unwrap();
        assert!(git(&p, &["add", "sample.txt"]).await.unwrap().0);
        assert!(git(&p, &["commit", "-m", "initial"]).await.unwrap().0);
        std::fs::write(root.join("sample.txt"), "staged\n").unwrap();
        git(&p, &["add", "sample.txt"]).await.unwrap();
        std::fs::write(root.join("sample.txt"), "working\n").unwrap();
        std::fs::write(root.join("new file.txt"), "new\n").unwrap();
        assert!(diff(&p, "sample.txt", true).await.unwrap()["patch"]
            .as_str()
            .unwrap()
            .contains("+staged"));
        assert!(diff(&p, "sample.txt", false).await.unwrap()["patch"]
            .as_str()
            .unwrap()
            .contains("+working"));
        assert!(diff(&p, "new file.txt", false).await.unwrap()["patch"]
            .as_str()
            .unwrap()
            .contains("+new"));
        assert_eq!(
            status(&p).await.unwrap()["files"].as_array().unwrap().len(),
            2
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_cannot_escape_the_project() {
        let root = fixture();
        let outside = fixture();
        std::fs::write(outside.join("secret"), "secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let p = Project::new(&root).unwrap();
        assert!(file(&p, "link/secret").await.is_err());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
}
