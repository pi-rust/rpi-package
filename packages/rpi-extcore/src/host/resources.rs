use super::*;
use crate::resources::*;
use std::io::Write;
#[cfg(test)]
#[path = "resource_tests.rs"]
mod tests;

pub(super) struct StartupFile {
    pub path: PathBuf,
    pub flag: &'static str,
}
impl StartupFile {
    pub(super) async fn prepare(options: &SessionOptions) -> Result<Option<Self>, String> {
        let Some(startup) = &options.resources else {
            return Ok(None);
        };
        let transition = matches!(startup, ResourceStartup::Transition(_));
        let capabilities = local::capabilities(options.executable.clone(), options.project.clone())
            .await
            .map_err(|e| e.to_string())?;
        capabilities
            .require_exact(transition)
            .map_err(|e| e.to_string())?;
        let (flag, bytes) = match startup {
            ResourceStartup::Exact(selection) => {
                selection.validate().map_err(|e| e.to_string())?;
                ("--resource-config", serde_json::to_vec(selection))
            }
            ResourceStartup::Transition(request) => {
                request.target.validate().map_err(|e| e.to_string())?;
                validate_operation_id(&request.operation_id).map_err(|e| e.to_string())?;
                if request.version != 1 || options.fork_source.is_some() {
                    return Err("invalid resource transition startup".into());
                }
                ("--resource-transition", serde_json::to_vec(request))
            }
        };
        let bytes = bytes.map_err(|e| e.to_string())?;
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err("resource startup exceeds 64 KiB".into());
        }
        std::fs::create_dir_all(&options.timings_dir).map_err(|e| e.to_string())?;
        let path = options
            .timings_dir
            .join(format!("resources-{}.json", uuid::Uuid::new_v4()));
        let owner = Self { path, flag };
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&owner.path)
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(Some(owner))
    }
}
impl Drop for StartupFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Holds short command exclusion plus the existing run lease. Queue and dialog
/// controls do not take the run lease; they remain legal during active runs.
pub(crate) struct ResourceFence {
    _admission: tokio::sync::OwnedRwLockWriteGuard<()>,
    _run: tokio::sync::OwnedMutexGuard<()>,
}
impl HostSession {
    pub async fn resource_state(&self) -> ResourceResult<ResourceObservation> {
        if !self.supports_command("get_resource_state")
            || self.ready["resourceState"]["version"] != 1
            || self.ready["resourceState"]["enabled"] != true
        {
            return Err(ResourceError::new(
                ResourceErrorKind::Unsupported,
                "native host has no exact resource observation",
            ));
        }
        let body = self
            .command(
                "get_resource_state",
                json!({"version":1}),
                Duration::from_secs(5),
            )
            .await
            .map_err(ResourceError::unavailable)?;
        let observation = ResourceObservation::parse(body)?;
        let ready_binding: NativeBinding =
            serde_json::from_value(self.ready["resourceState"]["binding"].clone())
                .map_err(|_| ResourceError::unavailable("invalid ready binding"))?;
        if observation.generation != self.ready["resourceState"]["hostId"].as_str().unwrap_or("")
            || observation.binding != ready_binding
        {
            return Err(ResourceError::conflict(
                "native ready/state binding or generation differs",
            ));
        }
        Ok(observation)
    }
    pub(crate) fn resource_fence(&self) -> ResourceResult<ResourceFence> {
        let admission = self
            .admission_gate
            .clone()
            .try_write_owned()
            .map_err(|_| ResourceError::conflict("session commands busy"))?;
        let run = self.try_run_lease().map_err(ResourceError::conflict)?;
        Ok(ResourceFence {
            _admission: admission,
            _run: run,
        })
    }
    async fn resource_idle_command(&self, kind: &str, mut params: Value) -> ResourceResult<Value> {
        params["type"] = json!(kind);
        let reply = self
            .send_unadmitted(params, Duration::from_secs(5))
            .await
            .map_err(ResourceError::unavailable)?;
        if reply["success"] != true {
            return Err(ResourceError::conflict(format!(
                "native {kind} idle preflight rejected"
            )));
        }
        Ok(reply["data"].clone())
    }
    pub(crate) async fn verify_resource_idle(
        &self,
        fence: &ResourceFence,
    ) -> ResourceResult<Value> {
        self.verify_fenced_idle(fence).await
    }
    pub(crate) async fn verify_project_idle(&self, fence: &ResourceFence) -> ResourceResult<Value> {
        self.verify_fenced_idle(fence).await
    }
    async fn verify_fenced_idle(&self, _fence: &ResourceFence) -> ResourceResult<Value> {
        if self.runtime.state.read().await.resource_unsettled
            || self
                .runtime
                .execution
                .lock()
                .expect("execution admission")
                .unsettled()
        {
            return Err(ResourceError::conflict(
                "local execution admission is unsettled",
            ));
        }
        let snapshot = self.snapshot().await;
        if snapshot.status != "ready" || !snapshot.pending_dialogs.is_empty() {
            return Err(ResourceError::conflict("session is not fully idle"));
        }
        if self
            .external_owner
            .lock()
            .expect("external owner")
            .is_some()
            || self.file_io_owner.lock().expect("file I/O owner").is_some()
        {
            return Err(ResourceError::new(ResourceErrorKind::Unsupported, "idle resource/project exclusion cannot prove external tools or client file I/O quiescent; fence their owner or close deliberately"));
        }
        for command in [
            "get_state",
            "get_messages",
            "get_queue",
            "get_session_branch",
        ] {
            if !self.supports_command(command) {
                return Err(ResourceError::new(
                    ResourceErrorKind::Unsupported,
                    "native host lacks complete idle inspection",
                ));
            }
        }
        let state = self.resource_idle_command("get_state", json!({})).await?;
        if state.get("activeOperation") != Some(&Value::Null)
            || state
                .pointer("/queue/steeringCount")
                .and_then(Value::as_u64)
                != Some(0)
            || state
                .pointer("/queue/followUpCount")
                .and_then(Value::as_u64)
                != Some(0)
        {
            return Err(ResourceError::conflict("native run/queue is not idle"));
        }
        let messages = self
            .resource_idle_command("get_messages", json!({"limit":1}))
            .await?;
        if messages["pendingResume"] != false {
            return Err(ResourceError::conflict(
                "native recovery/resume is pending or unknown",
            ));
        }
        let queue = self
            .resource_idle_command("get_queue", json!({"includeCompleted":false}))
            .await?;
        if queue["count"] != 0 || queue["items"].as_array().map(Vec::len) != Some(0) {
            return Err(ResourceError::conflict(
                "native persisted queue is not empty",
            ));
        }
        let branch = self
            .resource_idle_command(
                "get_session_branch",
                json!({"customTypes":[crate::durable::ENTRY_TYPE]}),
            )
            .await?;
        if branch["filtered"] != true {
            return Err(ResourceError::unavailable("unfiltered durable idle state"));
        }
        let entries = branch["entries"]
            .as_array()
            .ok_or_else(|| ResourceError::unavailable("invalid durable idle entries"))?;
        let durable = rpi_durable_core::DurableState::try_from_branch(entries)
            .map_err(ResourceError::unavailable)?;
        if durable.run.is_some_and(|run| {
            matches!(
                run.status,
                rpi_durable_core::RunStatus::Queued
                    | rpi_durable_core::RunStatus::Running
                    | rpi_durable_core::RunStatus::Interrupted
            )
        }) || durable.queue.iter().any(|item| {
            matches!(
                item.status,
                rpi_durable_core::QueueStatus::Queued | rpi_durable_core::QueueStatus::Claimed
            )
        }) {
            return Err(ResourceError::conflict("durable work is pending"));
        }
        Ok(state)
    }
    /// Only retained local owners are checked here. Closed execution intent and
    /// cached projection are NOT offline idle proof; the saved reader supplies
    /// fresh canonical native/durable facts after independent child exit.
    pub(crate) async fn verify_closed_resource_owners(
        &self,
        _fence: &ResourceFence,
    ) -> ResourceResult<()> {
        let state = self.runtime.state.read().await;
        if state.status != "closed" || !state.pending_dialogs.is_empty() {
            return Err(ResourceError::conflict(
                "closed host retains unsettled dialog authority",
            ));
        }
        if self
            .external_owner
            .lock()
            .expect("external owner")
            .is_some()
            || self.file_io_owner.lock().expect("file I/O owner").is_some()
        {
            return Err(ResourceError::new(
                ResourceErrorKind::Unsupported,
                "closed host retains external/client I/O authority",
            ));
        }
        Ok(())
    }
    pub(crate) async fn fenced_resource_state(
        &self,
        _fence: &ResourceFence,
    ) -> ResourceResult<ResourceObservation> {
        ResourceObservation::parse(
            self.resource_idle_command("get_resource_state", json!({"version":1}))
                .await?,
        )
    }
    pub(crate) fn launch_options(&self) -> ResourceResult<SessionOptions> {
        self.launch_options.clone().ok_or_else(|| {
            ResourceError::new(
                ResourceErrorKind::Unsupported,
                "host launch ownership unavailable",
            )
        })
    }
    pub(crate) fn block_resource_storage(&self) {
        self.retired.store(true, Ordering::Release);
    }
    pub(crate) fn is_resource_retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }
    /// Permanent fence: old Arcs can never execute after replacement/release.
    pub(crate) fn retire_resources(&self, _fence: &ResourceFence) {
        self.retired.store(true, Ordering::Release);
    }
    /// No abort/drop-as-exit inference. On timeout retain the owned worker and
    /// permanent fence; never start a second transcript writer.
    pub(crate) async fn await_resource_exit(&self) -> ResourceResult<()> {
        let _shutdown = self.shutdown_gate.lock().await;
        let _ = tokio::time::timeout(Duration::from_secs(1), self.stop()).await;
        let mut worker = self.worker.lock().await;
        if let Some(task) = worker.as_mut() {
            match tokio::time::timeout(Duration::from_secs(10), task).await {
                Err(_) => {
                    return Err(ResourceError::unavailable(
                        "native child exit not established; configuration remains blocked",
                    ))
                }
                Ok(_) => {
                    worker.take();
                }
            }
        }
        // Independent OS-reap evidence is mandatory, including after ordinary
        // shutdown aborted the RPC reader and removed its JoinHandle.
        if let Some(child) = &self.child_owner {
            child
                .stop_checked()
                .await
                .map_err(ResourceError::unavailable)?;
        } else {
            return Err(ResourceError::new(
                ResourceErrorKind::Unsupported,
                "native child exit ownership unavailable",
            ));
        }
        Ok(())
    }
}
