//! Archive creation/extraction without shelling out to `tar`/`zip`.
//!
//! Shelling out is not an option: on Windows the `tar` on `PATH` may be GNU tar
//! (no zip support) or bsdtar, and `Compress-Archive` means PowerShell - the
//! very dependency this tool removes.

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::path::{Path, PathBuf};

use crate::platform;

/// Create `out` from `stage`, stored under the top-level directory `name`.
///
/// The format follows the file extension: `.zip`, else gzip-compressed tar.
pub fn create(stage: &Path, name: &str, out: &Path) -> Result<()> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    if platform::is_zip(out) {
        create_zip(stage, name, out)
    } else {
        create_tar_gz(stage, name, out)
    }
}

/// Extract `archive` into `dest`. Format follows the file extension.
pub fn extract(archive: &Path, dest: &Path) -> Result<()> {
    if !archive.is_file() {
        bail!("archive not found: {}", archive.display());
    }
    std::fs::create_dir_all(dest)
        .with_context(|| format!("cannot create {}", dest.display()))?;
    if platform::is_zip(archive) {
        extract_zip(archive, dest)
    } else {
        extract_tar_gz(archive, dest)
    }
}

fn create_zip(stage: &Path, name: &str, out: &Path) -> Result<()> {
    let file = File::create(out).with_context(|| format!("cannot create {}", out.display()))?;
    let mut writer = zip::ZipWriter::new(file);
    let mut entries = Vec::new();
    collect(stage, &mut entries)?;
    entries.sort();
    for relative in entries {
        let path = stage.join(&relative);
        let stored = format!("{name}/{}", relative.to_string_lossy().replace('\\', "/"));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(unix_mode(&path));
        if path.is_dir() {
            writer
                .add_directory(format!("{stored}/"), options)
                .with_context(|| format!("cannot add {stored}"))?;
        } else {
            writer
                .start_file(&stored, options)
                .with_context(|| format!("cannot add {stored}"))?;
            let mut source =
                File::open(&path).with_context(|| format!("cannot read {}", path.display()))?;
            std::io::copy(&mut source, &mut writer)
                .with_context(|| format!("cannot write {stored}"))?;
        }
    }
    writer.finish().context("cannot finalize zip")?;
    Ok(())
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {
    let file =
        File::open(archive).with_context(|| format!("cannot open {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("{} is not a valid zip archive", archive.display()))?;
    // `enclosed_name` inside the zip crate rejects entries that would escape
    // `dest`, so extracting our own archives is safe.
    zip.extract(dest)
        .with_context(|| format!("cannot extract {}", archive.display()))?;
    Ok(())
}

fn create_tar_gz(stage: &Path, name: &str, out: &Path) -> Result<()> {
    let file = File::create(out).with_context(|| format!("cannot create {}", out.display()))?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder
        .append_dir_all(name, stage)
        .with_context(|| format!("cannot add {} to the archive", stage.display()))?;
    builder
        .into_inner()
        .context("cannot finalize tar")?
        .finish()
        .context("cannot finalize gzip")?;
    Ok(())
}

fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<()> {
    let file =
        File::open(archive).with_context(|| format!("cannot open {}", archive.display()))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    tar.unpack(dest)
        .with_context(|| format!("cannot extract {}", archive.display()))?;
    Ok(())
}

/// Recursively list everything below `root`, as paths relative to `root`.
fn collect(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    collect_into(root, root, out)
}

fn collect_into(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let path = entry?.path();
        // Relative to `root`, not to `dir`: nested entries must keep their
        // parent directories or the archive ends up flat.
        let relative = path
            .strip_prefix(root)
            .context("entry is not below the stage directory")?
            .to_path_buf();
        if path.is_dir() {
            out.push(relative);
            collect_into(root, &path, out)?;
        } else {
            out.push(relative);
        }
    }
    Ok(())
}

/// Files should be readable; executables need the execute bit so a bundle can
/// be unpacked on Unix and run straight away.
#[cfg(unix)]
fn unix_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o644)
}

#[cfg(not(unix))]
fn unix_mode(path: &Path) -> u32 {
    let executable = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    if executable {
        0o755
    } else {
        0o644
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build `<tmp>/rpi-pkg-install-<tag>/stage` plus a sibling archive path.
    /// Every test gets its own tree so the parallel test runner cannot race.
    fn fixture(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("rpi-pkg-install-archive-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let stage = base.join("stage");
        std::fs::create_dir_all(stage.join("nested")).unwrap();
        std::fs::write(stage.join("a.txt"), b"alpha").unwrap();
        std::fs::write(stage.join("nested").join("b.txt"), b"beta").unwrap();
        (base, stage)
    }

    fn round_trip(tag: &str, archive_name: &str) {
        let (base, stage) = fixture(tag);
        let archive = base.join(archive_name);
        create(&stage, "stage", &archive).unwrap();
        assert!(archive.is_file(), "{} was not created", archive.display());

        let dest = base.join("extracted");
        extract(&archive, &dest).unwrap();

        let root = dest.join("stage");
        assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), b"alpha");
        assert_eq!(
            std::fs::read(root.join("nested").join("b.txt")).unwrap(),
            b"beta"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn zip_round_trip() {
        round_trip("zip", "out.zip");
    }

    #[test]
    fn zip_preserves_nested_paths() {
        let (base, stage) = fixture("zip-nested");
        let archive = base.join("out.zip");
        create(&stage, "stage", &archive).unwrap();

        let file = File::open(&archive).unwrap();
        let names: Vec<String> = zip::ZipArchive::new(file)
            .unwrap()
            .file_names()
            .map(str::to_string)
            .collect();
        assert!(names.contains(&"stage/nested/b.txt".to_string()), "{names:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn tar_gz_round_trip() {
        round_trip("tgz", "out.tar.gz");
    }

    #[test]
    fn extract_rejects_missing_archive() {
        let missing = std::env::temp_dir().join("rpi-pkg-install-nope.zip");
        let _ = std::fs::remove_file(&missing);
        assert!(extract(&missing, &std::env::temp_dir()).is_err());
    }
}
