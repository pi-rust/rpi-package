//! Wire adapter only. Every operation targets the supplied workspace instance.
use crate::{ServerState, security};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Extension, Path, Query, State},
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{any, get},
};
use futures_util::StreamExt;
use rpi_node_core::{
    runtime::RuntimeError,
    workspace::{LocalError, WorkspaceError},
};
use serde_json::{Value, json};
use std::{collections::HashMap, convert::Infallible, sync::Arc, time::Duration};

pub(crate) mod session_operations;
mod timeline;

pub(crate) fn router(access: Arc<rpi_http_runtime::AccessState>) -> Router<Arc<ServerState>> {
    Router::new()
        .route("/.well-known/rpi-work-entry", get(hosted_entry))
        .merge(rpi_http_runtime::auth_router(access))
        .route("/api/v1/realtime", get(crate::realtime::upgrade))
        .route("/api/v1/transports", get(crate::realtime::discovery))
        .route("/api/v1/{*path}", any(dispatch))
        .fallback(assets)
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
}
async fn hosted_entry(State(state): State<Arc<ServerState>>) -> Response {
    if state.assets.is_none() && !state.web_proxy {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(json!({"version":1,"mode":"hosted","nodeId":state.node_id})).into_response()
}
pub(crate) struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
impl From<WorkspaceError> for ApiError {
    fn from(error: WorkspaceError) -> Self {
        // Map service errors without taking a dependency on extcore's private
        // implementation in the HTTP layer. Preserve actionable public input
        // errors but never return process paths/stderr for internal failures.
        match error {
            WorkspaceError::Runtime(RuntimeError::Unsupported {
                operation,
                protocol,
            }) => Self(
                StatusCode::NOT_IMPLEMENTED,
                format!("{operation} unsupported by {protocol}"),
            ),
            WorkspaceError::Local(LocalError::AuthoringIo(error)) => Self(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Authoring filesystem operation failed (OS code {:?}). Keep your unsaved text. Source/metadata candidates and pending/test recovery records are retained under .rpi/work-authoring; inspect before explicit repair, never reset or blindly replay.",
                    error.raw_os_error()
                ),
            ),
            WorkspaceError::Local(LocalError::InvalidInput(message)) => {
                Self(StatusCode::BAD_REQUEST, message)
            }
            WorkspaceError::Local(LocalError::NotFound(_)) => {
                Self(StatusCode::NOT_FOUND, "not found".into())
            }
            WorkspaceError::Local(LocalError::DuplicateProject(_)) => {
                Self(StatusCode::CONFLICT, "project already registered".into())
            }
            WorkspaceError::Local(LocalError::Unsupported(message)) => {
                Self(StatusCode::NOT_IMPLEMENTED, message)
            }
            WorkspaceError::Local(LocalError::Shutdown) => Self(
                StatusCode::SERVICE_UNAVAILABLE,
                "workspace is shutting down".into(),
            ),
            WorkspaceError::Local(LocalError::Service { kind, message }) => {
                let status = match format!("{kind:?}").as_str() {
                    "InvalidInput" => StatusCode::BAD_REQUEST,
                    "AccessDenied" => StatusCode::FORBIDDEN,
                    "NotFound" => StatusCode::NOT_FOUND,
                    "Conflict" => StatusCode::CONFLICT,
                    "Unavailable" | "HostUnavailable" => StatusCode::SERVICE_UNAVAILABLE,
                    "Unsupported" => StatusCode::NOT_IMPLEMENTED,
                    "LimitExceeded" => StatusCode::PAYLOAD_TOO_LARGE,
                    "UnsupportedContent" => StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "Timeout" => StatusCode::GATEWAY_TIMEOUT,
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                };
                Self(
                    status,
                    if status.is_server_error() {
                        "workspace operation failed".into()
                    } else {
                        message
                    },
                )
            }
            _ => Self(
                StatusCode::INTERNAL_SERVER_ERROR,
                "workspace operation failed".into(),
            ),
        }
    }
}
impl ApiError {
    pub(crate) fn into_realtime(self) -> rpi_node_api::api::realtime::RealtimeError {
        use rpi_node_api::api::realtime::RealtimeErrorCode as Code;
        // Once an owner call may have started, a timeout/internal/unavailable
        // response does not prove non-execution. Never make it safely retryable.
        crate::executor::error(match self.0.as_u16() {
            400 | 422 => Code::InvalidInput,
            401 | 403 => Code::AccessDenied,
            404 => Code::NotFound,
            501 => Code::Unsupported,
            409 => Code::Conflict,
            413 | 429 => Code::LimitExceeded,
            415 => Code::UnsupportedContent,
            _ => Code::OutcomeUnknown,
        })
    }
}
impl From<rpi_node_core::workspace::operations::OperationError> for ApiError {
    fn from(error: rpi_node_core::workspace::operations::OperationError) -> Self {
        use rpi_node_core::workspace::operations::OperationError as Error;
        let status = match error {
            Error::InvalidInput => StatusCode::BAD_REQUEST,
            Error::AccessDenied => StatusCode::FORBIDDEN,
            Error::Conflict => StatusCode::CONFLICT,
            Error::UnknownOperation => StatusCode::NOT_FOUND,
            Error::LimitExceeded => StatusCode::TOO_MANY_REQUESTS,
            Error::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        };
        Self(status, error.to_string())
    }
}
fn text<'a>(body: &'a Value, key: &str) -> Result<&'a str, ApiError> {
    body.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, format!("missing {key}")))
}
fn typed_body<T: serde::de::DeserializeOwned>(body: &Value) -> Result<T, ApiError> {
    serde_json::from_value(body.clone()).map_err(|_| {
        ApiError(
            StatusCode::BAD_REQUEST,
            "invalid typed resource request".into(),
        )
    })
}
fn optional<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str)
}
pub(crate) struct ApiRequest {
    path: String,
    query: HashMap<String, String>,
    method: Method,
    headers: HeaderMap,
    bytes: Bytes,
    reply: tokio::sync::oneshot::Sender<Response>,
    permit: tokio::sync::OwnedSemaphorePermit,
    auth: Option<security::AuthLease>,
}
async fn dispatch(
    State(state): State<Arc<ServerState>>,
    auth: Option<Extension<security::AuthLease>>,
    Path(path): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    if matches!(method, Method::GET)
        && matches!(
            path.split('/').collect::<Vec<_>>().as_slice(),
            ["sessions", _, "message-summaries" | "message-window"]
        )
    {
        if let Err(error) = timeline::reject_duplicates(&uri) {
            return error.into_response();
        }
    }
    let auth = auth.map(|Extension(lease)| lease);
    if path != "health" && auth.is_none() {
        return security::authentication_required();
    }
    let Ok(permit) = state.admission.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let (reply, result) = tokio::sync::oneshot::channel();
    let request = ApiRequest {
        path,
        query,
        method,
        headers,
        bytes,
        reply,
        permit,
        auth: auth.clone(),
    };
    let message = crate::executor::ExecutorRequest::Http(request);
    if let Some(auth) = &auth {
        if let Err(error) = crate::executor::publish_authenticated(&state, auth, message).await {
            return if error.code == rpi_node_api::api::realtime::RealtimeErrorCode::AccessDenied {
                security::lease_failure(&state, auth)
            } else {
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            };
        }
    } else if state.requests.send(message).await.is_err() {
        // Only health reaches the public execution path.
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    result
        .await
        .unwrap_or_else(|_| StatusCode::SERVICE_UNAVAILABLE.into_response())
}
pub(crate) async fn execute(state: Arc<ServerState>, request: ApiRequest) {
    let ApiRequest {
        path,
        query,
        method,
        headers,
        bytes,
        reply,
        permit: _permit,
        auth,
    } = request;
    if *state.shutdown.borrow() {
        let _ = reply.send(StatusCode::SERVICE_UNAVAILABLE.into_response());
        return;
    }
    if path != "health" {
        let Some(auth) = &auth else {
            let _ = reply.send(security::authentication_required());
            return;
        };
        // Grant execution before creating the owner future. Revocation after
        // this point does not roll back or cancel already-started work.
        if state
            .security
            .while_authorized(auth, &state.sharing, || ())
            .await
            .is_none()
        {
            let _ = reply.send(security::lease_failure(&state, auth));
            return;
        }
    }
    let response = perform(
        state.clone(),
        path,
        query,
        method,
        headers,
        bytes,
        auth.clone(),
    )
    .await
    .unwrap_or_else(IntoResponse::into_response);
    if let Some(auth) = &auth {
        let mut outgoing = Some((reply, response));
        state
            .security
            .while_authorized(auth, &state.sharing, || {
                if let Some((reply, response)) = outgoing.take() {
                    let _ = reply.send(response);
                }
            })
            .await;
        if let Some((reply, _)) = outgoing.take() {
            let _ = reply.send(security::lease_failure(&state, auth));
        }
    } else {
        let _ = reply.send(response);
    }
}
#[allow(clippy::too_many_lines)] // Explicit protocol mapping; no backend ownership here.
async fn perform(
    state: Arc<ServerState>,
    path: String,
    query: HashMap<String, String>,
    method: Method,
    headers: HeaderMap,
    bytes: Bytes,
    auth: Option<security::AuthLease>,
) -> Result<Response, ApiError> {
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid JSON".into()))?
    };
    let parts: Vec<_> = path.split('/').collect();
    let w = &state.workspace;
    let path_query = query.get("path").map_or("", String::as_str);
    let value = match (method.as_str(), parts.as_slice()) {
        ("POST", ["local", "control-session"]) if state.security.is_control() => {
            json!({"sessionId":w.control_session().await?})
        }
        ("POST", ["local", "project-directory"]) if state.security.is_control() => {
            json!({"project":w.create_project_directory(text(&body,"name")?, std::path::Path::new(text(&body,"path")?), body["initGit"].as_bool().unwrap_or(false)).await?})
        }
        ("GET", ["health"]) => {
            json!({"ok":true,"service":"rpi-work-desktop","version":env!("CARGO_PKG_VERSION"),"protocol":"rpi","nodeId":state.node_id,"sharingEnabled":state.sharing.enabled()})
        }
        ("GET", ["capabilities"]) => {
            serde_json::to_value(w.capabilities().await?).unwrap_or(Value::Null)
        }
        ("GET", ["realtime", "operations", epoch, id]) => {
            let lease = auth.as_ref().ok_or_else(|| {
                ApiError(StatusCode::UNAUTHORIZED, "authentication required".into())
            })?;
            let reference = rpi_node_api::api::realtime::OperationRef {
                owner_epoch: (*epoch).into(),
                operation_id: (*id).into(),
            };
            reference.validate().map_err(|_| {
                ApiError(
                    StatusCode::BAD_REQUEST,
                    "invalid operation reference".into(),
                )
            })?;
            json!(
                w.remote_operations()
                    .status(lease.principal(), &reference)?
            )
        }
        ("GET", ["components"]) => json!(w.component_catalog()?),
        ("POST", ["components"]) => json!(w.component_install(typed_body(&body)?)?),
        ("GET", ["components", id, "panels", panel]) => {
            json!({"html":w.component_panel(id, panel)?})
        }
        ("POST", ["components", id, "call"]) => {
            json!(w.component_call(id, typed_body(&body)?, state.security.is_control())?)
        }
        ("DELETE", ["components", id]) => {
            w.component_remove(id)?;
            json!({"ok":true})
        }
        ("GET", ["providers"]) => json!(w.provider_catalog().await?),
        ("POST", ["providers"]) => {
            let request: rpi_node_api::api::ProviderMutation = typed_body(&body)?;
            json!(w.provider_mutate(request).await?)
        }
        ("POST", ["providers", "release-idle-hosts"]) => {
            // Explicit empty object only; no session/project/path selectors.
            if !body.as_object().is_some_and(serde_json::Map::is_empty) {
                return Err(ApiError(
                    StatusCode::BAD_REQUEST,
                    "expected empty object".into(),
                ));
            }
            json!(w.provider_release_hosts().await?)
        }
        ("GET", ["projects"]) => json!({"projects":w.projects().await?}),
        ("POST", ["projects"]) => {
            json!({"project":w.create_project(text(&body,"name")?,optional(&body,"path"),body["initGit"].as_bool().unwrap_or(false)).await?})
        }
        ("GET", ["project-folders"]) => json!(w.project_folders(path_query).await?),
        ("POST", ["projects", id, "rename"]) => {
            json!({"project":w.rename_project(id,text(&body,"name")?).await?})
        }
        ("GET", ["projects", id, "files"]) => json!(w.files(id, path_query).await?),
        ("GET", ["projects", id, "file"]) => json!(w.file(id, path_query).await?),
        ("GET", ["projects", id, "image"]) => {
            let image = w.image(id, path_query).await?;
            return Ok((
                [
                    (header::CONTENT_TYPE, image.media_type.as_str()),
                    (header::CACHE_CONTROL, "no-store"),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                ],
                image.bytes,
            )
                .into_response());
        }
        ("GET", ["projects", id, "git", "status"]) => json!(w.git_status(id).await?),
        ("GET", ["projects", id, "git", "diff"]) => json!(
            w.diff(
                id,
                path_query,
                query.get("staged").is_some_and(|s| s == "true")
            )
            .await?
        ),
        ("GET", ["projects", id, "packages"]) => json!(w.package_catalog(id).await?),
        ("POST", ["projects", id, "packages", "bundle"]) => {
            json!(w.package_bundle(id, typed_body(&body)?).await?)
        }
        ("POST", ["projects", id, "packages", "acquire"]) => {
            json!(w.package_acquire(id, typed_body(&body)?).await?)
        }
        ("POST", ["projects", id, "packages", "install"]) => {
            json!(w.package_install(id, typed_body(&body)?).await?)
        }
        ("GET", ["projects", id, "native-resources"]) => json!(w.native_catalog(id).await?),
        ("POST", ["projects", id, "native-resources"]) => {
            json!(w.native_manage(id, typed_body(&body)?).await?)
        }
        ("POST", ["projects", id, "resource-sessions"]) => {
            json!({"sessionId":w.native_create(id, typed_body(&body)?).await?})
        }
        ("POST", ["projects", id, "release-idle-resource-hosts"]) => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Release {
                scope: rpi_node_api::api::NativeScope,
            }
            let request: Release = typed_body(&body)?;
            json!(w.native_release(id, request.scope).await?)
        }
        ("POST", ["projects", id, "authoring"]) => {
            json!(w.authoring(id, typed_body(&body)?).await?)
        }
        ("GET", ["sessions", id, "resources"]) => json!(w.native_session_resources(id).await?),
        ("POST", ["sessions", id, "resources"]) => {
            json!(w.native_apply(id, typed_body(&body)?).await?)
        }
        ("POST", ["sessions", id, "saved-resource-history"]) => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct History {
                cursor: Option<String>,
            }
            let request: History = typed_body(&body)?;
            json!(w.native_saved_history(id, request.cursor).await?)
        }
        ("GET", ["agent-backends"]) => json!({"backends":w.agent_backends().await?}),
        ("GET", ["sessions", id, "agent-binding"]) => json!(w.session_agent_binding(id).await?),
        ("GET", ["sessions", id, "access"]) => json!(w.session_access(id).await?),
        ("GET", ["sessions", id, "message-summaries"]) => {
            w.command_data::<Value>(
                id,
                "get_message_summaries",
                timeline::parameters(&query, false)?,
            )
            .await?
        }
        ("GET", ["sessions", id, "message-window"]) => {
            w.command_data::<Value>(
                id,
                "get_message_window",
                timeline::parameters(&query, true)?,
            )
            .await?
        }
        ("POST", ["sessions", id, "dialogs"]) => {
            use rpi_node_api::api::realtime::RealtimeOperationKind as Kind;
            let kind = if body.get("cancelled") == Some(&Value::Bool(true)) {
                Kind::CancelDialog
            } else {
                Kind::DialogResponse
            };
            session_operations::perform(w, id, kind, &body).await?
        }
        ("GET", ["sessions", id, "configuration"]) => json!(w.session_configuration(id).await?),
        ("POST", ["sessions", id, "configuration"]) => {
            session_operations::perform(
                w,
                id,
                rpi_node_api::api::realtime::RealtimeOperationKind::SetConfiguration,
                &body,
            )
            .await?
        }
        ("GET", ["sessions"]) => json!({"sessions":w.sessions().await?}),
        ("POST", ["sessions"]) => {
            json!({"sessionId":w.create_session_with_backend(optional(&body,"projectId"),optional(&body,"backend").unwrap_or("rpi")).await?})
        }
        ("GET", ["sessions", id]) => json!(w.get_session(id).await?),
        ("DELETE", ["sessions", id]) => {
            w.delete_session(id).await?;
            json!({"ok":true})
        }
        ("POST", ["sessions", id, "open"]) => json!(w.open_session(id).await?),
        ("POST", ["sessions", id, "rename"]) => {
            json!(w.rename_session(id, text(&body, "title")?).await?)
        }
        ("POST", ["sessions", id, "metadata"]) => json!(
            w.session_preferences(id, body["archived"].as_bool(), body["pinned"].as_bool())
                .await?
        ),
        ("POST", ["sessions", id, "fork"]) => json!(
            w.fork_session(id, optional(&body, "entryId"), optional(&body, "title"))
                .await?
        ),
        ("POST", ["sessions", id, "clone"]) => {
            json!(w.clone_session(id, optional(&body, "title")).await?)
        }
        ("GET", ["sessions", id, "snapshot"]) => json!(if let Some(limit) = query.get("limit") {
            w.snapshot_with_limit(
                id,
                limit
                    .parse()
                    .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid limit".into()))?,
            )
            .await?
        } else {
            w.snapshot(id).await?
        }),
        ("POST", ["sessions", id, "commands"]) => {
            session_operations::perform(
                w,
                id,
                rpi_node_api::api::realtime::RealtimeOperationKind::SessionCommand,
                &body,
            )
            .await?
        }
        ("POST", ["auth", "refresh"]) => w.refresh_auth(text(&body, "provider")?).await?,
        ("GET", ["sessions", id, "events"]) => {
            let cursor = headers
                .get("last-event-id")
                .map_or_else(
                    || Ok(query.get("after").map_or("0", String::as_str)),
                    |v| v.to_str(),
                )
                .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid Last-Event-ID".into()))?;
            let after = cursor
                .strip_prefix("seq-")
                .unwrap_or(cursor)
                .parse()
                .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid event cursor".into()))?;
            let lease = auth.ok_or_else(|| {
                ApiError(StatusCode::UNAUTHORIZED, "authentication required".into())
            })?;
            let mut source = w.subscribe(id, after).await?;
            let stopped = state.shutdown.clone();
            let policy = state.sharing.clone();
            let stream = async_stream::stream! {
                loop {
                    if *stopped.borrow() || !lease.is_valid(&policy) { break; }
                    let item = tokio::select! {
                        biased;
                        _ = lease.invalidated(&policy, &stopped) => break,
                        item = source.next() => item,
                    };
                    // A revoke can land after the select chose a ready event.
                    if *stopped.borrow() || !lease.is_valid(&policy) { break; }
                    match item {
                        Some(Ok(wire)) => {
                            let mut event = Event::default().event(wire.event_type).data(wire.data.to_string());
                            if let Some(id) = wire.id.or_else(|| wire.seq.map(|s| s.to_string())) { event = event.id(id); }
                            yield Ok::<_,Infallible>(event);
                        },
                        Some(Err(_)) => { yield Ok(Event::default().event("resync_required").data("{\"type\":\"resync_required\"}")); break; },
                        None => break,
                    }
                }
            };
            return Ok(Sse::new(stream)
                .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
                .into_response());
        }
        _ => {
            return Err(ApiError(
                StatusCode::NOT_FOUND,
                "unknown API endpoint".into(),
            ));
        }
    };
    let mut response = Json(value).into_response();
    if matches!(
        parts.as_slice(),
        ["sessions", _, "configuration" | "access" | "dialogs"]
    ) {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    }
    Ok(response)
}
async fn assets(State(state): State<Arc<ServerState>>, method: Method, uri: Uri) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let Some(root) = &state.assets else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let path = uri.path().trim_start_matches('/');
    if path
        .split('/')
        .any(|p| p == ".." || p.contains([char::from(92), ':', '%']))
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let selected = if path.is_empty() {
        root.join("index.html")
    } else {
        root.join(path)
    };
    let selected = if selected.is_file() {
        selected
    } else if !path.contains('.') {
        root.join("index.html")
    } else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(file) = selected.canonicalize() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !file.starts_with(root) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mime = match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript",
        "css" => "text/css",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    };
    let is_entry = file
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "index.html");
    let cache_control = if is_entry || path == "build.json" {
        "no-cache, must-revalidate"
    } else if path.starts_with("releases/") {
        // Release assets are content-addressed by the Web build and can remain
        // cached across restarts and deployments.
        "public, max-age=31536000, immutable"
    } else {
        // Compatibility copies at the bundle root retain a short TTL because
        // their names are stable and may be replaced by an older deployment.
        "public, max-age=300, must-revalidate"
    };
    match tokio::fs::read(file).await {
        Ok(bytes) => (
            [
                (header::CACHE_CONTROL, cache_control),
                (header::CONTENT_TYPE, mime),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            if method == Method::HEAD {
                vec![]
            } else {
                bytes
            },
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}
