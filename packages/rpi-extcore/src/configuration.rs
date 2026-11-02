//! Bounded, lease-free observation of native runtime configuration.
//!
//! Reads bracket the catalog and optional thinking choices with state reads.
//! This detects ordinary concurrent changes, not ABA or an atomic native
//! transaction. Polling coalesces to the latest value; it is not an event audit.
//! Failed reads retain the last valid snapshot. No configuration is persisted.
use crate::host::HostSession;
use serde_json::{json, Value};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::{watch, Mutex};

const BUDGET: Duration = Duration::from_secs(10);
const PERIOD: Duration = Duration::from_secs(2);
const MAX_MODELS: usize = 1024;
const MAX_LEVELS: usize = 32;
const MAX_STRING: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelIdentity {
    pub id: String,
    pub provider: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelOption {
    pub identity: ModelIdentity,
    pub name: String,
    pub context_window: Option<u64>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThinkingConfiguration {
    pub current: String,
    pub levels: Vec<String>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeConfiguration {
    pub model: ModelIdentity,
    pub models: Vec<ModelOption>,
    pub thinking: Option<ThinkingConfiguration>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigurationSnapshot {
    pub revision: u64,
    pub configuration: RuntimeConfiguration,
}

struct Shared {
    host: Weak<HostSession>,
    gate: Mutex<()>,
    latest: watch::Sender<ConfigurationSnapshot>,
    stop: watch::Sender<bool>,
}

/// One observer per native host. Retain this owner while using its watch feed.
/// `shutdown` cancels and joins; cancellation of shutdown leaves the join handle
/// available to a retry. Drop cancels and aborts as a non-async fallback.
pub struct ConfigurationRuntime {
    shared: Arc<Shared>,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ConfigurationRuntime {
    pub async fn try_attach(host: Arc<HostSession>) -> Result<Option<Arc<Self>>, String> {
        if !host.supports_command("get_state") || !host.supports_command("get_available_models") {
            return Ok(None);
        }
        // Held across initialization: concurrent adapters cannot create two pollers.
        let mut owner = host.configuration_owner.lock().await;
        if let Some(runtime) = owner.upgrade() {
            return Ok(Some(runtime));
        }
        let mut closed = host.closed_receiver();
        let configuration = tokio::select! {
            biased;
            _ = closed.wait_for(|v| *v) => return Err("configuration host closed".into()),
            result = tokio::time::timeout(BUDGET, read_configuration(&host)) => {
                result.map_err(|_| "configuration refresh timed out".to_string())??
            }
        };
        let shared = Arc::new(Shared {
            host: Arc::downgrade(&host),
            gate: Mutex::new(()),
            latest: watch::channel(ConfigurationSnapshot {
                revision: 1,
                configuration,
            })
            .0,
            stop: watch::channel(false).0,
        });
        let runtime = Arc::new(Self {
            shared: shared.clone(),
            worker: Mutex::new(None),
        });
        let first_poll = tokio::time::Instant::now() + PERIOD;
        let worker = tokio::spawn(async move {
            let mut stop = shared.stop.subscribe();
            let mut interval = tokio::time::interval_at(first_poll, PERIOD);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = async { let _ = stop.wait_for(|v| *v).await; } => break,
                    _ = async { let _ = closed.wait_for(|v| *v).await; } => break,
                    _ = interval.tick() => {
                        // No runtime/session strong owner is retained while idle.
                        let _ = shared.perform(Mutation::None).await;
                    }
                }
            }
        });
        *runtime.worker.lock().await = Some(worker);
        *owner = Arc::downgrade(&runtime);
        Ok(Some(runtime))
    }

    pub fn subscribe(&self) -> watch::Receiver<ConfigurationSnapshot> {
        self.shared.latest.subscribe()
    }

    pub fn snapshot(&self) -> ConfigurationSnapshot {
        self.shared.latest.borrow().clone()
    }

    pub async fn refresh(&self) -> Result<ConfigurationSnapshot, String> {
        self.shared.perform(Mutation::None).await
    }

    pub async fn set_model(
        &self,
        id: &str,
        provider: Option<&str>,
    ) -> Result<ConfigurationSnapshot, String> {
        checked_string(id)?;
        if let Some(provider) = provider {
            checked_string(provider)?;
        }
        self.shared.perform(Mutation::Model(id, provider)).await
    }

    pub async fn set_thinking_level(&self, level: &str) -> Result<ConfigurationSnapshot, String> {
        checked_string(level)?;
        self.shared.perform(Mutation::Thinking(level)).await
    }

    pub async fn shutdown(&self) {
        self.shared.stop.send_replace(true);
        let mut worker = self.worker.lock().await;
        if let Some(task) = worker.as_mut() {
            let _ = task.await;
        }
        worker.take();
    }
}

impl Drop for ConfigurationRuntime {
    fn drop(&mut self) {
        self.shared.stop.send_replace(true);
        if let Ok(worker) = self.worker.try_lock() {
            if let Some(task) = worker.as_ref() {
                task.abort();
            }
        }
    }
}

enum Mutation<'a> {
    None,
    Model(&'a str, Option<&'a str>),
    Thinking(&'a str),
}

impl Shared {
    async fn perform(&self, mutation: Mutation<'_>) -> Result<ConfigurationSnapshot, String> {
        let host = self.host.upgrade().ok_or("configuration host closed")?;
        let mut closed = host.closed_receiver();
        let mut stop = self.stop.subscribe();
        tokio::select! {
            biased;
            _ = stop.wait_for(|v| *v) => Err("configuration runtime stopped".into()),
            _ = closed.wait_for(|v| *v) => Err("configuration host closed".into()),
            result = tokio::time::timeout(BUDGET, async {
                let _gate = self.gate.lock().await;
                match mutation {
                    Mutation::None => {},
                    Mutation::Model(id, provider) => {
                        if !host.supports_command("set_model") {
                            return Err("native host does not support set_model".into());
                        }
                        let mut params = json!({"modelId": id, "model": id});
                        if let Some(provider) = provider {
                            params["provider"] = json!(provider);
                        }
                        host.command("set_model", params, BUDGET).await?;
                    },
                    Mutation::Thinking(level) => {
                        if !host.supports_command("set_thinking_level") {
                            return Err("native host does not support set_thinking_level".into());
                        }
                        host.command("set_thinking_level", json!({"level": level}), BUDGET).await?;
                    },
                }
                // Mutation and refresh share the gate. Failure is not rollback:
                // a native mutation may have committed before cancellation/error.
                let configuration = read_configuration(&host).await?;
                let current = self.latest.borrow().clone();
                if current.configuration == configuration {
                    return Ok(current);
                }
                let snapshot = ConfigurationSnapshot {
                    revision: current.revision.checked_add(1).ok_or("configuration revision exhausted")?,
                    configuration,
                };
                self.latest.send_replace(snapshot.clone());
                Ok(snapshot)
            }) => result.map_err(|_| "configuration refresh timed out".to_string())?,
        }
    }
}

#[derive(Eq, PartialEq)]
struct State {
    model: ModelIdentity,
    thinking: Option<String>,
}

async fn read_configuration(host: &HostSession) -> Result<RuntimeConfiguration, String> {
    let thinking = host.supports_command("get_available_thinking_levels");
    for _ in 0..2 {
        let before = parse_state(
            &host.command("get_state", json!({}), BUDGET).await?,
            thinking,
        )?;
        let models = parse_models(
            &host
                .command("get_available_models", json!({}), BUDGET)
                .await?,
        )?;
        let levels = if thinking {
            Some(parse_levels(
                &host
                    .command("get_available_thinking_levels", json!({}), BUDGET)
                    .await?,
            )?)
        } else {
            None
        };
        let after = parse_state(
            &host.command("get_state", json!({}), BUDGET).await?,
            thinking,
        )?;
        if before != after {
            continue;
        }
        let model = resolve_model(&after.model, &models)?;
        return Ok(RuntimeConfiguration {
            model,
            models,
            // Current is independent of choices. Native state can retain medium
            // on a model whose selectable levels only include off (or none).
            thinking: levels.map(|levels| ThinkingConfiguration {
                current: after.thinking.expect("validated thinking state"),
                levels,
            }),
        });
    }
    Err("configuration changed during both read attempts".into())
}

fn checked_string(value: &str) -> Result<String, String> {
    if value.is_empty() || value.len() > MAX_STRING || value.chars().any(char::is_control) {
        return Err("invalid configuration string".into());
    }
    Ok(value.to_owned())
}
fn string(value: &Value) -> Result<String, String> {
    checked_string(value.as_str().ok_or("configuration string required")?)
}
fn provider(value: Option<&Value>) -> Result<Option<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(value) => string(value).map(Some),
    }
}
fn parse_state(value: &Value, thinking: bool) -> Result<State, String> {
    let object = value
        .as_object()
        .ok_or("configuration state must be an object")?;
    let model = object
        .get("modelId")
        .filter(|v| !v.is_null())
        .or_else(|| object.get("model").filter(|v| v.is_string()))
        .or_else(|| value.pointer("/model/id"))
        .ok_or("configuration state has no model identity")?;
    let id = string(model)?;
    let provider = provider(
        object
            .get("provider")
            .filter(|v| !v.is_null())
            .or_else(|| value.pointer("/model/provider")),
    )?;
    let thinking = if thinking {
        Some(string(
            value
                .get("thinkingLevel")
                .ok_or("configuration state has no thinking level")?,
        )?)
    } else {
        None
    };
    Ok(State {
        model: ModelIdentity { id, provider },
        thinking,
    })
}
fn parse_models(value: &Value) -> Result<Vec<ModelOption>, String> {
    let array = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or("configuration models must be an array")?;
    if array.len() > MAX_MODELS {
        return Err("configuration model limit exceeded".into());
    }
    let mut models: Vec<ModelOption> = Vec::with_capacity(array.len());
    for value in array {
        let identity = ModelIdentity {
            id: string(&value["id"])?,
            provider: provider(value.get("provider"))?,
        };
        if models.iter().any(|model| model.identity == identity) {
            return Err("duplicate configuration model identity".into());
        }
        let name = match value.get("name") {
            None => identity.id.clone(),
            Some(value) => string(value)?,
        };
        let context_window = match value.get("contextWindow") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .ok_or("invalid configuration context window")?,
            ),
        };
        models.push(ModelOption {
            identity,
            name,
            context_window,
        });
    }
    Ok(models)
}
fn parse_levels(value: &Value) -> Result<Vec<String>, String> {
    let array = value
        .get("levels")
        .and_then(Value::as_array)
        .ok_or("configuration levels must be an array")?;
    if array.len() > MAX_LEVELS {
        return Err("configuration thinking limit exceeded".into());
    }
    let mut levels = Vec::with_capacity(array.len());
    for value in array {
        let level = string(value)?;
        if levels.contains(&level) {
            return Err("duplicate configuration thinking level".into());
        }
        levels.push(level);
    }
    Ok(levels)
}
fn resolve_model(current: &ModelIdentity, models: &[ModelOption]) -> Result<ModelIdentity, String> {
    // Prefer exact IDs (which may themselves contain '/') over legacy qualified
    // identities. Missing providers are resolved only when the catalog is unique.
    let matches: Vec<_> = models
        .iter()
        .filter(|model| {
            model.identity.id == current.id
                && current
                    .provider
                    .as_ref()
                    .map_or(true, |p| model.identity.provider.as_ref() == Some(p))
        })
        .collect();
    if matches.len() == 1 {
        return Ok(matches[0].identity.clone());
    }
    if matches.is_empty() {
        if let Some((provider, id)) = current.id.split_once('/') {
            let qualified = ModelIdentity {
                id: id.to_owned(),
                provider: Some(provider.to_owned()),
            };
            if current.provider.as_deref().map_or(true, |p| p == provider)
                && models.iter().any(|model| model.identity == qualified)
            {
                return Ok(qualified);
            }
        }
    }
    Err("configuration current model is absent or ambiguous in catalog".into())
}

#[cfg(test)]
#[path = "configuration_tests.rs"]
mod tests;
