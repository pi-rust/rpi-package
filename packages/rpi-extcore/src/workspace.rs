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
        let display = root
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_string();
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

pub async fn files(project: &Project, relative: &str) -> Reply {
    let path = resolve(&project.root, relative)?;
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
    let mut directory = tokio::fs::read_dir(path)
        .await
        .map_err(|_| failure(ReplyKind::NotFound, "directory not found"))?;
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

pub async fn file(project: &Project, relative: &str) -> Reply {
    let path = resolve(&project.root, relative)?;
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
