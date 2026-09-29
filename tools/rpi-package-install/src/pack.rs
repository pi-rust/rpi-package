//! Builds the distributable bundle: staged extension cdylibs + catalog + the
//! installer binary itself, so a bundle can be installed without any scripts.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::install;
use crate::platform;

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub manifest: Option<PathBuf>,
    pub source_dir: Option<PathBuf>,
    /// Where `dist/` lives.
    pub out_dir: Option<PathBuf>,
    pub all: bool,
    pub skip_archive: bool,
}

pub struct Outcome {
    pub stage_dir: PathBuf,
    pub archive: Option<PathBuf>,
    pub count: usize,
}

pub fn run(opts: &Options) -> Result<Outcome> {
    let workspace_root = match &opts.manifest {
        Some(manifest) => crate::metadata::load(manifest)?.root,
        None => PathBuf::from("."),
    };
    let out_dir = opts
        .out_dir
        .clone()
        .unwrap_or_else(|| workspace_root.join("dist"));

    let slug = platform::slug();
    let stage_dir = out_dir.join(format!("rpi-packages-{slug}"));
    if stage_dir.exists() {
        std::fs::remove_dir_all(&stage_dir)
            .with_context(|| format!("cannot clear {}", stage_dir.display()))?;
    }
    std::fs::create_dir_all(&stage_dir)
        .with_context(|| format!("cannot create {}", stage_dir.display()))?;

    // Stage the extension libraries through the same code path `install` uses.
    let installed = install::run(&install::Options {
        manifest: opts.manifest.clone(),
        source_dir: opts.source_dir.clone(),
        target_dir: Some(stage_dir.clone()),
        all: opts.all,
        exclude: Vec::new(),
        dry_run: false,
        quiet: true,
    })?;

    for extra in ["README.md"] {
        let from = workspace_root.join(extra);
        if from.is_file() {
            std::fs::copy(&from, stage_dir.join(extra))
                .with_context(|| format!("cannot stage {extra}"))?;
        }
    }
    let catalog = crate::catalog::default_path(&workspace_root);
    if catalog.is_file() {
        std::fs::create_dir_all(stage_dir.join("catalog"))?;
        std::fs::copy(&catalog, stage_dir.join("catalog").join("packages.json"))
            .context("cannot stage catalog/packages.json")?;
    }

    // Ship the installer itself: the bundle must be self-sufficient, which is
    // what the old install.ps1 / install.sh duo used to provide.
    let installer = install_installer(&stage_dir)?;

    if opts.skip_archive {
        return Ok(Outcome {
            stage_dir,
            archive: None,
            count: installed.len(),
        });
    }

    let archive = out_dir.join(platform::archive_name());
    let stage_name = stage_dir
        .file_name()
        .context("stage directory has no name")?
        .to_string_lossy()
        .to_string();
    crate::archive::create(&stage_dir, &stage_name, &archive)?;

    println!(
        "staged {} extension(s) -> {}",
        installed.len(),
        stage_dir.display()
    );
    if let Some(name) = installer {
        println!("bundled installer        -> {name}");
    }
    println!("archive                  -> {}", archive.display());

    Ok(Outcome {
        stage_dir,
        archive: Some(archive),
        count: installed.len(),
    })
}

/// Copy the running executable into the bundle, if we are running from a build
/// that is not itself inside the stage directory.
fn install_installer(stage_dir: &Path) -> Result<Option<String>> {
    let current = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    if !current.is_file() || current.starts_with(stage_dir) {
        return Ok(None);
    }
    let name = format!("rpi-package-install.{}", platform::exe_extension());
    let name = name.trim_end_matches('.').to_string();
    let dest = stage_dir.join(&name);
    if std::fs::copy(&current, &dest).is_err() {
        // A locked/undeletable binary should not fail the whole pack; the
        // archives are still useful with a system-installed tool.
        return Ok(None);
    }
    Ok(Some(name))
}

/// Install from a downloaded bundle: extract, then install the archives inside.
pub fn install_bundle(archive: &Path, target_dir: Option<PathBuf>) -> Result<Vec<PathBuf>> {
    if !archive.is_file() {
        bail!(
            "bundle not found: {}\n\
             download it first (the tool deliberately has no HTTP client; \
             use curl/Invoke-WebRequest and pass the local path)",
            archive.display()
        );
    }
    let extract_dir = std::env::temp_dir().join(format!(
        "rpi-package-bundle-{}-{}",
        std::process::id(),
        archive
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&extract_dir);
    crate::archive::extract(archive, &extract_dir)?;

    // The archive stores everything under one top-level directory; find the
    // directory that actually holds the libraries.
    let source = source_dir_inside(&extract_dir)?;
    let result = install::run(&install::Options {
        manifest: None,
        source_dir: Some(source),
        target_dir,
        all: true,
        exclude: Vec::new(),
        dry_run: false,
        quiet: false,
    });
    let _ = std::fs::remove_dir_all(&extract_dir);
    result
}

fn source_dir_inside(extract_dir: &Path) -> Result<PathBuf> {
    let mut candidates = vec![extract_dir.to_path_buf()];
    for entry in std::fs::read_dir(extract_dir)
        .with_context(|| format!("cannot read {}", extract_dir.display()))?
        .flatten()
    {
        if entry.path().is_dir() {
            candidates.push(entry.path());
        }
    }
    candidates
        .into_iter()
        .find(|dir| {
            install::discover_standalone(dir)
                .map(|files| !files.is_empty())
                .unwrap_or(false)
        })
        .with_context(|| {
            format!(
                "no extension libraries found inside {}",
                extract_dir.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_dir_inside_finds_the_nested_library_dir() {
        let base = std::env::temp_dir().join("rpi-pkg-install-bundle-src");
        let _ = std::fs::remove_dir_all(&base);
        let nested = base.join("rpi-packages-linux-x86_64");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join(format!("a.{}", platform::dylib_extension())),
            b"lib",
        )
        .unwrap();

        let found = source_dir_inside(&base).unwrap();
        assert_eq!(found, nested);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn source_dir_inside_reports_a_library_free_tree() {
        let base = std::env::temp_dir().join("rpi-pkg-install-bundle-empty");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("empty")).unwrap();
        std::fs::write(base.join("empty").join("README.md"), b"x").unwrap();

        assert!(source_dir_inside(&base).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn install_bundle_hints_at_missing_files() {
        let missing = std::env::temp_dir().join("rpi-pkg-install-no-such-bundle.zip");
        let _ = std::fs::remove_file(&missing);
        let err = install_bundle(&missing, None).unwrap_err().to_string();
        assert!(err.contains("bundle not found"), "{err}");
    }
}
