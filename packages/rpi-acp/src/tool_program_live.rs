//! Optional ACP live child-observation subscription.
//!
//! This is deliberately a vendor extension rather than a standard
//! `session/update`: ACP has no child-history subscription type. The native
//! host and extcore own event identity, branch proof and resync semantics;
//! this module only owns the connection-scoped task and wire notification.

use super::{request_key, tool_program_queries, Session, Writer};
use rpi_extcore::service::tool_program_snapshot::ChildSnapshotQuery;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tokio::sync::{oneshot, watch, Mutex};

pub(super) const SUBSCRIBE_METHOD: &str = "session/tool_program_subscribe";
pub(super) const UNSUBSCRIBE_METHOD: &str = "session/tool_program_unsubscribe";
pub(super) const NOTIFICATION_METHOD: &str = "session/tool_program_update";
const MAX_SUBSCRIPTIONS: usize = 16;

pub(super) fn capability() -> Value {
    json!({
        "version": 1,
        "optional": true,
        "snapshotFirst": true,
        "maxSubscriptions": MAX_SUBSCRIPTIONS,
        "subscribe": SUBSCRIBE_METHOD,
        "unsubscribe": UNSUBSCRIBE_METHOD,
        "notification": NOTIFICATION_METHOD,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubscribeRequest {
    session_id: String,
    subscription_id: String,
    query: ChildSnapshotQuery,
    #[serde(default, rename = "_meta")]
    _meta: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnsubscribeRequest {
    session_id: String,
    subscription_id: String,
    #[serde(default, rename = "_meta")]
    _meta: Map<String, Value>,
}

fn valid_token(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

fn parse_subscribe(params: Value) -> Result<SubscribeRequest, ()> {
    let request: SubscribeRequest = serde_json::from_value(params).map_err(|_| ())?;
    if !valid_token(&request.session_id) || !valid_token(&request.subscription_id) {
        return Err(());
    }
    Ok(request)
}

fn parse_unsubscribe(params: Value) -> Result<UnsubscribeRequest, ()> {
    let request: UnsubscribeRequest = serde_json::from_value(params).map_err(|_| ())?;
    if !valid_token(&request.session_id) || !valid_token(&request.subscription_id) {
        return Err(());
    }
    Ok(request)
}

struct Entry {
    session_id: String,
    token: u64,
    cancel: watch::Sender<bool>,
    done: Option<oneshot::Receiver<()>>,
    writer: Arc<Writer>,
}

#[derive(Clone, Default)]
pub(super) struct SubscriptionRegistry {
    entries: Arc<Mutex<HashMap<String, Entry>>>,
    pending: Arc<Mutex<HashMap<String, String>>>,
    next_token: Arc<AtomicU64>,
}

impl SubscriptionRegistry {
    pub(super) async fn contains_request(&self, key: &str) -> bool {
        self.pending.lock().await.contains_key(key)
    }

    pub(super) async fn start(
        &self,
        writer: Arc<Writer>,
        sessions: &Mutex<HashMap<String, Arc<Session>>>,
        id: Value,
        params: Value,
    ) {
        let request = match parse_subscribe(params) {
            Ok(request) => request,
            Err(()) => {
                writer
                    .respond_error(&id, -32602, "invalid child subscription params")
                    .await;
                return;
            }
        };
        let key = request_key(&id);
        if self.contains_request(&key).await {
            writer
                .respond_error(&id, -32600, "duplicate request id")
                .await;
            return;
        }
        let session = sessions.lock().await.get(&request.session_id).cloned();
        let Some(session) = session else {
            writer.respond_error(&id, -32602, "unknown sessionId").await;
            return;
        };

        let (cancel, mut cancel_rx) = watch::channel(false);
        let (done_tx, done_rx) = oneshot::channel();
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        {
            let mut entries = self.entries.lock().await;
            if entries.len() >= MAX_SUBSCRIPTIONS {
                writer
                    .respond_error(&id, -32001, "child subscription limit exceeded")
                    .await;
                return;
            }
            if entries.contains_key(&request.subscription_id) {
                writer
                    .respond_error(&id, -32602, "duplicate subscriptionId")
                    .await;
                return;
            }
            entries.insert(
                request.subscription_id.clone(),
                Entry {
                    session_id: request.session_id.clone(),
                    token,
                    cancel,
                    done: Some(done_rx),
                    writer: writer.clone(),
                },
            );
        }
        self.pending
            .lock()
            .await
            .insert(key.clone(), request.subscription_id.clone());

        let registry = self.clone();
        tokio::spawn(async move {
            let result = tokio::select! {
                biased;
                _ = cancel_rx.changed() => None,
                result = session.tool_program_subscription(request.query) => Some(result),
            };
            registry.pending.lock().await.remove(&key);

            if let Some(result) = result {
                match result {
                    Ok(mut subscription) => {
                        if !*cancel_rx.borrow() && !writer.closed.load(Ordering::Acquire) {
                            let snapshot = subscription.snapshot();
                            match serde_json::to_value(snapshot) {
                                Ok(snapshot) => {
                                    writer
                                        .respond(
                                            &id,
                                            json!({
                                                "sessionId": request.session_id,
                                                "subscriptionId": request.subscription_id,
                                                "snapshot": snapshot,
                                            }),
                                        )
                                        .await;
                                    // The response is flushed before this loop is
                                    // entered, establishing snapshot-before-update.
                                    loop {
                                        let item = tokio::select! {
                                            biased;
                                            _ = cancel_rx.changed() => break,
                                            _ = writer.terminal() => break,
                                            item = subscription.next() => item,
                                        };
                                        let Some(item) = item else { break };
                                        let payload = json!({
                                            "sessionId": request.session_id,
                                            "subscriptionId": request.subscription_id,
                                            "update": item,
                                        });
                                        writer.notify(NOTIFICATION_METHOD, payload).await;
                                        if writer.closed.load(Ordering::Acquire) {
                                            break;
                                        }
                                    }
                                }
                                Err(_) => {
                                    writer
                                        .respond_error(
                                            &id,
                                            -32603,
                                            "child subscription serialization failed",
                                        )
                                        .await;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        if !*cancel_rx.borrow() && !writer.closed.load(Ordering::Acquire) {
                            let value = tool_program_queries::error_value(error);
                            let _ = writer
                                .write(&json!({"jsonrpc":"2.0","id":id,"error":value}))
                                .await;
                        }
                    }
                }
            }
            registry.finish(&request.subscription_id, token).await;
            let _ = done_tx.send(());
        });
    }

    async fn finish(&self, subscription_id: &str, token: u64) {
        let mut entries = self.entries.lock().await;
        if entries
            .get(subscription_id)
            .is_some_and(|entry| entry.token == token)
        {
            entries.remove(subscription_id);
        }
    }

    async fn cancel_subscription(&self, subscription_id: &str) -> bool {
        let entry = self.entries.lock().await.remove(subscription_id);
        let Some(mut entry) = entry else { return false };
        let _ = entry.cancel.send(true);
        if let Some(mut done) = entry.done.take() {
            // A notification or the initial snapshot response may be blocked
            // behind a stalled ACP peer. Cancellation owns the connection
            // writer as a bounded fallback: closing it makes FrameWrite
            // terminal and wakes the task instead of waiting forever.
            if tokio::time::timeout(std::time::Duration::from_secs(2), &mut done)
                .await
                .is_err()
            {
                entry.writer.close();
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), &mut done).await;
            }
        }
        true
    }

    pub(super) async fn unsubscribe(&self, writer: &Writer, id: &Value, params: Value) {
        let request = match parse_unsubscribe(params) {
            Ok(request) => request,
            Err(()) => {
                writer
                    .respond_error(id, -32602, "invalid child unsubscription params")
                    .await;
                return;
            }
        };
        // The session identity is checked before cancellation so a client
        // cannot use an unrelated session to retire another session's stream.
        let matches = self
            .entries
            .lock()
            .await
            .get(&request.subscription_id)
            .is_some_and(|entry| entry.session_id == request.session_id);
        if self
            .entries
            .lock()
            .await
            .get(&request.subscription_id)
            .is_some_and(|entry| entry.session_id != request.session_id)
        {
            writer
                .respond_error(id, -32602, "subscriptionId belongs to another session")
                .await;
            return;
        }
        // Unsubscribe is idempotent so a terminal resync/EOF racing the
        // request does not turn a successful cleanup into an unknown-resource
        // error.
        if matches {
            self.cancel_subscription(&request.subscription_id).await;
        }
        writer.respond(id, json!({})).await;
    }

    pub(super) async fn cancel_request(&self, key: &str) -> bool {
        let subscription_id = self.pending.lock().await.remove(key);
        match subscription_id {
            Some(subscription_id) => self.cancel_subscription(&subscription_id).await,
            None => false,
        }
    }

    pub(super) async fn cancel_session(&self, session_id: &str) {
        let ids: Vec<String> = self
            .entries
            .lock()
            .await
            .iter()
            .filter(|(_, entry)| entry.session_id == session_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.cancel_subscription(&id).await;
        }
    }

    pub(super) async fn shutdown(&self) {
        let ids: Vec<String> = self.entries.lock().await.keys().cloned().collect();
        for id in ids {
            self.cancel_subscription(&id).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertises_optional_snapshot_first_wire_contract() {
        let capability = capability();
        assert_eq!(capability["snapshotFirst"], true);
        assert_eq!(capability["maxSubscriptions"], 16);
        assert_eq!(capability["notification"], NOTIFICATION_METHOD);
    }

    #[test]
    fn subscription_params_are_strict_and_bounded() {
        let query = json!({
            "children": {"limit":1,"maxBytes":1024,"direction":"newestFirst"},
            "phaseLimit":1
        });
        assert!(parse_subscribe(json!({
            "sessionId":"s","subscriptionId":"sub","query":query
        }))
        .is_ok());
        assert!(parse_subscribe(json!({
            "sessionId":"s","subscriptionId":"sub","query":{},"extra":1
        }))
        .is_err());
        assert!(parse_unsubscribe(json!({
            "sessionId":"s","subscriptionId":"sub","extra":1
        }))
        .is_err());
    }
}
