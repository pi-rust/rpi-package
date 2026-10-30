//! Native compaction lifecycle. Acceptance and compaction_finished are not
//! settlement; the existing foreground lease stays owned until settled/closed.
use crate::host::HostSession;
use futures_util::FutureExt;
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{broadcast, oneshot, watch, OwnedMutexGuard},
    time::Instant,
};

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub total: Duration,
    pub drain: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            total: Duration::from_secs(120),
            drain: Duration::from_secs(3),
        }
    }
}

pub(crate) async fn compact(
    host: Arc<HostSession>,
    instructions: Option<String>,
    events: broadcast::Receiver<Value>,
    cancellation: watch::Receiver<u64>,
    lease: OwnedMutexGuard<()>,
    limits: Limits,
) -> Result<Value, String> {
    let (alive, caller_gone) = oneshot::channel::<()>();
    let (reply, result) = oneshot::channel();
    let deadline = Instant::now() + limits.total;
    // Self-owned, bounded worker: dropping the API future closes caller_gone,
    // not the worker. Its join handle remains owned until cleanup completes.
    // No host worker/shutdown lock is held while awaiting native settlement.
    let owner = Arc::new(Mutex::new(None));
    let task_owner = owner.clone();
    {
        let mut slot = owner.lock().expect("compaction worker");
        *slot = Some(tokio::spawn(async move {
            let outcome = std::panic::AssertUnwindSafe(drive(
                &host,
                instructions,
                events,
                cancellation,
                caller_gone,
                deadline,
                limits.drain,
            ))
            .catch_unwind()
            .await;
            let outcome = match outcome {
                Ok(result) => result,
                Err(_) => {
                    host.terminate().await;
                    Err("native compaction worker panicked; backend terminated".into())
                }
            };
            drop(lease);
            let _ = reply.send(outcome);
            task_owner.lock().expect("compaction worker").take();
        }));
    }
    let result = result
        .await
        .map_err(|_| "native compaction worker stopped".to_string())?;
    drop(alive);
    result
}

#[derive(Default)]
struct Tracker {
    accepted: bool,
    invalid_ack: bool,
    id: Option<Value>,
    started: bool,
    finished: Option<Value>,
}
impl Tracker {
    fn observe(&mut self, event: Value, request: &Value) -> Option<Result<Value, String>> {
        match event["type"].as_str() {
            Some("response") if event["id"] == *request && !self.accepted => {
                if event["success"] != true {
                    return Some(Err(event["error"]
                        .as_str()
                        .unwrap_or("native compact request rejected")
                        .into()));
                }
                let data = &event["data"];
                // Older hosts return the final result in the response itself.
                if !self.accepted && data["outcome"].is_string() {
                    return Some(Ok(data.clone()));
                }
                if data["accepted"] == false {
                    return Some(Err(data["error"]
                        .as_str()
                        .unwrap_or("native compact request not accepted")
                        .into()));
                }
                if data["accepted"] == true && !self.accepted {
                    self.accepted = true;
                    self.id = data.get("compactionId").filter(|id| !id.is_null()).cloned();
                } else {
                    // A malformed successful response cannot establish whether
                    // native started work: fail closed instead of waiting 120s.
                    self.invalid_ack = true;
                    return Some(Err(
                        "native compact returned an invalid acknowledgement".into()
                    ));
                }
            }
            Some("compaction_started") if self.accepted && !self.started => {
                let id = event.get("compactionId").filter(|id| !id.is_null());
                if self
                    .id
                    .as_ref()
                    .map_or(true, |expected| id == Some(expected))
                {
                    self.id = id.cloned();
                    self.started = true;
                }
            }
            Some("compaction_finished") if self.accepted && self.started => {
                if self.matches_id(&event) {
                    self.finished = Some(event);
                }
            }
            Some("agent_settled") if self.finished.is_some() => {
                // Native currently omits compactionId on settlement. If a host
                // does provide it, it must agree with the start/finish pair.
                if event.get("compactionId").is_some_and(|id| !id.is_null())
                    && !self.matches_id(&event)
                {
                    return None;
                }
                if !matches!(
                    event["outcome"].as_str(),
                    Some("compacted" | "declined" | "aborted" | "failed")
                ) {
                    return None;
                }
                let mut result = self.finished.take().unwrap();
                result.as_object_mut().unwrap().remove("type");
                for (key, value) in event.as_object().unwrap() {
                    if key != "type" && !value.is_null() {
                        result[key] = value.clone();
                    }
                }
                return Some(Ok(result));
            }
            _ => {}
        }
        None
    }
    fn matches_id(&self, event: &Value) -> bool {
        match &self.id {
            Some(id) => event.get("compactionId") == Some(id),
            None => event.get("compactionId").map_or(true, Value::is_null),
        }
    }
}

async fn drive(
    host: &Arc<HostSession>,
    instructions: Option<String>,
    mut events: broadcast::Receiver<Value>,
    mut cancellation: watch::Receiver<u64>,
    mut caller_gone: oneshot::Receiver<()>,
    deadline: Instant,
    drain: Duration,
) -> Result<Value, String> {
    let request = json!(uuid::Uuid::new_v4().to_string());
    let mut tracker = Tracker::default();
    let send =
        host.notify(json!({"type":"compact", "id":request, "customInstructions":instructions}));
    // notify's mpsc send is cancellation-safe: until it succeeds there is
    // no compact command to abort or acknowledgement/settlement to drain.
    tokio::select! {
        biased;
        _ = cancellation.changed() => return Err("native compaction cancelled".into()),
        _ = &mut caller_gone => return Err("native compaction caller dropped".into()),
        _ = tokio::time::sleep_until(deadline) => return Err("native compaction timed out".into()),
        sent = send => sent?,
    }
    let mut cleanup = None;
    let mut end = deadline;
    loop {
        if let Some(reason) = cleanup.take() {
            end = Instant::now() + drain;
            // Abort is FIFO after the compact request, and is itself bounded.
            if !matches!(
                tokio::time::timeout_at(end, host.notify(json!({"type":"abort"}))).await,
                Ok(Ok(()))
            ) {
                host.terminate().await;
                return Err(format!("{reason}; abort failed, backend terminated"));
            }
            return drain_to_settlement(host, &mut events, &mut tracker, &request, end, reason)
                .await;
        }
        tokio::select! {
            biased;
            _ = cancellation.changed() => cleanup = Some("native compaction cancelled".into()),
            _ = &mut caller_gone => cleanup = Some("native compaction caller dropped".into()),
            _ = tokio::time::sleep_until(end) => cleanup = Some("native compaction timed out".into()),
            event = events.recv() => match checked_event(event) {
                Ok(event) => if let Some(result) = tracker.observe(event, &request) {
                    if tracker.invalid_ack { host.terminate().await; }
                    return result;
                },
                Err(error) => {
                    host.terminate().await;
                    return Err(error);
                }
            }
        }
    }
}

async fn drain_to_settlement(
    host: &Arc<HostSession>,
    events: &mut broadcast::Receiver<Value>,
    tracker: &mut Tracker,
    request: &Value,
    end: Instant,
    reason: String,
) -> Result<Value, String> {
    let settled = tokio::time::timeout_at(end, async {
        loop {
            let event = checked_event(events.recv().await)?;
            if let Some(result) = tracker.observe(event, request) {
                return result;
            }
        }
    })
    .await;
    match settled {
        // A timeout remains an error even when its abort subsequently settles.
        Ok(Ok(_)) if reason == "native compaction timed out" => Err(reason),
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => {
            host.terminate().await;
            Err(error)
        }
        Err(_) => {
            host.terminate().await;
            Err(format!(
                "{reason}; settlement timed out, backend terminated"
            ))
        }
    }
}

fn checked_event(event: Result<Value, broadcast::error::RecvError>) -> Result<Value, String> {
    match event {
        Ok(event) if !matches!(event["type"].as_str(), Some("host_closed" | "session_end")) => {
            Ok(event)
        }
        Ok(_) => Err("native host closed before compaction settled".into()),
        Err(error) => Err(format!("native compaction event stream lost: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::TestEvents;
    use tokio::{
        io::{AsyncBufReadExt, BufReader, DuplexStream, Lines},
        task::JoinHandle,
    };

    struct Peer {
        host: Arc<HostSession>,
        events: TestEvents,
        wire: Lines<BufReader<DuplexStream>>,
    }
    impl Peer {
        fn new(capacity: usize) -> Self {
            let (write, read) = tokio::io::duplex(4096);
            let (host, events) = HostSession::test_connection_with_ready(
                write,
                capacity,
                json!({"type":"ready","protocol":"rpi","supportedCommands":["compact","abort"]}),
            );
            Self {
                host,
                events,
                wire: BufReader::new(read).lines(),
            }
        }
        fn start(&self) -> JoinHandle<Result<Value, String>> {
            let host = self.host.clone();
            tokio::spawn(async move { host.compact(Some("keep details")).await })
        }
        fn start_limited(
            &self,
            total: Duration,
            drain: Duration,
        ) -> (JoinHandle<Result<Value, String>>, watch::Sender<u64>) {
            let host = self.host.clone();
            let events = host.subscribe_raw();
            let lease = host.try_run_lease().unwrap();
            let (cancel, cancellation) = watch::channel(0);
            (
                tokio::spawn(compact(
                    host,
                    None,
                    events,
                    cancellation,
                    lease,
                    Limits { total, drain },
                )),
                cancel,
            )
        }
        async fn command(&mut self) -> Value {
            let line = tokio::time::timeout(Duration::from_secs(1), self.wire.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            serde_json::from_str(&line).unwrap()
        }
        fn send(&self, value: Value) {
            self.events.send(value).unwrap();
        }
        fn ack(&self, request: &Value) {
            self.send(json!({"type":"response","command":"compact","id":request["id"],"success":true,"data":{"accepted":true}}));
        }
        fn started(&self) {
            self.send(json!({"type":"compaction_started","compactionId":"current"}));
        }
        fn finished(&self) {
            self.send(json!({"type":"compaction_finished","compactionId":"current","status":"completed","summary":"from finish","error":null}));
        }
        fn settled(&self, outcome: &str) {
            self.send(
                json!({"type":"agent_settled","outcome":outcome,"summary":null,"error":null}),
            );
        }
        async fn saturate_commands(&mut self) {
            // The first frame exceeds the duplex buffer. Seeing its first
            // bytes without consuming them proves the writer took that frame
            // and cannot finish it while we fill all 32 mpsc slots.
            self.host
                .notify(json!({"type":"padding","data":"x".repeat(8192)}))
                .await
                .unwrap();
            assert!(
                !tokio::time::timeout(Duration::from_secs(1), self.wire.get_mut().fill_buf(),)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_empty()
            );
            for index in 0..32 {
                self.host
                    .notify(json!({"type":"padding","index":index}))
                    .await
                    .unwrap();
            }
            // Directly polling an extra send proves saturation, and dropping
            // this pending future does not enqueue its command.
            assert!(self
                .host
                .notify(json!({"type":"must_not_send"}))
                .now_or_never()
                .is_none());
        }
        async fn assert_unsent_and_reusable(&mut self) {
            assert_eq!(self.host.snapshot().await.status, "ready");
            assert!(self.host.try_run_lease().is_ok());
            assert_eq!(self.command().await["data"], "x".repeat(8192));
            for index in 0..32 {
                assert_eq!(
                    self.command().await,
                    json!({"type":"padding","index":index})
                );
            }
            let prompt = self.host.begin_prompt("still usable".into()).await.unwrap();
            // FIFO: any stray compact/abort would precede this prompt.
            assert_eq!(self.command().await["type"], "prompt");
            assert!(
                tokio::time::timeout(Duration::from_millis(15), self.wire.next_line())
                    .await
                    .is_err()
            );
            drop(prompt);
            self.host.terminate().await;
        }
        async fn idle(&self) {
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if self.host.try_run_lease().is_ok() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
        }
    }
    async fn result(task: JoinHandle<Result<Value, String>>) -> Result<Value, String> {
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
    }
    async fn pending(task: &mut JoinHandle<Result<Value, String>>) {
        assert!(tokio::time::timeout(Duration::from_millis(15), task)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn asynchronous_declined_settles_immediately_and_preserves_terminal_data() {
        for terminal_override in [false, true] {
            let mut peer = Peer::new(64);
            let (mut task, _cancel) =
                peer.start_limited(Duration::from_secs(1), Duration::from_millis(40));
            let request = peer.command().await;
            peer.ack(&request);
            peer.started();
            peer.send(json!({"type":"compaction_finished","compactionId":"current","status":"declined","outcome":"declined","summary":"empty session","error":"nothing to compact"}));
            pending(&mut task).await;
            assert!(peer.host.try_run_lease().is_err());
            peer.send(json!({"type":"agent_settled","outcome":"declined","summary":if terminal_override { Some("terminal summary") } else { None },"error":if terminal_override { Some("terminal error") } else { None }}));
            // Do not wait for the operation timeout/abort drain: declined is
            // authoritative settlement, exactly like compacted or failed.
            let data = tokio::time::timeout(Duration::from_millis(200), task)
                .await
                .expect("declined must settle without an abort")
                .unwrap()
                .unwrap();
            assert_eq!(
                data,
                json!({"compactionId":"current","status":"declined","outcome":"declined","summary":if terminal_override { "terminal summary" } else { "empty session" },"error":if terminal_override { "terminal error" } else { "nothing to compact" }})
            );
            assert!(peer.host.try_run_lease().is_ok());
            assert_eq!(peer.host.snapshot().await.status, "ready");
            let prompt = peer
                .host
                .begin_prompt("after declined".into())
                .await
                .unwrap();
            assert_eq!(peer.command().await["type"], "prompt");
            drop(prompt);
            peer.host.terminate().await;
        }
    }

    #[tokio::test]
    async fn caller_drop_before_enqueue_releases_lease_without_abort() {
        let mut peer = Peer::new(64);
        peer.saturate_commands().await;
        let lease = peer.host.try_run_lease().unwrap();
        let (_cancel, cancellation) = watch::channel(0);
        let mut call = Box::pin(compact(
            peer.host.clone(),
            None,
            peer.host.subscribe_raw(),
            cancellation,
            lease,
            Limits {
                total: Duration::from_secs(1),
                drain: Duration::from_millis(40),
            },
        ));
        // Poll once to create the self-owned worker, then drop the API future
        // before that worker can run (this is a current-thread test runtime).
        assert!(call.as_mut().now_or_never().is_none());
        assert!(peer.host.try_run_lease().is_err());
        drop(call);
        peer.idle().await;
        peer.assert_unsent_and_reusable().await;
    }

    #[tokio::test]
    async fn cancellation_before_enqueue_releases_lease_without_abort() {
        let mut peer = Peer::new(64);
        peer.saturate_commands().await;
        let (task, cancel) = peer.start_limited(Duration::from_secs(1), Duration::from_millis(40));
        // No await: the cancellation is already visible when the worker's
        // initial select is first polled, with a provably full command queue.
        cancel.send_modify(|generation| *generation += 1);
        assert_eq!(
            result(task).await.unwrap_err(),
            "native compaction cancelled"
        );
        peer.assert_unsent_and_reusable().await;
    }

    #[tokio::test]
    async fn timeout_on_full_queue_releases_lease_without_abort() {
        let mut peer = Peer::new(64);
        peer.saturate_commands().await;
        let (task, _cancel) =
            peer.start_limited(Duration::from_millis(30), Duration::from_millis(40));
        assert_eq!(
            result(task).await.unwrap_err(),
            "native compaction timed out"
        );
        peer.assert_unsent_and_reusable().await;
    }

    #[tokio::test]
    async fn immediate_ack_finish_settlement_is_not_lost() {
        let mut peer = Peer::new(64);
        let task = peer.start();
        let request = peer.command().await;
        assert_eq!(request["customInstructions"], "keep details");
        peer.ack(&request);
        peer.started();
        peer.finished();
        peer.settled("compacted");
        let data = result(task).await.unwrap();
        assert_eq!(data["outcome"], "compacted");
        assert_eq!(data["summary"], "from finish");
        assert!(peer.host.try_run_lease().is_ok());
        peer.host.terminate().await;
    }

    #[tokio::test]
    async fn ack_and_finished_hold_lease_and_ignore_unrelated_or_early_frames() {
        let mut peer = Peer::new(64);
        let mut task = peer.start();
        let request = peer.command().await;
        peer.send(json!({"type":"response","id":"unrelated","success":true,"data":{"outcome":"compacted"}}));
        peer.settled("failed");
        pending(&mut task).await;
        peer.ack(&request);
        peer.settled("compacted");
        pending(&mut task).await;
        assert!(peer.host.begin_prompt("blocked".into()).await.is_err());
        assert!(peer.host.compact(None).await.is_err());
        peer.started();
        peer.send(json!({"type":"compaction_finished","compactionId":"old","status":"completed"}));
        peer.settled("compacted");
        pending(&mut task).await;
        peer.finished();
        peer.send(json!({"type":"agent_settled","outcome":"compacted","compactionId":"old"}));
        peer.settled("completed");
        pending(&mut task).await;
        assert!(peer.host.try_run_lease().is_err());
        peer.settled("compacted");
        assert_eq!(result(task).await.unwrap()["outcome"], "compacted");
        let prompt = peer.host.begin_prompt("available".into()).await.unwrap();
        assert_eq!(peer.command().await["type"], "prompt");
        drop(prompt);
        peer.host.terminate().await;
    }

    #[tokio::test]
    async fn ack_compaction_id_correlates_start_and_finish() {
        let mut peer = Peer::new(64);
        let mut task = peer.start();
        let request = peer.command().await;
        peer.send(json!({"type":"response","id":request["id"],"success":true,"data":{"accepted":true,"compactionId":"current"}}));
        peer.send(json!({"type":"compaction_started","compactionId":"old"}));
        peer.send(json!({"type":"compaction_finished","compactionId":"old"}));
        peer.settled("compacted");
        pending(&mut task).await;
        peer.started();
        peer.finished();
        peer.settled("compacted");
        assert!(result(task).await.is_ok());
        peer.host.terminate().await;
    }

    #[tokio::test]
    async fn rejection_returns_immediately_and_releases_lease() {
        for data in [
            json!({"success":false,"error":"rejected"}),
            json!({"success":true,"data":{"accepted":false,"error":"rejected"}}),
        ] {
            let mut peer = Peer::new(64);
            let task = peer.start();
            let request = peer.command().await;
            let mut response = data;
            response["type"] = json!("response");
            response["id"] = request["id"].clone();
            peer.send(response);
            assert_eq!(result(task).await.unwrap_err(), "rejected");
            assert!(peer.host.try_run_lease().is_ok());
            peer.host.terminate().await;
        }
    }

    #[tokio::test]
    async fn synchronous_legacy_preserves_terminal_data() {
        let mut peer = Peer::new(64);
        let task = peer.start();
        let request = peer.command().await;
        let data = json!({"outcome":"compacted","summary":"legacy","extra":42});
        peer.send(json!({"type":"response","id":request["id"],"success":true,"data":data}));
        assert_eq!(result(task).await.unwrap(), data);
        assert!(peer.host.try_run_lease().is_ok());
        peer.host.terminate().await;
    }

    #[tokio::test]
    async fn asynchronous_failure_preserves_summary_and_error() {
        let mut peer = Peer::new(64);
        let task = peer.start();
        let request = peer.command().await;
        peer.ack(&request);
        peer.started();
        peer.send(json!({"type":"compaction_finished","compactionId":"current","status":"failed","summary":"partial","error":"finish error"}));
        peer.send(json!({"type":"agent_settled","outcome":"failed","error":"terminal error"}));
        let data = result(task).await.unwrap();
        assert_eq!(data["outcome"], "failed");
        assert_eq!(data["summary"], "partial");
        assert_eq!(data["error"], "terminal error");
        assert!(peer.host.try_run_lease().is_ok());
        peer.host.terminate().await;
    }

    #[tokio::test]
    async fn abort_run_drains_until_settlement_before_releasing_lease() {
        let mut peer = Peer::new(64);
        let mut task = peer.start();
        let request = peer.command().await;
        peer.ack(&request);
        peer.started();
        peer.host.abort_run().await.unwrap();
        assert_eq!(peer.command().await["type"], "abort");
        // The lifecycle owner also issues an abort; either is harmless.
        assert_eq!(peer.command().await["type"], "abort");
        peer.finished();
        pending(&mut task).await;
        assert!(peer.host.try_run_lease().is_err());
        peer.settled("aborted");
        assert_eq!(result(task).await.unwrap()["outcome"], "aborted");
        assert!(peer.host.try_run_lease().is_ok());
        peer.host.terminate().await;
    }

    #[tokio::test]
    async fn caller_drop_aborts_and_retains_lease_until_settled() {
        let mut peer = Peer::new(64);
        let task = peer.start();
        let request = peer.command().await;
        peer.ack(&request);
        peer.started();
        task.abort();
        let _ = task.await;
        assert_eq!(peer.command().await["type"], "abort");
        peer.finished();
        tokio::time::sleep(Duration::from_millis(15)).await;
        assert!(peer.host.try_run_lease().is_err());
        peer.settled("aborted");
        peer.idle().await;
        assert_ne!(peer.host.snapshot().await.status, "closed");
        let prompt = peer.host.begin_prompt("after drop".into()).await.unwrap();
        assert_eq!(peer.command().await["type"], "prompt");
        drop(prompt);
        peer.host.terminate().await;
    }

    #[tokio::test]
    async fn timeout_without_settlement_terminates_host() {
        let mut peer = Peer::new(64);
        let (task, _cancel) =
            peer.start_limited(Duration::from_millis(30), Duration::from_millis(40));
        let request = peer.command().await;
        peer.ack(&request);
        peer.started();
        peer.finished();
        assert_eq!(peer.command().await["type"], "abort");
        let error = result(task).await.unwrap_err();
        assert!(error.contains("settlement timed out"), "{error}");
        assert_eq!(peer.host.snapshot().await.status, "closed");
        assert!(peer.host.try_run_lease().is_ok());
        assert!(peer.host.begin_prompt("closed".into()).await.is_err());
    }

    #[tokio::test]
    async fn caller_drop_without_settlement_is_bounded_and_closes_host() {
        let mut peer = Peer::new(64);
        let (task, _cancel) = peer.start_limited(Duration::from_secs(1), Duration::from_millis(40));
        let request = peer.command().await;
        peer.ack(&request);
        peer.started();
        task.abort();
        let _ = task.await;
        assert_eq!(peer.command().await["type"], "abort");
        peer.idle().await;
        assert_eq!(peer.host.snapshot().await.status, "closed");
        assert!(peer.host.begin_prompt("closed".into()).await.is_err());
    }

    #[tokio::test]
    async fn event_lag_or_host_closed_terminates_before_releasing_lease() {
        for lag in [false, true] {
            let mut peer = Peer::new(2);
            let task = peer.start();
            let request = peer.command().await;
            peer.ack(&request);
            if lag {
                // Current-thread runtime: overflow before the worker can poll.
                for _ in 0..16 {
                    peer.send(json!({"type":"noise"}));
                }
            } else {
                peer.send(json!({"type":"host_closed"}));
            }
            assert!(result(task).await.is_err());
            assert_eq!(peer.host.snapshot().await.status, "closed");
            assert!(peer.host.try_run_lease().is_ok());
            assert!(peer.host.begin_prompt("closed".into()).await.is_err());
        }
    }

    #[tokio::test]
    async fn caller_drop_before_ack_still_drains_correlated_result() {
        let mut peer = Peer::new(64);
        let task = peer.start();
        let request = peer.command().await;
        task.abort();
        let _ = task.await;
        assert_eq!(peer.command().await["type"], "abort");
        assert!(peer.host.try_run_lease().is_err());
        peer.ack(&request);
        peer.started();
        peer.finished();
        peer.settled("aborted");
        peer.idle().await;
        assert_ne!(peer.host.snapshot().await.status, "closed");
        peer.host.terminate().await;
    }

    #[tokio::test]
    async fn timeout_can_settle_abort_without_poisoning_next_run() {
        let mut peer = Peer::new(64);
        let (task, _cancel) =
            peer.start_limited(Duration::from_millis(30), Duration::from_millis(200));
        let request = peer.command().await;
        peer.ack(&request);
        peer.started();
        assert_eq!(peer.command().await["type"], "abort");
        assert!(peer.host.try_run_lease().is_err());
        peer.finished();
        peer.settled("aborted");
        assert_eq!(
            result(task).await.unwrap_err(),
            "native compaction timed out"
        );
        assert_ne!(peer.host.snapshot().await.status, "closed");
        let prompt = peer
            .host
            .begin_prompt("after timeout".into())
            .await
            .unwrap();
        assert_eq!(peer.command().await["type"], "prompt");
        drop(prompt);
        peer.host.terminate().await;
    }

    #[tokio::test]
    async fn cancellation_without_settlement_closes_host() {
        let mut peer = Peer::new(64);
        let (task, cancel) = peer.start_limited(Duration::from_secs(1), Duration::from_millis(40));
        let request = peer.command().await;
        peer.ack(&request);
        peer.started();
        cancel.send_modify(|generation| *generation += 1);
        assert_eq!(peer.command().await["type"], "abort");
        assert!(result(task)
            .await
            .unwrap_err()
            .contains("settlement timed out"));
        assert_eq!(peer.host.snapshot().await.status, "closed");
        assert!(peer.host.try_run_lease().is_ok());
    }

    #[tokio::test]
    async fn lag_while_draining_closes_host_instead_of_returning_idle() {
        let mut peer = Peer::new(2);
        let (task, cancel) = peer.start_limited(Duration::from_secs(1), Duration::from_millis(200));
        let request = peer.command().await;
        peer.ack(&request);
        tokio::task::yield_now().await;
        peer.started();
        cancel.send_modify(|generation| *generation += 1);
        assert_eq!(peer.command().await["type"], "abort");
        for _ in 0..16 {
            peer.send(json!({"type":"noise"}));
        }
        assert!(result(task).await.unwrap_err().contains("stream lost"));
        assert_eq!(peer.host.snapshot().await.status, "closed");
        assert!(peer.host.try_run_lease().is_ok());
    }

    #[tokio::test]
    async fn malformed_ack_fails_closed_immediately() {
        let mut peer = Peer::new(64);
        let task = peer.start();
        let request = peer.command().await;
        peer.send(json!({"type":"response","id":request["id"],"success":true,"data":{}}));
        assert!(result(task)
            .await
            .unwrap_err()
            .contains("invalid acknowledgement"));
        assert_eq!(peer.host.snapshot().await.status, "closed");
        assert!(peer.host.try_run_lease().is_ok());
    }

    #[tokio::test]
    async fn prompt_lease_excludes_compaction() {
        let mut peer = Peer::new(64);
        let prompt = peer.host.begin_prompt("existing".into()).await.unwrap();
        assert_eq!(peer.command().await["type"], "prompt");
        assert!(peer
            .host
            .compact(None)
            .await
            .unwrap_err()
            .contains("already running"));
        drop(prompt);
        peer.host.terminate().await;
    }

    #[tokio::test]
    async fn shutdown_while_compacting_does_not_deadlock() {
        let mut peer = Peer::new(64);
        let task = peer.start();
        let request = peer.command().await;
        peer.ack(&request);
        peer.started();
        tokio::time::timeout(Duration::from_secs(1), peer.host.shutdown())
            .await
            .unwrap();
        assert!(result(task).await.is_err());
        assert_eq!(peer.host.snapshot().await.status, "closed");
        assert!(peer.host.try_run_lease().is_ok());
    }
}
