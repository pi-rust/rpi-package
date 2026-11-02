//! ACP-only projection and transport ownership for neutral configuration snapshots.
use super::{config_options_with_identity, Writer};
use rpi_extcore::configuration::{ConfigurationRuntime, ConfigurationSnapshot, ModelIdentity};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::{watch, Mutex};

fn model_value(identity: &ModelIdentity) -> String {
    match identity.provider.as_deref() {
        Some(provider) => format!("{provider}/{}", identity.id),
        None => identity.id.clone(),
    }
}

pub(super) fn present(snapshot: &ConfigurationSnapshot) -> Value {
    let config = &snapshot.configuration;
    // ACP's legacy wire spelling is not injective (None + org/b versus
    // org + b). Omit the entire model select rather than advertise duplicates.
    let mut values = std::collections::HashSet::new();
    let model_collision = config
        .models
        .iter()
        .any(|model| !values.insert(model_value(&model.identity)));
    let models = config
        .models
        .iter()
        .filter(|_| !model_collision)
        .map(|model| {
            json!({
                "id": model.identity.id, "provider": model.identity.provider,
                "name": model.name, "contextWindow": model.context_window,
            })
        })
        .collect::<Vec<_>>();
    let thinking = config
        .thinking
        .as_ref()
        // Typed live state must never select a fallback value that the native
        // host did not report. Keep the legacy builder's fallback unchanged.
        .filter(|t| t.levels.contains(&t.current))
        .map(|t| json!(t.levels))
        .unwrap_or(json!([]));
    config_options_with_identity(
        &json!(models),
        &thinking,
        config.model.provider.as_deref().unwrap_or(""),
        &config.model.id,
        config
            .thinking
            .as_ref()
            .map(|t| t.current.as_str())
            .unwrap_or(""),
        false,
    )
}

#[derive(Default)]
pub(super) struct Projection {
    revision: u64,
    options: Value,
}
impl Projection {
    fn record(&mut self, snapshot: &ConfigurationSnapshot, options: Value) {
        self.revision = snapshot.revision;
        self.options = options;
    }
}

pub(super) struct Forwarder {
    pub gate: Arc<Mutex<Projection>>,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}
impl Forwarder {
    /// Subscribe before initial response. The receiver retains intervening changes.
    pub fn prepare(
        runtime: &Arc<ConfigurationRuntime>,
    ) -> (
        watch::Receiver<ConfigurationSnapshot>,
        Arc<Mutex<Projection>>,
        Value,
    ) {
        let rx = runtime.subscribe();
        let snapshot = rx.borrow().clone();
        let options = present(&snapshot);
        let mut projection = Projection::default();
        projection.record(&snapshot, options.clone());
        (rx, Arc::new(Mutex::new(projection)), options)
    }

    /// Called only after the initial response is fully written.
    pub fn start(
        rx: watch::Receiver<ConfigurationSnapshot>,
        gate: Arc<Mutex<Projection>>,
        writer: Arc<Writer>,
        session_id: String,
        mut lifecycle: tokio::sync::broadcast::Receiver<Value>,
    ) -> Self {
        let (stop, stopped) = watch::channel(false);
        let task_gate = gate.clone();
        let host_stop = stop.clone();
        let task = tokio::spawn(async move {
            let forwarding = forward(rx, task_gate, writer, session_id, stopped);
            tokio::pin!(forwarding);
            // Observe native closure independently of output backpressure, but
            // only request a drain: never drop a live partial-frame future.
            let closed = async {
                loop {
                    match lifecycle.recv().await {
                        Ok(event) if event["type"] == "host_closed" => break,
                        Ok(_) => {}
                        // Broadcast eviction is not authoritative host closure.
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            };
            tokio::select! {
                _ = &mut forwarding => {},
                _ = closed => {
                    host_stop.send_replace(true);
                    forwarding.await;
                }
            }
        });
        Self { gate, stop, task }
    }

    pub async fn stop(mut self, writer: &Writer) {
        self.stop.send_replace(true);
        // Never cancel a partial frame on a live connection. A blocked write
        // gets a chance to drain; otherwise this becomes terminal for ALL sessions.
        if tokio::time::timeout(std::time::Duration::from_secs(2), &mut self.task)
            .await
            .is_err()
        {
            writer.close();
            self.task.abort();
            let _ = self.task.await;
        }
    }
}

async fn forward(
    mut rx: watch::Receiver<ConfigurationSnapshot>,
    gate: Arc<Mutex<Projection>>,
    writer: Arc<Writer>,
    session_id: String,
    mut stopped: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            biased;
            _ = writer.terminal() => break,
            _ = stopped.changed() => break,
            changed = rx.changed() => if changed.is_err() { break; },
        }
        let mut delivered = tokio::select! {
            biased;
            _ = writer.terminal() => break,
            _ = stopped.changed() => break,
            gate = gate.lock() => gate,
        };
        if *stopped.borrow() {
            break;
        }
        // Re-read AFTER acquiring the gate: an earlier changed() value must
        // never overwrite a just-delivered setter response.
        let snapshot = rx.borrow_and_update().clone();
        let options = present(&snapshot);
        if snapshot.revision <= delivered.revision {
            continue;
        }
        if options != delivered.options {
            let frame = json!({"jsonrpc":"2.0", "method":"session/update", "params":{
                "sessionId": session_id, "update":{"sessionUpdate":"config_option_update", "configOptions": options["configOptions"]}
            }});
            // Only Writer's connection-terminal policy may interrupt this write.
            if writer.write_configuration(&frame).await.is_err() {
                break;
            }
        }
        delivered.record(&snapshot, options);
    }
}

pub(super) async fn set(
    runtime: &ConfigurationRuntime,
    gate: &Arc<Mutex<Projection>>,
    writer: &Writer,
    id: &Value,
    config_id: &str,
    value: &Value,
) {
    let mut projection = gate.lock().await;
    let result = match (config_id, value.as_str()) {
        ("model", Some(value)) => {
            let snapshot = runtime.snapshot();
            let mut matches = snapshot
                .configuration
                .models
                .iter()
                .filter(|model| model_value(&model.identity) == value);
            match (matches.next(), matches.next()) {
                (Some(model), None) => {
                    let identity = &model.identity;
                    runtime
                        .set_model(&identity.id, identity.provider.as_deref())
                        .await
                }
                (None, _) => Err("unknown model selector".to_owned()),
                _ => Err("ambiguous model selector".to_owned()),
            }
        }
        ("thinking", Some(value)) => runtime.set_thinking_level(value).await,
        ("model" | "thinking", None) => Err("configuration value must be a string".to_owned()),
        _ => Err(format!("unknown config option: {config_id}")),
    };
    match result {
        Ok(snapshot) => {
            let options = present(&snapshot);
            let frame = json!({"jsonrpc":"2.0", "id":id, "result": options});
            if writer.write_configuration(&frame).await.is_ok() {
                projection.record(&snapshot, options);
            }
        }
        Err(error) => {
            writer
                .session_error(id, "session/set_config_option", &error)
                .await
        }
    }
}

#[cfg(test)]
#[path = "configuration_tests.rs"]
mod tests;
