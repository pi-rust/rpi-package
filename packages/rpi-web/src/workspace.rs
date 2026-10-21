//! Read-only access to explicitly configured projects. Never accept arbitrary roots from HTTP.
use axum::{http::StatusCode, Json};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};

const FILE_LIMIT: u64 = 1024 * 1024;
const OUTPUT_LIMIT: u64 = 2 * 1024 * 1024;
pub type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

#[derive(Clone, Debug, Serialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
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
            root,
        })
    }
}

pub fn failure(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error": message})))
}

fn hidden(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        ".git" | ".rpi" | "node_modules" | "target" | ".deploy"
    ) || name.to_ascii_lowercase().starts_with(".env")
}

/// Check components before resolving, then check the canonical target/ancestor.
/// The ancestor check also covers deleted and untracked paths used by Git diff.
fn resolve(root: &Path, relative: &str) -> Result<PathBuf, (StatusCode, Json<Value>)> {
    if relative.contains(['\\', '\0', ':']) {
        return Err(failure(StatusCode::BAD_REQUEST, "invalid project path"));
    }
    let path = Path::new(relative);
    for part in path.components() {
        match part {
            Component::Normal(name) if !hidden(&name.to_string_lossy()) => {}
            _ => {
                return Err(failure(
                    StatusCode::FORBIDDEN,
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
                    return Err(failure(StatusCode::FORBIDDEN, "path leaves the project"));
                }
                let inside = actual.strip_prefix(root).unwrap();
                if inside.components().any(|part| matches!(part, Component::Normal(name) if hidden(&name.to_string_lossy()))) {
                    return Err(failure(StatusCode::FORBIDDEN, "path is outside the browsable project"));
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
                    .ok_or_else(|| failure(StatusCode::NOT_FOUND, "path not found"))?;
            }
            Err(_) => return Err(failure(StatusCode::FORBIDDEN, "path is not accessible")),
        }
    }
}

pub async fn files(project: &Project, relative: &str) -> Reply {
    let path = resolve(&project.root, relative)?;
    let mut directory = tokio::fs::read_dir(path)
        .await
        .map_err(|_| failure(StatusCode::NOT_FOUND, "directory not found"))?;
    let mut entries = Vec::new();
    while let Some(entry) = directory
        .next_entry()
        .await
        .map_err(|_| failure(StatusCode::FORBIDDEN, "cannot read directory"))?
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if hidden(&name) {
            continue;
        }
        let kind = entry
            .file_type()
            .await
            .map_err(|_| failure(StatusCode::FORBIDDEN, "cannot read file type"))?;
        if kind.is_symlink() || !(kind.is_file() || kind.is_dir()) {
            continue;
        }
        let metadata = entry.metadata().await.ok();
        entries.push(json!({"name":name,"path":if relative.is_empty() { name.clone() } else { format!("{relative}/{name}") },
            "kind":if kind.is_dir() {"directory"} else {"file"},"size":metadata.map(|m|m.len()).unwrap_or(0)}));
        if entries.len() > 2000 {
            return Err(failure(
                StatusCode::PAYLOAD_TOO_LARGE,
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
    Ok(Json(json!({"path":relative,"entries":entries})))
}

pub async fn file(project: &Project, relative: &str) -> Reply {
    let path = resolve(&project.root, relative)?;
    // Reject devices/FIFOs before opening: opening one may otherwise block indefinitely.
    match tokio::fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err(failure(StatusCode::BAD_REQUEST, "select a regular file")),
        Err(_) => return Err(failure(StatusCode::NOT_FOUND, "file not found")),
    }
    let handle = tokio::fs::File::open(path)
        .await
        .map_err(|_| failure(StatusCode::NOT_FOUND, "file not found"))?;
    if !handle
        .metadata()
        .await
        .map(|m| m.is_file())
        .unwrap_or(false)
    {
        return Err(failure(StatusCode::BAD_REQUEST, "select a regular file"));
    }
    let mut bytes = Vec::new();
    handle
        .take(FILE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| failure(StatusCode::FORBIDDEN, "cannot read file"))?;
    if bytes.len() as u64 > FILE_LIMIT {
        return Err(failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "preview supports files up to 1 MiB",
        ));
    }
    if bytes.contains(&0) {
        return Err(failure(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "binary files cannot be previewed",
        ));
    }
    let content = String::from_utf8(bytes).map_err(|_| {
        failure(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "preview supports UTF-8 text files",
        )
    })?;
    Ok(Json(json!({"path":relative,"content":content})))
}

async fn git(
    project: &Project,
    args: &[&str],
) -> Result<(bool, String), (StatusCode, Json<Value>)> {
    let mut child = Command::new("git")
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
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| {
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
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
    .map_err(|_| failure(StatusCode::GATEWAY_TIMEOUT, "Git request timed out"))?
    .map_err(|_| failure(StatusCode::INTERNAL_SERVER_ERROR, "Git request failed"))?;
    result.ok_or_else(|| {
        failure(
            StatusCode::PAYLOAD_TOO_LARGE,
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
    let (valid, _) = git(project, &["rev-parse", "--show-toplevel"]).await?;
    if !valid {
        return Ok(Json(json!({"isRepository":false,"files":[],"branch":null})));
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
        return Err(failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "cannot read Git status",
        ));
    }
    let (named_branch, mut branch) =
        git(project, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await?;
    if !named_branch {
        branch = git(project, &["rev-parse", "--short", "HEAD"]).await?.1;
    }
    let (_, prefix) = git(project, &["rev-parse", "--show-prefix"]).await?;
    Ok(Json(
        json!({"isRepository":true,"branch":branch.trim(),"files":parse_status(&output, prefix.trim_end_matches(['\r', '\n']))}),
    ))
}

pub async fn diff(project: &Project, relative: &str, staged: bool) -> Reply {
    if relative.is_empty() {
        return Err(failure(StatusCode::BAD_REQUEST, "select a changed file"));
    }
    let path = resolve(&project.root, relative)?;
    match tokio::fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err(failure(StatusCode::BAD_REQUEST, "select a changed file")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A deleted file is valid, but a deleted directory is still a recursive pathspec.
            let (_, tracked) =
                git(project, &["ls-files", "--full-name", "-z", "--", relative]).await?;
            let (_, prefix) = git(project, &["rev-parse", "--show-prefix"]).await?;
            let expected = format!("{}{relative}", prefix.trim_end_matches(['\r', '\n']));
            if !tracked.split('\0').any(|name| name == expected) {
                return Err(failure(StatusCode::BAD_REQUEST, "select a changed file"));
            }
        }
        Err(_) => return Err(failure(StatusCode::FORBIDDEN, "file is not accessible")),
    }
    let (valid, _) = git(project, &["rev-parse", "--show-toplevel"]).await?;
    if !valid {
        return Err(failure(
            StatusCode::BAD_REQUEST,
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
        return Err(failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "cannot read Git diff",
        ));
    }
    if patch.is_empty() && !staged {
        let (_, output) = git(project, &["status", "--porcelain=v1", "-z", "--", relative]).await?;
        if output.starts_with("?? ") {
            let Json(value) = file(project, relative).await?;
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
    Ok(Json(json!({"path":relative,"staged":staged,"patch":patch})))
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
            StatusCode::PAYLOAD_TOO_LARGE
        );
        std::fs::write(root.join("large.txt"), "after\n").unwrap();
        assert!(diff(&project, "large.txt", false).await.unwrap().0["patch"]
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
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn previews_are_scoped_bounded_and_text_only() {
        let root = fixture();
        let p = Project::new(&root).unwrap();
        std::fs::write(root.join("hello.rs"), "你好\nfn main() {}\n").unwrap();
        std::fs::write(root.join("binary"), [0, 1, 2]).unwrap();
        std::fs::write(root.join("large"), vec![b'a'; FILE_LIMIT as usize + 1]).unwrap();
        assert!(file(&p, "hello.rs").await.unwrap().0["content"]
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
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        assert_eq!(
            file(&p, "large").await.unwrap_err().0,
            StatusCode::PAYLOAD_TOO_LARGE
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
            if let Ok(Json(value)) = diff(&p, requested, false).await {
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
    async fn nested_projects_get_paths_relative_to_the_project() {
        let root = fixture();
        let parent = Project::new(&root).unwrap();
        git(&parent, &["init"]).await.unwrap();
        std::fs::create_dir(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/new.txt"), "new\n").unwrap();
        let p = Project::new(&root.join("nested")).unwrap();
        let Json(value) = status(&p).await.unwrap();
        assert_eq!(value["files"][0]["path"], "new.txt");
        assert!(diff(&p, "new.txt", false).await.unwrap().0["patch"]
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
        let Json(status) = status(&p).await.unwrap();
        assert_ne!(status["branch"], "HEAD");
        for name in ["sample[1].txt", "sample1.txt"] {
            std::fs::write(root.join(name), "before\n").unwrap();
        }
        git(&p, &["add", "."]).await.unwrap();
        std::fs::write(root.join("sample[1].txt"), "literal file\n").unwrap();
        std::fs::write(root.join("sample1.txt"), "other file\n").unwrap();
        let Json(value) = diff(&p, "sample[1].txt", false).await.unwrap();
        assert!(value["patch"].as_str().unwrap().contains("+literal file"));
        assert!(!value["patch"].as_str().unwrap().contains("+other file"));
        std::fs::remove_file(root.join("sample[1].txt")).unwrap();
        assert!(diff(&p, "sample[1].txt", false).await.unwrap().0["patch"]
            .as_str()
            .unwrap()
            .contains("-before"));
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
        assert!(diff(&p, "sample.txt", true).await.unwrap().0["patch"]
            .as_str()
            .unwrap()
            .contains("+staged"));
        assert!(diff(&p, "sample.txt", false).await.unwrap().0["patch"]
            .as_str()
            .unwrap()
            .contains("+working"));
        assert!(diff(&p, "new file.txt", false).await.unwrap().0["patch"]
            .as_str()
            .unwrap()
            .contains("+new"));
        assert_eq!(
            status(&p).await.unwrap().0["files"]
                .as_array()
                .unwrap()
                .len(),
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
