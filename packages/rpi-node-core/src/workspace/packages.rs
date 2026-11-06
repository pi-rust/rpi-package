//! Curated, data-only distribution on the existing owner. No network fetcher,
//! executable manifest, archive extractor, package manager or session authority.
use super::resources::{BASE, DEVELOPER, conflict, digest, immutable, invalid, plain_directory};
use super::{LocalError, LocalWorkspace, Path, PathBuf, WorkspaceError};
use rpi_node_api::api::{
    NativeKind, NativeManagement, NativeReceipt, PackageAcquisition, PackageBundle, PackageCatalog,
    PackageInstall, PackageManifest, PackageOffer, PackageRef,
};
use std::{collections::BTreeSet, io::Read};

const VERSION: &str = "1.0.0";
const MAX_SOURCE: usize = 1024 * 1024;

fn bundle(
    name: &str,
    kind: NativeKind,
    title: &str,
    description: &str,
    origin: &str,
    files: &[(&str, &str)],
) -> Result<PackageBundle, WorkspaceError> {
    let manifest = PackageManifest {
        schema_version: 1,
        id: name.into(),
        name: name.into(),
        version: VERSION.into(),
        kind,
        title: title.into(),
        description: description.into(),
        author: "RPI Work".into(),
        origin: origin.into(),
        instructions: if kind == NativeKind::Package {
            vec!["prompts/system.md".into()]
        } else {
            vec![]
        },
    };
    let sources = files
        .iter()
        .map(|(p, t)| ((*p).into(), (*t).into()))
        .collect();
    let sha256 = digest(&serde_json::to_vec(&(&manifest, &sources)).map_err(LocalError::from)?);
    let value = PackageBundle {
        manifest,
        sources,
        sha256,
    };
    validate(&value)?;
    Ok(value)
}

fn curated() -> Result<Vec<PackageBundle>, WorkspaceError> {
    Ok(vec![
        bundle(
            "rpi-work-base",
            NativeKind::Package,
            "基础工作包",
            "通用任务指令与 work-basics Skill。与普通新会话使用的内置基础包相同；不含可执行扩展。",
            "bundled:assets/packages/base",
            BASE,
        )?,
        bundle(
            "rpi-work-developer",
            NativeKind::Package,
            "资源开发包",
            "资源编写指令、resource-authoring Skill 与模板。与开发会话的内置包相同；安装不代表授予工具权限。",
            "bundled:assets/packages/developer",
            DEVELOPER,
        )?,
        bundle(
            "work-basics",
            NativeKind::Skill,
            "独立基础 Skill",
            "单独获取 work-basics 操作说明，可显式用于其他 Package 会话。不包含新的工具或可执行代码。",
            "bundled:assets/packages/base/skills/work-basics",
            &[(
                "SKILL.md",
                include_str!("../../assets/packages/base/skills/work-basics/SKILL.md"),
            )],
        )?,
    ])
}

fn identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.starts_with('-')
        && !value.ends_with('-')
        && !value.contains("--")
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

/// Portable paths, including Windows device/ADS/normalization aliases. V1 only
/// carries Markdown; native libraries, build scripts and archive links have no lane.
fn source_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 240
        && Path::new(value)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        && value.split('/').count() <= 12
        && value.split('/').all(|part| {
            let stem = part
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            !part.is_empty()
                && !part.starts_with('.')
                && !part.ends_with('.')
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
                && !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                && !(stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        })
}

fn validate(value: &PackageBundle) -> Result<(), WorkspaceError> {
    let m = &value.manifest;
    if m.schema_version != 1
        || !identity(&m.id)
        || m.id != m.name
        || m.version != VERSION
        || m.kind == NativeKind::Extension
        || value.sources.is_empty()
        || value.sources.len() > 64
        || value.sources.values().map(String::len).sum::<usize>() > MAX_SOURCE
    {
        return Err(invalid(
            "invalid data-only package identity/schema/kind/source budget",
        ));
    }
    let mut portable = BTreeSet::new();
    for (path, text) in &value.sources {
        if !source_path(path)
            || text.len() > 256 * 1024
            || text.contains('\0')
            || !portable.insert(path.to_ascii_lowercase())
        {
            return Err(invalid(
                "unsafe/duplicate package path or source exceeds budget",
            ));
        }
    }
    // Reject a file/directory prefix collision before any filesystem changes.
    for path in &portable {
        if path
            .split('/')
            .scan(String::new(), |prefix, part| {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                Some(prefix.clone())
            })
            .any(|prefix| prefix != *path && portable.contains(&prefix))
        {
            return Err(invalid("package file/directory collision"));
        }
    }
    match m.kind {
        NativeKind::Package => {
            if m.instructions.is_empty()
                || m.instructions.iter().any(|p| {
                    !p.starts_with("prompts/")
                        || value.sources.get(p).is_none_or(|t| t.trim().is_empty())
                })
            {
                return Err(invalid(
                    "Package requires explicit canonical instruction files",
                ));
            }
        }
        NativeKind::Skill => {
            let text = value
                .sources
                .get("SKILL.md")
                .ok_or_else(|| invalid("Skill requires SKILL.md"))?;
            let header = text
                .strip_prefix("---\n")
                .and_then(|t| t.split_once("\n---\n").map(|(h, _)| h))
                .ok_or_else(|| invalid("Skill requires name/description frontmatter"))?;
            let names: Vec<_> = header
                .lines()
                .filter_map(|l| l.strip_prefix("name:"))
                .collect();
            if names.len() != 1
                || names[0].trim() != m.name
                || !header.lines().any(|l| {
                    l.strip_prefix("description:")
                        .is_some_and(|s| !s.trim().is_empty())
                })
                || !m.instructions.is_empty()
            {
                return Err(invalid(
                    "Skill name must match installed directory and have a description",
                ));
            }
        }
        NativeKind::Extension => return Err(invalid("executable distributions are unsupported")),
    }
    let expected =
        digest(&serde_json::to_vec(&(&value.manifest, &value.sources)).map_err(LocalError::from)?);
    if expected != value.sha256 {
        return Err(conflict(
            "package digest differs from canonical metadata/source bytes",
        ));
    }
    Ok(())
}

fn resolve(package: &PackageRef) -> Result<PackageBundle, WorkspaceError> {
    // Never turn user input into a path, URL, argv or a fallback version.
    let value = curated()?
        .into_iter()
        .find(|p| p.manifest.id == package.id)
        .ok_or_else(|| invalid("package is not in this node's curated catalog"))?;
    if package.version != value.manifest.version || package.sha256 != value.sha256 {
        return Err(conflict(
            "package version/digest changed; refresh the catalog and confirm again",
        ));
    }
    Ok(value)
}

fn reference(bundle: &PackageBundle) -> PackageRef {
    PackageRef {
        id: bundle.manifest.id.clone(),
        version: bundle.manifest.version.clone(),
        sha256: bundle.sha256.clone(),
    }
}

impl LocalWorkspace {
    fn package_source(&self, bundle: &PackageBundle) -> PathBuf {
        // Share the existing base/developer source identity with shipped_selection.
        // Installing a curated copy must not break later ordinary session creation.
        self.inner
            .data
            .join("shipped")
            .join(&bundle.manifest.version)
            .join(&bundle.manifest.name)
    }

    /// Bounded IO is retained beyond an HTTP/UI timeout and fenced against node
    /// shutdown. No project Service/host is started by discovery or acquisition.
    async fn package_io<T, F>(&self, project: &str, work: F) -> Result<T, WorkspaceError>
    where
        T: Send + 'static,
        F: FnOnce(&Self) -> Result<T, WorkspaceError> + Send + 'static,
    {
        let owner = self.clone();
        let project = project.to_owned();
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            runtime.block_on(async move {
                let _lifecycle = owner.inner.lifecycle.read().await;
                let _serial = owner.inner.packages.lock().await;
                let _ = owner.project_root(&project)?;
                work(&owner)
            })
        })
        .await
        .map_err(|_| {
            LocalError::Initialization(
                "package IO worker failed; inspect retained source before retrying".into(),
            )
        })?
    }

    pub async fn package_catalog(&self, project: &str) -> Result<PackageCatalog, WorkspaceError> {
        self.package_io(project, |owner| {
            let mut diagnostics = vec![];
            let packages = curated()?.into_iter().map(|bundle| {
                let acquired = if let Ok(complete) = verify_tree(&owner.package_source(&bundle), &bundle) {
                    complete
                } else {
                    diagnostics.push(format!("{}: source could not be verified; not acquired. Existing bytes retained; inspect before explicit repair.", bundle.manifest.id));
                    false
                };
                PackageOffer { sha256: bundle.sha256, file_count: bundle.sources.len(), size_bytes: bundle.sources.values().map(String::len).sum(), manifest: bundle.manifest, acquired }
            }).collect();
            Ok(PackageCatalog { source: "bundled-curated-v1 (随应用发布的精选目录，非在线市场)".into(), packages, diagnostics })
        }).await
    }

    pub async fn package_bundle(
        &self,
        project: &str,
        package: PackageRef,
    ) -> Result<PackageBundle, WorkspaceError> {
        self.package_io(project, move |_| resolve(&package)).await
    }

    pub async fn package_acquire(
        &self,
        project: &str,
        package: PackageRef,
    ) -> Result<PackageAcquisition, WorkspaceError> {
        self.package_io(project, move |owner| {
            let bundle = resolve(&package)?;
            let source = owner.package_source(&bundle);
            // Partial matching bytes can be completed; different or additional
            // bytes refuse before writing, and are never overwritten/deleted.
            verify_tree(&source, &bundle)?;
            for (path, text) in &bundle.sources {
                immutable(&source.join(path), text.as_bytes())?;
            }
            if !verify_tree(&source, &bundle)? {
                return Err(conflict(
                    "package acquisition incomplete; reread before explicit retry",
                ));
            }
            Ok(PackageAcquisition {
                package: reference(&bundle),
                source: source.to_string_lossy().into_owned(),
            })
        })
        .await
    }

    pub async fn package_install(
        &self,
        project: &str,
        request: PackageInstall,
    ) -> Result<NativeReceipt, WorkspaceError> {
        if !request.trusted {
            return Err(invalid(
                "explicit package source trust/installation authorization required",
            ));
        }
        let (bundle, source) = self
            .package_io(project, move |owner| {
                let bundle = resolve(&request.package)?;
                let source = owner.package_source(&bundle);
                if !verify_tree(&source, &bundle)? {
                    return Err(conflict(
                        "package has not been acquired and verified on this node",
                    ));
                }
                Ok((bundle, source))
            })
            .await?;
        // Existing native manager owns leases, transactional installation and
        // receipts. We do not widen session selection or fabricate loaded facts.
        self.native_manage(
            project,
            NativeManagement::Install {
                scope: request.scope,
                kind: bundle.manifest.kind,
                name: bundle.manifest.name,
                source: source.to_string_lossy().into_owned(),
                version: Some(bundle.manifest.version),
                trusted: true,
            },
        )
        .await
    }
}

/// Check only bounded ordinary directories/files, exact expected bytes and no
/// extra entries. Missing matching files means incomplete, never acquired=true.
fn verify_tree(root: &Path, bundle: &PackageBundle) -> Result<bool, WorkspaceError> {
    match std::fs::symlink_metadata(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Even an absent leaf may have a junction/symlink ancestor.
            let parent = root
                .ancestors()
                .skip(1)
                .find(|p| p.exists())
                .ok_or_else(|| invalid("missing package parent"))?;
            plain_directory(parent, false)?;
            return Ok(false);
        }
        Err(e) => return Err(LocalError::from(e).into()),
        Ok(_) => plain_directory(root, false)?,
    }
    let mut seen = BTreeSet::new();
    walk(root, root, bundle, &mut seen, &mut 0)?;
    Ok(seen.len() == bundle.sources.len())
}

fn walk(
    root: &Path,
    directory: &Path,
    bundle: &PackageBundle,
    seen: &mut BTreeSet<String>,
    count: &mut usize,
) -> Result<(), WorkspaceError> {
    plain_directory(directory, false)?;
    for entry in std::fs::read_dir(directory).map_err(LocalError::from)? {
        let entry = entry.map_err(LocalError::from)?;
        *count += 1;
        if *count > 128 {
            return Err(invalid("package directory entry limit exceeded"));
        }
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| invalid("package source escape"))?
            .components()
            .map(|component| {
                let std::path::Component::Normal(part) = component else {
                    return Err(invalid("noncanonical package path"));
                };
                let text = part
                    .to_str()
                    .ok_or_else(|| invalid("non UTF-8 package path"))?;
                // On Unix a backslash is a literal filename character, NOT a
                // separator. Do not let flat aliases impersonate canonical trees.
                if text.contains(['\\', '/']) {
                    return Err(invalid("package filename contains a separator alias"));
                }
                Ok(text)
            })
            .collect::<Result<Vec<_>, WorkspaceError>>()?
            .join("/");
        let meta = std::fs::symlink_metadata(&path).map_err(LocalError::from)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err(invalid("package reparse point rejected"));
            }
        }
        if meta.file_type().is_symlink() {
            return Err(invalid("package symlink rejected"));
        }
        if meta.is_dir() {
            if !bundle
                .sources
                .keys()
                .any(|p| p.starts_with(&format!("{relative}/")))
            {
                return Err(conflict("extra package directory; existing bytes retained"));
            }
            walk(root, &path, bundle, seen, count)?;
        } else if meta.is_file() {
            let expected = bundle
                .sources
                .get(&relative)
                .ok_or_else(|| conflict("extra package file; existing bytes retained"))?;
            if meta.len() != expected.len() as u64 {
                return Err(conflict("package bytes changed; original retained"));
            }
            let mut actual = Vec::new();
            std::fs::File::open(&path)
                .map_err(LocalError::from)?
                .take(expected.len() as u64 + 1)
                .read_to_end(&mut actual)
                .map_err(LocalError::from)?;
            if actual != expected.as_bytes() {
                return Err(conflict("package bytes changed; original retained"));
            }
            if !seen.insert(relative) {
                return Err(conflict("duplicate package source key"));
            }
        } else {
            return Err(invalid("package special file rejected"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
