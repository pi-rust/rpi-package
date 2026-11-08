use super::AppState;
use crate::security::Security;
use axum::{
    extract::{Json, Path, Query, State},
    http::StatusCode,
    response::{sse::Event, IntoResponse, Sse},
};
use futures_util::{stream, Stream, StreamExt};
use rpi_extcore::host::BufferedEvent;
use serde_json::{json, Value};
use std::{convert::Infallible, future::Future, pin::Pin, sync::Arc};
use tokio::sync::broadcast;

#[derive(Debug, serde::Deserialize)]
pub(super) struct EventQuery {
    after: Option<u64>,
}

pub(super) async fn events(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: axum::http::HeaderMap,
    Query(query): Query<EventQuery>,
) -> axum::response::Response {
    let after = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .or(query.after);
    let Some(subscription) = state.backend.subscribe_events(&id, after).await else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"session not found"})),
        )
            .into_response();
    };
    let mut shutdown = state.shutdown;
    Sse::new(
        authorized_events(
            event_stream(
                subscription.ready,
                subscription.replay,
                subscription.receiver,
                subscription.gap,
                subscription.after,
            ),
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

pub(super) fn event_stream(
    ready: Value,
    replay: Vec<BufferedEvent>,
    receiver: broadcast::Receiver<BufferedEvent>,
    gap: bool,
    after: Option<u64>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let head: Vec<Result<Event, Infallible>> =
        std::iter::once(Ok(Event::default().event("ready").data(ready.to_string())))
            .chain(gap.then(|| {
                Ok(Event::default()
                    .event("resync_required")
                    .data("{\"type\":\"resync_required\"}"))
            }))
            .collect();
    stream::iter(head).chain(rpi_extcore::events::replay_stream(replay,receiver,after).map(|item| {
        Ok(match item {
            rpi_extcore::events::StreamItem::Event(event)=>to_sse(event),
            rpi_extcore::events::StreamItem::ResyncRequired{dropped}=>Event::default().event("resync_required").data(json!({"type":"resync_required","code":"event_stream_lagged","dropped":dropped}).to_string()),
        })
    }))
}

// The middleware checks only the initial HTTP request. A long-lived stream must
// also stop after another tab logs out, sharing is revoked or the lease expires.
pub(super) fn authorized_events(
    events: impl Stream<Item = Result<Event, Infallible>> + Send + 'static,
    security: Arc<Security>,
    headers: axum::http::HeaderMap,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let security_for_wait = security.clone();
    let headers_for_wait = headers.clone();
    let invalidation: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
        if !security_for_wait.enabled() {
            std::future::pending::<()>().await;
        }
        if let Some(lease) = security_for_wait.lease_for(&headers_for_wait).await {
            security_for_wait
                .access_state()
                .wait_invalidated(&lease)
                .await;
        }
    });
    stream::unfold(
        (Box::pin(events), invalidation),
        move |(mut events, mut invalidation)| {
            let security = security.clone();
            let headers = headers.clone();
            async move {
                tokio::select! {
                    biased;
                    _ = &mut invalidation => None,
                    event = events.next() => {
                        let event = event?;
                        if !security.authorized(&headers).await {
                            return None;
                        }
                        Some((event, (events, invalidation)))
                    }
                }
            }
        },
    )
}

pub(super) fn to_sse(mut event: BufferedEvent) -> Event {
    rpi_extcore::timing::refresh_event(&mut event.value);
    let event_type = event
        .value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    Event::default()
        .event(event_type)
        .id(event.seq.to_string())
        .data(event.value.to_string())
}

pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new().route("/api/v1/sessions/{id}/events", axum::routing::get(events))
}
