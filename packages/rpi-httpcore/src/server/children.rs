use super::{
    events::authorized_events,
    response::{strict_json, typed_reply},
    AppState, HttpReply,
};
use axum::{
    extract::{rejection::JsonRejection, Json, Path, Query, State},
    response::{sse::Event, IntoResponse, Sse},
};
use futures_util::{stream, StreamExt};
use rpi_extcore::{
    reply::{IntoReply, ReplyKind},
    service::{
        self,
        tool_program_live::ChildStreamItem,
        tool_program_queries::{ChildBodyQuery, ChildPageQuery, ChildPhaseQuery, Direction},
        ChildSnapshotQuery,
    },
};
use serde::Deserialize;
use serde_json::{json, Value};

pub(super) async fn child_summaries(
    State(state): State<AppState>,
    Path(id): Path<String>,
    query: Result<Json<ChildPageQuery>, JsonRejection>,
) -> Result<HttpReply, HttpReply> {
    let query = strict_json(query)?;
    Ok(typed_reply(
        service::child_summaries(&state.backend, id, query).await,
    ))
}

pub(super) async fn child_phases(
    State(state): State<AppState>,
    Path(id): Path<String>,
    query: Result<Json<ChildPhaseQuery>, JsonRejection>,
) -> Result<HttpReply, HttpReply> {
    let query = strict_json(query)?;
    Ok(typed_reply(
        service::child_phases(&state.backend, id, query).await,
    ))
}

pub(super) async fn child_body(
    State(state): State<AppState>,
    Path(id): Path<String>,
    query: Result<Json<ChildBodyQuery>, JsonRejection>,
) -> Result<HttpReply, HttpReply> {
    let query = strict_json(query)?;
    Ok(typed_reply(
        service::child_body(&state.backend, id, query).await,
    ))
}

pub(super) async fn child_snapshot(
    State(state): State<AppState>,
    Path(id): Path<String>,
    query: Result<Json<ChildSnapshotQuery>, JsonRejection>,
) -> Result<HttpReply, HttpReply> {
    let query = strict_json(query)?;
    Ok(typed_reply(
        service::child_snapshot(&state.backend, id, query).await,
    ))
}

// Reconnects deliberately establish a NEW bounded child snapshot. Ordinary
// transcript SSE IDs and Last-Event-ID replay semantics are not changed.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChildEventQuery {}

pub(super) async fn child_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: axum::http::HeaderMap,
    Query(_query): Query<ChildEventQuery>,
) -> axum::response::Response {
    let query = ChildSnapshotQuery {
        children: ChildPageQuery {
            leaf_id: None,
            cursor: None,
            direction: Direction::NewestFirst,
            limit: 8,
            max_bytes: 32768,
        },
        phase_limit: 8,
    };
    let subscription = match service::child_subscription(&state.backend, id, query).await {
        Ok(subscription) => subscription,
        Err(error) => return HttpReply(error.into_reply()).into_response(),
    };
    let initial = match child_snapshot_event(subscription.snapshot()) {
        Ok(event) => event,
        Err(_) => {
            return typed_reply::<Value>(Err((
                ReplyKind::Internal,
                json!({"error":"child stream serialization failed"}),
            )))
            .into_response();
        }
    };
    let updates = stream::unfold(Some(subscription), |subscription| async move {
        let mut subscription = subscription?;
        let item = subscription.next().await?;
        match child_update_event(&item) {
            Ok(event) => Some((Ok(event), Some(subscription))),
            Err(_) => Some((
                Ok(Event::default()
                    .event("resync_required")
                    .data("{\"code\":\"child_stream_serialization_failed\"}")),
                None,
            )),
        }
    });
    let mut shutdown = state.shutdown;
    Sse::new(
        authorized_events(
            stream::once(async move { Ok(initial) }).chain(updates),
            state.security,
            headers,
        )
        .take_until(async move {
            if !*shutdown.borrow_and_update() {
                let _ = shutdown.changed().await;
            }
        }),
    )
    .keep_alive(axum::response::sse::KeepAlive::default())
    .into_response()
}

pub(super) fn child_snapshot_event(
    snapshot: &service::ChildSnapshot,
) -> Result<Event, serde_json::Error> {
    Ok(Event::default()
        .event("tool_program_child_snapshot")
        .id(snapshot.last_seq.to_string())
        .data(serde_json::to_string(snapshot)?))
}

pub(super) fn child_update_event(item: &ChildStreamItem) -> Result<Event, serde_json::Error> {
    let (name, seq) = match item {
        ChildStreamItem::Update { seq, .. } => ("tool_program_child_update", seq),
        ChildStreamItem::ResyncRequired { seq, .. } => ("resync_required", seq),
    };
    Ok(Event::default()
        .event(name)
        .id(seq.to_string())
        .data(serde_json::to_string(item)?))
}

pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/api/v1/sessions/{id}/tool-program/children/query",
            axum::routing::post(child_summaries),
        )
        .route(
            "/api/v1/sessions/{id}/tool-program/phases/query",
            axum::routing::post(child_phases),
        )
        .route(
            "/api/v1/sessions/{id}/tool-program/body/query",
            axum::routing::post(child_body),
        )
        .route(
            "/api/v1/sessions/{id}/tool-program/snapshot",
            axum::routing::post(child_snapshot),
        )
        .route(
            "/api/v1/sessions/{id}/tool-program/events",
            axum::routing::get(child_events),
        )
}
