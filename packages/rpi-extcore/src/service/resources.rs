use super::*;
mod intent;
use intent::{planned_record, require_retry_headroom};
#[cfg(test)]
#[path = "resource_tests.rs"]
mod tests;
use crate::host::SessionOptions;
use crate::resources::local::{LocalResourceRequest, LocalResourceResult};
use crate::resources::*;

/// Caller-owned short project exclusion for authorized local filesystem CAS,
/// snapshot or validation. It fences ONLY this Service's owned native hosts;
/// external/ACP owners must be fenced by the application or rejected there.
/// Retain the guard in any blocking worker until that worker actually exits.
/// Dropping it releases admission without retirement or child shutdown.
#[must_use = "retain the guard through the complete protected operation"]
pub struct ResourceProjectFence {
    _owner: Service,
    _lifecycle: tokio::sync::OwnedMutexGuard<()>,
    _hosts: Vec<(Arc<HostSession>, crate::host::ResourceFence)>,
    sessions: Vec<String>,
}
impl ResourceProjectFence {
    pub fn session_ids(&self) -> &[String] {
        &self.sessions
    }
}

const RECORD_CAP: usize = 2 * 1024 * 1024;
// A successful observation is stored both as current state and as operation
// result. Also allow the complete target to replace a much smaller selection,
// bounded diagnostic/metadata and future integer widths. Reserve BEFORE stop.
const RESULT_HEADROOM: usize = 2 * MAX_STATE_BYTES + MAX_CONFIG_BYTES + 32 * 1024;

pub(super) fn reply_error(error: ResourceError) -> (ReplyKind, Value) {
    let kind = match error.kind {
        ResourceErrorKind::InvalidInput => ReplyKind::InvalidInput,
        ResourceErrorKind::Unsupported => ReplyKind::Unsupported,
        ResourceErrorKind::Conflict => ReplyKind::Conflict,
        ResourceErrorKind::LimitExceeded => ReplyKind::LimitExceeded,
        ResourceErrorKind::Unavailable => ReplyKind::HostUnavailable,
        ResourceErrorKind::Storage => ReplyKind::Internal,
    };
    (kind, json!({"error":error.message}))
}
fn storage(error: String) -> ResourceError {
    ResourceError::new(ResourceErrorKind::Storage, error)
}
fn entry(id: &str, project: &Project) -> RegistryEntry {
    RegistryEntry {
        session_id: id.into(),
        project_id: Some(project.id.clone()),
        title: None,
        parent_session_id: None,
        fork_entry_id: None,
        relation: None,
        created_at: 0,
        updated_at: 0,
    }
}
pub(super) async fn prepare_create(
    state: &Service,
    id: &str,
    project: &Project,
    selection: ResourceSelection,
) -> ResourceResult<()> {
    selection.validate()?;
    local::capabilities(state.config.rpi_bin.clone(), project.root.clone())
        .await?
        .require_exact(false)?;
    let record = ResourceConfiguration {
        selection,
        revision: 1,
        settings: Default::default(),
        current_settings: None,
        observation: None,
        operations: vec![],
    };
    state
        .registry
        .write()
        .await
        .set_resource_configuration(entry(id, project), record)
        .map_err(storage)
}
pub(super) async fn host_options(
    state: &Service,
    id: &str,
    project: &Project,
) -> ResourceResult<SessionOptions> {
    state.check_resource_storage(id)?;
    let record = state
        .registry
        .read()
        .await
        .resource_configuration(id)
        .cloned();
    if record.as_ref().is_some_and(ResourceConfiguration::blocked) {
        return Err(ResourceError::conflict(
            "resource operation blocked; explicitly retry identical operation",
        ));
    }
    let captured = record
        .as_ref()
        .and_then(|r| r.current_settings.as_ref())
        .map(|capture| capture.settings.clone());
    let settings = captured
        .clone()
        .or_else(|| record.as_ref().map(|r| r.settings.clone()))
        .unwrap_or_default();
    Ok(SessionOptions {
        executable: state.config.rpi_bin.clone(),
        project: project.root.clone(),
        session_id: id.into(),
        session_dir: state.config.session_dir.clone(),
        saved_session: None,
        fork_source: None,
        timings_dir: state.config.registry.with_extension("tool-timings"),
        model: captured.as_ref().and_then(|s| s.model.clone()),
        provider: captured.as_ref().and_then(|s| s.provider.clone()),
        permission_gate: settings.permission_gate,
        bypass: settings.bypass,
        resources: record.map(|r| ResourceStartup::Exact(r.selection)),
    })
}
async fn save_record(
    state: &Service,
    id: &str,
    record: ResourceConfiguration,
) -> ResourceResult<()> {
    let entry = state
        .registry
        .read()
        .await
        .get(id)
        .cloned()
        .ok_or_else(|| ResourceError::conflict("unknown session"))?;
    publish_record(state, entry, Some(record)).await
}
/// The final publication transaction. Resource failures latch even if rename
/// may already have committed; no live candidate may become reachable on error.
pub(super) async fn publish_record(
    state: &Service,
    mut entry: RegistryEntry,
    record: Option<ResourceConfiguration>,
) -> ResourceResult<()> {
    let id = entry.session_id.clone();
    let configured = record.is_some();
    let mut registry = state.registry.write().await;
    if let Some(latest) = registry.get(&id) {
        let updated_at = latest.updated_at.max(entry.updated_at);
        let project_id = latest.project_id.clone().or(entry.project_id);
        entry = latest.clone();
        entry.updated_at = updated_at;
        entry.project_id = project_id;
    }
    let result = match record {
        Some(record) => registry.set_resource_configuration(entry, record),
        None => registry.upsert(entry),
    };
    drop(registry);
    if let Err(error) = result {
        if configured {
            state
                .resource_storage_failed
                .lock()
                .expect("resource storage latch")
                .insert(id.clone());
            if let Some(host) = state.live_session(&id).await {
                host.block_resource_storage();
            }
        }
        return Err(storage(format!("resource registry write failed or uncertain; admission blocked until owner restart/inspection: {error}")));
    }
    Ok(())
}
pub(super) async fn prepare_open_observation(
    state: &Service,
    id: &str,
    host: &HostSession,
) -> ResourceResult<Option<ResourceConfiguration>> {
    let record = state
        .registry
        .read()
        .await
        .resource_configuration(id)
        .cloned();
    if let Some(mut record) = record {
        // Only the newest explicit owned stop capture may overlay native
        // baseline settings. Historical apply snapshots are never replayed.
        if let Some(level) = record
            .current_settings
            .as_ref()
            .and_then(|c| c.settings.thinking.as_ref())
        {
            host.set_thinking_level(level)
                .await
                .map_err(ResourceError::unavailable)?;
        }
        let observation = host.resource_state().await?;
        record.current_settings = None;
        observation.verify(id, &record.selection)?;
        record.observation = Some(observation);
        return Ok(Some(record));
    }
    Ok(None)
}
pub(super) async fn prepare_fork_selection(
    state: &Service,
    source: &str,
    id: &str,
    host: &HostSession,
    settings: Option<&ResourceSessionSettings>,
) -> ResourceResult<Option<ResourceConfiguration>> {
    let record = state
        .registry
        .read()
        .await
        .resource_configuration(source)
        .cloned();
    if let Some(mut record) = record {
        record.revision = 1;
        if let Some(level) = settings.and_then(|s| s.thinking.as_ref()).or_else(|| {
            record
                .current_settings
                .as_ref()
                .and_then(|c| c.settings.thinking.as_ref())
        }) {
            host.set_thinking_level(level)
                .await
                .map_err(ResourceError::unavailable)?;
        }
        record.operations.clear();
        record.current_settings = None;
        record.observation = Some(host.resource_state().await?);
        record
            .observation
            .as_ref()
            .unwrap()
            .verify(id, &record.selection)?;
        return Ok(Some(record));
    }
    Ok(None)
}
pub(super) fn capture_settings(
    options: &SessionOptions,
    state: &Value,
) -> ResourceResult<ResourceSessionSettings> {
    fn optional(state: &Value, key: &str) -> ResourceResult<Option<String>> {
        match state.get(key) {
            Some(Value::Null) | None => Ok(None),
            Some(Value::String(s))
                if !s.is_empty() && s.len() <= 4096 && !s.chars().any(char::is_control) =>
            {
                Ok(Some(s.clone()))
            }
            _ => Err(ResourceError::unavailable(
                "invalid native session settings",
            )),
        }
    }
    Ok(ResourceSessionSettings {
        model: optional(state, "modelId")?.or(optional(state, "model")?),
        provider: optional(state, "provider")?,
        thinking: optional(state, "thinkingLevel")?,
        permission_gate: options.permission_gate,
        bypass: options.bypass,
    })
}
pub(super) async fn capture_idle_settings_for_stop(
    state: &Service,
    id: &str,
    host: &HostSession,
    fence: &crate::host::ResourceFence,
) -> ResourceResult<()> {
    let Some(mut record) = state
        .registry
        .read()
        .await
        .resource_configuration(id)
        .cloned()
    else {
        return Ok(());
    };
    let native_state = host.verify_project_idle(fence).await?;
    let observation = host.fenced_resource_state(fence).await?;
    observation.verify(id, &record.selection)?;
    let settings = capture_settings(&host.launch_options()?, &native_state)?;
    record.current_settings = Some(ResourceSettingsCapture {
        settings,
        captured_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        host_id: observation.body["hostId"]
            .as_str()
            .ok_or_else(|| ResourceError::unavailable("missing capture host identity"))?
            .into(),
    });
    save_record(state, id, record).await
}
impl Service {
    pub(crate) fn check_resource_storage(&self, id: &str) -> ResourceResult<()> {
        if self
            .resource_storage_failed
            .lock()
            .expect("resource storage latch")
            .contains(id)
        {
            return Err(storage("resource registry write failed or uncertain; inspect/restart owner before recovery".into()));
        }
        Ok(())
    }
    async fn resource_record(&self, id: &str) -> ResourceResult<Option<ResourceConfiguration>> {
        self.check_resource_storage(id)?;
        let registry = self.registry.read().await;
        if registry.get(id).is_none() {
            return Err(ResourceError::conflict("unknown session"));
        }
        Ok(registry.resource_configuration(id).cloned())
    }
    /// Only a verified active configuration. Failed/pending legacy opt-in has
    /// no configuration; its original immutable intent remains in operations.
    pub async fn resource_configuration(
        &self,
        id: &str,
    ) -> ResourceResult<Option<ResourceConfiguration>> {
        Ok(self.resource_record(id).await?.filter(|r| r.revision != 0))
    }
    pub async fn resource_operations(&self, id: &str) -> ResourceResult<Vec<ResourceOperation>> {
        Ok(self
            .resource_record(id)
            .await?
            .map(|r| r.operations)
            .unwrap_or_default())
    }
    pub async fn resource_operation(
        &self,
        id: &str,
        operation_id: &str,
    ) -> ResourceResult<Option<ResourceOperation>> {
        validate_operation_id(operation_id)?;
        Ok(self.resource_record(id).await?.and_then(|r| {
            r.operations
                .into_iter()
                .find(|op| op.request.operation_id == operation_id)
        }))
    }
    /// Read-only native-owned view. No executable session is opened, no
    /// configured resources are loaded, and pending native work stays pending.
    pub async fn resource_saved_history(
        &self,
        id: String,
        query: ResourceHistoryRequest,
    ) -> ResourceResult<ResourceSavedHistory> {
        query.validate()?;
        let permit = self
            .resource_workers
            .clone()
            .try_acquire_owned()
            .map_err(|_| ResourceError::conflict("resource reader at capacity"))?;
        let owner = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _lifecycle = owner.session_lifecycle.lock().await;
            if owner.closing.load(std::sync::atomic::Ordering::Acquire) {
                return Err(ResourceError::unavailable("service shutting down"));
            }
            if owner.registry.read().await.deletion(&id).is_some() {
                return Err(ResourceError::conflict("session deletion is pending"));
            }
            let project = session_project(&owner, &id)
                .await
                .map_err(ResourceError::unavailable)?;
            let host = owner.live_session(&id).await;
            let _fence = if let Some(host) = &host {
                if host.is_resource_retired() {
                    None
                } else {
                    let fence = host.resource_fence()?;
                    if host.snapshot().await.status != "closed" {
                        return Err(ResourceError::conflict(
                            "live native session owns history; use live history API",
                        ));
                    }
                    Some(fence)
                }
            } else {
                None
            };
            if let Some(host) = &host {
                host.await_resource_exit().await?;
            }
            let read = crate::history::saved::read(
                owner.config.rpi_bin.clone(),
                project.root,
                owner.config.session_dir.clone(),
                id.clone(),
                query,
            )
            .await?;
            let mut data = read.data;
            data.blocked_operation_id = owner
                .registry
                .read()
                .await
                .resource_configuration(&id)
                .filter(|r| r.blocked())
                .and_then(|r| r.operations.last())
                .map(|op| op.request.operation_id.clone());
            Ok(data)
        })
        .await
        .map_err(|_| ResourceError::unavailable("saved history owner failed"))?
    }
    /// Caller cancellation does not cancel the owner or release replacement
    /// exclusion. Service shutdown waits on the same lifecycle owner.
    pub async fn apply_resources(
        &self,
        id: String,
        request: ApplyResourcesRequest,
    ) -> ResourceResult<ResourceOperation> {
        let permit = self
            .resource_workers
            .clone()
            .try_acquire_owned()
            .map_err(|_| ResourceError::conflict("resource operation at capacity"))?;
        let owner = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            owner.apply_resources_owned(id, request).await
        })
        .await
        .map_err(|_| {
            ResourceError::unavailable("resource operation owner failed; inspect durable status")
        })?
    }
    async fn apply_resources_owned(
        &self,
        id: String,
        request: ApplyResourcesRequest,
    ) -> ResourceResult<ResourceOperation> {
        request.validate()?;
        let _lifecycle = self.session_lifecycle.lock().await;
        if self.closing.load(std::sync::atomic::Ordering::Acquire) {
            return Err(ResourceError::unavailable("service shutting down"));
        }
        self.check_session_admission(&id)
            .await
            .map_err(|(_, value)| {
                ResourceError::conflict(value["error"].as_str().unwrap_or("session is unavailable"))
            })?;
        let mut record = self.resource_record(&id).await?;
        let existing = record.as_ref().and_then(|r| {
            r.operations
                .iter()
                .position(|op| op.request.operation_id == request.operation_id)
        });
        let mut superseding = None;
        if let Some(index) = existing {
            let op = &record.as_ref().unwrap().operations[index];
            if op.request != request {
                return Err(ResourceError::conflict(
                    "resource operation ID reused with different payload",
                ));
            }
            if matches!(
                op.status,
                ResourceOperationStatus::Applied | ResourceOperationStatus::Superseded
            ) {
                return Ok(op.clone());
            }
            require_retry_headroom(record.as_ref().unwrap())?;
        } else {
            if record.as_ref().map_or(0, |r| r.revision) != request.expected_revision {
                return Err(ResourceError::conflict(
                    "stale application resource revision",
                ));
            }
            if record.as_ref().is_some_and(ResourceConfiguration::blocked) {
                let prior = record.as_ref().unwrap();
                let index = prior.operations.len() - 1;
                if request.supersedes_operation_id.as_ref()
                    != Some(&prior.operations[index].request.operation_id)
                {
                    return Err(ResourceError::conflict(
                        "explicit latest blocked operation ID is required to supersede",
                    ));
                }
                superseding = Some(index);
            } else if request.supersedes_operation_id.is_some() {
                return Err(ResourceError::conflict(
                    "superseded operation is not the latest blocked intent",
                ));
            }
            if record
                .as_ref()
                .is_some_and(|r| r.operations.len() >= MAX_OPERATIONS)
            {
                return Err(ResourceError::new(
                    ResourceErrorKind::LimitExceeded,
                    "resource operation identity ledger full",
                ));
            }
        }
        // A retained observation is ONLY the original CAS expectation. It never
        // authorizes offline execution without canonical native idle proof.
        let expected = if let Some(index) = existing.or(superseding) {
            record.as_ref().unwrap().operations[index]
                .transition
                .expected
                .clone()
        } else if let Some(prior) = record.as_ref().and_then(|r| r.observation.as_ref()) {
            prior.verify(&id, &record.as_ref().unwrap().selection)?;
            ExpectedBinding {
                revision: prior.binding.revision,
                fingerprint: prior.binding.fingerprint.clone(),
            }
        } else {
            ExpectedBinding {
                revision: 0,
                fingerprint: None,
            }
        };
        // Pure whole-ledger/native-request preflight, including worst-case result
        // room, precedes even cleanup of an already closed/retired owned child.
        if existing.is_none() {
            planned_record(
                record.as_ref(),
                &request,
                expected.clone(),
                None,
                superseding,
            )?;
        }
        let project = session_project(self, &id)
            .await
            .map_err(ResourceError::unavailable)?;
        local::capabilities(self.config.rpi_bin.clone(), project.root.clone())
            .await?
            .require_exact(true)?;
        let host = self.live_session(&id).await;
        let mut fence = None;
        let mut live_expected = None;
        let mut live_settings = None;
        if let Some(host) = &host {
            if !host.is_resource_retired() {
                let claimed = host.resource_fence()?;
                if host.snapshot().await.status == "closed" {
                    host.verify_closed_resource_owners(&claimed).await?;
                } else {
                    if superseding.is_some() {
                        return Err(ResourceError::conflict(
                            "supersede requires the original owned native child to have exited",
                        ));
                    }
                    let state = host.verify_resource_idle(&claimed).await?;
                    live_settings = Some(capture_settings(&host.launch_options()?, &state)?);
                    if existing.is_none() {
                        live_expected = Some(if let Some(record) = &record {
                            let observation = host.fenced_resource_state(&claimed).await?;
                            observation.verify(&id, &record.selection)?;
                            ExpectedBinding {
                                revision: observation.binding.revision,
                                fingerprint: observation.binding.fingerprint,
                            }
                        } else {
                            let ready = host.ready();
                            if ready["resourceState"]["version"] != 1
                                || ready["resourceState"]["enabled"] != false
                                || ready["resourceState"]["binding"]["revision"] != 0
                            {
                                return Err(ResourceError::new(
                                    ResourceErrorKind::Unsupported,
                                    "legacy host has no explicit transition binding",
                                ));
                            }
                            ExpectedBinding {
                                revision: 0,
                                fingerprint: None,
                            }
                        });
                    }
                }
                fence = Some(claimed);
            }
        }
        let mut saved_read = None;
        if let Some(actual) = live_expected {
            if actual != expected {
                return Err(ResourceError::conflict(
                    "live native binding differs from original CAS expectation",
                ));
            }
        } else {
            if let Some(host) = &host {
                host.await_resource_exit().await?;
            }
            let read = crate::history::saved::read(
                self.config.rpi_bin.clone(),
                project.root.clone(),
                self.config.session_dir.clone(),
                id.clone(),
                ResourceHistoryRequest {
                    limit: 1,
                    cursor: None,
                },
            )
            .await?;
            read.data.ensure_idle()?;
            let actual = read.data.binding.expected();
            let original_committed = existing.is_some()
                && read.data.binding.operation_id.as_ref() == Some(&request.operation_id)
                && expected.revision.checked_add(1) == Some(actual.revision);
            if actual != expected && !original_committed {
                return Err(ResourceError::conflict("native binding committed/changed; original operation recovery is required, not supersede"));
            }
            // Retain the native shared observation lock through the single
            // registry transaction; no external writer can race noncommit proof.
            saved_read = Some(read);
        }
        if existing.is_none() {
            let next = planned_record(
                record.as_ref(),
                &request,
                expected,
                live_settings,
                superseding,
            )?;
            save_record(self, &id, next.clone()).await?;
            record = Some(next);
        }
        if let (Some(host), Some(claimed)) = (&host, &fence) {
            host.retire_resources(claimed);
        }
        // Native transition startup must now claim its own exclusive writer lock
        // and revalidate the exact original CAS/payload before loading resources.
        drop(saved_read);
        let mut record =
            record.ok_or_else(|| ResourceError::conflict("missing durable resource intent"))?;
        let index = record
            .operations
            .iter()
            .position(|op| op.request.operation_id == request.operation_id)
            .ok_or_else(|| ResourceError::conflict("missing operation"))?;
        let transition = record.operations[index].transition.clone();
        let restore_settings = record.operations[index].restore_settings.clone();
        let outcome = async {
            if let Some(host) = &host {
                host.await_resource_exit().await?;
            }
            detach_durable(self, &id).await;
            self.sessions.write().await.remove(&id);
            let settings = record.settings.clone();
            let options = SessionOptions {
                executable: self.config.rpi_bin.clone(),
                project: project.root.clone(),
                session_id: id.clone(),
                session_dir: self.config.session_dir.clone(),
                saved_session: None,
                fork_source: None,
                timings_dir: self.config.registry.with_extension("tool-timings"),
                model: restore_settings.as_ref().and_then(|s| s.model.clone()),
                provider: restore_settings.as_ref().and_then(|s| s.provider.clone()),
                permission_gate: settings.permission_gate,
                bypass: settings.bypass,
                resources: Some(ResourceStartup::Transition(transition.clone())),
            };
            let candidate = HostSession::spawn_options(options)
                .await
                .map_err(ResourceError::unavailable)?;
            let verified = async {
                if let Some(level) = restore_settings.as_ref().and_then(|s| s.thinking.as_ref()) {
                    candidate
                        .set_thinking_level(level)
                        .await
                        .map_err(ResourceError::unavailable)?;
                }
                let observation = candidate.resource_state().await?;
                observation.verify(&id, &request.target)?;
                if observation.binding.operation_id.as_deref() != Some(&request.operation_id) {
                    return Err(ResourceError::conflict(
                        "native resource operation association differs",
                    ));
                }
                let revision = record
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| ResourceError::conflict("application revision overflow"))?;
                record.selection = request.target.clone();
                record.current_settings = None;
                record.revision = revision;
                record.observation = Some(observation.clone());
                record.operations[index].status = ResourceOperationStatus::Applied;
                record.operations[index].result_revision = Some(revision);
                record.operations[index].observation = Some(observation);
                record.operations[index].error = None;
                // Attach optional projection before publication, then persist the
                // verified generation before exposing any execution entrance.
                attach_durable(self, &id, candidate.clone())
                    .await
                    .map_err(ResourceError::unavailable)?;
                save_record(self, &id, record.clone()).await?;
                Ok(())
            }
            .await;
            if let Err(error) = verified {
                detach_durable(self, &id).await;
                candidate.shutdown().await;
                return Err(error);
            }
            self.sessions.write().await.insert(id.clone(), candidate);
            Ok(())
        }
        .await;
        drop(fence);
        if let Err(error) = outcome {
            // Do not publish partially advanced application state on write error.
            let mut blocked = self
                .resource_record(&id)
                .await?
                .ok_or_else(|| ResourceError::conflict("lost resource intent"))?;
            blocked.operations[index].status = ResourceOperationStatus::Blocked;
            blocked.operations[index].error = Some(error.message.chars().take(1024).collect());
            save_record(self, &id, blocked.clone()).await?;
            return Ok(blocked.operations[index].clone());
        }
        Ok(record.operations[index].clone())
    }
    /// Immediate Busy rather than an unbounded guard acquisition queue. Native
    /// auto-queue work must be empty and canonical durable state freshly idle.
    pub async fn resource_project_fence(
        &self,
        project_id: &str,
    ) -> ResourceResult<ResourceProjectFence> {
        let lifecycle = self
            .session_lifecycle
            .clone()
            .try_lock_owned()
            .map_err(|_| ResourceError::conflict("session lifecycle busy"))?;
        if self.closing.load(std::sync::atomic::Ordering::Acquire) {
            return Err(ResourceError::unavailable("service shutting down"));
        }
        project_by_id(self, project_id)
            .await
            .map_err(|_| ResourceError::conflict("project not configured"))?;
        let sessions: Vec<_> = self
            .sessions
            .read()
            .await
            .iter()
            .map(|(id, host)| (id.clone(), host.clone()))
            .collect();
        if sessions.len() > 256 {
            return Err(ResourceError::new(
                ResourceErrorKind::LimitExceeded,
                "project fence host inspection limit exceeded",
            ));
        }
        let mut hosts = Vec::new();
        let mut ids = Vec::new();
        for (id, host) in sessions {
            if session_project(self, &id)
                .await
                .map_err(ResourceError::unavailable)?
                .id
                != project_id
            {
                continue;
            }
            self.check_resource_storage(&id)?;
            let fence = host.resource_fence()?;
            host.verify_project_idle(&fence).await?;
            ids.push(id);
            hosts.push((host, fence));
        }
        Ok(ResourceProjectFence {
            _owner: self.clone(),
            _lifecycle: lifecycle,
            _hosts: hosts,
            sessions: ids,
        })
    }

    /// Native catalog remains distinct from loaded host state. Admission is
    /// bounded and cancellation leaves the CLI/task owner and lifecycle intact.
    pub async fn manage_resources(
        &self,
        request: LocalResourceRequest,
    ) -> ResourceResult<LocalResourceResult> {
        let permit = self
            .resource_workers
            .clone()
            .try_acquire_owned()
            .map_err(|_| ResourceError::conflict("resource management at capacity"))?;
        let owner = self.clone();
        tokio::spawn(async move {
            let _lifecycle = owner.session_lifecycle.lock().await;
            if owner.closing.load(std::sync::atomic::Ordering::Acquire) {
                return Err(ResourceError::unavailable("service shutting down"));
            }
            let project = project_by_id(&owner, &request.project_id)
                .await
                .map_err(|_| ResourceError::conflict("project not configured"))?;
            local::execute(owner.config.rpi_bin.clone(), project.root, request, permit).await
        })
        .await
        .map_err(|_| ResourceError::unavailable("resource management owner failed"))?
    }
    /// All-or-none idle preflight under lifecycle exclusion. Global native
    /// leases can be held by any owned project host, so global release checks all.
    pub async fn release_idle_resources(
        &self,
        project_id: String,
        scope: ResourceScope,
    ) -> ResourceResult<Vec<String>> {
        let permit = self
            .resource_workers
            .clone()
            .try_acquire_owned()
            .map_err(|_| ResourceError::conflict("resource release at capacity"))?;
        let owner = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _lifecycle = owner.session_lifecycle.lock().await;
            if owner.closing.load(std::sync::atomic::Ordering::Acquire) {
                return Err(ResourceError::unavailable("service shutting down"));
            }
            project_by_id(&owner, &project_id)
                .await
                .map_err(|_| ResourceError::conflict("project not configured"))?;
            let hosts: Vec<_> = owner
                .sessions
                .read()
                .await
                .iter()
                .map(|(id, host)| (id.clone(), host.clone()))
                .collect();
            let mut selected = Vec::new();
            for (id, host) in hosts {
                let project = session_project(&owner, &id)
                    .await
                    .map_err(ResourceError::unavailable)?;
                if scope == ResourceScope::Project && project.id != project_id {
                    continue;
                }
                let fence = host.resource_fence()?;
                host.verify_resource_idle(&fence).await?;
                selected.push((id, host, fence));
            }
            for (id, host, fence) in &selected {
                capture_idle_settings_for_stop(&owner, id, host, fence).await?;
            }
            for (_, host, fence) in &selected {
                host.retire_resources(fence);
            }
            let mut released = Vec::new();
            for (id, host, _) in selected {
                host.await_resource_exit().await?;
                detach_durable(&owner, &id).await;
                owner.sessions.write().await.remove(&id);
                released.push(id);
            }
            Ok(released)
        })
        .await
        .map_err(|_| ResourceError::unavailable("idle release owner failed"))?
    }
}
