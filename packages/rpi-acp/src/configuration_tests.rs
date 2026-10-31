use super::super::config_options;
use super::*;
use rpi_extcore::configuration::{
    ModelIdentity, ModelOption, RuntimeConfiguration, ThinkingConfiguration,
};

fn snapshot(revision: u64, model: &str, thinking: &str) -> ConfigurationSnapshot {
    let identity = |id: &str| ModelIdentity {
        id: id.into(),
        provider: Some("p".into()),
    };
    ConfigurationSnapshot {
        revision,
        configuration: RuntimeConfiguration {
            model: identity(model),
            models: ["a", "b"]
                .into_iter()
                .map(|id| ModelOption {
                    identity: identity(id),
                    name: id.into(),
                    context_window: Some(100),
                })
                .collect(),
            thinking: Some(ThinkingConfiguration {
                current: thinking.into(),
                levels: vec!["off".into(), "high".into()],
            }),
        },
    }
}

#[test]
fn typed_projection_reuses_full_list_presentation() {
    let result = present(&snapshot(7, "b", "high"));
    let options = result["configOptions"].as_array().unwrap();
    assert_eq!(options.len(), 2);
    assert_eq!(options[0]["currentValue"], "p/b");
    assert_eq!(options[1]["currentValue"], "high");
    assert_eq!(options[0]["options"].as_array().unwrap().len(), 2);
    assert_eq!(options[0]["options"][0]["description"], "100 tokens");
}

#[tokio::test]
async fn independent_change_emits_full_list_without_a_prompt() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let (out, peer) = tokio::io::duplex(8192);
    let writer = Arc::new(Writer::with_output(out));
    let initial = snapshot(1, "a", "off");
    let (tx, rx) = watch::channel(initial.clone());
    let mut projection = Projection::default();
    projection.record(&initial, present(&initial));
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(forward(
        rx,
        Arc::new(Mutex::new(projection)),
        writer.clone(),
        "s".into(),
        stopped,
    ));
    tx.send_replace(snapshot(2, "b", "high"));
    let mut lines = BufReader::new(peer).lines();
    let line = tokio::time::timeout(std::time::Duration::from_secs(1), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        frame["params"]["update"]["sessionUpdate"],
        "config_option_update"
    );
    assert_eq!(
        frame["params"]["update"]["configOptions"],
        present(&snapshot(2, "b", "high"))["configOptions"]
    );
    stop.send_replace(true);
    task.await.unwrap();
}

#[tokio::test]
async fn queued_watch_cannot_overwrite_setter_delivered_revision() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let (out, peer) = tokio::io::duplex(8192);
    let writer = Arc::new(Writer::with_output(out));
    let (tx, rx) = watch::channel(snapshot(1, "a", "off"));
    let gate = Arc::new(Mutex::new(Projection::default()));
    let mut setter = gate.lock().await;
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(forward(
        rx,
        gate.clone(),
        writer.clone(),
        "s".into(),
        stopped,
    ));
    tx.send_replace(snapshot(2, "a", "high"));
    // Setter owns the gate across its response and records its newer revision.
    let applied = snapshot(3, "b", "off");
    writer.respond(&json!(10), present(&applied)).await;
    setter.record(&applied, present(&applied));
    drop(setter);
    let latest = snapshot(4, "b", "high");
    tx.send_replace(latest.clone());
    let mut lines = BufReader::new(peer).lines();
    let first: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(first["id"], 10);
    let second: Value = serde_json::from_str(
        &tokio::time::timeout(std::time::Duration::from_secs(1), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        second["params"]["update"]["configOptions"],
        present(&latest)["configOptions"]
    );
    stop.send_replace(true);
    task.await.unwrap();
}

#[tokio::test]
async fn terminal_close_joins_forwarder_after_observed_partial_write_pending() {
    let (output, _peer) = tokio::io::duplex(8);
    let pending = Arc::new(tokio::sync::Notify::new());
    let writer = Arc::new(Writer::with_output(
        super::super::protocol_configuration_tests::ObservedPending {
            output,
            pending: pending.clone(),
        },
    ));
    let (tx, rx) = watch::channel(snapshot(1, "a", "off"));
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(forward(
        rx,
        Arc::new(Mutex::new(Projection::default())),
        writer.clone(),
        "s".into(),
        stopped,
    ));
    tx.send_replace(snapshot(2, "b", "high"));
    tokio::time::timeout(std::time::Duration::from_secs(1), pending.notified())
        .await
        .unwrap();
    writer.close();
    tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    drop(stop);
}

#[tokio::test]
async fn session_stop_drains_partial_frame_and_keeps_transport_valid() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let (output, peer) = tokio::io::duplex(8);
    let pending = Arc::new(tokio::sync::Notify::new());
    let writer = Arc::new(Writer::with_output(
        super::super::protocol_configuration_tests::ObservedPending {
            output,
            pending: pending.clone(),
        },
    ));
    let (tx, rx) = watch::channel(snapshot(1, "a", "off"));
    let (stop, stopped) = watch::channel(false);
    let gate = Arc::new(Mutex::new(Projection::default()));
    let task = tokio::spawn(forward(
        rx,
        gate.clone(),
        writer.clone(),
        "s".into(),
        stopped,
    ));
    let watcher = Forwarder { gate, stop, task };
    tx.send_replace(snapshot(2, "b", "high"));
    tokio::time::timeout(std::time::Duration::from_secs(1), pending.notified())
        .await
        .unwrap();
    let drain = async {
        let mut lines = BufReader::new(peer).lines();
        let frame: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(
            frame["params"]["update"]["sessionUpdate"],
            "config_option_update"
        );
        let response: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(response["id"], 12);
    };
    let cleanup = async {
        watcher.stop(&writer).await;
        assert!(!writer.closed.load(std::sync::atomic::Ordering::Acquire));
        writer.respond(&json!(12), json!({})).await;
    };
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        tokio::join!(drain, cleanup);
    })
    .await
    .unwrap();
}

#[test]
fn typed_configuration_omits_unrepresentable_thinking_without_changing_legacy_fallback() {
    let result = present(&snapshot(8, "a", "medium"));
    let options = result["configOptions"].as_array().unwrap();
    assert_eq!(options.len(), 1);
    assert_eq!(options[0]["id"], "model");
    let legacy = config_options(
        &json!([]),
        &json!(["off", "high"]),
        &json!({"thinkingLevel":"medium"}),
    );
    assert_eq!(legacy["configOptions"][0]["currentValue"], "off");
}

#[test]
fn typed_model_identity_with_slash_never_uses_legacy_resolution() {
    for provider in [None, Some("p".to_owned())] {
        let mut state = snapshot(1, "org/a", "off");
        state.configuration.model.provider = provider.clone();
        state.configuration.models[1].identity = state.configuration.model.clone();
        let result = present(&state);
        let expected = if provider.is_some() {
            "p/org/a"
        } else {
            "org/a"
        };
        assert_eq!(result["configOptions"][0]["currentValue"], expected);
        let before = result;
        state.configuration.model.id = "org/b".into();
        state.configuration.models[1].identity = state.configuration.model.clone();
        let after = present(&state);
        assert_ne!(
            before, after,
            "real changes must not collapse to the first catalog entry"
        );
        assert_eq!(
            after["configOptions"][0]["currentValue"],
            if provider.is_some() {
                "p/org/b"
            } else {
                "org/b"
            }
        );
    }
}

#[tokio::test]
async fn lifecycle_lag_keeps_forwarding_until_explicit_host_closure() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let (output, peer) = tokio::io::duplex(8192);
    let writer = Arc::new(Writer::with_output(output));
    let initial = snapshot(1, "a", "off");
    let (tx, rx) = watch::channel(initial.clone());
    let mut projection = Projection::default();
    projection.record(&initial, present(&initial));
    let gate = Arc::new(Mutex::new(projection));
    let (events, lifecycle) = tokio::sync::broadcast::channel(2);
    // Overflow before starting the receiver: Lagged is deterministic, not a race.
    for _ in 0..16 {
        events.send(json!({"type":"agent_end"})).unwrap();
    }
    let watcher = Forwarder::start(rx, gate, writer.clone(), "s".into(), lifecycle);
    tokio::task::yield_now().await;
    assert!(!watcher.task.is_finished(), "lag is not host closure");
    tx.send_replace(snapshot(2, "b", "high"));
    let mut lines = BufReader::new(peer).lines();
    let frame = tokio::time::timeout(std::time::Duration::from_secs(1), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(
        frame["params"]["update"]["configOptions"],
        present(&snapshot(2, "b", "high"))["configOptions"]
    );
    events.send(json!({"type":"host_closed"})).unwrap();
    let mut watcher = watcher;
    tokio::time::timeout(std::time::Duration::from_secs(1), &mut watcher.task)
        .await
        .unwrap()
        .unwrap();
    assert!(!writer.closed.load(std::sync::atomic::Ordering::Acquire));
}
