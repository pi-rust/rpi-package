//! Opt-in isolated native sampling. Keep one owner per native root and call
//! `shutdown` before terminating that host. This is an executor, not consent:
//! callers must supply an explicit core `SamplingAuthorizer` in SamplingOptions.
use crate::host::HostSession;
use futures_util::FutureExt;
pub use rpi_mcp_core::sampling::*;
use rpi_mcp_core::Control;
use serde_json::json;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};
use tokio::{
    runtime::Handle,
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
};

/// Construct on the host main Tokio runtime, never an MCP callback runtime.
/// No Default: a live capable native root and its actual scope are required.
#[derive(Clone)]
pub struct NativeSamplingOwner {
    inner: Arc<Inner>,
}
struct Inner {
    host: Weak<HostSession>,
    runtime: Handle,
    scope: String,
    cwd: PathBuf,
    slots: Arc<Semaphore>,
    state: Mutex<State>,
    shutdown_gate: tokio::sync::Mutex<()>,
}
#[derive(Default)]
struct State {
    closed: bool,
    active: HashMap<String, Control>,
    cleanup: Vec<Arc<Cleanup>>,
    failed: bool,
}
struct Cleanup {
    join: tokio::sync::Mutex<Option<JoinHandle<Result<(), String>>>>,
}
impl Cleanup {
    fn reap(&self) -> Option<Result<(), String>> {
        let mut join = self.join.try_lock().ok()?;
        let handle = join.as_mut()?;
        if !handle.is_finished() {
            return None;
        }
        let result = handle.now_or_never()?;
        *join = None;
        Some(result.unwrap_or_else(|_| Err("Native sampling cleanup failed".into())))
    }
    async fn join(&self) -> Result<(), String> {
        let mut join = self.join.lock().await;
        if let Some(handle) = join.as_mut() {
            let result = handle
                .await
                .unwrap_or_else(|_| Err("Native sampling cleanup failed".into()));
            // Retain failure on the owner before removing the completed handle.
            // The caller stores it synchronously without another await.
            *join = None;
            result
        } else {
            Ok(())
        }
    }
}
impl State {
    fn reap(&mut self) {
        let mut failed = false;
        self.cleanup.retain(|c| match c.reap() {
            Some(result) => {
                failed |= result.is_err();
                false
            }
            None => true,
        });
        self.failed |= failed;
    }
}
impl NativeSamplingOwner {
    /// `scope` must be the actual native root session ID, not an MCP namespace.
    /// When ready.sessionId is present it is checked. `cwd` is the caller-known
    /// host project directory (HostSession does not expose its launch cwd).
    pub fn new(host: &Arc<HostSession>, scope: String, cwd: PathBuf) -> Result<Self, String> {
        let runtime =
            Handle::try_current().map_err(|_| "Native sampling requires the host Tokio runtime")?;
        if !host.supports_command("sample") || !host.supports_command("cancel_sample") {
            return Err("Native sampling commands unavailable".into());
        }
        if scope.trim().is_empty() || scope.len() > 4096 || scope.chars().any(char::is_control) {
            return Err("Invalid native sampling scope".into());
        }
        if let Some(id) = host.ready()["sessionId"].as_str() {
            if id != scope {
                return Err("Native sampling scope mismatch".into());
            }
        }
        if !cwd.is_absolute() {
            return Err("Native sampling requires an absolute project directory".into());
        }
        let cwd =
            std::fs::canonicalize(cwd).map_err(|_| "Invalid native sampling project directory")?;
        if !cwd.is_dir() {
            return Err("Invalid native sampling project directory".into());
        }
        Ok(Self {
            inner: Arc::new(Inner {
                host: Arc::downgrade(host),
                runtime,
                scope,
                cwd,
                slots: Arc::new(Semaphore::new(4)),
                state: Mutex::new(State::default()),
                shutdown_gate: tokio::sync::Mutex::new(()),
            }),
        })
    }
    pub fn executor(&self) -> Arc<dyn SamplingExecutor> {
        Arc::new(NativeSamplingExecutor {
            inner: self.inner.clone(),
        })
    }
    /// Close admission, signal every producer, then join even late Drop cleanup.
    /// Cancellation of this future retains every live join handle for retry.
    /// A closed/lost host is an error, never a native cancellation acknowledgement.
    pub async fn shutdown(&self) -> Result<(), String> {
        let _gate = self.inner.shutdown_gate.lock().await;
        {
            let mut state = self.inner.state.lock().unwrap();
            state.closed = true;
            for control in state.active.values() {
                control.cancel();
            }
        }
        loop {
            let (active, cleanup) = {
                let mut state = self.inner.state.lock().unwrap();
                state.reap();
                (!state.active.is_empty(), state.cleanup.clone())
            };
            for job in cleanup {
                let result = job.join().await;
                let mut state = self.inner.state.lock().unwrap();
                state.failed |= result.is_err();
                state.cleanup.retain(|c| !Arc::ptr_eq(c, &job));
            }
            {
                let state = self.inner.state.lock().unwrap();
                if !active && state.active.is_empty() && state.cleanup.is_empty() {
                    return if state.failed {
                        Err("Native sampling cleanup failed".into())
                    } else {
                        Ok(())
                    };
                }
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
}
/// Uses no foreground run lease, prompt, account/model mutation or root abort.
pub struct NativeSamplingExecutor {
    inner: Arc<Inner>,
}
struct Guard {
    inner: Arc<Inner>,
    id: String,
    permit: Option<OwnedSemaphorePermit>,
    armed: bool,
}
impl Guard {
    fn complete(&mut self) {
        self.inner.state.lock().unwrap().active.remove(&self.id);
        self.armed = false;
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut state = self.inner.state.lock().unwrap();
        let host = self.inner.host.clone();
        let id = self.id.clone();
        let permit = self.permit.take();
        // Registration and producer retirement share the admission lock. Shutdown
        // cannot observe quiescence between them. Never use callback runtime spawn.
        let handle = self.inner.runtime.spawn(async move {
            let _permit = permit;
            let host = host.upgrade().ok_or("Native sampling host unavailable")?;
            let reply = host
                .send_timeout(
                    json!({"type":"cancel_sample","sampleId":id}),
                    Duration::from_secs(5),
                )
                .await
                .map_err(|_| "Native sampling cancellation failed")?;
            if reply["success"] != true {
                return Err("Native sampling cancellation failed".into());
            }
            Ok(())
        });
        state.cleanup.push(Arc::new(Cleanup {
            join: tokio::sync::Mutex::new(Some(handle)),
        }));
        state.active.remove(&self.id);
    }
}
fn check(
    context: &SamplingContext,
    control: &Control,
    local: &Control,
) -> Result<Duration, SamplingError> {
    if control.check().is_err() || local.check().is_err() {
        return Err(SamplingError::Cancelled);
    }
    let remaining = context
        .deadline
        .saturating_duration_since(Instant::now())
        .min(Duration::from_secs(300));
    if remaining.as_millis() == 0 {
        return Err(SamplingError::TimedOut);
    }
    Ok(remaining)
}
impl SamplingExecutor for NativeSamplingExecutor {
    fn execute<'a>(
        &'a self,
        context: &'a SamplingContext,
        request: &'a SamplingRequest,
        control: &'a Control,
    ) -> SamplingFuture<'a> {
        Box::pin(async move {
            if context.scope != self.inner.scope || context.cwd != self.inner.cwd {
                return Err(SamplingError::Denied);
            }
            // Defense in depth for direct callers; consent still belongs to the
            // core authorizer, not to this low-level trusted executor interface.
            if request
                .messages()
                .iter()
                .any(|m| m["content"]["type"] != "text")
            {
                return Err(SamplingError::Unsupported);
            }
            let local = Control::default();
            check(context, control, &local)?;
            let mut guard = {
                let mut state = self.inner.state.lock().unwrap();
                state.reap();
                if state.closed || state.active.len() + state.cleanup.len() >= 128 {
                    return Err(SamplingError::Unavailable);
                }
                let permit = self
                    .inner
                    .slots
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| SamplingError::Unavailable)?;
                let id = uuid::Uuid::new_v4().to_string();
                state.active.insert(id.clone(), local.clone());
                Guard {
                    inner: self.inner.clone(),
                    id,
                    permit: Some(permit),
                    armed: true,
                }
            };
            let host = self
                .inner
                .host
                .upgrade()
                .ok_or(SamplingError::Unavailable)?;
            let remaining = check(context, control, &local)?;
            let response = tokio::select! {
                biased;
                _ = control.cancelled() => return Err(SamplingError::Cancelled),
                _ = local.cancelled() => return Err(SamplingError::Cancelled),
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(context.deadline)) => return Err(SamplingError::TimedOut),
                response = host.send_timeout(json!({"type":"sample","sampleId":guard.id,"request":request.as_value(),"timeoutMs":remaining.as_millis() as u64}), remaining) => response,
            };
            check(context, control, &local)?;
            let response = response.map_err(|_| SamplingError::ExecutionFailed)?;
            if response["success"] != true {
                return Err(SamplingError::ExecutionFailed);
            }
            let result = SamplingResult::parse(response["data"].clone())?;
            check(context, control, &local)?;
            guard.complete();
            Ok(result)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::TestEvents;
    use serde_json::Value;
    use tokio::io::{AsyncBufReadExt, BufReader, DuplexStream};
    struct Fixture {
        host: Arc<HostSession>,
        events: TestEvents,
        lines: tokio::io::Lines<BufReader<DuplexStream>>,
        owner: NativeSamplingOwner,
    }
    impl Fixture {
        fn new() -> Self {
            let (writer, reader) = tokio::io::duplex(1024 * 1024);
            let (host, events) = HostSession::test_connection_with_ready(
                writer,
                32,
                json!({"type":"ready","protocol":"rpi","sessionId":"root","supportedCommands":["sample","cancel_sample"]}),
            );
            let owner =
                NativeSamplingOwner::new(&host, "root".into(), std::env::current_dir().unwrap())
                    .unwrap();
            Self {
                host,
                events,
                lines: BufReader::new(reader).lines(),
                owner,
            }
        }
        async fn next(&mut self) -> Value {
            let line = tokio::time::timeout(Duration::from_secs(2), self.lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            serde_json::from_str(&line).unwrap()
        }
        fn ack(&self, command: &Value, data: Value) {
            self.events
                .send(json!({"type":"response","id":command["id"],"success":true,"data":data}))
                .unwrap();
        }
        fn context(&self) -> SamplingContext {
            SamplingContext {
                cwd: self.owner.inner.cwd.clone(),
                scope: "root".into(),
                namespace: "ns".into(),
                target: "target".into(),
                server: "server".into(),
                generation: 1,
                operation: "op".into(),
                deadline: Instant::now() + Duration::from_secs(3),
            }
        }
    }
    fn request() -> SamplingRequest {
        SamplingRequest::parse(json!({"messages":[{"role":"user","content":{"type":"text","text":"secret"}}],"maxTokens":32,
            "modelPreferences":{"hints":[{"name":"not-an-account"}]}})).unwrap()
    }
    fn result() -> Value {
        json!({"role":"assistant","content":{"type":"text","text":"answer"},"model":"actual/model","stopReason":"endTurn"})
    }
    fn start(f: &Fixture) -> JoinHandle<Result<SamplingResult, SamplingError>> {
        let executor = f.owner.executor();
        let context = f.context();
        tokio::spawn(async move {
            executor
                .execute(&context, &request(), &Control::default())
                .await
        })
    }
    #[tokio::test]
    async fn scope_cwd_and_capability_fail_closed_without_rpc() {
        let mut f = Fixture::new();
        let executor = f.owner.executor();
        let mut context = f.context();
        context.scope = "sibling".into();
        assert_eq!(
            executor
                .execute(&context, &request(), &Control::default())
                .await
                .unwrap_err(),
            SamplingError::Denied
        );
        context.scope = "root".into();
        context.cwd = context.cwd.join("other");
        assert_eq!(
            executor
                .execute(&context, &request(), &Control::default())
                .await
                .unwrap_err(),
            SamplingError::Denied
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), f.lines.next_line())
                .await
                .is_err()
        );
        assert!(
            NativeSamplingOwner::new(&f.host, "wrong".into(), f.owner.inner.cwd.clone()).is_err()
        );
        let (old, _) = HostSession::test_connection(tokio::io::sink(), 16);
        assert!(NativeSamplingOwner::new(&old, "root".into(), f.owner.inner.cwd.clone()).is_err());
        f.owner.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn success_actual_model_no_foreground_lease_or_mutation() {
        let mut f = Fixture::new();
        let _foreground = f.host.try_run_lease().unwrap();
        let task = start(&f);
        let command = f.next().await;
        assert_eq!(command["type"], "sample");
        assert!(uuid::Uuid::parse_str(command["sampleId"].as_str().unwrap()).is_ok());
        assert!(command["timeoutMs"].as_u64().unwrap() <= 3000);
        assert_eq!(command["request"], *request().as_value());
        f.ack(&command, result());
        assert_eq!(task.await.unwrap().unwrap().model(), "actual/model");
        f.owner.shutdown().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), f.lines.next_line())
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn dropped_future_cancels_exact_uuid_and_recovers_over_128_slots() {
        let mut f = Fixture::new();
        for _ in 0..140 {
            let task = start(&f);
            let sample = f.next().await;
            task.abort();
            let _ = task.await;
            let cancel = f.next().await;
            assert_eq!(cancel["type"], "cancel_sample");
            assert_eq!(sample["sampleId"], cancel["sampleId"]);
            f.ack(&cancel, json!({}));
            while f.owner.inner.slots.available_permits() != 4 {
                tokio::task::yield_now().await;
            }
        }
        f.owner.shutdown().await.unwrap();
        assert!(f.owner.inner.state.lock().unwrap().cleanup.is_empty());
    }
    #[tokio::test]
    async fn shutdown_waits_for_late_producer_drop_and_retry_joins_same_cleanup() {
        let mut f = Fixture::new();
        // Keep producer unpolled after its first native send, as a callback
        // runtime can do while unwinding. Owner must not miss its later Drop.
        let executor = f.owner.executor();
        let context = f.context();
        let req = request();
        let control = Control::default();
        let mut producer = executor.execute(&context, &req, &control);
        assert!(producer.as_mut().now_or_never().is_none());
        let sample = f.next().await;
        let owner = f.owner.clone();
        let shutdown = tokio::spawn(async move { owner.shutdown().await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!shutdown.is_finished());
        drop(producer);
        let cancel = f.next().await;
        assert_eq!(cancel["sampleId"], sample["sampleId"]);
        shutdown.abort();
        let _ = shutdown.await;
        let owner = f.owner.clone();
        let retry = tokio::spawn(async move { owner.shutdown().await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!retry.is_finished());
        f.ack(&cancel, json!({}));
        retry.await.unwrap().unwrap();
        assert!(f.owner.inner.state.lock().unwrap().cleanup.is_empty());
    }
    #[tokio::test]
    async fn admission_includes_cleanup_and_host_loss_is_not_join_ack() {
        let mut f = Fixture::new();
        let mut tasks = Vec::new();
        for _ in 0..4 {
            tasks.push(start(&f));
            f.next().await;
        }
        assert_eq!(
            start(&f).await.unwrap().unwrap_err(),
            SamplingError::Unavailable
        );
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
        for _ in 0..4 {
            assert_eq!(f.next().await["type"], "cancel_sample");
        }
        assert_eq!(
            start(&f).await.unwrap().unwrap_err(),
            SamplingError::Unavailable
        );
        f.host.terminate().await;
        assert!(f.owner.shutdown().await.is_err());
        assert!(f.owner.shutdown().await.is_err());
    }
    #[tokio::test]
    async fn core_wrapper_cancellation_drops_executor_and_joins_exact_sample() {
        let mut f = Fixture::new();
        let executor = f.owner.executor();
        let ctx = f.context();
        let parent = Control::default();
        let control = Control::default().linked(&parent);
        let policy = SamplingPolicy {
            context: ctx.clone(),
            max_messages: 128,
            max_tokens: 32,
            max_duration: Duration::from_secs(300),
        };
        let task = tokio::spawn(async move {
            execute_validated(
                Some(executor.as_ref()),
                Some(&policy),
                &ctx,
                &request(),
                &control,
            )
            .await
        });
        let sample = f.next().await;
        parent.cancel();
        assert_eq!(task.await.unwrap().unwrap_err(), SamplingError::Cancelled);
        let cancel = f.next().await;
        assert_eq!(cancel["type"], "cancel_sample");
        assert_eq!(sample["sampleId"], cancel["sampleId"]);
        f.ack(&cancel, json!({}));
        f.owner.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn invalid_oversized_result_and_deadline_are_redacted_and_cancelled() {
        let mut f = Fixture::new();
        let task = start(&f);
        let sample = f.next().await;
        let mut oversized = result();
        oversized["content"]["text"] = Value::String("x".repeat(256 * 1024));
        f.ack(&sample, oversized);
        assert_eq!(
            task.await.unwrap().unwrap_err(),
            SamplingError::InvalidResult
        );
        let cancel = f.next().await;
        f.ack(&cancel, json!({}));
        let executor = f.owner.executor();
        let mut ctx = f.context();
        ctx.deadline = Instant::now() + Duration::from_millis(40);
        let task = tokio::spawn(async move {
            executor
                .execute(&ctx, &request(), &Control::default())
                .await
        });
        let sample = f.next().await;
        assert!(sample["timeoutMs"].as_u64().unwrap() <= 40);
        let error = task.await.unwrap().unwrap_err();
        assert!(matches!(
            error,
            SamplingError::TimedOut | SamplingError::ExecutionFailed
        ));
        let cancel = f.next().await;
        assert_eq!(sample["sampleId"], cancel["sampleId"]);
        f.ack(&cancel, json!({}));
        f.owner.shutdown().await.unwrap();
    }
}
