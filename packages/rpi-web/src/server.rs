use crate::{
    host::{BufferedEvent, HostSession},
    projects::Projects,
    protocol::{self, ApiCommand},
    registry::{Registry, RegistryEntry},
    security::{self, Security},
    workspace::{self, Project},
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    middleware::from_fn_with_state,
    response::{
        sse::{Event, Sse},
        Html, IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use futures_util::{
    stream::{self, Stream},
    StreamExt,
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use std::{collections::HashMap, convert::Infallible, net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::process::Command;
use tokio::sync::{broadcast, watch, Mutex, RwLock};
use tower_http::services::ServeDir;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub project: PathBuf,
    /// Additional roots explicitly made available to the browser.
    pub projects: Vec<PathBuf>,
    pub projects_root: Option<PathBuf>,
    /// Web assets are independent of the agent's working directory.
    pub assets: Option<PathBuf>,
    pub rpi_bin: PathBuf,
    pub bind: String,
    pub port: u16,
    /// Optional Host allowlist; RPI_WEB_ALLOWED_HOSTS takes precedence.
    pub allowed_hosts: Option<Vec<String>>,
    /// Optional `--session-dir` forwarded to the native host.
    pub session_dir: Option<PathBuf>,
    /// Where the bridge remembers session ids across restarts.
    pub registry: PathBuf,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            project: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            projects: Vec::new(),
            projects_root: None,
            assets: None,
            rpi_bin: PathBuf::from("rpi"),
            bind: "127.0.0.1".into(),
            port: 3030,
            allowed_hosts: None,
            session_dir: None,
            registry: crate::registry::default_path(),
        }
    }
}

#[derive(Clone)]
struct AppState {
    config: AppConfig,
    projects: Arc<RwLock<Projects>>,
    default_project_id: String,
    sessions: Arc<RwLock<HashMap<String, Arc<HostSession>>>>,
    session_lifecycle: Arc<Mutex<()>>,
    registry: Arc<RwLock<Registry>>,
    security: Arc<Security>,
    shutdown: watch::Receiver<bool>,
}

pub async fn run(config: AppConfig) -> Result<(), Box<dyn std::error::Error>> {
    run_until(config, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

pub(crate) async fn run_until(
    config: AppConfig,
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut projects = vec![Project::new(&config.project)?];
    for path in &config.projects {
        let project = Project::new(path)?;
        if !projects.iter().any(|p| p.id == project.id) {
            projects.push(project);
        }
    }
    let default_project_id = projects[0].id.clone();
    let projects_root = config.projects_root.clone().unwrap_or_else(|| {
        config
            .registry
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("projects")
    });
    let projects = Projects::load(
        projects,
        projects_root,
        config.registry.with_file_name("projects.json"),
    )?;
    let security = Arc::new(Security::new(
        allowed_hosts(&config),
        configured_password().as_deref(),
    ));
    let (shutdown, shutdown_receiver) = watch::channel(false);
    let state = AppState {
        config: config.clone(),
        projects: Arc::new(RwLock::new(projects)),
        default_project_id,
        sessions: Arc::new(RwLock::new(HashMap::new())),
        session_lifecycle: Arc::new(Mutex::new(())),
        registry: Arc::new(RwLock::new(Registry::load(config.registry.clone())?)),
        security: security.clone(),
        shutdown: shutdown_receiver,
    };
    let asset_root = asset_root(&config);
    let files = ServeDir::new(asset_root.join("assets"));
    let files = if config.assets.is_none() {
        files.fallback(get(crate::assets::asset))
    } else {
        files.fallback(get(|| async { StatusCode::NOT_FOUND }))
    };
    let router = Router::new()
        .route("/", get(index))
        .route("/api/v1/health", get(health))
        .route("/api/v1/capabilities", get(capabilities))
        .route("/api/v1/projects", get(list_projects).post(create_project))
        .route("/api/v1/projects/{id}/files", get(project_files))
        .route("/api/v1/projects/{id}/file", get(project_file))
        .route("/api/v1/projects/{id}/git/status", get(project_status))
        .route("/api/v1/projects/{id}/git/diff", get(project_diff))
        .route(
            "/api/v1/auth",
            get(auth_status).post(auth_login).delete(auth_logout),
        )
        .route("/api/v1/auth/refresh", post(refresh_auth))
        .route("/api/v1/sessions", post(create_session).get(list_sessions))
        .route(
            "/api/v1/sessions/{id}",
            get(get_session).delete(delete_session),
        )
        .route("/api/v1/sessions/{id}/open", post(open_session))
        .route("/api/v1/sessions/{id}/fork", post(fork_session))
        .route("/api/v1/sessions/{id}/clone", post(clone_session))
        .route("/api/v1/sessions/{id}/rename", post(rename_session))
        .route("/api/v1/sessions/{id}/metadata", post(session_metadata))
        .route("/api/v1/sessions/{id}/commands", post(command))
        .route("/api/v1/sessions/{id}/snapshot", get(snapshot))
        .route("/api/v1/sessions/{id}/events", get(events))
        .nest_service("/assets", files)
        .route("/favicon.svg", get(favicon))
        .layer(from_fn_with_state(security, crate::security::guard))
        .with_state(state.clone());
    let address: SocketAddr = format!("{}:{}", config.bind, config.port).parse()?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!("rpi-web listening on http://{address}");
    let mut shutdown_receiver = state.shutdown.clone();
    let serving = std::future::IntoFuture::into_future(
        axum::serve(listener, router).with_graceful_shutdown(async move {
            if !*shutdown_receiver.borrow_and_update() {
                let _ = shutdown_receiver.changed().await;
            }
        }),
    );
    tokio::pin!(serving);
    tokio::select! {
        result = &mut serving => result?,
        _ = stop => {
            let _ = shutdown.send(true);
            let _ = tokio::time::timeout(std::time::Duration::from_secs(8), async {
                let _lifecycle = state.session_lifecycle.lock().await;
                let hosts: Vec<_> = state.sessions.read().await.values().cloned().collect();
                futures_util::future::join_all(hosts.iter().map(|host| host.shutdown())).await;
                let _ = serving.await;
            }).await;
        }
    }
    Ok(())
}

fn asset_root(config: &AppConfig) -> PathBuf {
    if let Some(path) = &config.assets {
        return path.clone();
    }
    let executable = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("assets")));
    let candidates = [
        executable,
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets")),
        Some(config.project.join("packages/rpi-web/assets")),
        Some(config.project.join("assets")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|p| p.join("index.html").is_file())
        .unwrap_or_else(|| config.project.join("packages/rpi-web/assets"))
}
fn configured_password() -> Option<String> {
    std::env::var("RPI_WEB_PASSWORD")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn allowed_hosts(config: &AppConfig) -> Vec<String> {
    std::env::var("RPI_WEB_ALLOWED_HOSTS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|host| !host.is_empty())
                .map(str::to_string)
                .collect()
        })
        .filter(|hosts: &Vec<String>| !hosts.is_empty())
        .or_else(|| config.allowed_hosts.clone())
        .unwrap_or_else(|| {
            vec![
                format!("127.0.0.1:{}", config.port),
                format!("localhost:{}", config.port),
            ]
        })
}

async fn auth_status(State(state): State<AppState>, headers: axum::http::HeaderMap) -> Json<Value> {
    security::status_for(&state.security, &headers).await
}

async fn auth_login(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(request): Json<security::LoginRequest>,
) -> impl IntoResponse {
    security::login_for(&state.security, &headers, request).await
}

async fn auth_logout(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    security::logout_for(&state.security, &headers).await
}

async fn index(State(state): State<AppState>) -> Html<String> {
    // Read the bundle manifest on each request so a frontend rebuild does not
    // require recompiling the Rust server. The embedded copy is the fallback
    // for a binary shipped without its `assets/` directory.
    let path = asset_root(&state.config).join("index.html");
    let html = tokio::fs::read_to_string(path)
        .await
        .unwrap_or_else(|_| include_str!("../assets/index.html").to_string());
    Html(html)
}

async fn favicon(State(state): State<AppState>) -> axum::response::Response {
    match tokio::fs::read(asset_root(&state.config).join("favicon.svg")).await {
        Ok(bytes) => ([(axum::http::header::CONTENT_TYPE, "image/svg+xml")], bytes).into_response(),
        Err(_) if state.config.assets.is_none() => crate::assets::favicon().await,
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn health() -> Json<Value> {
    Json(json!({"ok": true, "service": "rpi-web", "protocol": "rpi"}))
}

async fn capabilities() -> Json<Value> {
    Json(protocol::capabilities())
}

async fn list_projects(State(state): State<AppState>) -> Json<Value> {
    let projects = state.projects.read().await;
    Json(
        json!({"projects":projects.entries, "defaultProjectId":state.default_project_id,"projectsRoot":projects.root.to_string_lossy().trim_start_matches(r"\\?\")}),
    )
}

async fn project_by_id(state: &AppState, id: &str) -> Result<Project, (StatusCode, Json<Value>)> {
    state
        .projects
        .read()
        .await
        .entries
        .iter()
        .find(|p| p.id == id)
        .cloned()
        .ok_or_else(|| workspace::failure(StatusCode::NOT_FOUND, "project is not configured"))
}

#[derive(Deserialize)]
struct CreateProjectRequest {
    name: String,
    path: Option<String>,
    #[serde(rename = "initGit", default)]
    init_git: bool,
}

async fn create_project(
    State(state): State<AppState>,
    Json(request): Json<CreateProjectRequest>,
) -> impl IntoResponse {
    let mut projects = state.projects.write().await;
    if projects.entries.len() >= 200 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"workspace supports up to 200 projects"})),
        );
    }
    match projects
        .add(&request.name, request.path.as_deref(), request.init_git)
        .await
    {
        Ok(project) => (StatusCode::CREATED, Json(json!({"project":project}))),
        Err(error) => (StatusCode::BAD_REQUEST, Json(json!({"error":error}))),
    }
}

async fn session_project(state: &AppState, id: &str) -> Result<Project, String> {
    let registry = state.registry.read().await;
    let project_id = registry
        .get(id)
        .and_then(|entry| entry.project_id.as_deref())
        .unwrap_or(&state.default_project_id)
        .to_owned();
    drop(registry);
    state
        .projects
        .read()
        .await
        .entries
        .iter()
        .find(|p| p.id == project_id)
        .cloned()
        .ok_or_else(|| "session project is no longer configured".into())
}

#[derive(Default, Deserialize)]
struct FileQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    staged: bool,
}
async fn project_files(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> workspace::Reply {
    workspace::files(&project_by_id(&state, &id).await?, &query.path).await
}
async fn project_file(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> workspace::Reply {
    workspace::file(&project_by_id(&state, &id).await?, &query.path).await
}
async fn project_status(State(state): State<AppState>, Path(id): Path<String>) -> workspace::Reply {
    workspace::status(&project_by_id(&state, &id).await?).await
}
async fn project_diff(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> workspace::Reply {
    workspace::diff(
        &project_by_id(&state, &id).await?,
        &query.path,
        query.staged,
    )
    .await
}

#[derive(Debug, Deserialize)]
struct AuthRefreshRequest {
    provider: String,
}

fn auth_refresh_command(
    program: &std::path::Path,
    project: &std::path::Path,
    provider: &str,
) -> Command {
    let mut command = Command::new(program);
    command
        .args(["auth", "refresh", "--provider", provider])
        .current_dir(project)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    command
}

async fn refresh_auth(
    State(state): State<AppState>,
    Json(request): Json<AuthRefreshRequest>,
) -> impl IntoResponse {
    let provider = request.provider.trim();
    if provider.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "provider is required"})),
        )
            .into_response();
    }
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        auth_refresh_command(&state.config.rpi_bin, &state.config.project, provider).status(),
    )
    .await;
    match result {
        Ok(Ok(status)) if status.success() => (
            StatusCode::OK,
            Json(json!({"provider": provider, "refreshed": true})),
        )
            .into_response(),
        Ok(Ok(status)) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": format!("OAuth refresh failed with exit status {status}")
            })),
        )
            .into_response(),
        Ok(Err(error)) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("could not start OAuth refresh: {error}")})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(json!({"error": "OAuth refresh timed out"})),
        )
            .into_response(),
    }
}

#[derive(Debug, Default, Deserialize)]
struct CreateSessionRequest {
    #[serde(rename = "projectId")]
    project_id: Option<String>,
}

async fn create_session(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let requested: CreateSessionRequest = match optional_json(&body) {
        Ok(requested) => requested,
        Err(error) => return error,
    };
    let project = match project_by_id(
        &state,
        requested
            .project_id
            .as_deref()
            .unwrap_or(&state.default_project_id),
    )
    .await
    {
        Ok(project) => project,
        Err(error) => return error,
    };
    let id = uuid::Uuid::new_v4().to_string();
    match spawn_host(&state, &id, &project).await {
        Ok(session) => {
            let ready = session.ready();
            let snapshot = session.snapshot().await;
            let saved = state.registry.write().await.upsert(RegistryEntry {
                session_id: id.clone(),
                project_id: Some(project.id.clone()),
                title: None,
                parent_session_id: None,
                fork_entry_id: None,
                relation: None,
                created_at: snapshot.created_at,
                updated_at: snapshot.updated_at,
            });
            if let Err(error) = saved {
                session.stop().await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": error})),
                );
            }
            state.sessions.write().await.insert(id.clone(), session);
            (
                StatusCode::CREATED,
                Json(
                    json!({"sessionId": id, "projectId":project.id, "ready": ready, "status": "ready"}),
                ),
            )
        }
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": error, "code": "host_start_failed"})),
        ),
    }
}

async fn spawn_host(
    state: &AppState,
    id: &str,
    project: &Project,
) -> Result<Arc<HostSession>, String> {
    HostSession::spawn(
        state.config.rpi_bin.clone(),
        project.root.clone(),
        state.config.session_dir.clone(),
        id,
        state.config.registry.with_extension("tool-timings"),
    )
    .await
}

#[derive(Debug, Default, Deserialize)]
struct ForkRequest {
    #[serde(rename = "entryId")]
    entry_id: Option<String>,
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RenameRequest {
    title: String,
}

fn validated_title(title: Option<String>) -> Result<Option<String>, (StatusCode, Json<Value>)> {
    let Some(title) = title else { return Ok(None) };
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 120 || title.chars().any(char::is_control) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "title must contain 1-120 non-control characters",
                "code": "invalid_title"
            })),
        ));
    }
    Ok(Some(title.to_owned()))
}

async fn rename_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<RenameRequest>,
) -> impl IntoResponse {
    let title = match validated_title(Some(request.title)) {
        Ok(Some(title)) => title,
        Ok(None) => unreachable!(),
        Err((status, body)) => return (status, body).into_response(),
    };
    let known = state.registry.read().await.get(&id).is_some()
        || state.sessions.read().await.contains_key(&id);
    if !known {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown session", "code": "session_not_found"})),
        )
            .into_response();
    }
    let renamed = match state.registry.write().await.rename(&id, title.clone()) {
        Ok(renamed) => renamed,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": error})),
            )
                .into_response()
        }
    };
    if !renamed {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown session", "code": "session_not_found"})),
        )
            .into_response();
    }
    (
        StatusCode::OK,
        Json(json!({"sessionId": id, "title": title})),
    )
        .into_response()
}

#[derive(Deserialize)]
struct MetadataRequest {
    pinned: Option<bool>,
    archived: Option<bool>,
}

async fn session_metadata(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<MetadataRequest>,
) -> impl IntoResponse {
    let mut registry = state.registry.write().await;
    if registry.get(&id).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"session not found"})),
        );
    }
    match registry.update_preferences(&id, request.pinned, request.archived) {
        Ok(prefs) => (
            StatusCode::OK,
            Json(json!({"sessionId":id,"pinned":prefs.pinned,"archived":prefs.archived})),
        ),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error})),
        ),
    }
}

async fn fork_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ForkRequest>,
) -> impl IntoResponse {
    fork_session_impl(&state, &id, request.entry_id, request.title).await
}

async fn clone_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let request: ForkRequest = match optional_json(&body) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    fork_session_impl(&state, &id, request.entry_id, request.title).await
}

// An absent body keeps the legacy default, but a malformed supplied body must
// never turn into a successful creation with silently discarded parameters.
fn optional_json<T: DeserializeOwned + Default>(
    body: &[u8],
) -> Result<T, (StatusCode, Json<Value>)> {
    if body.is_empty() {
        return Ok(T::default());
    }
    serde_json::from_slice(body)
        .map_err(|_| workspace::failure(StatusCode::BAD_REQUEST, "invalid JSON request body"))
}

async fn fork_session_impl(
    state: &AppState,
    source_id: &str,
    entry_id: Option<String>,
    requested_title: Option<String>,
) -> axum::response::Response {
    let _lifecycle = state.session_lifecycle.lock().await;
    if state.registry.read().await.get(source_id).is_none()
        && state.sessions.read().await.get(source_id).is_none()
    {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown source session", "code": "session_not_found"})),
        )
            .into_response();
    }
    let title = match validated_title(requested_title) {
        Ok(Some(title)) => title,
        Ok(None) => {
            if entry_id.is_some() {
                format!("Fork of {source_id}")
            } else {
                format!("Clone of {source_id}")
            }
        }
        Err((status, body)) => return (status, body).into_response(),
    };
    let requested_id = uuid::Uuid::new_v4().to_string();
    let project = match session_project(state, source_id).await {
        Ok(project) => project,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error":error}))).into_response()
        }
    };
    let session = match HostSession::spawn_fork(
        state.config.rpi_bin.clone(),
        project.root.clone(),
        state.config.session_dir.clone(),
        &requested_id,
        source_id,
        state.config.registry.with_extension("tool-timings"),
    )
    .await
    {
        Ok(session) => session,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": error, "code": "host_start_failed"})),
            )
                .into_response()
        }
    };
    let state_response = session
        .send(json!({"type": "get_state", "id": uuid::Uuid::new_v4().to_string()}))
        .await;
    let actual_id = match state_response {
        Ok(response) if response["success"] == true => {
            response["data"]["sessionId"].as_str().map(str::to_owned)
        }
        _ => None,
    };
    let Some(actual_id) = actual_id else {
        session.stop().await;
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error":"forked host did not return its session id"})),
        )
            .into_response();
    };
    if actual_id.is_empty()
        || state.registry.read().await.get(&actual_id).is_some()
        || state.sessions.read().await.contains_key(&actual_id)
    {
        session.stop().await;
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error":"forked host returned an existing or invalid session id"})),
        )
            .into_response();
    }
    if let Some(target) = entry_id.as_deref() {
        let navigation = session
            .send(json!({
                "type": "navigate",
                "id": uuid::Uuid::new_v4().to_string(),
                "targetId": target,
            }))
            .await;
        match navigation {
            Err(error) => {
                session.stop().await;
                return (StatusCode::BAD_GATEWAY, Json(json!({"error":error}))).into_response();
            }
            Ok(response) => {
                let failed = response.get("success") == Some(&Value::Bool(false))
                    || response
                        .get("data")
                        .and_then(|data| data.get("outcome"))
                        .and_then(Value::as_str)
                        == Some("failed");
                if failed {
                    let _ = session
                        .send(json!({"type": "stop", "id": uuid::Uuid::new_v4().to_string()}))
                        .await;
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "error": response
                                .get("error")
                                .and_then(Value::as_str)
                                .or_else(|| {
                                    response
                                        .get("data")
                                        .and_then(|data| data.get("error"))
                                        .and_then(Value::as_str)
                                })
                                .unwrap_or("fork target could not be reached"),
                            "code": "invalid_fork_entry"
                        })),
                    )
                        .into_response();
                }
            }
        }
    }
    let snapshot = session.snapshot().await;
    let saved = state.registry.write().await.upsert(RegistryEntry {
        session_id: actual_id.clone(),
        project_id: Some(project.id.clone()),
        title: Some(title.clone()),
        parent_session_id: Some(source_id.to_string()),
        fork_entry_id: entry_id.clone(),
        relation: Some(if entry_id.is_some() { "fork" } else { "clone" }.into()),
        created_at: snapshot.created_at,
        updated_at: snapshot.updated_at,
    });
    if let Err(error) = saved {
        session.stop().await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": error})),
        )
            .into_response();
    }
    state
        .sessions
        .write()
        .await
        .insert(actual_id.clone(), session);
    (
        StatusCode::CREATED,
        Json(json!({
            "sessionId": actual_id,
            "sourceSessionId": source_id,
            "entryId": entry_id,
            "status": "ready",
            "title": title,
        })),
    )
        .into_response()
}

/// Reopen a remembered session whose native host is not running.
async fn open_session(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    // Concurrent tabs must not spawn two native writers for the same transcript.
    let _lifecycle = state.session_lifecycle.lock().await;
    let existing = state.sessions.read().await.get(&id).cloned();
    if let Some(session) = existing {
        let snapshot = session.snapshot().await;
        if snapshot.status != "closed" {
            let entry = state.registry.read().await.get(&id).cloned();
            return (
                StatusCode::OK,
                Json(json!({
                    "sessionId": id,
                    "status": snapshot.status,
                    "title": entry.as_ref().and_then(|entry|entry.title.clone()).or(snapshot.title),
                    "createdAt": entry.as_ref().map(|entry|entry.created_at).unwrap_or(snapshot.created_at),
                    "updatedAt": snapshot.updated_at,
                    "lastSeq": snapshot.last_seq,
                    "ready": snapshot.ready,
                })),
            );
        }
        // The native child exited (crash, kill, or `stop`); drop the corpse so
        // the branch can be reopened from its persisted session file.
        state.sessions.write().await.remove(&id);
    }
    let known = state.registry.read().await.get(&id).cloned();
    let Some(mut entry) = known else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown session", "code": "session_not_found"})),
        );
    };
    let project = match session_project(&state, &id).await {
        Ok(project) => project,
        Err(error) => return (StatusCode::BAD_REQUEST, Json(json!({"error":error}))),
    };
    match spawn_host(&state, &id, &project).await {
        Ok(session) => {
            let snapshot = session.snapshot().await;
            let saved = {
                let mut registry = state.registry.write().await;
                // A rename can finish while the native process is starting.
                // Merge against the current entry, never the pre-spawn copy.
                if let Some(latest) = registry.get(&id) {
                    entry = latest.clone();
                }
                registry.upsert(RegistryEntry {
                    session_id: id.clone(),
                    project_id: Some(project.id.clone()),
                    title: entry.title.clone(),
                    parent_session_id: entry.parent_session_id.clone(),
                    fork_entry_id: entry.fork_entry_id.clone(),
                    relation: entry.relation.clone(),
                    created_at: entry.created_at,
                    updated_at: snapshot.updated_at,
                })
            };
            if let Err(error) = saved {
                session.stop().await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": error})),
                );
            }
            state.sessions.write().await.insert(id.clone(), session);
            (
                StatusCode::OK,
                Json(json!({
                    "sessionId": id,
                    "status": snapshot.status,
                    "title": entry.title,
                    "createdAt": entry.created_at,
                    "updatedAt": snapshot.updated_at,
                    "lastSeq": snapshot.last_seq,
                    "ready": snapshot.ready,
                })),
            )
        }
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": error, "code": "host_start_failed"})),
        ),
    }
}

async fn list_sessions(State(state): State<AppState>) -> impl IntoResponse {
    let sessions = state.sessions.read().await;
    let registry = state.registry.read().await;
    let mut summaries = Vec::new();
    for entry in registry.entries() {
        let snapshot = match sessions.get(&entry.session_id) {
            Some(session) => Some(session.snapshot().await),
            None => None,
        };
        summaries.push(json!({
            "projectId": entry.project_id.as_deref().unwrap_or(&state.default_project_id),
            "pinned":registry.preferences(&entry.session_id).pinned,
            "archived":registry.preferences(&entry.session_id).archived,
            "sessionId": entry.session_id,
            "title": entry.title.clone().or_else(||snapshot.as_ref().and_then(|snapshot|snapshot.title.clone())),
            "status": snapshot
                .as_ref()
                .map(|snapshot| snapshot.status.clone())
                .unwrap_or_else(|| "offline".into()),
            "createdAt": entry.created_at,
            "updatedAt": snapshot
                .as_ref()
                .map(|snapshot| snapshot.updated_at.max(entry.updated_at))
                .unwrap_or(entry.updated_at),
            "lastSeq": snapshot
                .as_ref()
                .map(|snapshot| snapshot.last_seq)
                .unwrap_or(0),
            "parentSessionId": entry.parent_session_id,
            "forkEntryId": entry.fork_entry_id,
            "relation": entry.relation,
            "error": snapshot.as_ref().and_then(|snapshot| snapshot.last_error.clone()),
            "latestSettlement": snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.latest_settlement.clone()),
        }));
    }
    summaries.sort_by_key(|summary| {
        std::cmp::Reverse(summary["updatedAt"].as_u64().unwrap_or_default())
    });
    Json(json!({"sessions": summaries}))
}

async fn get_session(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let session = state.sessions.read().await.get(&id).cloned();
    let Some(session) = session else {
        let entry = state.registry.read().await.get(&id).cloned();
        let Some(entry) = entry else {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"session not found"})),
            );
        };
        return (
            StatusCode::OK,
            Json(json!({
                "sessionId": id,
                "status": "offline",
                "title": entry.title,
                "createdAt": entry.created_at,
                "updatedAt": entry.updated_at,
                "lastSeq": 0,
                "parentSessionId": entry.parent_session_id,
                "forkEntryId": entry.fork_entry_id,
                "relation": entry.relation,
                "error": null,
            })),
        );
    };
    let snapshot = session.snapshot().await;
    let entry = state.registry.read().await.get(&id).cloned();
    (
        StatusCode::OK,
        Json(json!({
            "sessionId": id,
            "ready": snapshot.ready,
            "status": snapshot.status,
            "title": entry.as_ref().and_then(|entry| entry.title.clone()).or(snapshot.title),
            "createdAt": entry.as_ref().map(|entry| entry.created_at).unwrap_or(snapshot.created_at),
            "updatedAt": entry.as_ref().map(|entry| entry.updated_at.max(snapshot.updated_at)).unwrap_or(snapshot.updated_at),
            "lastSeq": snapshot.last_seq,
            "parentSessionId": state
                .registry
                .read()
                .await
                .get(&id)
                .and_then(|entry| entry.parent_session_id.clone()),
            "forkEntryId": state
                .registry
                .read()
                .await
                .get(&id)
                .and_then(|entry| entry.fork_entry_id.clone()),
            "relation": state
                .registry
                .read()
                .await
                .get(&id)
                .and_then(|entry| entry.relation.clone()),
            "error": snapshot.last_error,
            "latestSettlement": snapshot.latest_settlement,
        })),
    )
}

async fn delete_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let _lifecycle = state.session_lifecycle.lock().await;
    let forgotten = match state.registry.write().await.remove(&id) {
        Ok(forgotten) => forgotten,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": error})),
            )
                .into_response()
        }
    };
    let removed = state.sessions.write().await.remove(&id);
    match removed {
        Some(session) => {
            session.stop().await;
            StatusCode::NO_CONTENT.into_response()
        }
        None if forgotten => StatusCode::NO_CONTENT.into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn command(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(command): Json<ApiCommand>,
) -> impl IntoResponse {
    if !protocol::SUPPORTED_COMMANDS.contains(&command.kind.as_str()) {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({
                "type": "response",
                "command": command.kind,
                "success": false,
                "error": "unsupported command"
            })),
        );
    }
    let session = state.sessions.read().await.get(&id).cloned();
    let Some(session) = session else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"session not found"})),
        );
    };
    if command.kind == "prompt" {
        if let Some(message) = command.payload.get("message").and_then(Value::as_str) {
            let title = summarize(message);
            let mut registry = state.registry.write().await;
            if let Some(mut entry) = registry
                .get(&id)
                .filter(|entry| entry.title.is_none())
                .cloned()
            {
                entry.title = Some(title.clone());
                entry.updated_at = session.snapshot().await.updated_at;
                // Persist before accepting a prompt. An I/O failure must not return a
                // failed request after the provider has already begun spending tokens.
                if let Err(error) = registry.upsert(entry) {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error":error})),
                    );
                }
                session.set_title_if_empty(&title).await;
            }
        }
    }
    let request_id = protocol::command_id(&command);
    let mut value = serde_json::to_value(&command).unwrap_or_else(|_| json!({}));
    value["id"] = request_id.clone();
    let result = session.send(value).await;
    match result {
        Ok(response) => (StatusCode::OK, Json(response)),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(protocol::response(
                Some(request_id),
                &command.kind,
                Err(error),
            )),
        ),
    }
}

/// A read-only transcript snapshot plus the events that landed while it was
/// being taken, so a client can render history and then follow `?after=` live
/// without a gap or an overlap.
async fn snapshot(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let session = state.sessions.read().await.get(&id).cloned();
    let Some(session) = session else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"session not found"})),
        )
            .into_response();
    };
    let host_snapshot = session.snapshot().await;
    let start = host_snapshot.last_seq;
    let request_id = json!(uuid::Uuid::new_v4().to_string());
    let response = session
        .send(json!({"type": "get_messages", "id": request_id}))
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": error, "code": "host_request_failed"})),
            )
                .into_response();
        }
    };
    if response["success"] != Value::Bool(true) {
        let error = response["error"]
            .as_str()
            .unwrap_or("get_messages is not supported by this host");
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({"error": error, "code": "snapshot_unsupported"})),
        )
            .into_response();
    }
    let data = &response["data"];
    let history = session
        .send(json!({"type": "get_history", "id": uuid::Uuid::new_v4().to_string(), "limit": 100}))
        .await
        .ok()
        .filter(|value| value["success"] == Value::Bool(true))
        .and_then(|value| value.get("data").cloned())
        .unwrap_or_else(|| json!({"records": [], "hasMore": false}));
    let latest_settlement = host_snapshot.latest_settlement.clone().or_else(|| {
        history
            .get("records")
            .and_then(Value::as_array)
            .and_then(|records| {
                records
                    .iter()
                    .rev()
                    .find(|record| {
                        record.get("type").and_then(Value::as_str) == Some("operation_finished")
                    })
                    .cloned()
            })
    });
    let events = session.history(Some(start)).await;
    let last_seq = events.last().map(|event| event.seq).unwrap_or(start);
    if replay_gap(Some(start), events.first().map(|event| event.seq), last_seq) {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error":"events changed too quickly while loading the snapshot; retry loading",
                "code":"snapshot_events_evicted"
            })),
        )
            .into_response();
    }
    let events = events
        .iter()
        .map(|event| {
            let mut value = event.value.clone();
            crate::timing::refresh_event(&mut value);
            json!({"seq": event.seq, "event": value})
        })
        .collect::<Vec<_>>();
    Json(json!({
        "sessionId": id,
        "messages": data["messages"].clone(),
        "status": host_snapshot.status,
        "total": data["total"].clone(),
        "truncated": data["truncated"].clone(),
        "firstId": data["firstId"].clone(),
        "lastId": data["lastId"].clone(),
        "hasMore": data["hasMore"].clone(),
        "leafId": data["leafId"].clone(),
        "pendingResume": data["pendingResume"].clone(),
        "latestSettlement": latest_settlement,
        "pendingDialog": host_snapshot.pending_dialog,
        "pendingDialogs": host_snapshot.pending_dialogs,
        "history": history,
        "lastSeq": last_seq,
        "events": events,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
struct EventQuery {
    after: Option<u64>,
}

async fn events(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: axum::http::HeaderMap,
    Query(query): Query<EventQuery>,
) -> axum::response::Response {
    let session = state.sessions.read().await.get(&id).cloned();
    let Some(session) = session else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"session not found"})),
        )
            .into_response();
    };
    let ready = session.ready();
    // Subscribe before reading history so nothing emitted in between is lost.
    let receiver = session.subscribe();
    let after = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .or(query.after);
    let retained = session.history(None).await;
    let latest = session.snapshot().await.last_seq;
    let gap = replay_gap(after, retained.first().map(|e| e.seq), latest);
    let replay = retained
        .into_iter()
        .filter(|event| after.map_or(true, |seq| event.seq > seq))
        .collect();
    let mut shutdown = state.shutdown;
    Sse::new(
        authorized_events(
            event_stream(ready, replay, receiver, gap, after),
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

fn replay_gap(after: Option<u64>, first: Option<u64>, latest: u64) -> bool {
    after.is_some_and(|after| {
        after > latest || first.is_some_and(|first| after.saturating_add(1) < first)
    })
}

fn event_stream(
    ready: Value,
    replay: Vec<BufferedEvent>,
    receiver: broadcast::Receiver<BufferedEvent>,
    gap: bool,
    after: Option<u64>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let cursor = replay
        .last()
        .map(|event| event.seq)
        .unwrap_or_default()
        .max(after.unwrap_or_default());
    let head: Vec<Result<Event, Infallible>> =
        std::iter::once(Ok(Event::default().event("ready").data(ready.to_string())))
            .chain(gap.then(|| {
                Ok(Event::default()
                    .event("resync_required")
                    .data("{\"type\":\"resync_required\"}"))
            }))
            .chain(replay.into_iter().map(|event| Ok(to_sse(event))))
            .collect();
    stream::iter(head).chain(stream::unfold(
        (receiver, cursor),
        |(mut receiver, cursor)| async move {
            loop {
                match receiver.recv().await {
                    Ok(event) if event.seq <= cursor => continue,
                    Ok(event) => {
                        let cursor = event.seq;
                        return Some((Ok(to_sse(event)), (receiver, cursor)));
                    }
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        let data = json!({
                            "type": "resync_required",
                            "code": "event_stream_lagged",
                            "dropped": count,
                        });
                        let event = Event::default()
                            .event("resync_required")
                            .data(data.to_string());
                        return Some((Ok(event), (receiver, cursor)));
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    ))
}

// The middleware checks only the initial HTTP request. A long-lived stream must
// also stop after another tab logs out or the authentication session expires.
fn authorized_events(
    events: impl Stream<Item = Result<Event, Infallible>> + Send + 'static,
    security: Arc<Security>,
    headers: axum::http::HeaderMap,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let expiry_check = tokio::time::interval(std::time::Duration::from_secs(10));
    stream::unfold(
        (Box::pin(events), expiry_check),
        move |(mut events, mut expiry_check)| {
            let security = security.clone();
            let headers = headers.clone();
            async move {
                loop {
                    tokio::select! {
                        event = events.next() => {
                            let event = event?;
                            if !security.authorized(&headers).await { return None; }
                            return Some((event, (events, expiry_check)));
                        }
                        _ = expiry_check.tick() => {
                            if !security.authorized(&headers).await { return None; }
                        }
                    }
                }
            }
        },
    )
}

fn to_sse(mut event: BufferedEvent) -> Event {
    crate::timing::refresh_event(&mut event.value);
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

/// Compact a prompt into a sidebar title.
fn summarize(message: &str) -> String {
    let line = message.lines().find(|line| !line.trim().is_empty());
    let text = line.unwrap_or(message).trim();
    let mut title: String = text.chars().take(60).collect();
    if text.chars().count() > 60 {
        title.push('…');
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_auth_refresh_terminates_its_native_process() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("rpi-auth-cancel-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let host = root.join("host");
        std::fs::write(&host, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut child = auth_refresh_command(&host, &root, "test").spawn().unwrap();
        let pid = child.id().unwrap().to_string();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), async move {
                child.wait().await
            })
            .await
            .is_err()
        );
        let mut terminated = false;
        for _ in 0..100 {
            let alive = Command::new("kill")
                .args(["-0", &pid])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .unwrap()
                .success();
            if !alive {
                terminated = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        if !terminated {
            let _ = Command::new("kill").args(["-KILL", &pid]).status().await;
        }
        std::fs::remove_dir_all(root).unwrap();
        assert!(terminated, "timed-out refresh must not survive the request");
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

    #[test]
    fn event_replay_detects_eviction_and_restarted_sequence() {
        assert!(replay_gap(Some(10), Some(100), 200));
        assert!(replay_gap(Some(100), Some(1), 20));
        assert!(!replay_gap(Some(99), Some(100), 200));
        assert!(!replay_gap(Some(200), Some(100), 200));
    }

    #[test]
    fn title_uses_first_non_empty_line() {
        assert_eq!(summarize("\n\n  Fix the parser  \nmore"), "Fix the parser");
    }

    #[test]
    fn title_is_truncated() {
        let title = summarize(&"x".repeat(120));
        assert_eq!(title.chars().count(), 61);
        assert!(title.ends_with('…'));
    }
}
