use super::*;
use crate::host::TestEvents;
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;
use tokio::io::{AsyncBufReadExt, BufReader};

fn ready(thinking: bool) -> Value {
    let mut commands = vec![
        "get_state",
        "get_available_models",
        "set_model",
        "set_thinking_level",
    ];
    if thinking {
        commands.push("get_available_thinking_levels");
    }
    json!({"type":"ready", "protocol":"rpi", "supportedCommands":commands})
}
fn state(id: &str) -> Value {
    json!({"modelId":id,"provider":"p","thinkingLevel":"medium"})
}
fn models() -> Value {
    json!({"models":[
        {"id":"a","provider":"p","name":"A","contextWindow":123},
        {"id":"b","provider":"p","name":"B","contextWindow":456}
    ]})
}
struct PeerState {
    state: Value,
    models: Value,
    levels: Value,
    states: VecDeque<Value>,
    commands: Vec<String>,
    stall: bool,
    reject_mutation: bool,
}
struct Fixture {
    host: Arc<HostSession>,
    peer: Arc<StdMutex<PeerState>>,
    worker: tokio::task::JoinHandle<()>,
}
impl Fixture {
    fn new(thinking: bool) -> Self {
        let (writer, reader) = tokio::io::duplex(65536);
        let (host, events) = HostSession::test_connection_with_ready(writer, 32, ready(thinking));
        let peer = Arc::new(StdMutex::new(PeerState {
            state: state("a"),
            models: models(),
            levels: json!({"levels":["off"]}),
            states: VecDeque::new(),
            commands: Vec::new(),
            stall: false,
            reject_mutation: false,
        }));
        let shared = peer.clone();
        let worker = tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let request: Value = serde_json::from_str(&line).unwrap();
                let response = {
                    let mut peer = shared.lock().unwrap();
                    let command = request["type"].as_str().unwrap();
                    peer.commands.push(command.to_string());
                    if peer.stall {
                        continue;
                    }
                    if peer.reject_mutation && command.starts_with("set_") {
                        json!({"type":"response", "id":request["id"], "success":false,"error":"fixture rejection"})
                    } else {
                        let data = match command {
                            "get_state" => peer
                                .states
                                .pop_front()
                                .unwrap_or_else(|| peer.state.clone()),
                            "get_available_models" => peer.models.clone(),
                            "get_available_thinking_levels" => peer.levels.clone(),
                            "set_model" => {
                                peer.state["modelId"] = request["modelId"].clone();
                                if !request["provider"].is_null() {
                                    peer.state["provider"] = request["provider"].clone();
                                }
                                json!({})
                            }
                            "set_thinking_level" => {
                                peer.state["thinkingLevel"] = request["level"].clone();
                                json!({})
                            }
                            _ => panic!("unexpected fixture command: {command}"),
                        };
                        json!({"type":"response", "id":request["id"], "success":true, "data":data})
                    }
                };
                reply(&events, response);
            }
        });
        Self { host, peer, worker }
    }
    async fn attach(&self) -> Arc<ConfigurationRuntime> {
        ConfigurationRuntime::try_attach(self.host.clone())
            .await
            .unwrap()
            .unwrap()
    }
    fn count(&self) -> usize {
        self.peer.lock().unwrap().commands.len()
    }
}
fn reply(events: &TestEvents, response: Value) {
    events.send(response).unwrap();
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

#[tokio::test]
async fn typed_snapshot_accepts_native_current_outside_choices_and_deduplicates() {
    let fixture = Fixture::new(true);
    let runtime = fixture.attach().await;
    let initial = runtime.snapshot();
    assert_eq!(initial.revision, 1);
    assert_eq!(initial.configuration.model.id, "a");
    assert_eq!(initial.configuration.models[0].context_window, Some(123));
    assert_eq!(
        initial.configuration.thinking,
        Some(ThinkingConfiguration {
            current: "medium".into(),
            levels: vec!["off".into()],
        })
    );
    let mut receiver = runtime.subscribe();
    assert_eq!(runtime.refresh().await.unwrap(), initial);
    assert!(!receiver.has_changed().unwrap());
    fixture.peer.lock().unwrap().state = state("b");
    let updated = runtime.refresh().await.unwrap();
    receiver.changed().await.unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(*receiver.borrow_and_update(), updated);
    runtime.shutdown().await;
}

#[tokio::test]
async fn concurrent_attach_has_one_owner_and_one_initial_read() {
    let fixture = Fixture::new(true);
    let (a, b) = tokio::join!(
        ConfigurationRuntime::try_attach(fixture.host.clone()),
        ConfigurationRuntime::try_attach(fixture.host.clone()),
    );
    let a = a.unwrap().unwrap();
    let b = b.unwrap().unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(fixture.count(), 4);
    a.shutdown().await;
}

#[tokio::test]
async fn consistency_retries_once_then_errors_without_publishing() {
    let fixture = Fixture::new(true);
    let runtime = fixture.attach().await;
    fixture.peer.lock().unwrap().states = [state("a"), state("b"), state("b"), state("b")].into();
    assert_eq!(runtime.refresh().await.unwrap().configuration.model.id, "b");
    assert_eq!(fixture.count(), 12);
    let valid = runtime.snapshot();
    fixture.peer.lock().unwrap().states = [state("a"), state("b"), state("a"), state("b")].into();
    assert!(runtime
        .refresh()
        .await
        .unwrap_err()
        .contains("both read attempts"));
    assert_eq!(runtime.snapshot(), valid);
    assert_eq!(fixture.count(), 20);
    runtime.shutdown().await;
}

#[tokio::test]
async fn capabilities_optional_thinking_and_advertised_bad_replies() {
    let (writer, _reader) = tokio::io::duplex(4096);
    let (old, _events) = HostSession::test_connection(writer, 16);
    assert!(ConfigurationRuntime::try_attach(old.clone())
        .await
        .unwrap()
        .is_none());
    old.terminate().await;
    let fixture = Fixture::new(false);
    let runtime = fixture.attach().await;
    assert!(runtime.snapshot().configuration.thinking.is_none());
    assert_eq!(fixture.count(), 3);
    fixture.peer.lock().unwrap().models = json!({"models":null});
    assert!(runtime.refresh().await.is_err());
    assert_eq!(runtime.snapshot().revision, 1);
    runtime.shutdown().await;
    let bad = Fixture::new(true);
    bad.peer.lock().unwrap().state = json!({"model":null});
    assert!(ConfigurationRuntime::try_attach(bad.host.clone())
        .await
        .is_err());
}

#[tokio::test]
async fn setters_and_refresh_are_serialized_and_lease_free() {
    let fixture = Fixture::new(true);
    let runtime = fixture.attach().await;
    let lease = fixture.host.try_run_lease().unwrap();
    let (model, thinking) = tokio::join!(
        runtime.set_model("b", Some("p")),
        runtime.set_thinking_level("off")
    );
    assert!(model.is_ok());
    assert!(thinking.is_ok());
    assert_eq!(runtime.snapshot().configuration.model.id, "b");
    assert_eq!(
        runtime.snapshot().configuration.thinking.unwrap().current,
        "off"
    );
    let commands = fixture.peer.lock().unwrap().commands.clone();
    assert_eq!(
        &commands[4..9],
        &[
            "set_model",
            "get_state",
            "get_available_models",
            "get_available_thinking_levels",
            "get_state"
        ]
    );
    assert_eq!(
        &commands[9..14],
        &[
            "set_thinking_level",
            "get_state",
            "get_available_models",
            "get_available_thinking_levels",
            "get_state"
        ]
    );
    assert!(fixture.host.try_run_lease().is_err());
    drop(lease);
    assert!(fixture.host.try_run_lease().is_ok());
    runtime.shutdown().await;
}

#[tokio::test]
async fn setter_failure_and_post_mutation_read_failure_preserve_last_valid_snapshot() {
    let fixture = Fixture::new(true);
    let runtime = fixture.attach().await;
    let valid = runtime.snapshot();
    fixture.peer.lock().unwrap().reject_mutation = true;
    assert!(runtime.set_model("b", Some("p")).await.is_err());
    assert_eq!(runtime.snapshot(), valid);
    {
        let mut peer = fixture.peer.lock().unwrap();
        peer.reject_mutation = false;
        peer.models = json!({"models":false});
    }
    assert!(runtime.set_model("b", Some("p")).await.is_err());
    assert_eq!(fixture.peer.lock().unwrap().state["modelId"], "b");
    assert_eq!(runtime.snapshot(), valid); // No rollback claim or false empty state.
    fixture.peer.lock().unwrap().models = models();
    assert_eq!(runtime.refresh().await.unwrap().configuration.model.id, "b");
    runtime.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn polls_coalesce_latest_skip_missed_ticks_and_stop_on_shutdown() {
    let fixture = Fixture::new(true);
    let runtime = fixture.attach().await;
    let receiver = runtime.subscribe();
    fixture.peer.lock().unwrap().state = state("b");
    tokio::time::advance(PERIOD).await;
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    assert_eq!(runtime.snapshot().revision, 2);
    fixture.peer.lock().unwrap().state = state("a");
    tokio::time::advance(Duration::from_secs(20)).await;
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    assert_eq!(runtime.snapshot().revision, 3);
    assert_eq!(receiver.borrow().configuration.model.id, "a");
    assert_eq!(fixture.count(), 12); // No ten-tick catch-up burst.
    runtime.shutdown().await;
    let count = fixture.count();
    tokio::time::advance(Duration::from_secs(20)).await;
    tokio::task::yield_now().await;
    assert_eq!(fixture.count(), count);
    assert!(runtime.refresh().await.unwrap_err().contains("stopped"));
}

#[tokio::test(start_paused = true)]
async fn refresh_total_timeout_does_not_publish_and_shutdown_can_be_retried() {
    let fixture = Fixture::new(true);
    let runtime = fixture.attach().await;
    fixture.peer.lock().unwrap().stall = true;
    assert!(runtime.refresh().await.unwrap_err().contains("timed out"));
    assert_eq!(runtime.snapshot().revision, 1);
    let worker = runtime.worker.lock().await;
    let mut shutdown = Box::pin(runtime.shutdown());
    // Poll far enough to signal cancellation, then cancel while waiting for
    // the join lock. A retry must still own and drain the retained handle.
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(shutdown.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(shutdown);
    drop(worker);
    runtime.shutdown().await;
    assert!(runtime.worker.lock().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn host_close_and_owner_drop_stop_observation_without_owner_cycles() {
    let fixture = Fixture::new(true);
    let runtime = fixture.attach().await;
    let strong = Arc::strong_count(&fixture.host);
    tokio::task::yield_now().await;
    assert_eq!(Arc::strong_count(&fixture.host), strong);
    fixture.host.terminate().await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    assert!(runtime.worker.lock().await.as_ref().unwrap().is_finished());
    assert!(runtime.refresh().await.unwrap_err().contains("closed"));
    runtime.shutdown().await;
    let fixture = Fixture::new(true);
    let runtime = fixture.attach().await;
    let weak = Arc::downgrade(&runtime);
    drop(runtime);
    assert!(weak.upgrade().is_none());
    let count = fixture.count();
    tokio::time::advance(Duration::from_secs(20)).await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    assert_eq!(fixture.count(), count);
    // The weak host slot does not retain a disposed owner.
    let replacement = fixture.attach().await;
    replacement.shutdown().await;
}

#[test]
fn parsing_bounds_duplicates_and_legacy_identities() {
    let catalog = parse_models(&models()).unwrap();
    for value in [
        json!({"modelId":"a","provider":"p"}),
        json!({"model":"p/a"}),
        json!({"modelId":"p/a","provider":"p"}),
        json!({"model":{"id":"a","provider":"p"}}),
        json!({"model":"a"}),
    ] {
        let state = parse_state(&value, false).unwrap();
        assert_eq!(
            resolve_model(&state.model, &catalog).unwrap(),
            catalog[0].identity
        );
    }
    assert!(parse_state(&json!({}), false).is_err());
    assert!(parse_state(&json!({"modelId":"a"}), true).is_err());
    assert!(parse_models(&json!({"models":[{"id":"a"},{"id":"a"}]})).is_err());
    assert!(parse_models(&json!({"models":vec![json!({"id":"a"}); MAX_MODELS+1]})).is_err());
    assert!(parse_models(&json!({"models":[{"id":"a","contextWindow":-1}]})).is_err());
    assert!(parse_levels(&json!({"levels":["off","off"]})).is_err());
    assert!(parse_levels(&json!({"levels":vec!["off";MAX_LEVELS+1]})).is_err());
    assert!(checked_string(&"a".repeat(MAX_STRING + 1)).is_err());
    assert!(checked_string("\n").is_err());
    let ambiguous =
        parse_models(&json!({"models":[{"id":"a","provider":"p"},{"id":"a","provider":"q"}]}))
            .unwrap();
    assert!(resolve_model(
        &ModelIdentity {
            id: "a".into(),
            provider: None
        },
        &ambiguous
    )
    .is_err());
    assert!(resolve_model(
        &ModelIdentity {
            id: "missing".into(),
            provider: None
        },
        &catalog
    )
    .is_err());
}
