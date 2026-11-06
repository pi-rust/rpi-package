//! Native workspace backed directly by the published rpi-extcore service.
use crate::runtime::RuntimeError;
use async_stream::try_stream;
use futures_util::StreamExt;
use rpi_extcore::{
    events::{self, StreamItem},
    registry::Registry,
    reply::{IntoReply, ReplyKind},
    service::{self, BackendConfig, Service},
    workspace,
};
use rpi_node_api::api::{
    AuthState, AuthStatus, Capabilities, Diff, FileContent, FileListing, GitStatus, Health,
    History, HostResponse, ModelDescriptor, Project, ProjectFolder, ProjectFolders,
    QueueCancellation, QueueMutation, QueueSnapshot, RuntimeState, SessionBranch, SessionMutation,
    SessionPreferences, SessionSummary, Snapshot, ToolCatalog, WireEvent, payload_with_type,
};
use rpi_node_api::{AgentDescriptor, CapabilitySet, TransportIdentity};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::pin::Pin;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
mod access;
mod agents;
mod api;
mod authoring;
mod command_allowlist;
mod components;
mod configuration;
mod migration;
pub mod operations;
mod packages;
mod providers;
mod resources;
use migration::{load_aliases, migrate_registries};

#[derive(Debug, thiserror::Error)]
pub enum LocalError {
    #[error("local workspace I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("authoring filesystem operation failed; retain editor/recovery bytes: {0}")]
    AuthoringIo(#[source] std::io::Error),
    #[error("invalid local workspace data: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid local workspace input: {0}")]
    InvalidInput(String),
    #[error("local workspace item not found: {0}")]
    NotFound(String),
    #[error("project is already registered: {0}")]
    DuplicateProject(String),
    #[error("native service initialization failed: {0}")]
    Initialization(String),
    #[error("native service returned {kind:?}: {message}")]
    Service { kind: ReplyKind, message: String },
    #[error("native operation is unsupported: {0}")]
    Unsupported(String),
    #[error("local workspace has been shut down")]
    Shutdown,
    #[error("local workspace state lock was poisoned")]
    Poisoned,
}

/// Workspace/application errors, distinct from transport and HTTP access errors.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error(transparent)]
    Local(#[from] LocalError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

pub type WorkspaceEventStream =
    Pin<Box<dyn futures_core::Stream<Item = Result<WireEvent, WorkspaceError>> + Send>>;
pub use agents::PreparedAgent;

fn unsupported(operation: &str, protocol: &str) -> WorkspaceError {
    RuntimeError::Unsupported {
        operation: operation.into(),
        protocol: protocol.into(),
    }
    .into()
}

#[derive(Clone, Serialize, Deserialize)]
struct Alias {
    name: String,
    path: PathBuf,
}
struct State {
    aliases: Vec<Alias>,
    services: HashMap<String, Arc<Service>>,
    closed: bool,
}
struct Inner {
    executable: PathBuf,
    data: PathBuf,
    control_root: PathBuf,
    state: Mutex<State>,
    // Separate from Alias so existing catalogs and legacy migration stay compatible.
    project_cache: Mutex<HashMap<PathBuf, Project>>,
    control: tokio::sync::Mutex<Option<String>>,
    // Read leases permit independent projects to run concurrently. Shutdown takes
    // the exclusive lease so no in-flight operation can start a host afterward.
    lifecycle: Arc<tokio::sync::RwLock<()>>,
    session_dir: Option<PathBuf>,
    agents: Arc<agents::Manager>,
    authoring: tokio::sync::Mutex<()>,
    packages: tokio::sync::Mutex<()>,
    providers: providers::Store,
    remote_operations: operations::RemoteOperations,
    components: rpi_components::host::Host,
}

/// A cloneable native backend. No listener, HTTP client, or SSE transport is used.
#[derive(Clone)]
pub struct LocalWorkspace {
    inner: Arc<Inner>,
}

fn local<T>(result: Result<T, LocalError>) -> Result<T, WorkspaceError> {
    result.map_err(WorkspaceError::Local)
}
fn decode<T: for<'de> Deserialize<'de>>(reply: impl IntoReply) -> Result<T, WorkspaceError> {
    let reply = reply.into_reply();
    if !matches!(
        reply.kind,
        ReplyKind::Success | ReplyKind::Created | ReplyKind::Empty
    ) {
        return Err(WorkspaceError::Local(LocalError::Service {
            kind: reply.kind,
            message: reply
                .value
                .get("error")
                .and_then(Value::as_str)
                .map_or_else(|| reply.value.to_string(), str::to_owned),
        }));
    }
    local(serde_json::from_value(reply.value).map_err(LocalError::from))
}
fn alias_name(name: &str) -> Result<String, LocalError> {
    let name = name.trim();
    if name.is_empty() || name.chars().any(char::is_control) {
        return Err(LocalError::InvalidInput(
            "project name must be nonempty and contain no control characters".into(),
        ));
    }
    Ok(name.to_owned())
}
fn public_project(alias: &Alias) -> Result<Project, LocalError> {
    let core = workspace::Project::new(&alias.path)?;
    Ok(Project {
        id: core.id,
        name: alias.name.clone(),
        path: core.path,
    })
}

struct CreatedDirectory {
    path: PathBuf,
    registered: bool,
}
impl Drop for CreatedDirectory {
    fn drop(&mut self) {
        if !self.registered {
            // Only remove our Git metadata and an otherwise empty directory;
            // never recursively delete files another actor may have added.
            let _ = std::fs::remove_dir_all(self.path.join(".git"));
            let _ = std::fs::remove_dir(&self.path);
        }
    }
}

// Match the remote workspace's async façade, even for synchronous local operations,
// preserving execution on poll rather than performing work when creating the future.
#[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
impl LocalWorkspace {
    pub fn new(executable: PathBuf, data: PathBuf) -> Result<Self, WorkspaceError> {
        Self::with_session_dir(executable, data, None)
    }

    /// Override the host transcript directory (useful for isolated installations
    /// and smoke tests). None preserves access to legacy native transcripts.
    pub fn with_session_dir(
        executable: PathBuf,
        data: PathBuf,
        session_dir: Option<PathBuf>,
    ) -> Result<Self, WorkspaceError> {
        local((|| {
            std::fs::create_dir_all(&data)?;
            let data = std::fs::canonicalize(data)?;
            std::fs::create_dir_all(data.join("rpc"))?;
            let aliases = load_aliases(&data)?;
            let mut project_cache: HashMap<PathBuf, Project> =
                match std::fs::read(data.join("rpc/project-cache.json")) {
                    Ok(bytes) => serde_json::from_slice(&bytes)?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
                    Err(error) => return Err(error.into()),
                };
            for alias in &aliases {
                if let Ok(project) = public_project(alias) {
                    project_cache.insert(alias.path.clone(), project);
                }
            }
            Self::save_project_cache(&data, &project_cache)?;
            migrate_registries(&data, &aliases)?;
            let control_root = data.join("rpc").join("control");
            std::fs::create_dir_all(&control_root)?;
            Ok(Self {
                inner: Arc::new(Inner {
                    components: rpi_components::host::Host::open(data.join("components"))
                        .map_err(LocalError::Initialization)?,
                    agents: Arc::new(agents::Manager::new(&data)?),
                    authoring: tokio::sync::Mutex::new(()),
                    packages: tokio::sync::Mutex::new(()),
                    providers: providers::Store::new()?,
                    remote_operations: operations::RemoteOperations::new().map_err(|_| {
                        LocalError::Initialization("remote operation entropy unavailable".into())
                    })?,
                    executable,
                    data,
                    control_root,
                    state: Mutex::new(State {
                        aliases,
                        services: HashMap::new(),
                        closed: false,
                    }),
                    project_cache: Mutex::new(project_cache),
                    control: tokio::sync::Mutex::new(None),
                    lifecycle: Arc::new(tokio::sync::RwLock::new(())),
                    session_dir,
                }),
            })
        })())
    }

    /// Receipts belong to this owner, surviving HTTP listener and socket changes.
    #[must_use]
    pub fn remote_operations(&self) -> &operations::RemoteOperations {
        &self.inner.remote_operations
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, LocalError> {
        let state = self.inner.state.lock().map_err(|_| LocalError::Poisoned)?;
        if state.closed {
            return Err(LocalError::Shutdown);
        }
        Ok(state)
    }
    fn save_aliases(&self, aliases: &[Alias]) -> Result<(), LocalError> {
        let path = self.inner.data.join("rpc/projects.json");
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(aliases)?)?;
        std::fs::rename(temporary, path)?;
        Ok(())
    }
    fn save_project_cache(
        data: &Path,
        cache: &HashMap<PathBuf, Project>,
    ) -> Result<(), LocalError> {
        let path = data.join("rpc/project-cache.json");
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(cache)?)?;
        std::fs::rename(temporary, path)?;
        Ok(())
    }
    fn public_project(&self, alias: &Alias) -> Result<Project, LocalError> {
        public_project(alias).or_else(|error| {
            let cache = self
                .inner
                .project_cache
                .lock()
                .map_err(|_| LocalError::Poisoned)?;
            let mut project = cache.get(&alias.path).cloned().ok_or(error)?;
            project.name.clone_from(&alias.name);
            Ok(project)
        })
    }
    fn registry_path(&self, id: &str) -> PathBuf {
        self.inner
            .data
            .join("rpc")
            .join("projects")
            .join(id)
            .join("sessions.json")
    }
    fn service_for_root(&self, root: &Path) -> Result<Arc<Service>, LocalError> {
        let project = workspace::Project::new(root)?;
        let mut state = self.lock()?;
        if let Some(service) = state.services.get(&project.id) {
            return Ok(service.clone());
        }
        let registry = self.registry_path(&project.id);
        std::fs::create_dir_all(registry.parent().expect("registry parent"))?;
        // Always explicit: never inherit the process's current working directory.
        let config = BackendConfig {
            project: project.root,
            projects: Vec::new(),
            projects_root: Some(self.inner.data.join("managed-projects")),
            rpi_bin: self.inner.executable.clone(),
            session_dir: self.inner.session_dir.clone(),
            registry,
        };
        let service =
            Service::new(config).map_err(|e| LocalError::Initialization(e.to_string()))?;
        state.services.insert(project.id, service.clone());
        Ok(service)
    }
    fn project_root(&self, id: &str) -> Result<PathBuf, LocalError> {
        for alias in &self.lock()?.aliases {
            if self
                .public_project(alias)
                .is_ok_and(|project| project.id == id)
            {
                return Ok(alias.path.clone());
            }
        }
        Err(LocalError::NotFound(format!("project {id}")))
    }
    fn project_service(&self, id: &str) -> Result<Arc<Service>, WorkspaceError> {
        local(
            self.project_root(id)
                .and_then(|root| self.service_for_root(&root)),
        )
    }
    fn roots(&self) -> Result<Vec<PathBuf>, LocalError> {
        let mut roots: Vec<_> = self
            .lock()?
            .aliases
            .iter()
            .map(|a| a.path.clone())
            .collect();
        roots.push(self.inner.control_root.clone());
        Ok(roots)
    }
    fn session_service(&self, id: &str) -> Result<Arc<Service>, WorkspaceError> {
        local((|| {
            for root in self.roots()? {
                // An unavailable drive must not hide sessions in other projects.
                let Ok(project) = workspace::Project::new(&root) else {
                    continue;
                };
                let registry = Registry::load(self.registry_path(&project.id))
                    .map_err(LocalError::Initialization)?;
                if registry.get(id).is_some() {
                    return self.service_for_root(&root);
                }
            }
            Err(LocalError::NotFound(format!("session {id}")))
        })())
    }

    #[must_use]
    pub fn descriptor(&self) -> AgentDescriptor {
        AgentDescriptor {
            identity: TransportIdentity {
                protocol: "rpi".into(),
                version: None,
                transport: "native".into(),
                agent_name: Some("rpi".into()),
            },
            capabilities: CapabilitySet::http_default(),
        }
    }
    pub async fn health(&self) -> Result<Health, WorkspaceError> {
        local(self.lock().map(|_| ()))?;
        Ok(Health {
            ok: true,
            service: Some("rpi-extcore".into()),
            version: Some("17ba89c0098f018db76e22051fe8c73e2792dc1c".into()),
            protocol: Some("rpi".into()),
        })
    }
    pub async fn auth(&self) -> Result<AuthState, WorkspaceError> {
        local(self.lock().map(|_| ()))?;
        Ok(AuthState {
            required: false,
            authenticated: true,
        })
    }
    pub async fn login(&self, _password: &str) -> Result<AuthState, WorkspaceError> {
        Err(WorkspaceError::Local(LocalError::Unsupported(
            "workspace password login (native access uses OS permissions)".into(),
        )))
    }
    pub async fn logout(&self) -> Result<AuthState, WorkspaceError> {
        Err(WorkspaceError::Local(LocalError::Unsupported(
            "workspace password logout (native access uses OS permissions)".into(),
        )))
    }
    pub async fn capabilities(&self) -> Result<Capabilities, WorkspaceError> {
        decode(rpi_extcore::protocol::capabilities())
    }
    pub async fn projects(&self) -> Result<Vec<Project>, WorkspaceError> {
        let state = local(self.lock())?;
        // Legacy aliases that were already inaccessible on first upgrade have
        // no known core ID yet. Keep them on disk; expose them once accessible.
        Ok(state
            .aliases
            .iter()
            .filter_map(|alias| self.public_project(alias).ok())
            .collect())
    }
    pub async fn create_project(
        &self,
        name: &str,
        path: Option<&str>,
        init_git: bool,
    ) -> Result<Project, WorkspaceError> {
        if let Some(path) = path {
            return self.register_project(name, Path::new(path));
        }
        let name = local(alias_name(name))?;
        if Path::new(&name).components().count() != 1
            || name == "."
            || name == ".."
            || name.contains(['/', char::from(92), ':'])
        {
            return Err(WorkspaceError::Local(LocalError::InvalidInput(
                "new project name must be a single directory name".into(),
            )));
        }
        let root = self.inner.data.join("managed-projects");
        local(std::fs::create_dir_all(&root).map_err(LocalError::from))?;
        self.create_project_directory(&name, &root.join(&name), init_git)
            .await
    }
    fn register_project(&self, name: &str, path: &Path) -> Result<Project, WorkspaceError> {
        local((|| {
            let name = alias_name(name)?;
            let core = workspace::Project::new(path)?;
            let mut state = self.lock()?;
            if let Some(existing) = state
                .aliases
                .iter()
                .filter_map(|alias| self.public_project(alias).ok())
                .find(|p| p.id == core.id)
            {
                return Ok(existing);
            }
            let alias = Alias {
                name,
                path: core.root,
            };
            let project = public_project(&alias)?;
            {
                let mut cache = self
                    .inner
                    .project_cache
                    .lock()
                    .map_err(|_| LocalError::Poisoned)?;
                cache.insert(alias.path.clone(), project.clone());
                Self::save_project_cache(&self.inner.data, &cache)?;
            }
            let mut aliases = state.aliases.clone();
            aliases.push(alias);
            self.save_aliases(&aliases)?;
            state.aliases = aliases;
            Ok(project)
        })())
    }
    /// Create a directory at an explicitly selected absolute path, then register it.
    pub async fn create_project_directory(
        &self,
        name: &str,
        path: &Path,
        init_git: bool,
    ) -> Result<Project, WorkspaceError> {
        local(alias_name(name))?;
        if !path.is_absolute() {
            return Err(WorkspaceError::Local(LocalError::InvalidInput(
                "select an absolute project directory".into(),
            )));
        }
        local(self.lock().map(|_| ()))?;
        local(std::fs::create_dir(path).map_err(LocalError::from))?;
        let mut created = CreatedDirectory {
            path: path.into(),
            registered: false,
        };
        if init_git {
            let outcome = tokio::time::timeout(
                Duration::from_secs(15),
                rpi_extcore::process::background_command("git")
                    .args(["-c", "core.fsmonitor=false", "init", "--quiet"])
                    .arg(path)
                    .stdin(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .output(),
            )
            .await;
            if !matches!(outcome, Ok(Ok(ref output)) if output.status.success()) {
                return Err(WorkspaceError::Local(LocalError::Initialization(
                    "git initialization failed; new directory registration was rolled back".into(),
                )));
            }
        }
        let project = self.register_project(name, path)?;
        created.registered = true;
        Ok(project)
    }
    pub async fn rename_project(
        &self,
        project_id: &str,
        name: &str,
    ) -> Result<Project, WorkspaceError> {
        local((|| {
            let name = alias_name(name)?;
            let mut state = self.lock()?;
            let mut aliases = state.aliases.clone();
            let alias = aliases
                .iter_mut()
                .find(|a| self.public_project(a).is_ok_and(|p| p.id == project_id))
                .ok_or_else(|| LocalError::NotFound(format!("project {project_id}")))?;
            alias.name = name;
            let project = self.public_project(alias)?;
            self.save_aliases(&aliases)?;
            state.aliases = aliases;
            Ok(project)
        })())
    }
    /// Browse an explicit directory; empty selects the managed-projects directory.
    pub async fn project_folders(&self, path: &str) -> Result<ProjectFolders, WorkspaceError> {
        local((|| {
            let root = if path.is_empty() {
                self.inner.data.join("managed-projects")
            } else {
                PathBuf::from(path)
            };
            if path.is_empty() {
                std::fs::create_dir_all(&root)?;
            }
            let root = std::fs::canonicalize(root)?;
            let mut folders = Vec::new();
            for entry in std::fs::read_dir(&root)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    folders.push(ProjectFolder {
                        name: entry.file_name().to_string_lossy().into(),
                        path: entry.path().to_string_lossy().into(),
                    });
                }
            }
            folders.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(ProjectFolders {
                root: root.to_string_lossy().into(),
                path: root.to_string_lossy().into(),
                folders,
            })
        })())
    }
    pub async fn files(&self, project_id: &str, path: &str) -> Result<FileListing, WorkspaceError> {
        let service = self.project_service(project_id)?;
        decode(
            service::project_files(
                &service,
                project_id.into(),
                service::FileQuery {
                    path: path.into(),
                    staged: false,
                },
            )
            .await,
        )
    }
    pub async fn file(&self, project_id: &str, path: &str) -> Result<FileContent, WorkspaceError> {
        let service = self.project_service(project_id)?;
        decode(
            service::project_file(
                &service,
                project_id.into(),
                service::FileQuery {
                    path: path.into(),
                    staged: false,
                },
            )
            .await,
        )
    }
    pub async fn image(
        &self,
        project_id: &str,
        path: &str,
    ) -> Result<rpi_node_api::api::ProjectImage, WorkspaceError> {
        let service = self.project_service(project_id)?;
        let image = service::project_image(
            &service,
            project_id.into(),
            service::FileQuery {
                path: path.into(),
                staged: false,
            },
        )
        .await
        .map_err(|(kind, value)| {
            WorkspaceError::Local(LocalError::Service {
                kind,
                message: value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("project image read failed")
                    .to_owned(),
            })
        })?;
        Ok(rpi_node_api::api::ProjectImage {
            bytes: image.bytes,
            media_type: image.media_type.to_owned(),
        })
    }
    pub async fn git_status(&self, project_id: &str) -> Result<GitStatus, WorkspaceError> {
        let service = self.project_service(project_id)?;
        decode(service::project_status(&service, project_id.into()).await)
    }
    pub async fn diff(
        &self,
        project_id: &str,
        path: &str,
        staged: bool,
    ) -> Result<Diff, WorkspaceError> {
        let service = self.project_service(project_id)?;
        decode(
            service::project_diff(
                &service,
                project_id.into(),
                service::FileQuery {
                    path: path.into(),
                    staged,
                },
            )
            .await,
        )
    }
    pub async fn agent_backends(
        &self,
    ) -> Result<Vec<rpi_node_api::api::AgentBackendOption>, WorkspaceError> {
        local(self.lock().map(|_| ()))?;
        agents::Manager::backends()
    }
    pub async fn session_agent_binding(
        &self,
        id: &str,
    ) -> Result<rpi_node_api::api::SessionAgentBinding, WorkspaceError> {
        local(self.lock().map(|_| ()))?;
        if self.inner.agents.contains(id)? {
            return self.inner.agents.binding(id);
        }
        self.get_session(id).await?;
        Ok(rpi_node_api::api::SessionAgentBinding {
            backend: "rpi".into(),
            external_session_id: None,
        })
    }
    pub async fn create_session_with_backend(
        &self,
        project: Option<&str>,
        backend: &str,
    ) -> Result<String, WorkspaceError> {
        if backend == "rpi" {
            return self.create_session(project).await;
        }
        let _lease = self.inner.lifecycle.read().await;
        local(self.lock().map(|_| ()))?;
        let root = match project {
            Some(id) => local(self.project_root(id))?,
            None => local(
                self.lock()
                    .map(|s| s.aliases.first().map(|a| a.path.clone())),
            )?
            .unwrap_or_else(|| self.inner.control_root.clone()),
        };
        let bound_project = workspace::Project::new(&root).map_err(LocalError::from)?.id;
        self.inner
            .agents
            .create(Some(bound_project), root, backend)
            .await
    }
    pub async fn prepare_agent_session(
        &self,
        id: &str,
    ) -> Result<Option<PreparedAgent>, WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        local(self.lock().map(|_| ()))?;
        if self.inner.agents.contains(id)? {
            return self.inner.agents.prepare(id, false).await.map(Some);
        }
        self.get_session(id).await?;
        Ok(None)
    }
    pub async fn sessions(&self) -> Result<Vec<SessionSummary>, WorkspaceError> {
        #[derive(Deserialize)]
        struct Listing {
            sessions: Vec<SessionSummary>,
        }
        let mut all = Vec::new();
        let aliases = local(self.lock().map(|s| s.aliases.clone()))?;
        for alias in aliases {
            if workspace::Project::new(&alias.path).is_err() {
                continue;
            }
            let service = local(self.service_for_root(&alias.path))?;
            let listing: Listing = decode(
                service::list_sessions(&service, service::SessionListQuery::default()).await,
            )?;
            all.extend(listing.sessions);
        }
        all.extend(self.inner.agents.summaries()?);
        Ok(all)
    }
    pub async fn create_session(&self, project_id: Option<&str>) -> Result<String, WorkspaceError> {
        #[derive(Deserialize)]
        struct Created {
            #[serde(rename = "sessionId")]
            session_id: String,
        }
        let _lease = self.inner.lifecycle.read().await;
        let service = if let Some(id) = project_id {
            self.project_service(id)?
        } else {
            let root = local(
                self.lock()
                    .map(|s| s.aliases.first().map(|a| a.path.clone())),
            )?
            .unwrap_or_else(|| self.inner.control_root.clone());
            local(self.service_for_root(&root))?
        };
        let resources = self.shipped_selection(&service, false).await?;
        let result: Created = decode(
            service::create_session(
                &service,
                service::CreateSessionRequest {
                    project_id: project_id.map(str::to_owned),
                    resources: Some(resources),
                },
            )
            .await,
        )?;
        Ok(result.session_id)
    }
    pub async fn control_session(&self) -> Result<String, WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        let mut control = self.inner.control.lock().await;
        local(self.lock().map(|_| ()))?;
        if let Some(id) = control.as_ref() {
            return Ok(id.clone());
        }
        let service = local(self.service_for_root(&self.inner.control_root))?;
        let registry = local(
            Registry::load(service.config.registry.clone()).map_err(LocalError::Initialization),
        )?;
        if let Some(entry) = registry.entries().first() {
            let _: SessionSummary =
                decode(service::open_session(&service, entry.session_id.clone()).await)?;
            *control = Some(entry.session_id.clone());
        } else {
            let value: Value = decode(
                service::create_session(&service, service::CreateSessionRequest::default()).await,
            )?;
            let id = value
                .get("sessionId")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    WorkspaceError::Local(LocalError::Initialization(
                        "create_session omitted sessionId".into(),
                    ))
                })?;
            *control = Some(id.into());
        }
        control.clone().ok_or_else(|| {
            WorkspaceError::Local(LocalError::Initialization(
                "control session was not assigned".into(),
            ))
        })
    }
    pub async fn get_session(&self, session_id: &str) -> Result<SessionSummary, WorkspaceError> {
        local(self.lock().map(|_| ()))?;
        if self.inner.agents.contains(session_id)? {
            return self.inner.agents.summary(session_id);
        }
        let service = self.session_service(session_id)?;
        let summary = decode(service::get_session(&service, session_id.into()).await)?;
        Self::enrich_summary(&service, summary)
    }

    fn enrich_summary(
        service: &Service,
        mut summary: SessionSummary,
    ) -> Result<SessionSummary, WorkspaceError> {
        let registry = local(
            Registry::load(service.config.registry.clone()).map_err(LocalError::Initialization),
        )?;
        if let Some(entry) = registry.get(&summary.session_id) {
            summary.project_id.clone_from(&entry.project_id);
            let prefs = registry.preferences(&summary.session_id);
            summary.pinned = prefs.pinned;
            summary.archived = prefs.archived;
        }
        Ok(summary)
    }
    pub async fn rename_session(
        &self,
        session_id: &str,
        title: &str,
    ) -> Result<SessionMutation, WorkspaceError> {
        local(self.lock().map(|_| ()))?;
        if self.inner.agents.contains(session_id)? {
            return self.inner.agents.rename(session_id, title);
        }
        let service = self.session_service(session_id)?;
        decode(
            service::rename_session(
                &service,
                session_id.into(),
                service::RenameRequest {
                    title: title.into(),
                },
            )
            .await,
        )
    }
    pub async fn session_preferences(
        &self,
        session_id: &str,
        archived: Option<bool>,
        pinned: Option<bool>,
    ) -> Result<SessionPreferences, WorkspaceError> {
        local(self.lock().map(|_| ()))?;
        if self.inner.agents.contains(session_id)? {
            return self.inner.agents.preferences(session_id, archived, pinned);
        }
        let service = self.session_service(session_id)?;
        decode(
            service::session_metadata(
                &service,
                session_id.into(),
                service::MetadataRequest {
                    archived,
                    pinned,
                    tags: None,
                },
            )
            .await,
        )
    }
    pub async fn fork_session(
        &self,
        session_id: &str,
        entry_id: Option<&str>,
        title: Option<&str>,
    ) -> Result<SessionMutation, WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        if self.inner.agents.contains(session_id)? {
            return Err(unsupported("fork_session", "acp"));
        }
        let service = self.session_service(session_id)?;
        decode(
            service::fork_session(
                &service,
                session_id.into(),
                service::ForkRequest {
                    entry_id: entry_id.map(str::to_owned),
                    title: title.map(str::to_owned),
                },
            )
            .await,
        )
    }
    pub async fn clone_session(
        &self,
        session_id: &str,
        title: Option<&str>,
    ) -> Result<SessionMutation, WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        if self.inner.agents.contains(session_id)? {
            return Err(unsupported("clone_session", "acp"));
        }
        let service = self.session_service(session_id)?;
        decode(
            service::clone_session(
                &service,
                session_id.into(),
                service::ForkRequest {
                    entry_id: None,
                    title: title.map(str::to_owned),
                },
            )
            .await,
        )
    }
    pub async fn delete_session(&self, session_id: &str) -> Result<(), WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        local(self.lock().map(|_| ()))?;
        if self.inner.agents.contains(session_id)? {
            return self.inner.agents.delete(session_id).await;
        }
        let service = self.session_service(session_id)?;
        let _: Value = decode(service::delete_session(&service, session_id.into()).await)?;
        Ok(())
    }
    pub async fn open_session(&self, session_id: &str) -> Result<SessionSummary, WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        local(self.lock().map(|_| ()))?;
        if self.inner.agents.contains(session_id)? {
            self.inner.agents.prepare(session_id, false).await?;
            return self.inner.agents.summary(session_id);
        }
        let service = self.session_service(session_id)?;
        let summary = decode(service::open_session(&service, session_id.into()).await)?;
        Self::enrich_summary(&service, summary)
    }
    pub async fn snapshot(&self, session_id: &str) -> Result<Snapshot, WorkspaceError> {
        local(self.lock().map(|_| ()))?;
        if self.inner.agents.contains(session_id)? {
            return self.inner.agents.snapshot(session_id);
        }
        let service = self.session_service(session_id)?;
        decode(service::snapshot(&service, session_id.into()).await)
    }
    pub async fn snapshot_with_limit(
        &self,
        session_id: &str,
        limit: u64,
    ) -> Result<Snapshot, WorkspaceError> {
        local(self.lock().map(|_| ()))?;
        if self.inner.agents.contains(session_id)? {
            return Err(unsupported("message_pagination", "acp"));
        }
        let limit = usize::try_from(limit)
            .map_err(|_| unsupported("message_pagination", "snapshot limit exceeds usize"))?;
        let service = self.session_service(session_id)?;
        decode(
            service::snapshot_with_options(
                &service,
                session_id.into(),
                service::SnapshotOptions {
                    limit: Some(limit),
                    ..Default::default()
                },
            )
            .await,
        )
    }
    pub async fn command<T: for<'de> Deserialize<'de>>(
        &self,
        session_id: &str,
        payload: Value,
    ) -> Result<HostResponse<T>, WorkspaceError> {
        if self.inner.agents.contains(session_id)? {
            // ACP currently accepts only legacy text, never silently omit blocks.
            if payload.get("contentBlocks").is_some() || payload.get("images").is_some() {
                return Err(unsupported("prompt_content", "acp"));
            }
            let kind = payload["type"].as_str().unwrap_or_default();
            if !matches!(kind, "prompt" | "abort" | "cancel") {
                return Err(unsupported(kind, "acp"));
            }
            let client = self
                .prepare_agent_session(session_id)
                .await?
                .ok_or_else(|| unsupported(kind, "acp"))?;
            let value = client.request(session_id, kind, payload.clone()).await?;
            return Ok(HostResponse {
                response_type: "response".into(),
                id: None,
                command: kind.into(),
                success: true,
                data: Some(local(
                    serde_json::from_value(value).map_err(LocalError::from),
                )?),
                error: None,
            });
        }
        let ordered = payload.get("contentBlocks").is_some();
        if payload.get("images").is_some() {
            return Err(WorkspaceError::Local(LocalError::InvalidInput(
                "images alias is unsupported; use contentBlocks".into(),
            )));
        }
        let command: rpi_extcore::protocol::ApiCommand =
            local(serde_json::from_value(payload.clone()).map_err(LocalError::from))?;
        if !command_allowlist::allowed_commands().any(|allowed| allowed == command.kind) {
            return Err(WorkspaceError::Local(LocalError::Unsupported(command.kind)));
        }
        let lease = self.inner.lifecycle.read().await;
        let service = self.session_service(session_id)?;
        let _: SessionSummary = decode(service::open_session(&service, session_id.into()).await)?;
        let host = service.live_session(session_id).await.ok_or_else(|| {
            WorkspaceError::Local(LocalError::NotFound(format!("live session {session_id}")))
        })?;
        if !host.supports_command(&command.kind) {
            return Err(WorkspaceError::Local(LocalError::Unsupported(format!(
                "host did not advertise {}",
                command.kind
            ))));
        }
        if ordered {
            access::validate_prompt_content(&payload, &host.ready()["promptContent"])?;
            drop(lease);
            // HostSession::send does not validate prompts and replaces id with
            // a UUID. The validator reserves that exact envelope size above.
            let response = host.send(payload).await.map_err(|_| {
                WorkspaceError::Local(LocalError::Service {
                    kind: ReplyKind::HostUnavailable,
                    message: "native ordered prompt unavailable".into(),
                })
            })?;
            return local(serde_json::from_value(response).map_err(LocalError::from));
        }
        // The host is guaranteed live after open, but waiting for its response
        // must not prevent shutdown from stopping an unresponsive child.
        drop(lease);
        // extcore's raw command gate predates these two bounded reads. Use the
        // same live host, never a second process or a transcript fallback.
        if command_allowlist::is_timeline_read(&command.kind)
            && !rpi_extcore::protocol::SUPPORTED_COMMANDS.contains(&command.kind.as_str())
        {
            let request_id = rpi_extcore::protocol::command_id(&command);
            let mut value = local(serde_json::to_value(&command).map_err(LocalError::from))?;
            value["id"] = request_id;
            let response = host.send(value).await.map_err(|_| {
                WorkspaceError::Local(LocalError::Service {
                    kind: ReplyKind::HostUnavailable,
                    message: "native message query unavailable".into(),
                })
            })?;
            return local(serde_json::from_value(response).map_err(LocalError::from));
        }
        decode(service::command(&service, session_id.into(), command).await)
    }
    pub async fn command_data<T: for<'de> Deserialize<'de>>(
        &self,
        session_id: &str,
        command: &str,
        payload: Value,
    ) -> Result<T, WorkspaceError> {
        let response: HostResponse<T> = self
            .command(session_id, payload_with_type(command, payload))
            .await?;
        if !response.success {
            let message = response
                .error
                .unwrap_or_else(|| format!("{command} failed"));
            return Err(WorkspaceError::Local(LocalError::Service {
                // Match the native bounded-read branch pin conflict contract;
                // do not reclassify failures from unrelated raw commands.
                kind: if command_allowlist::is_timeline_read(command)
                    && message.contains("branch_changed:")
                {
                    ReplyKind::Conflict
                } else {
                    ReplyKind::InvalidInput
                },
                message,
            }));
        }
        response.data.ok_or_else(|| {
            WorkspaceError::Local(LocalError::Service {
                kind: ReplyKind::Internal,
                message: format!("{command} returned no data"),
            })
        })
    }
    pub async fn refresh_auth(&self, provider: &str) -> Result<Value, WorkspaceError> {
        let id = self.control_session().await?;
        let service = self.session_service(&id)?;
        let host = service
            .live_session(&id)
            .await
            .ok_or_else(|| WorkspaceError::Local(LocalError::NotFound("control host".into())))?;
        if !host.supports_command("oauth") {
            return Err(WorkspaceError::Local(LocalError::Unsupported(
                "host did not advertise oauth".into(),
            )));
        }
        self.command_data(
            &id,
            "oauth",
            json!({ "provider": provider, "action": "refresh" }),
        )
        .await
    }
    pub async fn subscribe(
        &self,
        session_id: &str,
        after: u64,
    ) -> Result<WorkspaceEventStream, WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        local(self.lock().map(|_| ()))?;
        if self.inner.agents.contains(session_id)? {
            return self
                .inner
                .agents
                .prepare(session_id, false)
                .await?
                .subscribe(session_id, after)
                .await;
        }
        let service = self.session_service(session_id)?;
        let _: SessionSummary = decode(service::open_session(&service, session_id.into()).await)?;
        let subscription = service
            .subscribe_events(session_id, Some(after))
            .await
            .ok_or_else(|| {
                WorkspaceError::Local(LocalError::NotFound(format!("session {session_id}")))
            })?;
        let stream = try_stream! {
            let _service = service;
            yield WireEvent { id: None, seq: None, event_type: "ready".into(), data: subscription.ready };
            if subscription.gap {
                yield WireEvent { id: None, seq: None, event_type: "resync_required".into(), data: json!({"type":"resync_required"}) };
            }
            let events = events::replay_stream(subscription.replay, subscription.receiver, subscription.after);
            futures_util::pin_mut!(events);
            while let Some(item) = events.next().await {
                match item {
                    StreamItem::Event(event) => {
                        let event_type = event.value.get("type").and_then(Value::as_str).unwrap_or("unknown").to_owned();
                        yield WireEvent { id: Some(event.seq.to_string()), seq: Some(event.seq), event_type, data: event.value };
                    }
                    StreamItem::ResyncRequired { dropped } => {
                        yield WireEvent { id: None, seq: None, event_type: "resync_required".into(), data: json!({"type":"resync_required", "dropped": dropped}) };
                    }
                }
            }
        };
        Ok(Box::pin(stream))
    }
    /// Stop all children without closing the project catalog. The same facade
    /// can reopen persisted sessions after cleanup releases its lifecycle lease.
    pub async fn suspend(&self) {
        self.stop(false).await;
    }

    pub async fn shutdown(&self) {
        self.stop(true).await;
    }

    async fn stop(&self, permanent: bool) {
        let inner = self.inner.clone();
        // Spawn before acquiring the lease: cancelling even a queued caller must
        // not abandon cleanup. Reopening serializes behind this exclusive lease.
        let cleanup = tokio::spawn(async move {
            let _lease = inner.lifecycle.clone().write_owned().await;
            if permanent {
                inner.remote_operations.close();
            }
            let services = if let Ok(mut state) = inner.state.lock() {
                state.closed |= permanent;
                state
                    .services
                    .drain()
                    .map(|(_, service)| service)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            // No state or control mutex may be held while awaiting child exit.
            *inner.control.lock().await = None;
            inner.agents.shutdown().await;
            futures_util::future::join_all(services.iter().map(|service| service.shutdown())).await;
        });
        let _ = cleanup.await;
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.remote_operations.close();
        let services = self
            .state
            .get_mut()
            .map(|state| {
                state
                    .services
                    .drain()
                    .map(|(_, service)| service)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let agents = self.agents.clone();
        let cleanup = async move {
            agents.shutdown().await;
            futures_util::future::join_all(services.iter().map(|service| service.shutdown())).await;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(cleanup);
        } else {
            let _ = std::thread::Builder::new()
                .name("local-workspace-cleanup".into())
                .spawn(move || {
                    if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        runtime.block_on(async {
                            let _ = tokio::time::timeout(Duration::from_secs(5), cleanup).await;
                        });
                    }
                });
        }
    }
}

impl LocalWorkspace {
    pub async fn session_branch(&self, session_id: &str) -> Result<SessionBranch, WorkspaceError> {
        self.command_data(session_id, "get_session_branch", Value::Null)
            .await
    }

    pub async fn history(
        &self,
        session_id: &str,
        after_seq: Option<u64>,
        before_seq: Option<u64>,
        limit: u64,
    ) -> Result<History, WorkspaceError> {
        self.command_data(
            session_id,
            "get_history",
            json!({ "afterSeq": after_seq, "beforeSeq": before_seq, "limit": limit }),
        )
        .await
    }

    pub async fn runtime_state(&self, session_id: &str) -> Result<RuntimeState, WorkspaceError> {
        self.command_data(session_id, "get_state", Value::Null)
            .await
    }

    /// Read the explicitly reported active-tools field without allowing the
    /// `RuntimeState` serde default to turn a missing field into an empty list.
    pub async fn active_tools_state(
        &self,
        session_id: &str,
    ) -> Result<Vec<String>, WorkspaceError> {
        let state: Value = self
            .command_data(session_id, "get_state", Value::Null)
            .await?;
        let tools = state
            .get("activeTools")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                WorkspaceError::Local(LocalError::InvalidInput(
                    "get_state omitted activeTools".into(),
                ))
            })?;
        tools
            .iter()
            .map(|tool| {
                tool.as_str().map(str::to_owned).ok_or_else(|| {
                    WorkspaceError::Local(LocalError::InvalidInput(
                        "get_state returned an invalid activeTools entry".into(),
                    ))
                })
            })
            .collect()
    }

    pub async fn models(&self, session_id: &str) -> Result<Vec<ModelDescriptor>, WorkspaceError> {
        #[derive(Deserialize)]
        struct Response {
            models: Vec<ModelDescriptor>,
        }
        Ok(self
            .command_data::<Response>(session_id, "get_available_models", Value::Null)
            .await?
            .models)
    }

    pub async fn auth_status(&self, session_id: &str) -> Result<AuthStatus, WorkspaceError> {
        self.command_data(session_id, "get_auth_status", Value::Null)
            .await
    }

    pub async fn thinking_levels(&self, session_id: &str) -> Result<Vec<String>, WorkspaceError> {
        #[derive(Deserialize)]
        struct Response {
            levels: Vec<String>,
        }
        Ok(self
            .command_data::<Response>(session_id, "get_available_thinking_levels", Value::Null)
            .await?
            .levels)
    }

    pub async fn tools(&self, session_id: &str) -> Result<ToolCatalog, WorkspaceError> {
        self.command_data(session_id, "get_available_tools", Value::Null)
            .await
    }

    pub async fn set_model(
        &self,
        session_id: &str,
        model_id: &str,
    ) -> Result<Value, WorkspaceError> {
        self.command_data(session_id, "set_model", json!({ "modelId": model_id }))
            .await
    }

    pub async fn set_thinking_level(
        &self,
        session_id: &str,
        level: &str,
    ) -> Result<Value, WorkspaceError> {
        self.command_data(session_id, "set_thinking_level", json!({ "level": level }))
            .await
    }

    pub async fn set_active_tools(
        &self,
        session_id: &str,
        tools: &[String],
    ) -> Result<Value, WorkspaceError> {
        if !self.inner.agents.contains(session_id)? {
            let service = self.session_service(session_id)?;
            if service
                .resource_configuration(session_id)
                .await
                .map_err(resources::resource_error)?
                .is_some()
                || !service
                    .resource_operations(session_id)
                    .await
                    .map_err(resources::resource_error)?
                    .is_empty()
            {
                return Err(unsupported(
                    "set_active_tools: use versioned exact resource apply",
                    "rpi-exact",
                ));
            }
        }
        self.command_data(session_id, "set_active_tools", json!({ "tools": tools }))
            .await
    }

    pub async fn resume_pending(&self, session_id: &str) -> Result<Value, WorkspaceError> {
        self.command_data(session_id, "resume_pending", Value::Null)
            .await
    }

    pub async fn compact(
        &self,
        session_id: &str,
        custom_instructions: Option<&str>,
    ) -> Result<Value, WorkspaceError> {
        self.command_data(
            session_id,
            "compact",
            json!({ "customInstructions": custom_instructions }),
        )
        .await
    }

    pub async fn queue(
        &self,
        session_id: &str,
        include_completed: bool,
    ) -> Result<QueueSnapshot, WorkspaceError> {
        self.command_data(
            session_id,
            "get_queue",
            json!({ "includeCompleted": include_completed }),
        )
        .await
    }

    pub async fn queue_message(
        &self,
        session_id: &str,
        message: &str,
        deliver_as: &str,
    ) -> Result<QueueMutation, WorkspaceError> {
        self.command_data(
            session_id,
            "queue_message",
            json!({ "message": message, "deliverAs": deliver_as }),
        )
        .await
    }

    pub async fn cancel_queued(
        &self,
        session_id: &str,
        entry_id: &str,
    ) -> Result<QueueCancellation, WorkspaceError> {
        self.command_data(session_id, "cancel_queued", json!({ "entryId": entry_id }))
            .await
    }

    pub async fn clear_queue(&self, session_id: &str) -> Result<Value, WorkspaceError> {
        self.command_data(session_id, "clear_queue", Value::Null)
            .await
    }

    pub async fn dialog_response(
        &self,
        session_id: &str,
        request_id: &str,
        value: Value,
    ) -> Result<Value, WorkspaceError> {
        if let Some(result) = self
            .agent_dialog_response(session_id, request_id, Some(&value))
            .await?
        {
            return Ok(result);
        }
        self.command_data(
            session_id,
            "ui_dialog_response",
            json!({ "requestId": request_id, "answer": value }),
        )
        .await
    }

    pub async fn cancel_dialog(
        &self,
        session_id: &str,
        request_id: &str,
    ) -> Result<Value, WorkspaceError> {
        if let Some(result) = self
            .agent_dialog_response(session_id, request_id, None)
            .await?
        {
            return Ok(result);
        }
        self.command_data(
            session_id,
            "ui_dialog_response",
            json!({ "requestId": request_id, "cancelled": true }),
        )
        .await
    }

    pub async fn prompt(
        &self,
        session_id: &str,
        message: &str,
    ) -> Result<HostResponse<Value>, WorkspaceError> {
        self.command(session_id, json!({ "type": "prompt", "message": message }))
            .await
    }

    pub async fn prompt_content(
        &self,
        session_id: &str,
        content_blocks: &Value,
    ) -> Result<HostResponse<Value>, WorkspaceError> {
        self.command(
            session_id,
            json!({"type": "prompt", "contentBlocks": content_blocks}),
        )
        .await
    }

    pub async fn abort(&self, session_id: &str) -> Result<HostResponse<Value>, WorkspaceError> {
        self.command(session_id, json!({ "type": "abort" })).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "rpi-local-workspace-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(std::fs::canonicalize(path).unwrap())
        }
        fn client(&self) -> LocalWorkspace {
            LocalWorkspace::new("not-started-by-project-tests".into(), self.0.join("data")).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn run<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }
    #[test]
    fn arbitrary_existing_directory_aliases_survive_rename_and_restart() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("outside-managed-root");
        std::fs::create_dir(&directory).unwrap();
        let client = fixture.client();
        let project =
            run(client.create_project("A friendly alias", directory.to_str(), false)).unwrap();
        assert_eq!(project.id, workspace::Project::new(&directory).unwrap().id);
        assert_eq!(project.name, "A friendly alias");
        assert!(
            client.inner.state.lock().unwrap().services.is_empty(),
            "registration must not create a service or launch a host"
        );
        let renamed = run(client.rename_project(&project.id, "Renamed alias")).unwrap();
        assert_eq!(project.id, renamed.id);
        assert_eq!(project.path, renamed.path);
        drop(client);
        let restarted = fixture.client();
        assert_eq!(run(restarted.projects()).unwrap(), vec![renamed]);
        assert!(
            directory.is_dir(),
            "alias rename must not move the directory"
        );
    }
    #[test]
    fn canonical_duplicate_registration_reuses_original_alias() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("existing");
        std::fs::create_dir(&directory).unwrap();
        let client = fixture.client();
        let first = run(client.create_project("First", directory.to_str(), false)).unwrap();
        let alternate = directory.join(".");
        assert_eq!(
            run(client.create_project("Second", alternate.to_str(), false)).unwrap(),
            first
        );
        assert_eq!(run(client.projects()).unwrap().len(), 1);
    }
    #[test]
    fn explicit_creation_and_managed_creation_are_distinct_from_open() {
        let fixture = Fixture::new();
        let client = fixture.client();
        let explicit = fixture.0.join("explicit-new-directory");
        let created =
            run(client.create_project_directory("Explicit alias", &explicit, false)).unwrap();
        assert!(explicit.is_dir());
        assert_eq!(created.name, "Explicit alias");
        assert!(run(client.create_project_directory("No overwrite", &explicit, false)).is_err());
        let managed = run(client.create_project("Managed", None, false)).unwrap();
        assert!(Path::new(&managed.path).is_dir());
        let missing = fixture.0.join("not-created-by-open");
        assert!(run(client.create_project("Missing", missing.to_str(), false)).is_err());
        assert!(!missing.exists());
        assert!(
            run(client.create_project_directory("Relative", Path::new("relative-project"), false))
                .is_err()
        );
        assert_eq!(run(client.projects()).unwrap().len(), 2);
    }
    #[test]
    fn failed_registration_rolls_back_only_new_directory() {
        let fixture = Fixture::new();
        let client = fixture.client();
        // Force alias persistence to fail after directory creation.
        std::fs::create_dir(fixture.0.join("data/rpc/projects.json")).unwrap();
        let path = fixture.0.join("rolled-back");
        assert!(run(client.create_project_directory("Alias", &path, false)).is_err());
        assert!(!path.exists());
        assert_eq!(run(client.projects()).unwrap().len(), 0);
        let existing = fixture.0.join("existing");
        std::fs::create_dir(&existing).unwrap();
        std::fs::write(existing.join("keep.txt"), "untouched").unwrap();
        assert!(run(client.create_project_directory("Alias", &existing, true)).is_err());
        assert_eq!(
            std::fs::read_to_string(existing.join("keep.txt")).unwrap(),
            "untouched"
        );
    }

    #[test]
    fn offline_summary_preserves_registry_project_and_preferences() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("project");
        std::fs::create_dir(&directory).unwrap();
        let client = fixture.client();
        let project = run(client.create_project("Alias", directory.to_str(), false)).unwrap();
        let service = client.project_service(&project.id).unwrap();
        std::fs::write(
            &service.config.registry,
            serde_json::to_vec(&json!({
                "sessions": [{"sessionId": "offline", "projectId": project.id, "title": "Title"}],
                "preferences": {"offline": {"pinned": true, "archived": true}}
            }))
            .unwrap(),
        )
        .unwrap();
        // A new service must load the on-disk registry, not a fabricated host.
        run(client.shutdown());
        let restarted = fixture.client();
        let summary = run(restarted.get_session("offline")).unwrap();
        assert_eq!(summary.project_id, Some(project.id));
        assert!(summary.archived && summary.pinned);
        assert_eq!(summary.title.as_deref(), Some("Title"));
        assert_eq!(summary.status, "offline");
        run(restarted.shutdown());
    }

    #[test]
    fn shutdown_waits_for_host_start_lease_and_is_permanent() {
        run(async {
            let fixture = Fixture::new();
            let client = fixture.client();
            let lease = client.inner.lifecycle.read().await;
            let shutdown = client.shutdown();
            tokio::pin!(shutdown);
            assert!(
                tokio::time::timeout(Duration::from_millis(10), &mut shutdown)
                    .await
                    .is_err()
            );
            assert!(client.health().await.is_ok());
            drop(lease);
            shutdown.await;
            client.shutdown().await;
            assert!(matches!(
                client.create_session(None).await,
                Err(WorkspaceError::Local(LocalError::Shutdown))
            ));
            assert!(
                fixture.client().health().await.is_ok(),
                "restart requires a new facade"
            );
        });
    }

    #[test]
    fn services_use_explicit_canonical_roots_and_per_project_registries() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("project");
        std::fs::create_dir(&directory).unwrap();
        let client = fixture.client();
        let project = run(client.create_project("Alias", directory.to_str(), false)).unwrap();
        let service = client.project_service(&project.id).unwrap();
        assert_eq!(
            service.config.project,
            std::fs::canonicalize(directory).unwrap()
        );
        assert_eq!(
            service.config.registry,
            fixture
                .0
                .join("data/rpc/projects")
                .join(project.id)
                .join("sessions.json")
        );
        run(client.shutdown());
        assert!(matches!(
            run(client.projects()),
            Err(WorkspaceError::Local(LocalError::Shutdown))
        ));
    }
}
