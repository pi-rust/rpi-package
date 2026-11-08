//! Session and workspace operations shared by protocol adapters.
pub use crate::run_timing::RunTimingsQuery;
mod message_queries;
mod tool_program_access;
mod tool_program_branch;
pub mod tool_program_events;
pub mod tool_program_live;
pub use tool_program_live::{child_subscription, child_subscription_for_host};
pub mod tool_program_queries;
pub mod tool_program_snapshot;
pub use tool_program_queries::{
    child_body, child_body_for_host, child_phases, child_phases_for_host, child_summaries,
    child_summaries_for_host,
};
pub use tool_program_snapshot::{
    child_snapshot, child_snapshot_for_host, ChildSnapshot, ChildSnapshotQuery,
};
mod resources;
mod retirement;
#[cfg(test)]
#[path = "service_retirement_tests.rs"]
mod service_retirement_tests;
use crate::{
    host::HostSession,
    projects::Projects,
    protocol::{self, ApiCommand},
    registry::{Registry, RegistryEntry},
    reply::{IntoReply, ReplyKind, ServiceReply, ServiceResult},
    workspace::{self, Project},
};
pub use message_queries::{
    message_summaries, message_window, MessageSummary, MessageSummaryPage, MessageSummaryQuery,
    MessageWindowPage, MessageWindowQuery,
};
pub use resources::ResourceProjectFence;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    process::Command,
    sync::{Mutex, RwLock},
};
/// Query persisted native run elapsed metadata without starting a native child.
/// A conflict invalidates the client's accumulated timings and cursor.
pub async fn run_timings(state: &Service, id: String, query: RunTimingsQuery) -> ServiceReply {
    let _lifecycle = state.session_lifecycle.lock().await;
    if state.registry.read().await.deletion(&id).is_some() {
        return retirement::deleting_error().into_reply();
    }
    if uuid::Uuid::parse_str(&id).is_err() {
        return (
            ReplyKind::InvalidInput,
            json!({"error":"session ID must be a UUID"}),
        )
            .into_reply();
    }
    if let Err(error) = query.validate() {
        return error.into_reply();
    }
    if state.registry.read().await.get(&id).is_none()
        && !state.sessions.read().await.contains_key(&id)
    {
        return (ReplyKind::NotFound, json!({"error":"unknown session"})).into_reply();
    }
    let project = match session_project(state, &id).await {
        Ok(project) => project,
        Err(error) => return (ReplyKind::Unavailable, json!({"error":error})).into_reply(),
    };
    let configured = state.config.session_dir.clone();
    let cache = state.run_timing_cache.clone();
    let permit = match state.run_timing_workers.clone().acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => {
            return (
                ReplyKind::Unavailable,
                json!({"error":"timing reader closed"}),
            )
                .into_reply()
        }
    };
    match tokio::task::spawn_blocking(move || {
        // Both the permit and cache serialization remain owned by the worker even
        // when the async request is cancelled.
        let _permit = permit;
        let root = crate::run_timing::session_root(&project.root, configured)?;
        let mut cache = cache
            .lock()
            .map_err(|_| crate::reply::failure(ReplyKind::Internal, "timing cache poisoned"))?;
        let page = cache.query(&id, &root, &query)?;
        serde_json::to_value(page)
            .map_err(|_| crate::reply::failure(ReplyKind::Internal, "timing serialization failed"))
    })
    .await
    {
        Ok(result) => result.into_reply(),
        Err(_) => (ReplyKind::Internal, json!({"error":"timing reader failed"})).into_reply(),
    }
}

#[derive(Clone, Debug)]
pub struct BackendConfig {
    pub project: PathBuf,
    /// Additional workspace roots explicitly made available to clients.
    pub projects: Vec<PathBuf>,
    pub projects_root: Option<PathBuf>,
    pub rpi_bin: PathBuf,
    /// Optional `--session-dir` forwarded to the native host.
    pub session_dir: Option<PathBuf>,
    /// Where the bridge remembers session ids across restarts.
    pub registry: PathBuf,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            project: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            projects: Vec::new(),
            projects_root: None,
            rpi_bin: PathBuf::from("rpi"),
            session_dir: None,
            registry: crate::registry::default_path(),
        }
    }
}

#[derive(Clone)]
pub struct Service {
    pub config: BackendConfig,
    projects: Arc<RwLock<Projects>>,
    default_project_id: String,
    sessions: Arc<RwLock<HashMap<String, Arc<HostSession>>>>,
    /// Monotonic Web/API activity used only for optional idle host retirement.
    /// Native transcript timestamps remain the source of truth for history.
    session_activity: Arc<RwLock<HashMap<String, Instant>>>,
    session_lifecycle: Arc<Mutex<()>>,
    registry: Arc<RwLock<Registry>>,
    run_timing_cache: Arc<std::sync::Mutex<crate::run_timing::RunTimingCache>>,
    run_timing_workers: Arc<tokio::sync::Semaphore>,
    resource_workers: Arc<tokio::sync::Semaphore>,
    retirement_workers: Arc<tokio::sync::Semaphore>,
    closing: Arc<std::sync::atomic::AtomicBool>,
    resource_storage_failed: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}
impl Service {
    pub fn new(
        config: BackendConfig,
    ) -> Result<Arc<Self>, Box<dyn std::error::Error + Send + Sync>> {
        let mut projects = vec![Project::new(&config.project)?];
        for path in &config.projects {
            let project = Project::new(path)?;
            if !projects.iter().any(|p| p.id == project.id) {
                projects.push(project);
            }
        }
        let default_project_id = projects[0].id.clone();
        let root = config.projects_root.clone().unwrap_or_else(|| {
            config
                .registry
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join("projects")
        });
        let projects = Projects::load(
            projects,
            root,
            config.registry.with_file_name("projects.json"),
        )?;
        let registry = Registry::load(config.registry.clone())?;
        Ok(Arc::new(Self {
            config,
            projects: Arc::new(RwLock::new(projects)),
            default_project_id,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            session_activity: Arc::new(RwLock::new(HashMap::new())),
            session_lifecycle: Arc::new(Mutex::new(())),
            registry: Arc::new(RwLock::new(registry)),
            run_timing_cache: Arc::new(std::sync::Mutex::new(Default::default())),
            run_timing_workers: Arc::new(tokio::sync::Semaphore::new(2)),
            resource_workers: Arc::new(tokio::sync::Semaphore::new(2)),
            retirement_workers: Arc::new(tokio::sync::Semaphore::new(16)),
            closing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            resource_storage_failed: Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
        }))
    }
    pub async fn live_session(&self, id: &str) -> Option<Arc<HostSession>> {
        self.sessions.read().await.get(id).cloned()
    }

    /// Mark a live session as recently used by a Web/API client. This is
    /// deliberately separate from `live_session`: internal probes and the
    /// reaper must not keep a session alive just by inspecting it.
    pub async fn touch_session(&self, id: &str) {
        if self.sessions.read().await.contains_key(id) {
            self.session_activity
                .write()
                .await
                .insert(id.to_owned(), Instant::now());
        }
    }

    /// Start an optional service-owned reaper for idle native hosts. Closing a
    /// host preserves its registry entry and native transcript; the next Web
    /// open can restore it.
    pub fn spawn_idle_reaper(self: &Arc<Self>, timeout: Duration) -> tokio::task::JoinHandle<()> {
        let state = Arc::clone(self);
        tokio::spawn(async move {
            let interval = timeout
                .min(Duration::from_secs(60))
                .max(Duration::from_secs(1));
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if state.closing.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                state.reap_idle_sessions(timeout).await;
            }
        })
    }

    /// Run one bounded idle-retirement pass. Exposed for deterministic tests;
    /// the Web/HTTP adapter normally calls this through `spawn_idle_reaper`.
    pub async fn reap_idle_sessions(&self, timeout: Duration) {
        retirement::reap_idle(self, timeout).await;
    }

    pub async fn subscribe_events(
        &self,
        id: &str,
        after: Option<u64>,
    ) -> Option<crate::events::Subscription> {
        let session = self.live_session(id).await?;
        Some(crate::events::Subscription::new(&session, after).await)
    }
    pub async fn shutdown(&self) {
        self.closing
            .store(true, std::sync::atomic::Ordering::Release);
        let _lifecycle = self.session_lifecycle.lock().await;
        let sessions: Vec<_> = self
            .sessions
            .read()
            .await
            .iter()
            .map(|(id, session)| (id.clone(), session.clone()))
            .collect();
        futures_util::future::join_all(sessions.iter().map(|(id, session)| async move {
            if let Ok(fence) = session.resource_fence() {
                // Native runtime setters are not persisted. Only a fresh idle
                // owned-stop capture may be replayed on a later ordinary open.
                let _ = resources::capture_idle_settings_for_stop(self, id, session, &fence).await;
                session.retire_resources(&fence);
            }
            session.shutdown().await;
        }))
        .await;
    }
}

pub async fn list_projects(state: &Service) -> Value {
    let projects = state.projects.read().await;

    json!({"projects":projects.entries, "defaultProjectId":state.default_project_id,"projectsRoot":projects.root.to_string_lossy().trim_start_matches(r"\\?\")})
}

#[derive(Debug, Default, Deserialize)]
pub struct ProjectFoldersQuery {
    pub path: Option<String>,
}

pub async fn list_project_folders(state: &Service, query: ProjectFoldersQuery) -> workspace::Reply {
    let root = state.projects.read().await.root.clone();
    let relative = query.path.as_deref().unwrap_or("");
    let folders = workspace::child_directories(&root, relative).await?;
    Ok(json!({
        "root": root.to_string_lossy().trim_start_matches(r"\\?\"),
        "path": relative,
        "folders": folders
            .into_iter()
            .map(|(name, path)| json!({"name": name, "path": path}))
            .collect::<Vec<_>>(),
    }))
}

async fn project_by_id(state: &Service, id: &str) -> Result<Project, (ReplyKind, Value)> {
    state
        .projects
        .read()
        .await
        .entries
        .iter()
        .find(|p| p.id == id)
        .cloned()
        .ok_or_else(|| workspace::failure(ReplyKind::NotFound, "project is not configured"))
}

#[derive(Deserialize)]
pub struct CreateProjectRequest {
    pub name: String,
    pub path: Option<String>,
    #[serde(rename = "initGit", default)]
    pub init_git: bool,
}

pub async fn create_project(state: &Service, request: CreateProjectRequest) -> (ReplyKind, Value) {
    let mut projects = state.projects.write().await;
    if projects.entries.len() >= 200 {
        return (
            ReplyKind::InvalidInput,
            json!({"error":"workspace supports up to 200 projects"}),
        );
    }
    match projects
        .add(&request.name, request.path.as_deref(), request.init_git)
        .await
    {
        Ok(project) => (ReplyKind::Created, json!({"project":project})),
        Err(error) => (ReplyKind::InvalidInput, json!({"error":error})),
    }
}

#[derive(Debug, Deserialize)]
pub struct RenameProjectRequest {
    pub name: String,
}

pub async fn rename_project(
    state: &Service,
    id: String,
    request: RenameProjectRequest,
) -> (ReplyKind, Value) {
    let mut projects = state.projects.write().await;
    match projects.rename(&id, &request.name).await {
        Ok(project) => (ReplyKind::Success, json!({"project": project})),
        Err(error) => (ReplyKind::InvalidInput, json!({"error": error})),
    }
}

pub async fn project_metadata(
    state: &Service,
    id: String,
    request: ProjectMetadataRequest,
) -> (ReplyKind, Value) {
    let mut projects = state.projects.write().await;
    match projects
        .set_archived(&id, request.archived.unwrap_or(false))
        .await
    {
        Ok(project) => (ReplyKind::Success, json!({"project": project})),
        Err(error) => (ReplyKind::InvalidInput, json!({"error": error})),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ProjectMetadataRequest {
    pub archived: Option<bool>,
}

async fn session_project(state: &Service, id: &str) -> Result<Project, String> {
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

pub async fn upload_project_file(
    state: &Service,
    id: String,
    filename: String,
    bytes: Vec<u8>,
) -> workspace::Reply {
    workspace::upload(&project_by_id(state, &id).await?, &filename, &bytes).await
}

#[derive(Default, Deserialize)]
pub struct FileQuery {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub staged: bool,
}
pub async fn project_files(state: &Service, id: String, query: FileQuery) -> workspace::Reply {
    workspace::files(&project_by_id(state, &id).await?, &query.path).await
}
pub async fn project_file(state: &Service, id: String, query: FileQuery) -> workspace::Reply {
    workspace::file(&project_by_id(state, &id).await?, &query.path).await
}
pub async fn project_image(
    state: &Service,
    id: String,
    query: FileQuery,
) -> Result<workspace::ProjectImage, (ReplyKind, Value)> {
    workspace::image(&project_by_id(state, &id).await?, &query.path).await
}
pub async fn project_status(state: &Service, id: String) -> workspace::Reply {
    workspace::status(&project_by_id(state, &id).await?).await
}
pub async fn project_diff(state: &Service, id: String, query: FileQuery) -> workspace::Reply {
    workspace::diff(&project_by_id(state, &id).await?, &query.path, query.staged).await
}

#[derive(Debug, Deserialize)]
pub struct AuthRefreshRequest {
    pub provider: String,
}

fn auth_refresh_command(
    program: &std::path::Path,
    project: &std::path::Path,
    provider: &str,
) -> Command {
    let mut command = crate::process::background_command(program);
    command
        .args(["auth", "refresh", "--provider", provider])
        .current_dir(project)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

pub async fn refresh_auth(state: &Service, request: AuthRefreshRequest) -> ServiceReply {
    let provider = request.provider.trim();
    if provider.is_empty() {
        return (
            ReplyKind::InvalidInput,
            json!({"error": "provider is required"}),
        )
            .into_reply();
    }
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        auth_refresh_command(&state.config.rpi_bin, &state.config.project, provider).status(),
    )
    .await;
    match result {
        Ok(Ok(status)) if status.success() => (
            ReplyKind::Success,
            json!({"provider": provider, "refreshed": true}),
        )
            .into_reply(),
        Ok(Ok(status)) => (
            ReplyKind::HostUnavailable,
            json!({
                "error": format!("OAuth refresh failed with exit status {status}")
            }),
        )
            .into_reply(),
        Ok(Err(error)) => (
            ReplyKind::HostUnavailable,
            json!({"error": format!("could not start OAuth refresh: {error}")}),
        )
            .into_reply(),
        Err(_) => (
            ReplyKind::Timeout,
            json!({"error": "OAuth refresh timed out"}),
        )
            .into_reply(),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct CreateSessionRequest {
    #[serde(rename = "projectId")]
    pub project_id: Option<String>,
    /// Omitted is legacy; an empty exact declaration is not a fallback.
    #[serde(default)]
    pub resources: Option<crate::resources::ResourceSelection>,
}

pub async fn create_session(
    state: &Service,
    requested: CreateSessionRequest,
) -> (ReplyKind, Value) {
    let owner = state.clone();
    tokio::spawn(async move { create_session_owned(&owner, requested).await })
        .await
        .unwrap_or_else(|_| {
            (
                ReplyKind::Internal,
                json!({"error":"session creation owner failed"}),
            )
        })
}

async fn create_session_owned(
    state: &Service,
    requested: CreateSessionRequest,
) -> (ReplyKind, Value) {
    let _lifecycle = state.session_lifecycle.lock().await;
    if state.closing.load(std::sync::atomic::Ordering::Acquire) {
        return (
            ReplyKind::Unavailable,
            json!({"error":"service shutting down"}),
        );
    }
    let project = match project_by_id(
        state,
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
    if let Some(selection) = requested.resources {
        if let Err(error) = resources::prepare_create(state, &id, &project, selection).await {
            return resources::reply_error(error);
        }
    }
    match spawn_host(state, &id, &project).await {
        Ok((session, record)) => {
            let ready = session.ready();
            let snapshot = session.snapshot().await;
            let saved = resources::publish_record(
                state,
                RegistryEntry {
                    session_id: id.clone(),
                    project_id: Some(project.id.clone()),
                    title: None,
                    parent_session_id: None,
                    fork_entry_id: None,
                    relation: None,
                    created_at: snapshot.created_at,
                    updated_at: snapshot.updated_at,
                },
                record,
            )
            .await;
            if let Err(error) = saved {
                session.shutdown().await;
                return resources::reply_error(error);
            }
            state.sessions.write().await.insert(id.clone(), session);
            state.touch_session(&id).await;
            (
                ReplyKind::Created,
                json!({"sessionId": id, "projectId":project.id, "ready": ready, "status": "ready"}),
            )
        }
        Err(error) => (
            ReplyKind::HostUnavailable,
            json!({"error": error, "code": "host_start_failed"}),
        ),
    }
}

async fn spawn_host(
    state: &Service,
    id: &str,
    project: &Project,
) -> Result<
    (
        Arc<HostSession>,
        Option<crate::resources::ResourceConfiguration>,
    ),
    String,
> {
    let options = resources::host_options(state, id, project)
        .await
        .map_err(|e| e.to_string())?;
    let session = HostSession::spawn_options(options).await?;
    match resources::prepare_open_observation(state, id, &session).await {
        Ok(record) => Ok((session, record)),
        Err(error) => {
            session.shutdown().await;
            Err(error.to_string())
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ForkRequest {
    #[serde(rename = "entryId")]
    pub entry_id: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RenameRequest {
    pub title: String,
}

fn validated_title(title: Option<String>) -> Result<Option<String>, (ReplyKind, Value)> {
    let Some(title) = title else { return Ok(None) };
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 120 || title.chars().any(char::is_control) {
        return Err((
            ReplyKind::InvalidInput,
            json!({
                "error": "title must contain 1-120 non-control characters",
                "code": "invalid_title"
            }),
        ));
    }
    Ok(Some(title.to_owned()))
}

pub async fn rename_session(state: &Service, id: String, request: RenameRequest) -> ServiceReply {
    let _lifecycle = state.session_lifecycle.lock().await;
    if state.registry.read().await.deletion(&id).is_some() {
        return retirement::deleting_error().into_reply();
    }
    let title = match validated_title(Some(request.title)) {
        Ok(Some(title)) => title,
        Ok(None) => unreachable!(),
        Err((status, body)) => return (status, body).into_reply(),
    };
    let known = state.registry.read().await.get(&id).is_some()
        || state.sessions.read().await.contains_key(&id);
    if !known {
        return (
            ReplyKind::NotFound,
            json!({"error": "unknown session", "code": "session_not_found"}),
        )
            .into_reply();
    }
    let renamed = match state.registry.write().await.rename(&id, title.clone()) {
        Ok(renamed) => renamed,
        Err(error) => return (ReplyKind::Internal, json!({"error": error})).into_reply(),
    };
    if !renamed {
        return (
            ReplyKind::NotFound,
            json!({"error": "unknown session", "code": "session_not_found"}),
        )
            .into_reply();
    }
    (ReplyKind::Success, json!({"sessionId": id, "title": title})).into_reply()
}

#[derive(Deserialize)]
pub struct MetadataRequest {
    pub pinned: Option<bool>,
    pub archived: Option<bool>,
    pub tags: Option<Vec<String>>,
}

pub async fn session_metadata(
    state: &Service,
    id: String,
    request: MetadataRequest,
) -> (ReplyKind, Value) {
    retirement::metadata(state, id, request).await
}

pub async fn fork_session(state: &Service, id: String, request: ForkRequest) -> ServiceReply {
    fork_session_impl(state, &id, request.entry_id, request.title).await
}

pub async fn clone_session(state: &Service, id: String, request: ForkRequest) -> ServiceReply {
    fork_session_impl(state, &id, request.entry_id, request.title).await
}

async fn fork_session_impl(
    state: &Service,
    source_id: &str,
    entry_id: Option<String>,
    requested_title: Option<String>,
) -> ServiceReply {
    let permit = match state.resource_workers.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return (
                ReplyKind::Conflict,
                json!({"error":"fork operation at capacity"}),
            )
                .into_reply()
        }
    };
    let owner = state.clone();
    let source_id = source_id.to_owned();
    tokio::spawn(async move {
        let _permit = permit;
        fork_session_owned(&owner, &source_id, entry_id, requested_title).await
    })
    .await
    .unwrap_or_else(|_| (ReplyKind::Internal, json!({"error":"fork owner failed"})).into_reply())
}
async fn fork_session_owned(
    state: &Service,
    source_id: &str,
    entry_id: Option<String>,
    requested_title: Option<String>,
) -> ServiceReply {
    let _lifecycle = state.session_lifecycle.lock().await;
    if state.closing.load(std::sync::atomic::Ordering::Acquire) {
        return (
            ReplyKind::Unavailable,
            json!({"error":"service shutting down"}),
        )
            .into_reply();
    }
    if let Err(error) = state.check_session_admission(source_id).await {
        return error.into_reply();
    }
    if let Err(error) = state.check_resource_storage(source_id) {
        return resources::reply_error(error).into_reply();
    }
    if state.registry.read().await.get(source_id).is_none()
        && state.sessions.read().await.get(source_id).is_none()
    {
        return (
            ReplyKind::NotFound,
            json!({"error": "unknown source session", "code": "session_not_found"}),
        )
            .into_reply();
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
        Err((status, body)) => return (status, body).into_reply(),
    };
    let requested_id = uuid::Uuid::new_v4().to_string();
    let project = match session_project(state, source_id).await {
        Ok(project) => project,
        Err(error) => return (ReplyKind::InvalidInput, json!({"error":error})).into_reply(),
    };
    let mut fork_options = match resources::host_options(state, source_id, &project).await {
        Ok(options) => options,
        Err(error) => return resources::reply_error(error).into_reply(),
    };
    // Retain source admission through destination completion. Runtime setters
    // are not native transcript settings and must be captured from the live host.
    let source = state.live_session(source_id).await;
    let mut _source_fence = None;
    let mut live_settings = None;
    if let Some(source) = source.as_ref().filter(|_| fork_options.resources.is_some()) {
        let fence = match source.resource_fence() {
            Ok(fence) => fence,
            Err(error) => return resources::reply_error(error).into_reply(),
        };
        if source.snapshot().await.status != "closed" {
            let captured = async {
                let native = source.verify_resource_idle(&fence).await?;
                let observation = source.fenced_resource_state(&fence).await?;
                if let Some(crate::resources::ResourceStartup::Exact(selection)) =
                    &fork_options.resources
                {
                    observation.verify(source_id, selection)?;
                }
                resources::capture_settings(&source.launch_options()?, &native)
            }
            .await;
            let settings = match captured {
                Ok(settings) => settings,
                Err(error) => return resources::reply_error(error).into_reply(),
            };
            fork_options.model = settings.model.clone();
            fork_options.provider = settings.provider.clone();
            fork_options.permission_gate = settings.permission_gate;
            fork_options.bypass = settings.bypass;
            live_settings = Some(settings);
        }
        _source_fence = Some(fence);
    }
    fork_options.session_id = requested_id.clone();
    fork_options.fork_source = Some(source_id.to_owned());
    let session = match HostSession::spawn_options(fork_options).await {
        Ok(session) => session,
        Err(error) => {
            return (
                ReplyKind::HostUnavailable,
                json!({"error": error, "code": "host_start_failed"}),
            )
                .into_reply()
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
        session.shutdown().await;
        return (
            ReplyKind::HostUnavailable,
            json!({"error":"forked host did not return its session id"}),
        )
            .into_reply();
    };
    if !tool_program_queries::valid_id(&actual_id)
        || state.registry.read().await.get(&actual_id).is_some()
        || state.sessions.read().await.contains_key(&actual_id)
    {
        session.shutdown().await;
        return (
            ReplyKind::HostUnavailable,
            json!({"error":"forked host returned an existing or invalid session id"}),
        )
            .into_reply();
    }
    // Native --fork does not consume requested_id. Bind its validated readback
    // once, before publishing this owner under the actual destination ID.
    if session.finalize_fork_destination(&actual_id).is_err() {
        session.shutdown().await;
        return (
            ReplyKind::HostUnavailable,
            json!({"error":"forked host destination identity unavailable"}),
        )
            .into_reply();
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
                session.shutdown().await;
                return (ReplyKind::HostUnavailable, json!({"error":error})).into_reply();
            }
            Ok(response) => {
                let failed = response.get("success") == Some(&Value::Bool(false))
                    || response
                        .get("data")
                        .and_then(|data| data.get("outcome"))
                        .and_then(Value::as_str)
                        == Some("failed");
                if failed {
                    session.shutdown().await;
                    return (
                        ReplyKind::InvalidInput,
                        json!({
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
                        }),
                    )
                        .into_reply();
                }
            }
        }
    }
    let prepared = resources::prepare_fork_selection(
        state,
        source_id,
        &actual_id,
        &session,
        live_settings.as_ref(),
    )
    .await;
    let record = match prepared {
        Ok(record) => record,
        Err(error) => {
            session.shutdown().await;
            return resources::reply_error(error).into_reply();
        }
    };
    // Shutdown sets closing before waiting for this lifecycle owner. Caller
    // cancellation alone never discards completion; shutdown may refuse it.
    if state.closing.load(std::sync::atomic::Ordering::Acquire) {
        session.shutdown().await;
        return (
            ReplyKind::Unavailable,
            json!({"error":"service shutting down"}),
        )
            .into_reply();
    }
    let snapshot = session.snapshot().await;
    let saved = resources::publish_record(
        state,
        RegistryEntry {
            session_id: actual_id.clone(),
            project_id: Some(project.id.clone()),
            title: Some(title.clone()),
            parent_session_id: Some(source_id.to_string()),
            fork_entry_id: entry_id.clone(),
            relation: Some(if entry_id.is_some() { "fork" } else { "clone" }.into()),
            created_at: snapshot.created_at,
            updated_at: snapshot.updated_at,
        },
        record,
    )
    .await;
    if let Err(error) = saved {
        session.shutdown().await;
        return resources::reply_error(error).into_reply();
    }
    state
        .sessions
        .write()
        .await
        .insert(actual_id.clone(), session);
    state.touch_session(&actual_id).await;
    (
        ReplyKind::Created,
        json!({
            "sessionId": actual_id,
            "sourceSessionId": source_id,
            "entryId": entry_id,
            "status": "ready",
            "title": title,
        }),
    )
        .into_reply()
}

/// Reopen a remembered session whose native host is not running.
pub async fn open_session(state: &Service, id: String) -> (ReplyKind, Value) {
    let owner = state.clone();
    tokio::spawn(async move { open_session_owned(&owner, id).await })
        .await
        .unwrap_or_else(|_| {
            (
                ReplyKind::Internal,
                json!({"error":"session restoration owner failed"}),
            )
        })
}
async fn open_session_owned(state: &Service, id: String) -> (ReplyKind, Value) {
    // Concurrent tabs must not spawn two native writers for the same transcript.
    let _lifecycle = state.session_lifecycle.lock().await;
    if state.closing.load(std::sync::atomic::Ordering::Acquire) {
        return (
            ReplyKind::Unavailable,
            json!({"error":"service shutting down"}),
        );
    }
    if let Err(error) = state.check_session_admission(&id).await {
        return error;
    }
    if let Err(error) = state.check_resource_storage(&id) {
        return resources::reply_error(error);
    }
    if let Some(record) = state.registry.read().await.resource_configuration(&id) {
        if record.blocked() {
            return (
                ReplyKind::Conflict,
                json!({"error":"resource operation pending or blocked; retry the same operation explicitly"}),
            );
        }
    }
    let existing = state.sessions.read().await.get(&id).cloned();
    if let Some(session) = existing {
        let snapshot = session.snapshot().await;
        if snapshot.status != "closed" {
            state.touch_session(&id).await;
            let entry = state.registry.read().await.get(&id).cloned();
            return (
                ReplyKind::Success,
                json!({
                    "sessionId": id,
                    "status": snapshot.status,
                    "title": entry.as_ref().and_then(|entry|entry.title.clone()).or(snapshot.title),
                    "createdAt": entry.as_ref().map(|entry|entry.created_at).unwrap_or(snapshot.created_at),
                    "updatedAt": snapshot.updated_at,
                    "lastSeq": snapshot.last_seq,
                    "ready": snapshot.ready,
                }),
            );
        }
        // The native child exited (crash, kill, or `stop`); drop the corpse so
        // the branch can be reopened from its persisted session file.
        if let Err(error) = session.await_resource_exit().await {
            return resources::reply_error(error);
        }
        state.sessions.write().await.remove(&id);
        state.session_activity.write().await.remove(&id);
    }
    let known = state.registry.read().await.get(&id).cloned();
    let Some(mut entry) = known else {
        return (
            ReplyKind::NotFound,
            json!({"error": "unknown session", "code": "session_not_found"}),
        );
    };
    let project = match session_project(state, &id).await {
        Ok(project) => project,
        Err(error) => return (ReplyKind::InvalidInput, json!({"error":error})),
    };
    match spawn_host(state, &id, &project).await {
        Ok((session, record)) => {
            let snapshot = session.snapshot().await;
            // Configuration publication preserves the latest stored metadata,
            // even if a rename completed while the native candidate started.
            if let Some(latest) = state.registry.read().await.get(&id) {
                entry = latest.clone();
            }
            let saved = resources::publish_record(
                state,
                RegistryEntry {
                    session_id: id.clone(),
                    project_id: Some(project.id.clone()),
                    title: entry.title.clone(),
                    parent_session_id: entry.parent_session_id.clone(),
                    fork_entry_id: entry.fork_entry_id.clone(),
                    relation: entry.relation.clone(),
                    created_at: entry.created_at,
                    updated_at: snapshot.updated_at,
                },
                record,
            )
            .await;
            if let Err(error) = saved {
                session.shutdown().await;
                return resources::reply_error(error);
            }
            state.sessions.write().await.insert(id.clone(), session);
            state.touch_session(&id).await;
            (
                ReplyKind::Success,
                json!({
                    "sessionId": id,
                    "status": snapshot.status,
                    "title": entry.title,
                    "createdAt": entry.created_at,
                    "updatedAt": snapshot.updated_at,
                    "lastSeq": snapshot.last_seq,
                    "ready": snapshot.ready,
                }),
            )
        }
        Err(error) => (
            ReplyKind::HostUnavailable,
            json!({"error": error, "code": "host_start_failed"}),
        ),
    }
}

#[derive(Default, Deserialize)]
pub struct SessionListQuery {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(rename = "projectId", default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub archived: Option<bool>,
    #[serde(default)]
    pub pinned: Option<bool>,
}

pub async fn list_sessions(state: &Service, query: SessionListQuery) -> Value {
    let sessions = state.sessions.read().await;
    let registry = state.registry.read().await;
    let project_names: HashMap<String, String> = state
        .projects
        .read()
        .await
        .entries
        .iter()
        .map(|project| (project.id.clone(), project.name.clone()))
        .collect();
    let search = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    let tag = query
        .tag
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    let status_filter = query
        .status
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    let mut summaries = Vec::new();
    for entry in registry.entries() {
        let snapshot = match sessions.get(&entry.session_id) {
            Some(session) => Some(session.snapshot().await),
            None => None,
        };
        let project_id = entry
            .project_id
            .clone()
            .unwrap_or_else(|| state.default_project_id.clone());
        let preferences = registry.preferences(&entry.session_id);
        let title = entry.title.clone().or_else(|| {
            snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.title.clone())
        });
        let status = snapshot
            .as_ref()
            .map(|snapshot| snapshot.status.clone())
            .unwrap_or_else(|| "offline".into());
        let project_name = project_names.get(&project_id).cloned().unwrap_or_default();
        let searchable = format!(
            "{} {} {}",
            title.as_deref().unwrap_or_default(),
            entry.session_id,
            project_name
        )
        .to_lowercase();
        if let Some(search) = &search {
            if !searchable.contains(search)
                && !preferences
                    .tags
                    .iter()
                    .any(|item| item.to_lowercase().contains(search))
            {
                continue;
            }
        }
        if query
            .project_id
            .as_deref()
            .is_some_and(|value| value != project_id)
        {
            continue;
        }
        if let Some(tag) = &tag {
            if !preferences
                .tags
                .iter()
                .any(|item| item.to_lowercase() == *tag)
            {
                continue;
            }
        }
        if status_filter
            .as_deref()
            .is_some_and(|value| value != status.to_lowercase())
        {
            continue;
        }
        if query
            .archived
            .is_some_and(|value| value != preferences.archived)
            || query
                .pinned
                .is_some_and(|value| value != preferences.pinned)
        {
            continue;
        }
        summaries.push(json!({
            "projectId": project_id,
            "pinned": preferences.pinned,
            "archived": preferences.archived,
            "deleting": registry.deletion(&entry.session_id).is_some(),
            "tags": preferences.tags,
            "sessionId": entry.session_id,
            "title": title,
            "status": status,
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
    json!({"sessions": summaries})
}

pub async fn get_session(state: &Service, id: String) -> (ReplyKind, Value) {
    let (preferences, deleting) = {
        let registry = state.registry.read().await;
        (registry.preferences(&id), registry.deletion(&id).is_some())
    };
    let session = state.sessions.read().await.get(&id).cloned();
    let Some(session) = session else {
        let entry = state.registry.read().await.get(&id).cloned();
        let Some(entry) = entry else {
            return (ReplyKind::NotFound, json!({"error":"session not found"}));
        };
        return (
            ReplyKind::Success,
            json!({
                "sessionId": id,
                "pinned": preferences.pinned,
                "archived": preferences.archived,
                "tags": preferences.tags,
                "deleting": deleting,
                "status": "offline",
                "title": entry.title,
                "createdAt": entry.created_at,
                "updatedAt": entry.updated_at,
                "lastSeq": 0,
                "parentSessionId": entry.parent_session_id,
                "forkEntryId": entry.fork_entry_id,
                "relation": entry.relation,
                "error": null,
            }),
        );
    };
    let snapshot = session.snapshot().await;
    let entry = state.registry.read().await.get(&id).cloned();
    (
        ReplyKind::Success,
        json!({
            "sessionId": id,
            "pinned": preferences.pinned,
            "archived": preferences.archived,
            "tags": preferences.tags,
            "deleting": deleting,
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
        }),
    )
}

pub async fn delete_session(state: &Service, id: String) -> ServiceReply {
    retirement::delete(state, id).await
}

pub async fn command(state: &Service, id: String, command: ApiCommand) -> (ReplyKind, Value) {
    state.touch_session(&id).await;
    if let Err(error) = state.check_session_admission(&id).await {
        return error;
    }
    if !protocol::SUPPORTED_COMMANDS.contains(&command.kind.as_str()) {
        return (
            ReplyKind::Unsupported,
            json!({
                "type": "response",
                "command": command.kind,
                "success": false,
                "error": "unsupported command"
            }),
        );
    }
    let session = state.sessions.read().await.get(&id).cloned();
    let Some(session) = session else {
        return (ReplyKind::NotFound, json!({"error":"session not found"}));
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
                    return (ReplyKind::Internal, json!({"error":error}));
                }
                session.set_title_if_empty(&title).await;
            }
        }
    }
    let request_id = protocol::command_id(&command);
    let mut value = serde_json::to_value(&command).unwrap_or_else(|_| json!({}));
    value["id"] = request_id.clone();
    // Host admission coordinates this retained Arc with retirement, including
    // races after the metadata check. Unrelated lifecycle work must never delay
    // abort/dialog replies, so command admission takes no service-wide mutex.
    let result = session.send(value).await;
    match result {
        Ok(response) => (ReplyKind::Success, response),
        Err(error) => (
            ReplyKind::HostUnavailable,
            protocol::response(Some(request_id), &command.kind, Err(error)),
        ),
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SnapshotOptions {
    pub limit: Option<usize>,
    /// Skip the diagnostic history read on latency-sensitive transcript loads.
    /// The web client fetches this shortly after the conversation is visible.
    #[serde(rename = "includeHistory")]
    pub include_history: Option<bool>,
}

/// A read-only transcript snapshot plus the events that landed while it was
/// being taken, so a client can render history and then follow `?after=` live
/// without a gap or an overlap.
pub async fn snapshot(state: &Service, id: String) -> ServiceReply {
    snapshot_with_options(state, id, SnapshotOptions::default()).await
}

pub async fn snapshot_with_options(
    state: &Service,
    id: String,
    options: SnapshotOptions,
) -> ServiceReply {
    let session = state.sessions.read().await.get(&id).cloned();
    let Some(session) = session else {
        return (ReplyKind::NotFound, json!({"error":"session not found"})).into_reply();
    };
    state.touch_session(&id).await;
    let host_snapshot = session.snapshot().await;
    let start = host_snapshot.last_seq;
    // These are independent read-only requests. Dispatch both after the same
    // sequence boundary so snapshot latency is bounded by the slower request,
    // rather than the sum of both native RPC round trips.
    let messages_request = session.send(json!({
        "type": "get_messages",
        "id": uuid::Uuid::new_v4().to_string(),
        "limit": options.limit.unwrap_or(80).clamp(1, 400),
    }));
    // History is useful for the diagnostics panel, but it is not required to
    // paint the conversation. Allow latency-sensitive clients to defer it while
    // keeping the legacy default for other callers.
    let history_request = async {
        if options.include_history != Some(false) {
            Some(
                session
                    .send(json!({
                        "type": "get_history",
                        "id": uuid::Uuid::new_v4().to_string(),
                        "limit": 100,
                    }))
                    .await,
            )
        } else {
            None
        }
    };
    let (response, history_response) = tokio::join!(messages_request, history_request);
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            return (
                ReplyKind::HostUnavailable,
                json!({"error": error, "code": "host_request_failed"}),
            )
                .into_reply();
        }
    };
    if response["success"] != Value::Bool(true) {
        let error = response["error"]
            .as_str()
            .unwrap_or("get_messages is not supported by this host");
        return (
            ReplyKind::Unsupported,
            json!({"error": error, "code": "snapshot_unsupported"}),
        )
            .into_reply();
    }
    let data = &response["data"];
    let history = history_response
        .and_then(Result::ok)
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
            ReplyKind::Conflict,
            json!({
                "error":"events changed too quickly while loading the snapshot; retry loading",
                "code":"snapshot_events_evicted"
            }),
        )
            .into_reply();
    }
    let events = events
        .iter()
        .map(|event| {
            let mut value = event.value.clone();
            crate::timing::refresh_event(&mut value);
            json!({"seq": event.seq, "event": value})
        })
        .collect::<Vec<_>>();
    json!({
        "sessionId": id,
        "messages": data["messages"].clone(),
        "supportedCommands": session.ready()["supportedCommands"].clone(),
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
    })
    .into_reply()
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

pub fn replay_gap(after: Option<u64>, first: Option<u64>, latest: u64) -> bool {
    after.is_some_and(|after| {
        after > latest || first.is_some_and(|first| after.saturating_add(1) < first)
    })
}

#[cfg(test)]
impl Service {
    pub(crate) async fn test_register_host(&self, id: &str, host: Arc<HostSession>) {
        self.sessions.write().await.insert(id.to_owned(), host);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn snapshot_limits_native_history_without_losing_the_earlier_cursor() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let root = std::env::temp_dir().join(format!("rpi-snapshot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let service = Service::new(BackendConfig {
            project: root.clone(),
            registry: root.join("sessions.json"),
            ..BackendConfig::default()
        })
        .unwrap();
        let (writer, peer) = tokio::io::duplex(4096);
        let (session, replies) = HostSession::test_connection(writer, 16);
        service
            .sessions
            .write()
            .await
            .insert("history".into(), session);
        let fixture = tokio::spawn(async move {
            let mut lines = BufReader::new(peer).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let data = if request["type"] == "get_messages" {
                    let limit = request["limit"].as_u64().unwrap_or(400);
                    json!({
                        "messages": (1000-limit..1000).map(|id| json!({"entryId":id.to_string(),"role":"user","content":"history"})).collect::<Vec<_>>(),
                        "total": 1000, "firstId": (1000-limit).to_string(), "hasMore": true
                    })
                } else {
                    json!({"records":[],"hasMore":false})
                };
                replies
                    .send(json!({"type":"response","id":request["id"],"success":true,"data":data}))
                    .unwrap();
            }
        });
        for (limit, expected) in [(None, 80), (Some(10), 10), (Some(0), 1), (Some(999), 400)] {
            let result = snapshot_with_options(
                &service,
                "history".into(),
                SnapshotOptions {
                    limit,
                    include_history: None,
                },
            )
            .await;
            assert_eq!(result.kind, ReplyKind::Success);
            assert_eq!(result.value["messages"].as_array().unwrap().len(), expected);
            assert_eq!(result.value["firstId"], (1000 - expected).to_string());
            assert_eq!(result.value["hasMore"], true);
            assert_eq!(result.value["total"], 1000);
        }
        fixture.abort();
        service.shutdown().await;
        drop(service);
        std::fs::remove_dir_all(root).unwrap();
    }
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

#[cfg(test)]
mod run_timing_service_tests {
    use super::*;
    #[tokio::test]
    async fn run_timings_reads_known_native_transcripts_without_starting_a_host() {
        let root =
            std::env::temp_dir().join(format!("rpi-timing-service-{}", uuid::Uuid::new_v4()));
        let sessions = root.join(".rpi/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let state = Service::new(BackendConfig {
            project: root.clone(),
            registry: root.join("registry.json"),
            session_dir: Some(PathBuf::from(".rpi/sessions")),
            rpi_bin: root.join("must-not-start.exe"),
            ..BackendConfig::default()
        })
        .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let query = || RunTimingsQuery {
            entry_ids: vec!["actual".into()],
            cursor: None,
        };
        assert_eq!(
            run_timings(&state, "../escape".into(), query()).await.kind,
            ReplyKind::InvalidInput
        );
        assert_eq!(
            run_timings(&state, id.clone(), query()).await.kind,
            ReplyKind::NotFound
        );
        state
            .registry
            .write()
            .await
            .upsert(RegistryEntry {
                session_id: id.clone(),
                project_id: Some(state.default_project_id.clone()),
                title: None,
                parent_session_id: None,
                fork_entry_id: None,
                relation: None,
                created_at: 0,
                updated_at: 0,
            })
            .unwrap();
        let rows = [
            json!({"kind":"header","version":4,"id":id}),
            json!({"kind":"record","type":"operation_started","id":"run","timestamp":1234,"lane":"main","intent":{"kind":"run"}}),
            json!({"kind":"entry","type":"message","id":"actual","lane":"main","message":{"role":"assistant","content":[{"type":"toolCall","id":"tool","name":"read"}]}}),
            json!({"kind":"record","type":"operation_finished","id":"finish","runId":"run","timestamp":7890,"lane":"main","outcome":"completed"}),
        ];
        let path = sessions.join(format!("2026_{id}.jsonl"));
        std::fs::write(
            &path,
            rows.iter().map(|v| format!("{v}\n")).collect::<String>(),
        )
        .unwrap();
        let before = std::fs::read(state.config.registry.clone()).unwrap();
        let reply = run_timings(&state, id.clone(), query()).await;
        assert_eq!(reply.kind, ReplyKind::Success);
        assert_eq!(
            reply.value["timings"][0],
            json!({"runId":"run","lane":"main","finalEntryId":"actual","startedAt":1234,"finishedAt":7890,"elapsedMs":6656,"outcome":"completed"})
        );
        assert_eq!(reply.value["complete"], true);
        assert!(reply.value["cursor"].is_string());
        let bad = run_timings(
            &state,
            id.clone(),
            RunTimingsQuery {
                cursor: Some("invalid".into()),
                ..query()
            },
        )
        .await;
        assert_eq!(bad.kind, ReplyKind::Conflict);
        let empty = run_timings(&state, id, RunTimingsQuery::default()).await;
        assert_eq!(empty.kind, ReplyKind::Success);
        assert_eq!(empty.value["timings"], json!([]));
        assert!(state.sessions.read().await.is_empty());
        assert_eq!(
            std::fs::read(state.config.registry.clone()).unwrap(),
            before
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
