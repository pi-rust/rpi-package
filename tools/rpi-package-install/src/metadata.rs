//! Derives the extension list from Cargo itself, so adding a package never
//! requires touching an install script again.

use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One extension cdylib produced by the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    /// Cargo package name, e.g. `rpi-langfuse`.
    pub package: String,
    /// Built artifact file stem, e.g. `rpi_langfuse`.
    pub artifact: String,
}

impl Extension {
    /// File name of the built artifact on this host, e.g. `rpi_langfuse.dll`.
    pub fn file_name(&self) -> String {
        format!("{}.{}", self.artifact, super::platform::dylib_extension())
    }
}

/// What `cargo metadata` tells us about the workspace.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub target_dir: PathBuf,
    pub extensions: Vec<Extension>,
}

impl Workspace {
    /// `target/release`, honouring a redirected `CARGO_TARGET_DIR`.
    pub fn release_dir(&self) -> PathBuf {
        self.target_dir.join("release")
    }
}

/// Walk up from `start` looking for a `Cargo.toml` that declares `[workspace]`.
pub fn find_manifest(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    loop {
        let candidate = dir.join("Cargo.toml");
        if candidate.is_file() {
            if let Ok(text) = std::fs::read_to_string(&candidate) {
                if text.lines().any(|l| l.trim() == "[workspace]") {
                    return Some(candidate);
                }
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

pub fn load(manifest: &Path) -> Result<Workspace> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(&cargo)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .arg("--manifest-path")
        .arg(manifest)
        .output()
        .with_context(|| {
            format!(
                "failed to run `{}` (needed to discover the extension list)",
                cargo.to_string_lossy()
            )
        })?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed for {}:\n{}",
            manifest.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("cargo metadata returned invalid JSON")?;
    parse(&json)
}

fn parse(json: &serde_json::Value) -> Result<Workspace> {
    let root = json["workspace_root"]
        .as_str()
        .context("cargo metadata: missing workspace_root")?;
    let target_dir = json["target_directory"]
        .as_str()
        .context("cargo metadata: missing target_directory")?;

    let members: BTreeSet<&str> = json["workspace_members"]
        .as_array()
        .context("cargo metadata: missing workspace_members")?
        .iter()
        .filter_map(|m| m.as_str())
        .collect();

    let mut extensions = Vec::new();
    for package in json["packages"]
        .as_array()
        .context("cargo metadata: missing packages")?
    {
        let id = package["id"].as_str().unwrap_or_default();
        if !members.contains(id) {
            continue;
        }
        let name = package["name"].as_str().unwrap_or_default().to_string();
        for target in package["targets"].as_array().into_iter().flatten() {
            let is_cdylib = target["crate_types"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|c| c.as_str() == Some("cdylib"));
            if !is_cdylib {
                continue;
            }
            // Cargo uses the lib target name for the artifact file stem.
            let artifact = target["name"]
                .as_str()
                .with_context(|| format!("{name}: cdylib target without a name"))?
                .to_string();
            extensions.push(Extension {
                package: name.clone(),
                artifact,
            });
        }
    }
    extensions.sort_by(|a, b| a.package.cmp(&b.package));
    extensions.dedup_by(|a, b| a.package == b.package);

    Ok(Workspace {
        root: PathBuf::from(root),
        target_dir: PathBuf::from(target_dir),
        extensions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> serde_json::Value {
        serde_json::json!({
            "workspace_root": "/ws",
            "target_directory": "/ws/target",
            "workspace_members": ["pkg a", "pkg b"],
            "packages": [
                {
                    "id": "pkg a",
                    "name": "rpi-langfuse",
                    "targets": [
                        { "name": "rpi_langfuse", "crate_types": ["cdylib", "rlib"] },
                        { "name": "unrelated_bin", "crate_types": ["bin"] }
                    ]
                },
                {
                    "id": "pkg b",
                    "name": "not-a-plugin",
                    "targets": [ { "name": "plain", "crate_types": ["lib"] } ]
                },
                {
                    "id": "path+file:///dep",
                    "name": "external-dep",
                    "targets": [ { "name": "dep", "crate_types": ["cdylib"] } ]
                }
            ]
        })
    }

    #[test]
    fn keeps_only_workspace_cdylibs() {
        let ws = parse(&meta()).unwrap();
        assert_eq!(ws.root, PathBuf::from("/ws"));
        assert_eq!(ws.target_dir, PathBuf::from("/ws/target"));
        assert_eq!(
            ws.extensions,
            vec![Extension {
                package: "rpi-langfuse".into(),
                artifact: "rpi_langfuse".into(),
            }]
        );
    }

    #[test]
    fn artifact_file_name_uses_host_extension() {
        let ext = Extension {
            package: "rpi-langfuse".into(),
            artifact: "rpi_langfuse".into(),
        };
        assert_eq!(ext.file_name(), format!("rpi_langfuse.{}", super::super::platform::dylib_extension()));
    }

    #[test]
    fn release_dir_follows_redirected_target_dir() {
        let ws = parse(&meta()).unwrap();
        assert!(ws.release_dir().ends_with("release"));
    }

    #[test]
    fn find_manifest_walks_up_to_workspace_root() {
        let tmp = std::env::temp_dir().join("rpi-pkg-install-find-manifest");
        let nested = tmp.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(tmp.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        // A package manifest on the way up must not shadow the workspace root.
        std::fs::write(nested.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();

        let found = find_manifest(&nested).expect("should find the workspace manifest");
        assert_eq!(
            found.canonicalize().unwrap(),
            tmp.join("Cargo.toml").canonicalize().unwrap()
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
