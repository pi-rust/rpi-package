//! Retained session retirement owners. Transport cancellation must not release
//! lifecycle exclusion while native close or filesystem deletion is in flight.
use super::*;

pub(super) fn deleting_error() -> (ReplyKind, Value) {
    (
        ReplyKind::Conflict,
        json!({"error":"Session deletion is pending; retry DELETE", "code":"session_deleting"}),
    )
}

impl Service {
    /// Call under session_lifecycle before any same-ID native writer admission.
    /// Resource configuration adapters must use this gate too, not live_session
    /// filtering: treating a deleting host as absent could spawn a replacement.
    pub(crate) async fn check_session_admission(&self, id: &str) -> ServiceResult<()> {
        let registry = self.registry.read().await;
        if registry.deletion(id).is_some() {
            return Err(deleting_error());
        }
        if registry.preferences(id).archived {
            return Err((
                ReplyKind::Conflict,
                json!({"error":"Session is archived; restore it before opening", "code":"session_archived"}),
            ));
        }
        Ok(())
    }
}

fn owner_error() -> (ReplyKind, Value) {
    (
        ReplyKind::Unavailable,
        json!({"error":"Session retirement worker failed", "code":"session_retirement_unavailable"}),
    )
}

async fn idle_close(state: &Service, id: &str) -> ServiceResult<()> {
    let session = state.sessions.read().await.get(id).cloned();
    if let Some(session) = session {
        session.shutdown_if_idle().await.map_err(|error| match error {
            crate::host::IdleCloseError::Busy => (ReplyKind::Conflict, json!({"error":"Session is busy", "code":"session_busy"})),
            crate::host::IdleCloseError::Unavailable(_) => (ReplyKind::Unavailable, json!({"error":"Session idle state could not be confirmed", "code":"session_idle_unknown"})),
        })?;
        // The host has confirmed child exit, not merely accepted stop.
        state.sessions.write().await.remove(id);
        detach_durable(state, id).await;
    }
    Ok(())
}

pub(super) async fn metadata(
    state: &Service,
    id: String,
    mut request: MetadataRequest,
) -> (ReplyKind, Value) {
    // Reject malformed metadata before probing or closing a live host.
    if let Some(tags) = request.tags.take() {
        request.tags = match crate::registry::normalize_tags(tags) {
            Ok(tags) => Some(tags),
            Err(_) => {
                return (
                    ReplyKind::InvalidInput,
                    json!({"error":"Invalid session tags", "code":"invalid_session_tags"}),
                )
            }
        };
    }
    let permit = match state.retirement_workers.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return (
                ReplyKind::Conflict,
                json!({"error":"Session retirement is at capacity; try again later", "code":"session_retirement_capacity"}),
            )
        }
    };
    let owner = state.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let _lifecycle = owner.session_lifecycle.lock().await;
        if owner.closing.load(std::sync::atomic::Ordering::Acquire) {
            return owner_error();
        }
        {
            let registry = owner.registry.read().await;
            if registry.get(&id).is_none() {
                return (ReplyKind::NotFound, json!({"error":"session not found"}));
            }
            if registry.deletion(&id).is_some() {
                return deleting_error();
            }
        }
        if request.archived == Some(true) {
            if let Err(error) = idle_close(&owner, &id).await {
                return error;
            }
        }
        // Restore and ordinary metadata updates never spawn native processes.
        let mut registry = owner.registry.write().await;
        match registry.update_preferences(&id, request.pinned, request.archived, request.tags) {
            Ok(prefs) => (ReplyKind::Success, json!({"sessionId":id,"pinned":prefs.pinned,"archived":prefs.archived,"tags":prefs.tags})),
            Err(_) => (ReplyKind::Internal, json!({"error":"Session preferences could not be saved"})),
        }
    }).await.unwrap_or_else(|_| owner_error())
}

pub(super) async fn delete(state: &Service, id: String) -> ServiceReply {
    let permit = match state.retirement_workers.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return (ReplyKind::Conflict, json!({"error":"Session retirement is at capacity; try again later", "code":"session_retirement_capacity"})).into_reply(),
    };
    let owner = state.clone();
    tokio::spawn(async move {
        let _permit = permit;
        delete_owned(&owner, id).await
    })
    .await
    .unwrap_or_else(|_| owner_error().into_reply())
}

async fn delete_owned(state: &Service, id: String) -> ServiceReply {
    let _lifecycle = state.session_lifecycle.lock().await;
    if state.closing.load(std::sync::atomic::Ordering::Acquire) {
        return owner_error().into_reply();
    }
    let pending = state.registry.read().await.deletion(&id).cloned();
    let execution = if let Some(plan) = pending {
        // Exact persisted manifest only. Never probe/spawn native or rediscover
        // a new plan after a partial removal or service restart.
        tokio::task::spawn_blocking(move || crate::deletion::execute(&plan))
    } else {
        let live = state.sessions.read().await.get(&id).cloned();
        let remembered = state.registry.read().await.get(&id).is_some();
        if !remembered && live.is_none() {
            return (
                ReplyKind::NotFound,
                json!({"error":"unknown session", "code":"session_not_found"}),
            )
                .into_reply();
        }
        if let Err(error) = idle_close(state, &id).await {
            return error.into_reply();
        }
        // Normally publication remembers every live host. Retain the legacy
        // live-only deletion contract without weakening begin_deletion's
        // require-known invariant or changing metadata on a Busy rejection.
        if !remembered {
            let snapshot = live
                .as_ref()
                .expect("known or live checked")
                .snapshot()
                .await;
            if state
                .registry
                .write()
                .await
                .upsert(RegistryEntry {
                    session_id: id.clone(),
                    project_id: Some(state.default_project_id.clone()),
                    title: snapshot.title,
                    parent_session_id: None,
                    fork_entry_id: None,
                    relation: None,
                    created_at: snapshot.created_at,
                    updated_at: snapshot.updated_at,
                })
                .is_err()
            {
                return (ReplyKind::Unavailable, json!({"error":"Session deletion intent could not be saved", "code":"session_deletion_storage"})).into_reply();
            }
        }
        let project = match session_project(state, &id).await {
            Ok(project) => project,
            Err(_) => {
                return (
                    ReplyKind::Unavailable,
                    json!({"error":"Session project is unavailable"}),
                )
                    .into_reply()
            }
        };
        let configured = state.config.session_dir.clone();
        let timings = state.config.registry.with_extension("tool-timings");
        let preparing_id = id.clone();
        let prepared = match tokio::task::spawn_blocking(move || {
            let root = crate::run_timing::session_root(&project.root, configured)?;
            crate::deletion::prepare_locked(&root, &preparing_id, &timings)
        })
        .await
        {
            Ok(Ok(plan)) => plan,
            Ok(Err(error)) => return error.into_reply(),
            Err(_) => return owner_error().into_reply(),
        };
        if state
            .registry
            .write()
            .await
            .begin_deletion(&id, prepared.plan().clone())
            .is_err()
        {
            return (ReplyKind::Unavailable, json!({"error":"Session deletion intent could not be saved", "code":"session_deletion_storage"})).into_reply();
        }
        // The retained owner holds the native lock through registry persistence;
        // move the same lock into execution, without a release/reacquire gap.
        tokio::task::spawn_blocking(move || crate::deletion::execute_prepared(prepared))
    };
    match execution.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return error.into_reply(),
        Err(_) => return owner_error().into_reply(),
    }
    // Registry failure retains the exact manifest, so DELETE can safely retry.
    if state.registry.write().await.remove(&id).is_err() {
        return (ReplyKind::Unavailable, json!({"error":"Session deletion completion could not be saved", "code":"session_deletion_storage"})).into_reply();
    }
    ReplyKind::Empty.into_reply()
}
