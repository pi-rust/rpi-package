use crate::{access::Admission, security::Security};
use axum::{middleware::from_fn_with_state, Router};
use rpi_extcore::service::{BackendConfig, Service};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::watch;

mod auth;
mod children;
mod events;
mod middleware;
mod projects;
mod response;
mod sessions;
mod system;

use auth::{allowed_hosts, configured_password};
use middleware::{admit_owner, image_cache_control, touch_session_activity};
use response::HttpReply;

#[derive(Clone)]
struct AppState {
    backend: Arc<Service>,
    security: Arc<Security>,
    admission: Admission,
    shutdown: watch::Receiver<bool>,
    service_name: &'static str,
}
pub struct Listening {
    pub address: SocketAddr,
    pub hosts: Vec<String>,
    pub password_enabled: bool,
}
pub async fn run(
    config: crate::config::AppConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    serve_until(
        config,
        Router::new(),
        |info| println!("rpi HTTP API listening on {}", info.address),
        async {
            let _ = tokio::signal::ctrl_c().await;
        },
    )
    .await
}
pub async fn serve_until(
    config: crate::config::AppConfig,
    frontend: Router,
    on_listen: impl FnOnce(Listening),
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut config = config;
    let requested = SocketAddr::new(config.bind.parse()?, config.port);
    let listener = tokio::net::TcpListener::bind(requested)
        .await
        .map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("cannot listen on {requested}: {error}"),
            )
        })?;
    let address = listener.local_addr()?;
    config.port = address.port();
    let backend = Service::new(BackendConfig {
        project: config.project.clone(),
        projects: config.projects.clone(),
        projects_root: config.projects_root.clone(),
        rpi_bin: config.rpi_bin.clone(),
        session_dir: config.session_dir.clone(),
        registry: config.registry.clone(),
    })?;
    let hosts = allowed_hosts(&config);
    let password = configured_password(&config);
    let security = Arc::new(Security::new(hosts.clone(), password.as_deref()));
    let (shutdown, receiver) = watch::channel(false);
    let state = AppState {
        backend,
        security: security.clone(),
        admission: Admission::new(config.admission_limit)
            .expect("the configured HTTP admission limit must be non-zero"),
        shutdown: receiver,
        service_name: config.service_name,
    };
    let idle_reaper = config
        .session_timeout_seconds
        .filter(|seconds| *seconds > 0)
        .map(|seconds| {
            state
                .backend
                .spawn_idle_reaper(Duration::from_secs(seconds))
        });
    let router = routes(state.clone())
        .merge(frontend)
        .layer(from_fn_with_state(security, crate::security::guard))
        .layer(axum::middleware::from_fn(image_cache_control));
    on_listen(Listening {
        address,
        hosts,
        password_enabled: state.security.enabled(),
    });
    std::io::Write::flush(&mut std::io::stdout())?;
    let mut shutdown_receiver = state.shutdown.clone();
    let serving = std::future::IntoFuture::into_future(
        axum::serve(listener, router).with_graceful_shutdown(async move {
            if !*shutdown_receiver.borrow_and_update() {
                let _ = shutdown_receiver.changed().await;
            }
        }),
    );
    tokio::pin!(serving);
    let stopped_by_signal = tokio::select! {
        result = &mut serving => {
            result?;
            false
        }
        _ = stop => {
            println!("  Status      Stopping...");
            let _ = shutdown.send(true);
            let _ = tokio::time::timeout(std::time::Duration::from_secs(8), async {
                state.admission.drain().await;
                state.backend.shutdown().await;
                let _ = serving.await;
            }).await;
            true
        }
    };
    if !stopped_by_signal {
        state.backend.shutdown().await;
    }
    if let Some(reaper) = idle_reaper {
        reaper.abort();
        let _ = reaper.await;
    }
    println!("  Status      Stopped");
    Ok(())
}

/// Build only the API routes around an existing shared service.
/// The port must be the actual listener port when validating Host headers.
pub fn router(
    backend: Arc<Service>,
    config: &crate::config::AppConfig,
    shutdown: watch::Receiver<bool>,
) -> Router {
    rpi_routes(backend, config, shutdown)
}

/// Build the standard rpi API router without a frontend. This named adapter
/// can be merged with a caller-owned transport router while the legacy
/// `router` entry point remains unchanged.
pub fn rpi_routes(
    backend: Arc<Service>,
    config: &crate::config::AppConfig,
    shutdown: watch::Receiver<bool>,
) -> Router {
    let security = Arc::new(Security::new(
        allowed_hosts(config),
        configured_password(config).as_deref(),
    ));
    let state = AppState {
        backend,
        security: security.clone(),
        admission: Admission::new(config.admission_limit)
            .expect("the configured HTTP admission limit must be non-zero"),
        shutdown,
        service_name: config.service_name,
    };
    routes(state)
        .layer(from_fn_with_state(security, crate::security::guard))
        // Outside the guard so authentication and extractor failures are not cached either.
        .layer(axum::middleware::from_fn(image_cache_control))
}

fn routes(state: AppState) -> Router {
    Router::new()
        .merge(system::routes())
        .merge(projects::routes())
        .merge(auth::routes())
        .merge(sessions::routes())
        .merge(children::routes())
        .merge(events::routes())
        .layer(from_fn_with_state(state.clone(), admit_owner))
        .layer(from_fn_with_state(state.clone(), touch_session_activity))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::{
        children::{child_snapshot_event, child_update_event},
        events::{authorized_events, event_stream},
        response::{optional_json, typed_reply},
    };
    use crate::security::Security;
    use axum::{
        http::StatusCode,
        response::{sse::Sse, IntoResponse},
    };
    use futures_util::{stream, StreamExt};
    use rpi_extcore::{
        host::BufferedEvent,
        reply::ReplyKind,
        service::{self, tool_program_live::ChildStreamItem, CreateSessionRequest, ForkRequest},
    };
    use serde_json::{json, Value};
    use std::{convert::Infallible, sync::Arc};
    use tokio::sync::broadcast;

    #[tokio::test]
    async fn child_sse_preserves_independent_snapshot_boundary_and_typed_updates() {
        use service::{
            tool_program_live::ChildResyncReason, tool_program_snapshot::ReplayDisposition,
        };
        let snapshot: service::ChildSnapshot = serde_json::from_value(json!({
            "sessionId":"session","lane":"main","page":{"leafId":null,"children":[],
            "nextCursor":null,"scanExhausted":true},"phases":[],"observations":[],
            "startSeq":3,"lastSeq":7,"events":[],"resyncRequired":false
        }))
        .unwrap();
        let child = "tpchild-11111111-1111-4111-8111-111111111111";
        let event = serde_json::from_value(json!({"type":"tool_program_child_begin", "schemaVersion":1,
            "sourceSessionId":"session","lane":"main","entryId":child,"parentEntryId":null,
            "seq":100,"customType":"rpi.tool_program.child_begin.v1","data":{"version":1,
            "childId":child,"origin":{"runId":"run","assistantEntryId":"assistant","toolIndex":0},
            "rootCallId":"call","rootToolName":"program","toolName":"read","ordinal":0,"parentChildId":null}
        })).unwrap();
        let update = ChildStreamItem::Update {
            seq: 8,
            event: Box::new(event),
            observation: None,
            disposition: ReplayDisposition::Applied,
        };
        let resync = ChildStreamItem::ResyncRequired {
            seq: 9,
            reason: ChildResyncReason::Lagged,
        };
        let frames = vec![
            child_snapshot_event(&snapshot).unwrap(),
            child_update_event(&update).unwrap(),
            child_update_event(&resync).unwrap(),
        ];
        let response =
            Sse::new(stream::iter(frames.into_iter().map(Ok::<_, Infallible>))).into_response();
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let bytes = axum::body::to_bytes(response.into_body(), 32768)
            .await
            .unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        let frames: Vec<_> = text.trim_end().split("\n\n").collect();
        assert_eq!(frames.len(), 3);
        for (frame, name, seq, expected) in [
            (
                frames[0],
                "tool_program_child_snapshot",
                7,
                serde_json::to_value(&snapshot).unwrap(),
            ),
            (
                frames[1],
                "tool_program_child_update",
                8,
                serde_json::to_value(&update).unwrap(),
            ),
            (
                frames[2],
                "resync_required",
                9,
                serde_json::to_value(&resync).unwrap(),
            ),
        ] {
            assert!(frame.lines().any(|line| line == format!("event: {name}")));
            assert!(frame.lines().any(|line| line == format!("id: {seq}")));
            let data = frame
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .unwrap();
            assert_eq!(serde_json::from_str::<Value>(data).unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn typed_child_results_preserve_json_and_existing_status_mapping() {
        use service::tool_program_queries::ChildPage;

        let page = ChildPage {
            leaf_id: Some("leaf".into()),
            children: vec![],
            next_cursor: None,
            scan_exhausted: true,
        };
        let expected = serde_json::to_value(&page).unwrap();
        let response = typed_reply(Ok(page)).into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "application/json");
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), expected);

        for (kind, status) in [
            (ReplyKind::InvalidInput, 400),
            (ReplyKind::NotFound, 404),
            (ReplyKind::Conflict, 409),
            (ReplyKind::LimitExceeded, 413),
            (ReplyKind::Unsupported, 501),
            (ReplyKind::HostUnavailable, 502),
            (ReplyKind::Timeout, 504),
        ] {
            let value = json!({"error":"neutral core error","code":"test_code"});
            let response = typed_reply::<ChildPage>(Err((kind, value.clone()))).into_response();
            assert_eq!(response.status().as_u16(), status);
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), value);
        }
    }

    #[test]
    fn optional_creation_bodies_reject_invalid_json_and_types() {
        assert!(optional_json::<CreateSessionRequest>(b"").is_ok());
        assert!(optional_json::<CreateSessionRequest>(br#"{"projectId":"project-a"}"#).is_ok());
        for body in [b"{".as_slice(), b"null", br#"{"projectId":42}"#] {
            assert_eq!(
                optional_json::<CreateSessionRequest>(body).unwrap_err().0,
                StatusCode::BAD_REQUEST
            );
        }
        assert!(optional_json::<ForkRequest>(b"").is_ok());
        assert!(optional_json::<ForkRequest>(br#"{"entryId":42}"#).is_err());
    }

    #[tokio::test]
    async fn live_events_do_not_duplicate_replay_or_the_snapshot_cursor() {
        for replay in [
            vec![],
            vec![BufferedEvent {
                seq: 5,
                value: json!({"type":"agent_start"}),
            }],
        ] {
            let (sender, receiver) = broadcast::channel(8);
            for seq in [4, 5, 6] {
                sender
                    .send(BufferedEvent {
                        seq,
                        value: json!({"type":"agent_start"}),
                    })
                    .unwrap();
            }
            drop(sender);
            let has_replay = !replay.is_empty();
            let stream = event_stream(json!({"type":"ready"}), replay, receiver, false, Some(5));
            let body = axum::body::to_bytes(Sse::new(stream).into_response().into_body(), 4096)
                .await
                .unwrap();
            let text = String::from_utf8(body.to_vec()).unwrap();
            assert!(!text.contains("id: 4\n"));
            assert_eq!(text.matches("id: 5\n").count(), usize::from(has_replay));
            assert_eq!(text.matches("id: 6\n").count(), 1);
        }
    }

    #[tokio::test]
    async fn public_event_streams_remain_open_without_password_authentication() {
        let security = Arc::new(Security::new(["example.test".into()], None));
        let (sender, receiver) = broadcast::channel(8);
        let stream = authorized_events(
            event_stream(json!({"type":"ready"}), vec![], receiver, false, None),
            security,
            axum::http::HeaderMap::new(),
        );
        futures_util::pin_mut!(stream);
        assert!(stream.next().await.is_some());
        sender
            .send(BufferedEvent {
                seq: 1,
                value: json!({"type":"message_end"}),
            })
            .unwrap();
        assert!(stream.next().await.is_some());
    }

    #[tokio::test]
    async fn logout_revokes_an_already_open_event_stream() {
        let security = Arc::new(Security::new(["example.test".into()], Some("secret")));
        let token = security.login("secret").await.unwrap();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("rpi_web_session={token}").parse().unwrap(),
        );
        let (sender, receiver) = broadcast::channel(8);
        let stream = authorized_events(
            event_stream(json!({"type":"ready"}), vec![], receiver, false, None),
            security.clone(),
            headers.clone(),
        );
        futures_util::pin_mut!(stream);
        assert!(stream.next().await.is_some());
        security.logout(&headers).await;
        sender
            .send(BufferedEvent {
                seq: 1,
                value: json!({"type":"message_end","message":"private"}),
            })
            .unwrap();
        assert!(stream.next().await.is_none());
    }
}
