//! Package a standalone bridge binary as a flat per-platform archive.
//!
//! `pack` produces the extension bundle an rpi host installs. A *bridge* is the
//! other shape: one executable at the archive root that a client (Zed for ACP,
//! any MCP client for MCP) launches directly, without an rpi host loading an
//! extension. Both bridges share this code so their archives cannot drift.

use anyhow::{bail, Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::{archive, metadata, platform};

/// What distinguishes one bridge from another.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    /// Cargo package that builds the binary, used in the "build it first" hint.
    pub package: &'static str,
    /// `[[bin]] name` and executable stem.
    pub binary_stem: &'static str,
    /// Stage directory and archive prefix, for example `rpi-acp`.
    pub prefix: &'static str,
    /// README shipped beside the binary, relative to the workspace root.
    pub readme: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub manifest: Option<PathBuf>,
    /// Where `dist/` lives (default: `<workspace>/dist`).
    pub out_dir: Option<PathBuf>,
    /// Stop after staging, do not create the archive.
    pub skip_archive: bool,
}

pub struct Outcome {
    pub stage_dir: PathBuf,
    pub binary: PathBuf,
    pub archive: Option<PathBuf>,
    pub sha256: Option<String>,
}

/// Resolve the workspace, then stage (and archive) the built binary.
pub fn run(spec: &Spec, opts: &Options) -> Result<Outcome> {
    let manifest = opts
        .manifest
        .clone()
        .unwrap_or_else(|| PathBuf::from("Cargo.toml"));
    let workspace = metadata::load(&manifest)?;
    let root = workspace.root.clone();
    let out_dir = opts.out_dir.clone().unwrap_or_else(|| root.join("dist"));
    let binary_name = platform::exe_name(spec.binary_stem);
    let built = workspace.release_dir().join(&binary_name);
    if !built.is_file() {
        bail!(
            "{} not found; build it first:\n    cargo build --release -p {} --bin {}",
            built.display(),
            spec.package,
            spec.binary_stem
        );
    }
    stage(
        spec,
        &built,
        &root.join(spec.readme),
        &out_dir,
        opts.skip_archive,
    )
}

/// Copy `built` (and its README) into `dist/<prefix>-<slug>/`, then archive it.
///
/// Kept separate from [`run`] so the staging rules are testable without a
/// release build.
pub fn stage(
    spec: &Spec,
    built: &Path,
    readme: &Path,
    out_dir: &Path,
    skip_archive: bool,
) -> Result<Outcome> {
    let binary_name = platform::exe_name(spec.binary_stem);
    let slug = platform::slug();
    let stage_dir = out_dir.join(format!("{}-{slug}", spec.prefix));
    if stage_dir.exists() {
        std::fs::remove_dir_all(&stage_dir)
            .with_context(|| format!("cannot clear {}", stage_dir.display()))?;
    }
    std::fs::create_dir_all(&stage_dir)
        .with_context(|| format!("cannot create {}", stage_dir.display()))?;

    let binary = stage_dir.join(&binary_name);
    std::fs::copy(built, &binary).with_context(|| format!("cannot stage {}", built.display()))?;

    if readme.is_file() {
        std::fs::copy(readme, stage_dir.join("README.md"))
            .with_context(|| format!("cannot stage {}", readme.display()))?;
    }

    if skip_archive {
        println!("staged {binary_name} -> {}", stage_dir.display());
        return Ok(Outcome {
            stage_dir,
            binary,
            archive: None,
            sha256: None,
        });
    }

    let archive = out_dir.join(format!(
        "{}-{slug}.{}",
        spec.prefix,
        platform::archive_extension()
    ));
    archive::create_flat(&stage_dir, &archive)?;
    let sha256 = sha256_file(&archive).ok();

    println!("staged  {binary_name} -> {}", stage_dir.display());
    println!("archive -> {}", archive.display());
    if let Some(sum) = &sha256 {
        println!("sha256  -> {sum}");
    }

    Ok(Outcome {
        stage_dir,
        binary,
        archive: Some(archive),
        sha256,
    })
}

/// SHA-256 of a file, as the lowercase hex the release manifests record.
pub fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut file =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: Spec = Spec {
        package: "rpi-demo",
        binary_stem: "rpi-demo-bridge",
        prefix: "rpi-demo",
        readme: "packages/rpi-demo/README.md",
    };

    struct Temp {
        dir: PathBuf,
    }

    impl Temp {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("rpi-standalone-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            Self { dir }
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn stages_the_binary_and_readme_before_archiving() {
        let temp = Temp::new("stage");
        let built = temp.dir.join(platform::exe_name(SPEC.binary_stem));
        std::fs::write(&built, b"binary").expect("built");
        let readme = temp.dir.join("README.md");
        std::fs::write(&readme, b"docs").expect("readme");

        let outcome = stage(&SPEC, &built, &readme, &temp.dir, true).expect("stage");

        assert!(outcome.archive.is_none());
        assert_eq!(
            outcome.binary.file_name().unwrap(),
            platform::exe_name(SPEC.binary_stem).as_str()
        );
        let staged_readme = outcome.stage_dir.join("README.md");
        assert!(
            staged_readme.is_file(),
            "README is shipped beside the binary"
        );
        assert_eq!(std::fs::read(&staged_readme).unwrap(), b"docs");
    }

    #[test]
    fn archives_are_flat_and_hashed() {
        let temp = Temp::new("archive");
        let built = temp.dir.join(platform::exe_name(SPEC.binary_stem));
        std::fs::write(&built, b"binary").expect("built");
        let readme = temp.dir.join("README.md");
        std::fs::write(&readme, b"docs").expect("readme");

        let outcome = stage(&SPEC, &built, &readme, &temp.dir, false).expect("stage");
        let archive_path = outcome.archive.expect("archive");
        assert!(archive_path.is_file());
        let expected_suffix = format!(
            "{}-{}.{}",
            SPEC.prefix,
            platform::slug(),
            platform::archive_extension()
        );
        assert!(
            archive_path.file_name().unwrap().to_string_lossy() == expected_suffix,
            "{archive_path:?}"
        );
        // Clients launch `./<binary>` from the archive root, so the binary must
        // not be nested under a stage directory.
        let extracted = temp.dir.join("extracted");
        archive::extract(&archive_path, &extracted).expect("extract");
        assert!(
            extracted
                .join(platform::exe_name(SPEC.binary_stem))
                .is_file(),
            "binary is not at the archive root"
        );
    }

    #[test]
    fn a_missing_readme_does_not_fail_staging() {
        let temp = Temp::new("no-readme");
        let built = temp.dir.join(platform::exe_name(SPEC.binary_stem));
        std::fs::write(&built, b"binary").expect("built");
        let outcome =
            stage(&SPEC, &built, &temp.dir.join("absent.md"), &temp.dir, true).expect("stage");
        assert!(!outcome.stage_dir.join("README.md").exists());
    }

    #[test]
    fn sha256_matches_known_vector() {
        // sha256("abc") — the canonical NIST test vector.
        let temp = Temp::new("sha");
        let path = temp.dir.join("abc.txt");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_reports_missing_files() {
        let temp = Temp::new("sha-missing");
        assert!(sha256_file(&temp.dir.join("nope.bin")).is_err());
    }
}
