//! Session and workspace operations shared by protocol adapters.
#[cfg(test)]
use crate::durable::ENTRY_TYPE;
use crate::{
    durable::{
        DurableCoordinator, DurableError, DurableMutation, DurableRuntime, DurableSubscription,
    },
    host::HostSession,
    projects::Projects,
    protocol::{self, ApiCommand},
    registry::{Registry, RegistryEntry},
    reply::{IntoReply, ReplyKind, ServiceReply, ServiceResult},
    workspace::{self, Project},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::{
    process::Command,
    sync::{Mutex, RwLock},
};
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

pub struct Service {
    pub config: BackendConfig,
    projects: Arc<RwLock<Projects>>,
    default_project_id: String,
    sessions: Arc<RwLock<HashMap<String, Arc<HostSession>>>>,
    durable: Arc<RwLock<HashMap<String, Arc<DurableRuntime>>>>,
    durable_observers: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    session_lifecycle: Arc<Mutex<()>>,
    registry: Arc<RwLock<Registry>>,
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
            durable: Arc::new(RwLock::new(HashMap::new())),
            durable_observers: Arc::new(Mutex::new(HashMap::new())),
            session_lifecycle: Arc::new(Mutex::new(())),
            registry: Arc::new(RwLock::new(registry)),
        }))
    }
    pub async fn live_session(&self, id: &str) -> Option<Arc<HostSession>> {
        self.sessions.read().await.get(id).cloned()
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
        let _lifecycle = self.session_lifecycle.lock().await;
        let sessions: Vec<_> = self.sessions.read().await.values().cloned().collect();
        futures_util::future::join_all(sessions.iter().map(|session| session.shutdown())).await;
        let observers: Vec<_> = self
            .durable_observers
            .lock()
            .await
            .drain()
            .map(|(_, handle)| handle)
            .collect();
        for observer in observers {
            observer.abort();
        }
        self.durable.write().await.clear();
    }

    /// Return the durable runtime for a live native session.
    pub async fn durable_runtime(&self, id: &str) -> Option<Arc<DurableRuntime>> {
        self.durable.read().await.get(id).cloned()
    }

    pub async fn durable_subscription(
        &self,
        id: &str,
        after: Option<u64>,
    ) -> Option<DurableSubscription> {
        let runtime = self.durable_runtime(id).await?;
        Some(runtime.subscribe_changes(after).await)
    }

    /// Return the durable coordinator for a live native session.
    pub async fn durable_coordinator(&self, id: &str) -> Option<Arc<Mutex<DurableCoordinator>>> {
        self.durable_runtime(id)
            .await
            .map(|runtime| runtime.coordinator())
    }

    /// Apply one protocol-neutral durable mutation and persist it as a native
    /// custom entry. The in-memory mutation is rolled back if the append fails.
    pub async fn update_durable<F, T>(&self, id: &str, update: F) -> Result<T, String>
    where
        F: FnOnce(&mut DurableCoordinator) -> Result<T, DurableError>,
    {
        if self.live_session(id).await.is_none() {
            return Err("session not found".to_string());
        }
        let runtime = self
            .durable_runtime(id)
            .await
            .ok_or_else(|| "durable coordinator is not initialized".to_string())?;
        runtime.update(update).await
    }

    /// Return the current durable state for a live session.
    pub async fn durable_state(&self, id: &str) -> ServiceResult {
        if self.live_session(id).await.is_none() {
            return Err((
                ReplyKind::NotFound,
                json!({"error": "session not found", "code": "session_not_found"}),
            ));
        }
        let runtime = self.durable_runtime(id).await.ok_or_else(|| {
            (
                ReplyKind::Unavailable,
                json!({
                    "error": "durable coordinator is not initialized",
                    "code": "durable_unavailable"
                }),
            )
        })?;
        Ok(json!({"sessionId": id, "state": runtime.state().await}))
    }

    /// Apply one durable control-plane mutation and return the resulting state.
    pub async fn mutate_durable(&self, id: &str, mutation: DurableMutation) -> ServiceResult {
        let result = self
            .update_durable(id, |coordinator| coordinator.apply(mutation))
            .await;
        let result = match result {
            Ok(result) => result,
            Err(error) if error == "session not found" => {
                return Err((ReplyKind::NotFound, json!({"error": error})))
            }
            Err(error) if error == "durable coordinator is not initialized" => {
                return Err((
                    ReplyKind::Unavailable,
                    json!({
                        "error": error,
                        "code": "durable_unavailable"
                    }),
                ))
            }
            Err(error) => {
                let kind = if error.contains("queue item")
                    || error.contains("only an interrupted run")
                    || error.contains("run is ")
                {
                    ReplyKind::Conflict
                } else if error.contains("required") || error.contains("must be") {
                    ReplyKind::InvalidInput
                } else if error.contains("queue cannot contain") {
                    ReplyKind::LimitExceeded
                } else {
                    ReplyKind::HostUnavailable
                };
                return Err((kind, json!({"error": error})));
            }
        };
        let coordinator = self.durable_coordinator(id).await.ok_or_else(|| {
            (
                ReplyKind::Unavailable,
                json!({"error": "durable coordinator disappeared"}),
            )
        })?;
        let state = coordinator.lock().await.state().clone();
        Ok(json!({
            "sessionId": id,
            "result": result,
            "state": state,
        }))
    }
}

pub async fn durable_state(state: &Service, id: String) -> ServiceResult {
    state.durable_state(&id).await
}

pub async fn mutate_durable(
    state: &Service,
    id: String,
    mutation: DurableMutation,
) -> ServiceResult {
    state.mutate_durable(&id, mutation).await
}

async fn attach_durable(
    state: &Service,
    id: &str,
    session: Arc<HostSession>,
) -> Result<(), String> {
    if let Some(previous) = state.durable_observers.lock().await.remove(id) {
        previous.abort();
    }
    state.durable.write().await.remove(id);

    let Some(runtime) = DurableRuntime::try_attach(session).await? else {
        // Durable is an optional native capability. Keep the ordinary session
        // alive when an older host does not expose branch RPCs.
        return Ok(());
    };
    state
        .durable
        .write()
        .await
        .insert(id.to_owned(), runtime.clone());
    let observer = runtime.spawn_observer();
    state
        .durable_observers
        .lock()
        .await
        .insert(id.to_owned(), observer);
    Ok(())
}

async fn detach_durable(state: &Service, id: &str) {
    if let Some(observer) = state.durable_observers.lock().await.remove(id) {
        observer.abort();
    }
    state.durable.write().await.remove(id);
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
}

pub async fn create_session(
    state: &Service,
    requested: CreateSessionRequest,
) -> (ReplyKind, Value) {
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
    match spawn_host(state, &id, &project).await {
        Ok(session) => {
            if let Err(error) = attach_durable(state, &id, session.clone()).await {
                session.stop().await;
                return (
                    ReplyKind::HostUnavailable,
                    json!({
                        "error": error,
                        "code": "durable_runtime_unavailable"
                    }),
                );
            }
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
                detach_durable(state, &id).await;
                session.stop().await;
                return (ReplyKind::Internal, json!({"error": error}));
            }
            state.sessions.write().await.insert(id.clone(), session);
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
    let mut registry = state.registry.write().await;
    if registry.get(&id).is_none() {
        return (ReplyKind::NotFound, json!({"error":"session not found"}));
    }
    match registry.update_preferences(&id, request.pinned, request.archived, request.tags) {
        Ok(prefs) => (
            ReplyKind::Success,
            json!({
                "sessionId": id,
                "pinned": prefs.pinned,
                "archived": prefs.archived,
                "tags": prefs.tags,
            }),
        ),
        Err(error) => (ReplyKind::Internal, json!({"error":error})),
    }
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
    let _lifecycle = state.session_lifecycle.lock().await;
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
        session.stop().await;
        return (
            ReplyKind::HostUnavailable,
            json!({"error":"forked host did not return its session id"}),
        )
            .into_reply();
    };
    if actual_id.is_empty()
        || state.registry.read().await.get(&actual_id).is_some()
        || state.sessions.read().await.contains_key(&actual_id)
    {
        session.stop().await;
        return (
            ReplyKind::HostUnavailable,
            json!({"error":"forked host returned an existing or invalid session id"}),
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
                session.stop().await;
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
                    let _ = session
                        .send(json!({"type": "stop", "id": uuid::Uuid::new_v4().to_string()}))
                        .await;
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
        return (ReplyKind::Internal, json!({"error": error})).into_reply();
    }
    state
        .sessions
        .write()
        .await
        .insert(actual_id.clone(), session.clone());
    if let Err(error) = attach_durable(state, &actual_id, session.clone()).await {
        state.sessions.write().await.remove(&actual_id);
        session.stop().await;
        return (
            ReplyKind::HostUnavailable,
            json!({
                "error": error,
                "code": "durable_runtime_unavailable"
            }),
        )
            .into_reply();
    }
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
    // Concurrent tabs must not spawn two native writers for the same transcript.
    let _lifecycle = state.session_lifecycle.lock().await;
    let existing = state.sessions.read().await.get(&id).cloned();
    if let Some(session) = existing {
        let snapshot = session.snapshot().await;
        if snapshot.status != "closed" {
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
        detach_durable(state, &id).await;
        state.sessions.write().await.remove(&id);
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
        Ok(session) => {
            if let Err(error) = attach_durable(state, &id, session.clone()).await {
                session.stop().await;
                return (
                    ReplyKind::HostUnavailable,
                    json!({
                        "error": error,
                        "code": "durable_runtime_unavailable"
                    }),
                );
            }
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
                detach_durable(state, &id).await;
                session.stop().await;
                return (ReplyKind::Internal, json!({"error": error}));
            }
            state.sessions.write().await.insert(id.clone(), session);
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
    let _lifecycle = state.session_lifecycle.lock().await;
    let forgotten = match state.registry.write().await.remove(&id) {
        Ok(forgotten) => forgotten,
        Err(error) => return (ReplyKind::Internal, json!({"error": error})).into_reply(),
    };
    let removed = state.sessions.write().await.remove(&id);
    detach_durable(state, &id).await;
    match removed {
        Some(session) => {
            session.stop().await;
            ReplyKind::Empty.into_reply()
        }
        None if forgotten => ReplyKind::Empty.into_reply(),
        None => ReplyKind::NotFound.into_reply(),
    }
}

pub async fn command(state: &Service, id: String, command: ApiCommand) -> (ReplyKind, Value) {
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
    let history_request = session.send(json!({
        "type": "get_history",
        "id": uuid::Uuid::new_v4().to_string(),
        "limit": 100,
    }));
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
mod tests {
    use super::*;
    #[tokio::test]
    async fn durable_service_recovers_branch_and_persists_live_mutations() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncBufReadExt, BufReader};

        let root =
            std::env::temp_dir().join(format!("rpi-durable-service-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let service = Service::new(BackendConfig {
            project: root.clone(),
            registry: root.join("sessions.json"),
            ..BackendConfig::default()
        })
        .unwrap();
        let (writer, peer) = tokio::io::duplex(4096);
        let (session, injector) = HostSession::test_connection(writer, 16);
        let injector_for_fixture = injector.clone();
        let appends = Arc::new(AtomicUsize::new(0));
        let appends_for_fixture = appends.clone();
        let fixture = tokio::spawn(async move {
            let mut lines = BufReader::new(peer).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let data = if request["type"] == "get_session_branch" {
                    json!({"entries": [{
                        "type": "custom",
                        "customType": ENTRY_TYPE,
                        "data": {
                            "revision": 3,
                            "nextRunId": 1,
                            "run": {"id": "run-1", "status": "running", "tools": []},
                            "queue": []
                        }
                    }]})
                } else {
                    appends_for_fixture.fetch_add(1, Ordering::SeqCst);
                    json!({"entryId": "entry-durable"})
                };
                injector_for_fixture
                    .send(json!({
                        "type": "response",
                        "id": request["id"],
                        "success": true,
                        "data": data
                    }))
                    .unwrap();
            }
        });
        service
            .sessions
            .write()
            .await
            .insert("durable".into(), session.clone());

        attach_durable(&service, "durable", session.clone())
            .await
            .unwrap();
        let recovered = service.durable_coordinator("durable").await.unwrap();
        assert_eq!(
            recovered.lock().await.state().run.as_ref().unwrap().status,
            crate::durable::RunStatus::Interrupted
        );

        let item_id = service
            .update_durable("durable", |coordinator| {
                Ok(coordinator.enqueue("persist this")?.id.clone())
            })
            .await
            .unwrap();
        assert_eq!(item_id, "item-1");
        assert!(appends.load(Ordering::SeqCst) >= 2);

        let claimed = service
            .mutate_durable(
                "durable",
                DurableMutation::Claim {
                    id: item_id.clone(),
                },
            )
            .await
            .unwrap();
        assert_eq!(claimed["result"]["changed"], true);
        let snapshot = service.durable_state("durable").await.unwrap();
        assert_eq!(snapshot["state"]["queue"][0]["status"], "claimed");

        detach_durable(&service, "durable").await;
        fixture.abort();
        service.shutdown().await;
        drop(service);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn durable_observer_folds_native_lifecycle_events() {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let root =
            std::env::temp_dir().join(format!("rpi-durable-events-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let service = Service::new(BackendConfig {
            project: root.clone(),
            registry: root.join("sessions.json"),
            ..BackendConfig::default()
        })
        .unwrap();
        let (writer, peer) = tokio::io::duplex(4096);
        let (session, injector) = HostSession::test_connection(writer, 16);
        let injector_for_fixture = injector.clone();
        let fixture = tokio::spawn(async move {
            let mut lines = BufReader::new(peer).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let data = if request["type"] == "get_session_branch" {
                    json!({"entries": []})
                } else {
                    json!({"entryId": "entry-durable"})
                };
                injector_for_fixture
                    .send(json!({
                        "type": "response",
                        "id": request["id"],
                        "success": true,
                        "data": data
                    }))
                    .unwrap();
            }
        });
        service
            .sessions
            .write()
            .await
            .insert("events".into(), session.clone());
        attach_durable(&service, "events", session.clone())
            .await
            .unwrap();
        injector.send(json!({"type": "agent_start"})).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let state = service.durable_coordinator("events").await.unwrap();
        assert_eq!(
            state.lock().await.state().run.as_ref().unwrap().status,
            crate::durable::RunStatus::Running
        );

        detach_durable(&service, "events").await;
        fixture.abort();
        service.shutdown().await;
        drop(service);
        std::fs::remove_dir_all(root).unwrap();
    }

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
            let result =
                snapshot_with_options(&service, "history".into(), SnapshotOptions { limit }).await;
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
