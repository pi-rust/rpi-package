//! Authorization/resolution only. Lifecycle, selection journal and install engine are core/native.
use super::{
    Arc, LocalError, LocalWorkspace, Path, PathBuf, ReplyKind, Service, Value, WorkspaceError,
    decode, json, service, unsupported, workspace,
};
use rpi_extcore::resources::{
    ApplyResourcesRequest, MainPackage, NativeBinding, ResourceError, ResourceErrorKind,
    ResourceHistoryCursor, ResourceHistoryRequest, ResourceObservation, ResourceOperation,
    ResourceOperationStatus, ResourceScope, ResourceSelection, ResourceTools,
    local::{LocalResourceOperation, LocalResourceRequest, ResourceKind, capabilities},
};
use rpi_node_api::api::{
    NativeApply, NativeCatalog, NativeConfiguration, NativeCreate, NativeDesired, NativeKind,
    NativeManagement, NativeOperation, NativeOperationStatus, NativeReadiness, NativeReceipt,
    NativeReference, NativeReleaseBlock, NativeReleaseReport, NativeResource, NativeScope,
    NativeSelection, NativeSessionResources, NativeTools,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn invalid(message: &str) -> WorkspaceError {
    LocalError::InvalidInput(message.into()).into()
}
pub(super) fn conflict(message: &str) -> WorkspaceError {
    LocalError::Service {
        kind: ReplyKind::Conflict,
        message: message.into(),
    }
    .into()
}
pub(super) fn resource_error(error: ResourceError) -> WorkspaceError {
    let kind = match error.kind {
        ResourceErrorKind::InvalidInput => ReplyKind::InvalidInput,
        ResourceErrorKind::Conflict => ReplyKind::Conflict,
        ResourceErrorKind::Unsupported => ReplyKind::Unsupported,
        ResourceErrorKind::LimitExceeded => ReplyKind::LimitExceeded,
        ResourceErrorKind::Unavailable | ResourceErrorKind::Storage => ReplyKind::Unavailable,
    };
    LocalError::Service {
        kind,
        message: error.message,
    }
    .into()
}
fn scope(s: NativeScope) -> ResourceScope {
    match s {
        NativeScope::Project => ResourceScope::Project,
        NativeScope::Global => ResourceScope::Global,
    }
}
fn kind(k: NativeKind) -> ResourceKind {
    match k {
        NativeKind::Package => ResourceKind::Package,
        NativeKind::Skill => ResourceKind::Skill,
        NativeKind::Extension => ResourceKind::Extension,
    }
}
pub(super) fn safe_relative(value: &str) -> Result<PathBuf, WorkspaceError> {
    if value.is_empty()
        || value.len() > 240
        || value.contains('\\')
        || value.contains(':')
        || value.chars().any(char::is_control)
    {
        return Err(invalid("expected bounded project-relative path"));
    }
    let path = Path::new(value);
    if path
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(invalid("relative path traversal rejected"));
    }
    Ok(path.into())
}
/// Refuse symlink/junction ancestors before source or immutable IO. This is
/// a bounded cooperative check, not protection from malicious concurrent OS
/// writers swapping directory entries after admission (documented limitation).
pub(super) fn plain_directory(path: &Path, create: bool) -> Result<(), WorkspaceError> {
    let ancestors: Vec<_> = path
        .ancestors()
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    if ancestors.len() > 64 {
        return Err(invalid("directory nesting exceeds limit"));
    }
    for directory in ancestors.into_iter().rev() {
        let meta = match std::fs::symlink_metadata(directory) {
            Ok(meta) => meta,
            Err(e) if create && e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(directory).map_err(LocalError::from)?;
                std::fs::symlink_metadata(directory).map_err(LocalError::from)?
            }
            Err(e) => return Err(LocalError::from(e).into()),
        };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(invalid("directory symlink/special file rejected"));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err(invalid("directory reparse point rejected"));
            }
        }
    }
    Ok(())
}
fn observation_matches(actual: &Value, saved: &ResourceObservation) -> bool {
    actual["hostId"] == saved.generation
        && serde_json::from_value::<NativeBinding>(actual["binding"].clone())
            .is_ok_and(|binding| binding == saved.binding)
}
/// Publish a fully synced same-filesystem staging inode without replacing an
/// existing identity. A failed publication retains staging bytes for diagnosis.
pub(super) fn immutable(path: &Path, bytes: &[u8]) -> Result<(), WorkspaceError> {
    use std::io::{Read, Write};
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(invalid("immutable content exceeds 8 MiB"));
    }
    let parent = path.parent().ok_or_else(|| invalid("missing parent"))?;
    plain_directory(parent, true)?;
    let compare = || -> Result<(), WorkspaceError> {
        let meta = std::fs::symlink_metadata(path).map_err(LocalError::from)?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() != bytes.len() as u64 {
            return Err(conflict(
                "immutable identity differs; original bytes retained",
            ));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err(invalid("immutable reparse point rejected"));
            }
        }
        let mut actual = Vec::new();
        std::fs::File::open(path)
            .map_err(LocalError::from)?
            .take(bytes.len() as u64 + 1)
            .read_to_end(&mut actual)
            .map_err(LocalError::from)?;
        if actual != bytes {
            return Err(conflict(
                "immutable content/provenance differs; original retained",
            ));
        }
        Ok(())
    };
    if path.try_exists().map_err(LocalError::from)? {
        return compare();
    }
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let stage = parent.join(format!(".rpi-work-stage-{}-{time}-{n}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stage)
        .map_err(LocalError::from)?;
    file.write_all(bytes).map_err(LocalError::from)?;
    file.sync_all().map_err(LocalError::from)?;
    drop(file);
    // hard_link is no-replace and atomic on supported local filesystems. We do
    // not fall back to create+write(final) on unsupported/network filesystems.
    match std::fs::hard_link(&stage, path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => compare()?,
        Err(e) => {
            eprintln!(
                "immutable hard_link failed: kind={:?} os={:?}; staging retained",
                e.kind(),
                e.raw_os_error()
            );
            return Err(LocalError::from(e).into());
        }
    }
    std::fs::remove_file(stage).map_err(|e| {
        eprintln!(
            "immutable stage cleanup failed: kind={:?} os={:?}; final/staging retained",
            e.kind(),
            e.raw_os_error()
        );
        LocalError::from(e)
    })?;
    Ok(())
}
impl LocalWorkspace {
    pub(super) fn native_service(&self, id: &str) -> Result<Arc<Service>, WorkspaceError> {
        if self.inner.agents.contains(id)? {
            return Err(unsupported("exact native resources", "acp"));
        }
        self.session_service(id)
    }
    async fn catalog_for_scopes(
        &self,
        service: &Service,
        scopes: &[NativeScope],
    ) -> Result<NativeCatalog, WorkspaceError> {
        let profile = capabilities(
            self.inner.executable.clone(),
            service.config.project.clone(),
        )
        .await
        .map_err(resource_error)?
        .profile;
        let project = workspace::Project::new(&service.config.project)
            .map_err(LocalError::from)?
            .id;
        let mut resources = Vec::new();
        let mut diagnostics = Vec::new();
        for &s in scopes {
            let receipt = service
                .manage_resources(LocalResourceRequest {
                    project_id: project.clone(),
                    scope: scope(s),
                    operation: LocalResourceOperation::List,
                })
                .await
                .map_err(resource_error)?;
            if !receipt.success {
                return Err(conflict(&format!(
                    "native catalog refused: {}",
                    receipt.observation
                )));
            }
            if !receipt.diagnostics.is_empty() {
                diagnostics.push(receipt.diagnostics);
            }
            let items = receipt.observation["data"]["resources"]
                .as_array()
                .ok_or_else(|| invalid("invalid native catalog resources"))?;
            for facts in items {
                let k: NativeKind =
                    serde_json::from_value(facts["kind"].clone()).map_err(LocalError::from)?;
                let name = facts["name"]
                    .as_str()
                    .ok_or_else(|| invalid("invalid installed name"))?
                    .to_owned();
                let reference = digest(
                    &serde_json::to_vec(&json!([
                        project,
                        s,
                        k,
                        name,
                        facts["recordedDigest"],
                        facts["path"]
                    ]))
                    .map_err(LocalError::from)?,
                );
                resources.push(NativeResource {
                    reference,
                    scope: s,
                    kind: k,
                    name,
                    facts: facts.clone(),
                });
            }
        }
        Ok(NativeCatalog {
            profile,
            resources,
            diagnostics,
        })
    }
    pub(super) async fn catalog_for(
        &self,
        service: &Service,
    ) -> Result<NativeCatalog, WorkspaceError> {
        self.catalog_for_scopes(service, &[NativeScope::Project, NativeScope::Global])
            .await
    }
    async fn project_catalog_for(
        &self,
        service: &Service,
    ) -> Result<NativeCatalog, WorkspaceError> {
        self.catalog_for_scopes(service, &[NativeScope::Project])
            .await
    }
    pub async fn native_catalog(&self, project: &str) -> Result<NativeCatalog, WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        let service = self.project_service(project)?;
        self.catalog_for(&service).await
    }
    fn resolve<'a>(
        catalog: &'a NativeCatalog,
        reference: &str,
        expected: Option<NativeKind>,
    ) -> Result<&'a NativeResource, WorkspaceError> {
        catalog
            .resources
            .iter()
            .find(|r| r.reference == reference && expected.is_none_or(|k| k == r.kind))
            .ok_or_else(|| {
                conflict("installed reference changed or is outside this project; refresh catalog")
            })
    }
    fn authorized_source(
        &self,
        service: &Service,
        source: &str,
    ) -> Result<PathBuf, WorkspaceError> {
        // Local means the execution node, not the browser. No arbitrary DLL/argv session paths.
        let path = Path::new(source);
        let path = if path.is_absolute() {
            path.into()
        } else {
            service.config.project.join(safe_relative(source)?)
        };
        let canonical = std::fs::canonicalize(path).map_err(LocalError::from)?;
        let project = std::fs::canonicalize(&service.config.project).map_err(LocalError::from)?;
        let shipped = self.inner.data.join("shipped");
        let published = self.inner.data.join("artifacts");
        if !canonical.starts_with(project)
            && !canonical.starts_with(shipped)
            && !canonical.starts_with(published)
        {
            return Err(LocalError::Service {
                kind: ReplyKind::AccessDenied,
                message:
                    "source must be inside authorized project or node-owned shipped/artifact roots"
                        .into(),
            }
            .into());
        }
        Ok(canonical)
    }
    pub async fn native_manage(
        &self,
        project: &str,
        request: NativeManagement,
    ) -> Result<NativeReceipt, WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        let service = self.project_service(project)?;
        let catalog = self.catalog_for(&service).await?;
        let (s, operation) = match request {
            NativeManagement::Recover { scope: s } => (s, LocalResourceOperation::Recover),
            NativeManagement::Inspect { ref reference }
            | NativeManagement::Remove { ref reference } => {
                let r = Self::resolve(&catalog, reference, None)?;
                let op = if matches!(request, NativeManagement::Inspect { .. }) {
                    LocalResourceOperation::Inspect {
                        kind: kind(r.kind),
                        name: r.name.clone(),
                    }
                } else {
                    LocalResourceOperation::Remove {
                        kind: kind(r.kind),
                        name: r.name.clone(),
                    }
                };
                (r.scope, op)
            }
            NativeManagement::Install {
                scope: s,
                kind: k,
                name,
                source,
                version,
                trusted,
            } => {
                if !trusted {
                    return Err(invalid(
                        "explicit trusted-source authorization required; install does not compile",
                    ));
                }
                let source = self.authorized_source(&service, &source)?;
                (
                    s,
                    LocalResourceOperation::Install {
                        kind: kind(k),
                        name,
                        source,
                        version,
                    },
                )
            }
            NativeManagement::Update {
                ref reference,
                ref source,
                ref version,
                trusted,
            }
            | NativeManagement::Reinstall {
                ref reference,
                ref source,
                ref version,
                trusted,
            } => {
                if !trusted {
                    return Err(invalid("explicit trusted-source authorization required"));
                }
                let r = Self::resolve(&catalog, reference, None)?;
                let source = self.authorized_source(&service, source)?;
                let op = if matches!(request, NativeManagement::Update { .. }) {
                    LocalResourceOperation::Update {
                        kind: kind(r.kind),
                        name: r.name.clone(),
                        source,
                        version: version.clone(),
                    }
                } else {
                    LocalResourceOperation::Reinstall {
                        kind: kind(r.kind),
                        name: r.name.clone(),
                        source,
                        version: version.clone(),
                    }
                };
                (r.scope, op)
            }
        };
        let result = service
            .manage_resources(LocalResourceRequest {
                project_id: project.into(),
                scope: scope(s),
                operation,
            })
            .await
            .map_err(resource_error)?;
        Ok(NativeReceipt {
            success: result.success,
            observation: result.observation,
            diagnostics: result.diagnostics,
        })
    }
    pub(super) async fn resolve_selection(
        &self,
        service: &Service,
        target: NativeSelection,
    ) -> Result<ResourceSelection, WorkspaceError> {
        let catalog = self.catalog_for(service).await?;
        let main = Self::resolve(&catalog, &target.main_package, Some(NativeKind::Package))?;
        if main.facts["ownership"] != "resource" || main.facts["installed"] != true {
            return Err(invalid("select a native manager-owned installed Package"));
        }
        let root = PathBuf::from(
            main.facts["path"]
                .as_str()
                .ok_or_else(|| invalid("missing installed root"))?,
        );
        let root = std::fs::canonicalize(root).map_err(LocalError::from)?;
        for instruction in &target.package_instructions {
            let path = std::fs::canonicalize(root.join(safe_relative(instruction)?))
                .map_err(LocalError::from)?;
            if !path.starts_with(&root) || !path.is_file() {
                return Err(invalid(
                    "Package instruction escapes installed canonical root",
                ));
            }
        }
        let paths = |references: &[String], expected| -> Result<Vec<String>, WorkspaceError> {
            references
                .iter()
                .map(|reference| {
                    let r = Self::resolve(&catalog, reference, Some(expected))?;
                    if r.facts["ownership"] != "resource" || r.facts["installed"] != true {
                        return Err(invalid("resource is not manager-owned installed content"));
                    }
                    let field = if expected == NativeKind::Skill {
                        "skills"
                    } else {
                        "extensions"
                    };
                    r.facts["selectable"][field][0]
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| invalid("native resource has no selectable entry"))
                })
                .collect()
        };
        if target.supplementary_instructions.len() > 64 * 1024 {
            return Err(invalid(
                "supplementary instruction text exceeds native 64 KiB",
            ));
        }
        let mut instructions = Vec::new();
        if !target.supplementary_instructions.is_empty() {
            let bytes = target.supplementary_instructions.as_bytes();
            let path = self
                .inner
                .data
                .join("instructions")
                .join(format!("{}.md", digest(bytes)));
            immutable(&path, bytes)?;
            instructions.push(path.to_string_lossy().into_owned());
        }
        let selection = ResourceSelection {
            version: 1,
            main_package: Some(MainPackage {
                name: main.name.clone(),
                scope: Some(scope(main.scope)),
                instructions: target.package_instructions,
            }),
            skills: paths(&target.skills, NativeKind::Skill)?,
            extensions: paths(&target.extensions, NativeKind::Extension)?,
            instructions,
            tools: serde_json::from_value(
                serde_json::to_value(target.tools).map_err(LocalError::from)?,
            )
            .map_err(LocalError::from)?,
        };
        selection.validate().map_err(resource_error)?;
        Ok(selection)
    }
    pub async fn native_session_resources(
        &self,
        id: &str,
    ) -> Result<NativeSessionResources, WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        let service = self.native_service(id)?;
        let catalog = self.catalog_for(&service).await?;
        let saved = service
            .resource_configuration(id)
            .await
            .map_err(resource_error)?;
        let operations: Vec<_> = service
            .resource_operations(id)
            .await
            .map_err(resource_error)?
            .iter()
            .map(|op| self.project_operation(&catalog, op, id))
            .collect();
        let configuration = saved.as_ref().map(|c| NativeConfiguration {
            revision: c.revision,
            desired: self.project_selection(&catalog, &c.selection),
            historical_observation: c.observation.as_ref().map(|o| o.body.clone()),
        });
        let (actual, diagnostic) = match service.live_session(id).await {
            Some(host) => match host.resource_state().await { Ok(state) => (Some(state.body), None), Err(e) => (None, Some(e.to_string())) },
            None => (None, Some("No live verified host; saved observations are historical. Read blocked history or reopen/recover the original operation.".into())),
        };
        let pending = operations.last().filter(|op| {
            matches!(
                op.status,
                NativeOperationStatus::Pending | NativeOperationStatus::Blocked
            )
        });
        let readiness = if let Some(op) = pending {
            if op.status == NativeOperationStatus::Pending {
                NativeReadiness::Applying
            } else {
                NativeReadiness::Blocked
            }
        } else if configuration.is_none() {
            NativeReadiness::Legacy
        } else if actual.as_ref().is_some_and(|a| {
            saved
                .as_ref()
                .and_then(|c| c.observation.as_ref())
                .is_some_and(|o| observation_matches(a, o))
        }) {
            NativeReadiness::Verified
        } else if actual.is_some() {
            NativeReadiness::Unknown
        } else {
            NativeReadiness::Closed
        };
        let executable = match readiness {
            NativeReadiness::Verified => Some(true),
            NativeReadiness::Legacy | NativeReadiness::Unknown => None,
            _ => Some(false),
        };
        let generation = actual
            .as_ref()
            .and_then(|a| a["hostId"].as_str())
            .map(str::to_owned);
        Ok(NativeSessionResources {
            configuration,
            operations,
            readiness,
            executable,
            actual,
            generation,
            diagnostic,
        })
    }
    pub async fn native_apply(
        &self,
        id: &str,
        request: NativeApply,
    ) -> Result<NativeOperation, WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        let service = self.native_service(id)?;
        let intent_path = self.resource_intent_path(id, &request.operation_id);
        let intent = match read_intent_bytes(&intent_path) {
            Ok(bytes) if bytes.len() <= 128 * 1024 => {
                let intent: WorkIntent =
                    serde_json::from_slice(&bytes).map_err(LocalError::from)?;
                if intent.request != request {
                    return Err(conflict(
                        "operation ID is already bound to different application payload",
                    ));
                }
                intent
            }
            Ok(_) => {
                return Err(invalid(
                    "stored application resource intent exceeds budget; original retained",
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let target = self
                    .resolve_selection(&service, request.target.clone())
                    .await?;
                let native = ApplyResourcesRequest {
                    expected_revision: request.expected_revision,
                    operation_id: request.operation_id.clone(),
                    target,
                    supersedes_operation_id: request.supersedes_operation_id.clone(),
                };
                let intent = WorkIntent { request, native };
                let bytes = serde_json::to_vec(&intent).map_err(LocalError::from)?;
                if bytes.len() > 128 * 1024 {
                    return Err(invalid(
                        "application resource intent exceeds 128 KiB after encoding; shorten the selection/text before execution",
                    ));
                }
                immutable(&intent_path, &bytes)?;
                intent
            }
            Err(e) => return Err(LocalError::from(e).into()),
        };
        // The cached resolution is immutable and admitted only for the IDENTICAL
        // original application request. Retries need not resolve deleted sources.
        let result = service
            .apply_resources(id.into(), intent.native)
            .await
            .map_err(resource_error)?;
        // A replacement has a new host generation; existing streams must resync normally.
        let catalog = self.catalog_for(&service).await?;
        Ok(self.project_operation(&catalog, &result, id))
    }
    pub async fn native_create(
        &self,
        project: &str,
        request: NativeCreate,
    ) -> Result<String, WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        let service = self.project_service(project)?;
        let selection = match request.selection {
            Some(target) => self.resolve_selection(&service, target).await?,
            None => self.shipped_selection(&service, request.developer).await?,
        };
        let result: Value = decode(
            service::create_session(
                &service,
                service::CreateSessionRequest {
                    project_id: Some(project.into()),
                    resources: Some(selection),
                },
            )
            .await,
        )?;
        result["sessionId"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| invalid("native create omitted session identity"))
    }
    pub async fn native_release(
        &self,
        project: &str,
        s: NativeScope,
    ) -> Result<NativeReleaseReport, WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        self.project_root(project)?;
        let services: Vec<_> = if s == NativeScope::Global {
            // Independent core owners cannot provide a batch transaction. Report
            // partial release truthfully; active/Ask/queued owners refuse in core.
            let state = self.lock()?;
            state.services.values().cloned().collect()
        } else {
            vec![self.project_service(project)?]
        };
        let mut report = NativeReleaseReport {
            released: vec![],
            blocked: vec![],
        };
        for service in services {
            let pid = workspace::Project::new(&service.config.project)
                .map_err(LocalError::from)?
                .id;
            match service.release_idle_resources(pid.clone(), scope(s)).await {
                Ok(ids) => report.released.extend(ids),
                Err(error) => report.blocked.push(NativeReleaseBlock {
                    project_id: pid,
                    diagnostic: error.message,
                }),
            }
        }
        Ok(report)
    }
    pub async fn native_saved_history(
        &self,
        id: &str,
        cursor: Option<String>,
    ) -> Result<Value, WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        let service = self.native_service(id)?;
        // Cursor is validated by the core/native reader, not interpreted as a path.
        let cursor: Option<ResourceHistoryCursor> = cursor
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(LocalError::from)?;
        let request = ResourceHistoryRequest { limit: 64, cursor };
        serde_json::to_value(
            service
                .resource_saved_history(id.into(), request)
                .await
                .map_err(resource_error)?,
        )
        .map_err(LocalError::from)
        .map_err(Into::into)
    }
    pub(super) async fn shipped_selection(
        &self,
        service: &Service,
        developer: bool,
    ) -> Result<ResourceSelection, WorkspaceError> {
        let (name, files) = if developer {
            ("rpi-work-developer", DEVELOPER)
        } else {
            ("rpi-work-base", BASE)
        };
        let source = self.inner.data.join("shipped").join("1.0.0").join(name);
        for (path, bytes) in files {
            immutable(&source.join(path), bytes.as_bytes())?;
        }
        // Session creation only needs the project-owned shipped Package. Do not
        // enumerate the global catalog here: an unrelated oversized global
        // resource must not prevent creating a session.
        let catalog = self.project_catalog_for(service).await?;
        if let Some(existing) = catalog.resources.iter().find(|r| {
            r.scope == NativeScope::Project && r.kind == NativeKind::Package && r.name == name
        }) {
            let recorded = existing.facts["source"].as_str().map(PathBuf::from);
            if existing.facts["ownership"] != "resource"
                || recorded
                    .as_ref()
                    .and_then(|p| std::fs::canonicalize(p).ok())
                    != std::fs::canonicalize(&source).ok()
                || existing.facts["version"] != "1.0.0"
                || existing.facts["recordedDigest"] != existing.facts["observed"]["sha256"]
            {
                return Err(conflict(
                    "shipped Package identity/content is owned by different bytes; inspect and explicitly repair, never overwrite",
                ));
            }
        } else {
            let pid = workspace::Project::new(&service.config.project)
                .map_err(LocalError::from)?
                .id;
            let result = service
                .manage_resources(LocalResourceRequest {
                    project_id: pid,
                    scope: ResourceScope::Project,
                    operation: LocalResourceOperation::Install {
                        kind: ResourceKind::Package,
                        name: name.into(),
                        source,
                        version: Some("1.0.0".into()),
                    },
                })
                .await
                .map_err(resource_error)?;
            if !result.success {
                return Err(conflict(&format!(
                    "shipped install refused (release owned idle hosts explicitly if leased): {}",
                    result.observation
                )));
            }
        }
        Ok(ResourceSelection {
            version: 1,
            main_package: Some(MainPackage {
                name: name.into(),
                scope: Some(ResourceScope::Project),
                instructions: vec!["prompts/system.md".into()],
            }),
            skills: vec![],
            extensions: vec![],
            instructions: vec![],
            tools: ResourceTools::Default {},
        })
    }
}
pub(super) const BASE: &[(&str, &str)] = &[
    (
        "prompts/system.md",
        include_str!("../../assets/packages/base/prompts/system.md"),
    ),
    (
        "skills/work-basics/SKILL.md",
        include_str!("../../assets/packages/base/skills/work-basics/SKILL.md"),
    ),
    (
        "PROVENANCE.md",
        "RPI Work shipped base Package, version 1.0.0. Resource-only canonical native layout. Source: assets/packages/base. No build or executable payload.\n",
    ),
];
pub(super) const DEVELOPER: &[(&str, &str)] = &[
    (
        "prompts/system.md",
        include_str!("../../assets/packages/developer/prompts/system.md"),
    ),
    (
        "skills/resource-authoring/SKILL.md",
        include_str!("../../assets/packages/developer/skills/resource-authoring/SKILL.md"),
    ),
    (
        "skills/resource-authoring/references/layout.md",
        include_str!(
            "../../assets/packages/developer/skills/resource-authoring/references/layout.md"
        ),
    ),
    (
        "skills/resource-authoring/templates/system.md",
        "Explain the resource's purpose and concrete operating rules here. Do not claim permissions or tools not actually loaded.\n",
    ),
    (
        "PROVENANCE.md",
        "RPI Work shipped developer Package, version 1.0.0. Resource-only canonical native layout. Source: assets/packages/developer. No build or executable payload.\n",
    ),
];
impl LocalWorkspace {
    fn project_selection(
        &self,
        catalog: &NativeCatalog,
        selection: &ResourceSelection,
    ) -> NativeDesired {
        let reference = |identity: &str, expected: NativeKind, s: Option<ResourceScope>| {
            let found = catalog.resources.iter().find(|r| {
                r.kind == expected
                    && if expected == NativeKind::Package {
                        r.name == identity && scope(r.scope) == s.unwrap_or(ResourceScope::Project)
                    } else {
                        let field = if expected == NativeKind::Skill {
                            "skills"
                        } else {
                            "extensions"
                        };
                        r.facts["selectable"][field]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|p| p.as_str() == Some(identity)))
                    }
            });
            NativeReference { reference: found.map(|r| r.reference.clone()), identity: identity.into(), diagnostic: found.is_none().then(|| "Unresolved installed identity; saved intent retained, not executable authority".into()) }
        };
        let supplementary_instructions = if selection.instructions.is_empty() {
            Some(String::new())
        } else {
            selection
                .instructions
                .iter()
                .map(|p| {
                    let path = Path::new(p);
                    if !path.starts_with(self.inner.data.join("instructions")) {
                        return None;
                    }
                    let metadata = std::fs::symlink_metadata(path).ok()?;
                    if !metadata.is_file() || metadata.len() > 65536 {
                        return None;
                    }
                    std::fs::read_to_string(path).ok()
                })
                .collect::<Option<Vec<_>>>()
                .map(|s| s.join("\n"))
        };
        NativeDesired {
            main_package: selection
                .main_package
                .as_ref()
                .map(|p| reference(&p.name, NativeKind::Package, p.scope)),
            package_instructions: selection
                .main_package
                .as_ref()
                .map_or_else(Vec::new, |p| p.instructions.clone()),
            skills: selection
                .skills
                .iter()
                .map(|p| reference(p, NativeKind::Skill, None))
                .collect(),
            extensions: selection
                .extensions
                .iter()
                .map(|p| reference(p, NativeKind::Extension, None))
                .collect(),
            tools: match &selection.tools {
                ResourceTools::Default {} => NativeTools::Default {},
                ResourceTools::None {} => NativeTools::None {},
                ResourceTools::Only { names } => NativeTools::Only {
                    names: names.clone(),
                },
            },
            supplementary_instructions,
        }
    }
    fn resource_intent_path(&self, id: &str, operation: &str) -> PathBuf {
        self.inner
            .data
            .join("resource-intents")
            .join(digest(id.as_bytes()))
            .join(format!("{}.json", digest(operation.as_bytes())))
    }
    fn project_operation(
        &self,
        catalog: &NativeCatalog,
        op: &ResourceOperation,
        id: &str,
    ) -> NativeOperation {
        let retry = (|| {
            use std::io::Read;
            let path = self.resource_intent_path(id, &op.request.operation_id);
            let file = std::fs::File::open(path).ok()?;
            if file.metadata().ok()?.len() > 128 * 1024 {
                return None;
            }
            let mut bytes = Vec::new();
            file.take(128 * 1024 + 1).read_to_end(&mut bytes).ok()?;
            let intent: WorkIntent = serde_json::from_slice(&bytes).ok()?;
            (intent.native == op.request).then_some(intent.request)
        })();
        NativeOperation {
            retry,
            operation_id: op.request.operation_id.clone(),
            expected_revision: op.request.expected_revision,
            target: self.project_selection(catalog, &op.request.target),
            supersedes_operation_id: op.request.supersedes_operation_id.clone(),
            status: match op.status {
                ResourceOperationStatus::Pending => NativeOperationStatus::Pending,
                ResourceOperationStatus::Blocked => NativeOperationStatus::Blocked,
                ResourceOperationStatus::Applied => NativeOperationStatus::Applied,
                ResourceOperationStatus::Superseded => NativeOperationStatus::Superseded,
            },
            result_revision: op.result_revision,
            error: op.error.clone(),
            generation: op.observation.as_ref().map(|o| o.generation.clone()),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkIntent {
    request: NativeApply,
    native: ApplyResourcesRequest,
}

fn read_intent_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 128 * 1024 {
        return Err(std::io::Error::other("invalid bounded resource intent"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(128 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 128 * 1024 {
        return Err(std::io::Error::other("resource intent exceeds bound"));
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests;
