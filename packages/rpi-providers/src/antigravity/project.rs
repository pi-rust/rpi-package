//! Managed project discovery, onboarding and the original per-refresh cache.
use super::{
    auth::{self, format_refresh, json_body, Endpoints, Port, RefreshParts, Request},
    json::{describe, finite_number, string, trim, truthy},
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

pub const DEFAULT_PROJECT: &str = "rising-fact-p41fc";
const CACHE_TTL_MS: f64 = 30.0 * 60.0 * 1000.0;

pub trait ProjectPort: Port + Send + 'static {
    fn wait(&self, milliseconds: u64) -> impl Future<Output = ()> + Send;
    fn endpoints(&self) -> &Endpoints;
}
fn bootstrap(port: &impl Port, access: &str, url: String, body: String) -> Request {
    Request {
        url,
        method: "POST".into(),
        headers: auth::bootstrap_headers(access, port.user_agent()),
        body: Some(body),
        timeout_ms: 180_000,
    }
}
pub async fn load_managed(port: &impl ProjectPort, access: &str) -> Option<Value> {
    for endpoint in &port.endpoints().load {
        let attempt = async {
            let response = port
                .send(bootstrap(
                    port,
                    access,
                    format!("{endpoint}/v1internal:loadCodeAssist"),
                    "{\"metadata\":{\"ideType\":\"ANTIGRAVITY\"}}".into(),
                ))
                .await?;
            if !response.ok() {
                return Ok(None);
            }
            json_body(response.body).await.map(Some)
        }
        .await;
        if let Ok(Some(payload)) = attempt {
            return Some(payload);
        }
    }
    None
}
pub async fn onboard(
    port: &impl ProjectPort,
    access: &str,
    tier: &Value,
    project: Option<&str>,
    attempts: u32,
    delay_ms: u64,
) -> Option<Value> {
    let body = describe(&json!({"tierId":tier}));
    // Onboarding tries the production endpoint first, as Pi does.
    let endpoints: Vec<_> = port.endpoints().load.iter().rev().cloned().collect();
    for endpoint in endpoints {
        for _ in 0..attempts {
            let attempt = async {
                let response = port
                    .send(bootstrap(
                        port,
                        access,
                        format!("{endpoint}/v1internal:onboardUser"),
                        body.clone(),
                    ))
                    .await?;
                if !response.ok() {
                    return Ok(None);
                }
                json_body(response.body).await.map(Some)
            }
            .await;
            let Ok(Some(payload)) = attempt else {
                break;
            };
            let managed = &payload["response"]["cloudaicompanionProject"]["id"];
            if truthy(&payload["done"]) {
                if truthy(managed) {
                    return Some(managed.clone());
                }
                if let Some(project) = project.filter(|project| !project.is_empty()) {
                    return Some(project.into());
                }
            }
            port.wait(delay_ms).await;
        }
    }
    None
}
fn cache_key(refresh: Option<&Value>) -> Result<Option<String>, String> {
    let Some(refresh) = refresh.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let packed = trim(refresh.as_str().ok_or("refresh?.trim is not a function")?);
    if packed.is_empty() {
        return Ok(None);
    }
    let parts = auth::parse_refresh(packed);
    let key = trim(&parts.refresh_token);
    Ok(Some(if key.is_empty() { packed } else { key }.into()))
}
fn result(auth: Value, project: Value, tier: Option<Value>) -> Value {
    let mut result = json!({"auth":auth,"effectiveProjectId":project});
    if let Some(tier) = tier {
        result["capturedTier"] = tier;
    }
    result
}
type Pending = watch::Receiver<Option<Result<Value, String>>>;
#[derive(Default)]
struct State {
    cached: HashMap<String, (Value, f64)>,
    pending: HashMap<String, Pending>,
    provision_failed: HashSet<String>,
}
#[derive(Default)]
pub struct ProjectStore {
    state: Mutex<State>,
}
impl ProjectStore {
    /// Resolves the effective project. Concurrent callers for one refresh share one lookup.
    pub async fn ensure<P: ProjectPort>(
        self: &Arc<Self>,
        port: Arc<P>,
        auth: Value,
    ) -> Result<Value, String> {
        if !truthy(&auth["access"]) {
            return Ok(result(auth, "".into(), None));
        }
        let key = cache_key(auth.get("refresh"))?;
        let mut receiver = {
            let mut state = self.state.lock().unwrap();
            if let Some(key) = &key {
                if let Some((value, at)) = state.cached.get(key) {
                    if port.now_ms() - at < CACHE_TTL_MS {
                        return Ok(value.clone());
                    }
                    state.cached.remove(key);
                }
            }
            if let Some(receiver) = key.as_ref().and_then(|key| state.pending.get(key)) {
                receiver.clone()
            } else {
                let (sender, receiver) = watch::channel(None);
                if let Some(key) = &key {
                    state.pending.insert(key.clone(), receiver.clone());
                }
                let store = self.clone();
                // JavaScript promises continue resolving when an individual caller stops waiting.
                tokio::spawn(async move {
                    let resolved = store.resolve(port.as_ref(), auth, key.as_deref()).await;
                    if let Some(key) = key {
                        let mut state = store.state.lock().unwrap();
                        state.pending.remove(&key);
                        if let Ok(value) = &resolved {
                            let next_key = cache_key(value["auth"].get("refresh"))
                                .ok()
                                .flatten()
                                .unwrap_or_else(|| key.clone());
                            state
                                .cached
                                .insert(next_key.clone(), (value.clone(), port.now_ms()));
                            if next_key != key {
                                state.cached.remove(&key);
                            }
                        }
                    }
                    let _ = sender.send(Some(resolved));
                });
                receiver
            }
        };
        loop {
            if let Some(value) = receiver.borrow().clone() {
                return value;
            }
            receiver
                .changed()
                .await
                .map_err(|_| "Antigravity project resolution stopped".to_owned())?;
        }
    }

    async fn resolve(
        &self,
        port: &impl ProjectPort,
        auth: Value,
        key: Option<&str>,
    ) -> Result<Value, String> {
        let parts = auth::parse_refresh(
            auth.get("refresh")
                .filter(|value| !value.is_null())
                .map(|value| value.as_str().ok_or("refresh.split is not a function"))
                .transpose()?
                .unwrap_or_default(),
        );
        if let Some(managed) = parts.managed_project_id {
            return Ok(result(auth, managed.into(), None));
        }
        if key.is_some_and(|key| self.state.lock().unwrap().provision_failed.contains(key)) {
            return Ok(result(
                auth,
                parts
                    .project_id
                    .unwrap_or_else(|| DEFAULT_PROJECT.into())
                    .into(),
                None,
            ));
        }
        let access = string(&auth["access"]);
        let payload = load_managed(port, &access).await.unwrap_or(Value::Null);
        let current = &payload["currentTier"]["id"];
        let paid = if payload["paidTier"].is_string() {
            &payload["paidTier"]
        } else {
            &payload["paidTier"]["id"]
        };
        let tier = truthy(current).then(|| {
            let mut tier = json!({"id":current});
            if truthy(paid) {
                tier["paidId"] = paid.clone();
            }
            tier["capturedAt"] = finite_number(port.now_ms());
            tier
        });
        let managed = payload["cloudaicompanionProject"]
            .as_str()
            .or_else(|| payload["cloudaicompanionProject"]["id"].as_str())
            .filter(|value| !value.is_empty())
            .map(Value::from);
        let managed = if managed.is_some() {
            managed
        } else {
            let allowed = &payload["allowedTiers"];
            let tier = if allowed.is_null() || !truthy(allowed) {
                Value::Null
            } else {
                let tiers = allowed.as_array().ok_or("allowedTiers is not iterable")?;
                tiers
                    .iter()
                    .find(|tier| truthy(&tier["isDefault"]))
                    .or_else(|| tiers.first())
                    .map(|tier| tier["id"].clone())
                    .unwrap_or(Value::Null)
            };
            let tier = if tier.is_null() {
                Value::from("free-tier")
            } else {
                tier
            };
            onboard(port, &access, &tier, parts.project_id.as_deref(), 10, 5000).await
        };
        if let Some(managed) = managed {
            let mut updated = auth;
            updated["refresh"] = format_refresh(&RefreshParts {
                refresh_token: parts.refresh_token,
                project_id: parts.project_id,
                managed_project_id: Some(string(&managed)),
            })
            .into();
            return Ok(result(updated, managed, tier));
        }
        if let Some(key) = key {
            self.state
                .lock()
                .unwrap()
                .provision_failed
                .insert(key.into());
        }
        Ok(result(
            auth,
            parts
                .project_id
                .unwrap_or_else(|| DEFAULT_PROJECT.into())
                .into(),
            tier,
        ))
    }
}
