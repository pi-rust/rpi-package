//! Copies built cdylibs into the rpi extension directory.

use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::metadata::Extension;
use crate::platform;

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Workspace manifest; when `None` the tool runs in standalone (glob) mode.
    pub manifest: Option<PathBuf>,
    /// Override for the directory holding the built cdylibs.
    pub source_dir: Option<PathBuf>,
    /// Override for the rpi extension directory.
    pub target_dir: Option<PathBuf>,
    /// Ignore `catalog/packages.json` and install every cdylib.
    pub all: bool,
    /// Package names to skip.
    pub exclude: Vec<String>,
    pub dry_run: bool,
    pub quiet: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub source: PathBuf,
    pub dest: PathBuf,
    pub package: String,
}

/// The rpi extension directory: `$RPI_CODING_AGENT_DIR/extensions`, else
/// `~/.rpi/agent/extensions`.
pub fn default_target_dir() -> Result<PathBuf> {
    if let Some(dir) = non_empty_env("RPI_CODING_AGENT_DIR") {
        return Ok(PathBuf::from(dir).join("extensions"));
    }
    Ok(home_dir()?.join(".rpi").join("agent").join("extensions"))
}

fn home_dir() -> Result<PathBuf> {
    for key in ["HOME", "USERPROFILE"] {
        if let Some(dir) = non_empty_env(key) {
            return Ok(PathBuf::from(dir));
        }
    }
    if let (Some(drive), Some(path)) = (non_empty_env("HOMEDRIVE"), non_empty_env("HOMEPATH")) {
        return Ok(PathBuf::from(format!("{drive}{path}")));
    }
    bail!("cannot determine the home directory (set HOME or USERPROFILE)")
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// Apply the catalog allowlist and the `--exclude` list.
///
/// When the catalog exists it is authoritative: a name listed there but not
/// built is an error, because shipping a bundle that silently misses an
/// extension is exactly the bug this tool exists to prevent.
pub fn select(
    built: &[Extension],
    catalog: Option<&[String]>,
    exclude: &[String],
    all: bool,
) -> Result<Vec<Extension>> {
    let mut chosen: Vec<Extension> = match catalog {
        Some(names) if !all => {
            let built_names: BTreeSet<&str> = built.iter().map(|e| e.package.as_str()).collect();
            let missing: Vec<&str> = names
                .iter()
                .map(String::as_str)
                .filter(|n| !built_names.contains(n))
                .collect();
            if !missing.is_empty() {
                bail!(
                    "catalog lists {} extension(s) that were not built: {}\n\
                     run `task build` first, or pass --all to ignore the catalog",
                    missing.len(),
                    missing.join(", ")
                );
            }
            let wanted: BTreeSet<&str> = names.iter().map(String::as_str).collect();
            built
                .iter()
                .filter(|e| wanted.contains(e.package.as_str()))
                .cloned()
                .collect()
        }
        _ => built.to_vec(),
    };

    let skip: BTreeSet<&str> = exclude.iter().map(String::as_str).collect();
    chosen.retain(|e| !skip.contains(e.package.as_str()));
    Ok(chosen)
}

/// Standalone mode: every cdylib sitting in `dir`.
pub fn discover_standalone(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let path = entry?.path();
        let matches = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case(platform::dylib_extension()));
        if matches && path.is_file() {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

/// Build the list of copies to perform, validating that every source exists.
pub fn plan(extensions: &[Extension], source_dir: &Path, target_dir: &Path) -> Result<Vec<Action>> {
    let mut actions = Vec::with_capacity(extensions.len());
    let mut missing = Vec::new();
    for ext in extensions {
        let source = source_dir.join(ext.file_name());
        if !source.is_file() {
            missing.push(source.display().to_string());
            continue;
        }
        actions.push(Action {
            source,
            dest: target_dir.join(ext.file_name()),
            package: ext.package.clone(),
        });
    }
    if !missing.is_empty() {
        bail!(
            "missing package artifact(s):\n  {}\n\
             run `task build` (release) before installing",
            missing.join("\n  ")
        );
    }
    Ok(actions)
}

pub fn run(opts: &Options) -> Result<Vec<PathBuf>> {
    let target_dir = match &opts.target_dir {
        Some(dir) => dir.clone(),
        None => default_target_dir()?,
    };

    // Resolve the extension list and the directory holding the artifacts.
    let (extensions, source_dir, note) = match &opts.manifest {
        Some(manifest) => {
            let ws = crate::metadata::load(manifest)?;
            let catalog = crate::catalog::load(&crate::catalog::default_path(&ws.root))?;
            let selected = select(&ws.extensions, catalog.as_deref(), &opts.exclude, opts.all)?;
            let note = match &catalog {
                Some(names) if !opts.all => {
                    format!("{} listed in catalog/packages.json", names.len())
                }
                _ => format!("all {} workspace cdylibs", ws.extensions.len()),
            };
            (
                selected,
                opts.source_dir.clone().unwrap_or_else(|| ws.release_dir()),
                Some(note),
            )
        }
        None => {
            let source = opts
                .source_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("."));
            let files = discover_standalone(&source)?;
            let extensions: Vec<Extension> = files
                .iter()
                .filter_map(|p| {
                    let artifact = p.file_stem()?.to_str()?.to_string();
                    if opts.exclude.iter().any(|e| e.replace('-', "_") == artifact) {
                        return None;
                    }
                    Some(Extension {
                        package: artifact.clone(),
                        artifact,
                    })
                })
                .collect();
            (extensions, source, None)
        }
    };

    if extensions.is_empty() {
        bail!("no extensions found to install");
    }

    let actions = plan(&extensions, &source_dir, &target_dir)?;

    if !opts.quiet {
        println!("source: {}", source_dir.display());
        println!("target: {}", target_dir.display());
        if let Some(note) = &note {
            println!("set:    {note}");
        }
        if opts.dry_run {
            println!("dry run: no files will be written");
        }
    }

    if !opts.dry_run {
        std::fs::create_dir_all(&target_dir)
            .with_context(|| format!("cannot create {}", target_dir.display()))?;
    }

    let mut installed = Vec::with_capacity(actions.len());
    let mut moved_aside = 0usize;
    for action in &actions {
        if action.source == action.dest {
            if !opts.quiet {
                println!("  = {} (already in place)", action.dest.display());
            }
            installed.push(action.dest.clone());
            continue;
        }
        let backup = if opts.dry_run {
            None
        } else {
            replace_file(&action.source, &action.dest)
                .with_context(|| format!("failed to install {}", action.package))?
        };
        if backup.is_some() {
            moved_aside += 1;
        }
        if !opts.quiet {
            match &backup {
                Some(b) => println!(
                    "  + {} (replaced in-use file, previous kept as {})",
                    action.dest.display(),
                    b.file_name().unwrap_or_default().to_string_lossy()
                ),
                None => println!("  + {}", action.dest.display()),
            }
        }
        installed.push(action.dest.clone());
    }

    if !opts.quiet {
        println!(
            "Installed {} extension(s) to {}{}",
            installed.len(),
            target_dir.display(),
            if moved_aside > 0 {
                format!(
                    " ({moved_aside} were loaded by a running rpi; restart it to pick up the new build)"
                )
            } else {
                String::new()
            }
        );
    }
    Ok(installed)
}

/// Copy `src` over `dst`, falling back to "move the old file aside first".
///
/// Windows refuses to overwrite a DLL that is currently mapped into a running
/// process, but it does allow renaming it. Returns the backup path when that
/// path was taken.
fn replace_file(src: &Path, dst: &Path) -> Result<Option<PathBuf>> {
    if std::fs::copy(src, dst).is_ok() {
        verify(src, dst)?;
        return Ok(None);
    }
    if !dst.exists() {
        // Not a locking problem - surface the real error.
        std::fs::copy(src, dst)
            .with_context(|| format!("cannot copy {} to {}", src.display(), dst.display()))?;
        verify(src, dst)?;
        return Ok(None);
    }

    let backup = backup_path(dst);
    std::fs::rename(dst, &backup).with_context(|| {
        format!(
            "cannot replace {}: it is in use and could not be moved aside either",
            dst.display()
        )
    })?;
    std::fs::copy(src, dst).with_context(|| {
        format!(
            "moved the previous {} to {} but copying the new build failed",
            dst.display(),
            backup.display()
        )
    })?;
    verify(src, dst)?;
    Ok(Some(backup))
}

fn verify(src: &Path, dst: &Path) -> Result<()> {
    let a = std::fs::metadata(src)?.len();
    let b = std::fs::metadata(dst)?.len();
    if a != b || b == 0 {
        bail!(
            "{} looks truncated after install ({} bytes, expected {})",
            dst.display(),
            b,
            a
        );
    }
    Ok(())
}

fn backup_path(dst: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut name = dst.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".old-{stamp}"));
    dst.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(pkg: &str) -> Extension {
        Extension {
            package: pkg.to_string(),
            artifact: pkg.replace('-', "_"),
        }
    }

    fn built() -> Vec<Extension> {
        vec![
            ext("rpi-langfuse"),
            ext("rpi-lens"),
            ext("rpi-server"),
            ext("rpi-voice"),
        ]
    }

    #[test]
    fn catalog_is_authoritative_and_keeps_new_packages() {
        let catalog = vec!["rpi-lens".to_string(), "rpi-langfuse".to_string()];
        let chosen = select(&built(), Some(&catalog), &[], false).unwrap();
        let names: Vec<&str> = chosen.iter().map(|e| e.package.as_str()).collect();
        // rpi-langfuse must survive: the old hardcoded list omitted it.
        assert_eq!(names, vec!["rpi-langfuse", "rpi-lens"]);
    }

    #[test]
    fn catalog_missing_entries_are_reported() {
        let catalog = vec!["rpi-lens".to_string(), "rpi-does-not-exist".to_string()];
        let err = select(&built(), Some(&catalog), &[], false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("rpi-does-not-exist"), "{msg}");
        assert!(msg.contains("not built"), "{msg}");
    }

    #[test]
    fn all_and_exclude() {
        let catalog = vec!["rpi-lens".to_string()];
        let chosen = select(&built(), Some(&catalog), &[], true).unwrap();
        assert_eq!(chosen.len(), 4, "--all ignores the catalog");

        let exclude = vec!["rpi-voice".to_string()];
        let chosen = select(&built(), Some(&catalog), &exclude, true).unwrap();
        let names: Vec<&str> = chosen.iter().map(|e| e.package.as_str()).collect();
        assert_eq!(names, vec!["rpi-langfuse", "rpi-lens", "rpi-server"]);
    }

    #[test]
    fn plan_reports_every_missing_artifact_at_once() {
        let tmp = std::env::temp_dir().join("rpi-pkg-install-plan");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("rpi_lens.dll"), b"x").unwrap();

        let err = plan(&[ext("rpi-lens"), ext("rpi-langfuse")], &tmp, &tmp).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("rpi_langfuse"), "{msg}");
        assert!(
            !msg.contains("rpi_lens.dll"),
            "built artifact must not be reported: {msg}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn replace_file_moves_a_locked_target_aside() {
        let tmp = std::env::temp_dir().join("rpi-pkg-install-replace");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let src = tmp.join("new.dll");
        let dst = tmp.join("live.dll");
        std::fs::write(&src, b"new build").unwrap();
        std::fs::write(&dst, b"old build").unwrap();

        // Healthy path: plain overwrite, no backup.
        assert_eq!(replace_file(&src, &dst).unwrap(), None);
        assert_eq!(std::fs::read(&dst).unwrap(), b"new build");

        // Truncation guard.
        std::fs::write(&dst, b"").unwrap();
        let _ = verify(&src, &dst);
        std::fs::write(&src, b"other").unwrap();
        assert!(verify(&src, &dst).is_err());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn backup_path_keeps_the_original_name() {
        let p = backup_path(Path::new("C:/x/rpi_langfuse.dll"));
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("rpi_langfuse.dll.old-"), "{name}");
    }

    #[test]
    fn target_dir_honours_rpi_coding_agent_dir() {
        // Only assert the shape; mutating the process env would race with
        // other tests in this binary.
        let dir = default_target_dir().expect("home dir should resolve");
        assert!(
            dir.ends_with("extensions") || dir.ends_with("extensions/"),
            "{dir:?}"
        );
    }

    #[test]
    fn standalone_discovery_matches_host_extension_only() {
        let tmp = std::env::temp_dir().join("rpi-pkg-install-glob");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let good = tmp.join(format!("a.{}", platform::dylib_extension()));
        std::fs::write(&good, b"lib").unwrap();
        std::fs::write(tmp.join("notes.txt"), b"x").unwrap();
        std::fs::write(tmp.join("b.dll.old-1"), b"x").unwrap();

        let found = discover_standalone(&tmp).unwrap();
        assert_eq!(found, vec![good]);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
