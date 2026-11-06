//! Authenticated HTTP access to an existing desktop workspace. Never a second owner.
use rpi_node_core::workspace::LocalWorkspace;
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::sync::{mpsc, oneshot, watch};
use tower_http::compression::CompressionLayer;
mod executor;
mod realtime;
mod routes;
pub(crate) use rpi_http_runtime::security;
pub use rpi_http_runtime::{QrAuthorization, SharingControl, public_address, random_secret};

#[derive(Clone)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub password: String,
    pub allowed_hosts: Vec<String>,
    pub node_id: String,
    pub assets: Option<PathBuf>,
    /// Declare a hosted Web entrance served by a same-origin reverse proxy.
    pub web_proxy: bool,
    /// Host policy supplied by the owner (saved preference or first-run default).
    pub allow_sharing: bool,
}
pub(crate) struct ServerState {
    pub workspace: Arc<LocalWorkspace>,
    pub access: Arc<rpi_http_runtime::AccessState>,
    pub assets: Option<PathBuf>,
    pub web_proxy: bool,
    pub requests: mpsc::Sender<executor::ExecutorRequest>,
    pub admission: Arc<tokio::sync::Semaphore>,
    pub realtime: Arc<realtime::RealtimeState>,
}
impl std::ops::Deref for ServerState {
    type Target = rpi_http_runtime::AccessState;
    fn deref(&self) -> &Self::Target {
        &self.access
    }
}
/// A desktop-lifetime executor. Listener shutdown leaves the executor alive;
/// only drop this guard AFTER workspace.shutdown().await has stopped children.
pub struct HttpServer {
    shutdown: watch::Sender<bool>,
    drained: watch::Receiver<bool>,
    finish: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    address: SocketAddr,
    sharing: SharingControl,
    pairing: Arc<rpi_http_runtime::pairing::Pairing>,
    realtime: Arc<realtime::RealtimeState>,
}
impl HttpServer {
    /// # Errors
    /// Returns an error for invalid configuration, unavailable assets or bind failure.
    pub fn start(workspace: Arc<LocalWorkspace>, config: ServerConfig) -> Result<Self, String> {
        Self::start_inner(workspace, config, None, None)
    }
    pub fn start_with_password_digest(
        workspace: Arc<LocalWorkspace>,
        config: ServerConfig,
        digest: [u8; 32],
    ) -> Result<Self, String> {
        Self::start_inner(workspace, config, None, Some(digest))
    }
    /// Private parent/client entrance to the SAME owner. Public sharing policy
    /// cannot disable this listener or grant its component privileges.
    pub fn start_control(
        workspace: Arc<LocalWorkspace>,
        config: ServerConfig,
        capability: String,
    ) -> Result<Self, String> {
        if !config.bind.ip().is_loopback() || config.assets.is_some() || config.web_proxy {
            return Err("Local control must bind loopback without Web assets".into());
        }
        Self::start_inner(workspace, config, Some(capability), None)
    }
    fn start_inner(
        workspace: Arc<LocalWorkspace>,
        mut config: ServerConfig,
        control: Option<String>,
        password_digest: Option<[u8; 32]>,
    ) -> Result<Self, String> {
        if config.password.is_empty() && password_digest.is_none() {
            return Err("HTTP password must not be empty".into());
        }
        if config.node_id.is_empty() {
            return Err("node identity must not be empty".into());
        }
        if let Some(root) = &config.assets {
            let root = root
                .canonicalize()
                .map_err(|_| "HTTP assets directory is unavailable")?;
            if !root.is_dir() {
                return Err("HTTP assets path is not a directory".into());
            }
            config.assets = Some(root);
        }
        let sharing = SharingControl::new(control.is_some() || config.allow_sharing);
        let access = sharing.clone();
        let pairing = Arc::new(rpi_http_runtime::pairing::Pairing::new(
            config.node_id.clone(),
        ));
        let pairing_state = pairing.clone();
        let realtime = Arc::new(realtime::RealtimeState::new());
        let realtime_state = realtime.clone();
        let (shutdown, stopped) = watch::channel(false);
        let (drained_tx, drained) = watch::channel(false);
        let (finish, finished) = oneshot::channel();
        let (ready, prepared) = std::sync::mpsc::sync_channel(1);
        let thread=std::thread::Builder::new().name("desktop-workspace-http".into()).spawn(move || {
            let runtime=match tokio::runtime::Builder::new_current_thread().enable_all().build(){
                Ok(rt)=>rt,Err(e)=>{let _=ready.send(Err(e.to_string()));return;}
            };
            runtime.block_on(tokio::task::LocalSet::new().run_until(async move {
                let listener=match tokio::net::TcpListener::bind(config.bind).await {
                    Ok(l)=>l,Err(e)=>{let _=ready.send(Err(format!("桌面 HTTP 绑定失败（不会连接占用端口的其他节点）：{e}")));return;}
                };
                let address=match listener.local_addr(){Ok(a)=>a,Err(e)=>{let _=ready.send(Err(e.to_string()));return;}};
                config.allowed_hosts.push(address.to_string());
                if config.bind.port()==0 {for host in &mut config.allowed_hosts {if let Some(name)=host.strip_suffix(":0"){*host=format!("{name}:{}",address.port());}}}
                if address.ip().is_loopback(){config.allowed_hosts.push(format!("localhost:{}",address.port()));}
                let security=match control {
                    Some(capability)=>security::Security::new_control(&capability,config.allowed_hosts,&config.node_id,access.snapshot()),
                    None=>match password_digest {Some(digest)=>security::Security::from_password_digest(digest,config.allowed_hosts,&config.node_id),None=>security::Security::new(&config.password,config.allowed_hosts,&config.node_id)},
                };
                let security=match security{Ok(s)=>s,Err(e)=>{let _=ready.send(Err(e));return;}};
                let (requests,mut pending)=mpsc::channel::<executor::ExecutorRequest>(128);
                let state=Arc::new(ServerState{workspace,access:Arc::new(rpi_http_runtime::AccessState{security,shutdown:stopped.clone(),node_id:config.node_id,sharing:access,pairing:pairing_state}),assets:config.assets,web_proxy:config.web_proxy,requests,admission:Arc::new(tokio::sync::Semaphore::new(64)),realtime:realtime_state});
                let worker_state=state.clone();
                let (listener_done,mut listener_closed)=watch::channel(false);
                tokio::task::spawn_local(async move {
                    let mut tasks=tokio::task::JoinSet::new();
                    loop {
                        tokio::select! {
                            biased;
                            _=listener_closed.changed()=>break,
                            Some(_)=tasks.join_next(), if !tasks.is_empty()=>{},
                            Some(request)=pending.recv()=>{
                                let state=worker_state.clone();
                                tasks.spawn_local(async move {executor::execute(state,request).await;});
                            }
                        }
                    }
                    pending.close();
                    while let Some(request)=pending.recv().await { executor::execute(worker_state.clone(),request).await; }
                    // Client cancellation may have dropped its reply receiver;
                    // do not shut down the workspace before the underlying RPC
                    // request has finished too.
                    while tasks.join_next().await.is_some() {}
                    let _=drained_tx.send(true);
                });
                let app=routes::router(state.access.clone())
                    .layer(CompressionLayer::new().br(true).gzip(true))
                    .layer(axum::middleware::from_fn_with_state(state.access.clone(),security::guard))
                    .with_state(state);
                let serving=tokio::spawn(async move {
                    let mut stopped=stopped;
                    let _=axum::serve(listener,app).with_graceful_shutdown(async move {
                        if !*stopped.borrow(){let _=stopped.changed().await;}
                    }).await;
                    let _=listener_done.send(true);
                });
                let _=ready.send(Ok(address));
                let _=finished.await;
                serving.abort();
            }));
        }).map_err(|e|e.to_string())?;
        match prepared
            .recv()
            .map_err(|_| "HTTP startup thread exited".to_string())?
        {
            Ok(address) => Ok(Self {
                shutdown,
                drained,
                finish: Some(finish),
                thread: Some(thread),
                address,
                sharing,
                pairing,
                realtime,
            }),
            Err(e) => {
                let _ = thread.join();
                Err(e)
            }
        }
    }
    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    #[must_use]
    pub fn sharing_control(&self) -> SharingControl {
        self.sharing.clone()
    }
    /// Explicit owner-requested delegation. No HTTP sharing control is exposed.
    /// # Errors
    /// Fails when sharing is disabled, shutdown has begun, or bounded capacity is exhausted.
    pub fn issue_qr_authorization(&self) -> Result<QrAuthorization, String> {
        if *self.shutdown.borrow() {
            return Err("HTTP server is shutting down".into());
        }
        self.pairing.issue(&self.sharing, self.sharing.snapshot())
    }
    /// Owner-only issuer without transferring ownership of the server guard.
    /// Issuance still checks shutdown and the current sharing generation.
    #[must_use]
    pub fn qr_authorization_issuer(
        &self,
    ) -> Arc<dyn Fn() -> Result<QrAuthorization, String> + Send + Sync> {
        let pairing = self.pairing.clone();
        let sharing = self.sharing.clone();
        let shutdown = self.shutdown.subscribe();
        Arc::new(move || {
            if *shutdown.borrow() {
                return Err("HTTP server is shutting down".into());
            }
            pairing.issue(&sharing, sharing.snapshot())
        })
    }
    pub fn begin_shutdown(&self) {
        let _ = self.shutdown.send(true);
    }
    /// # Errors
    /// Returns an error if active owner operations or upgraded WS connections
    /// cannot drain within five seconds.
    pub async fn drain(&mut self) -> Result<(), String> {
        self.begin_shutdown();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !*self.drained.borrow() {
                self.drained
                    .changed()
                    .await
                    .map_err(|_| "HTTP executor exited")?;
            }
            self.realtime.wait_closed().await.map_err(str::to_owned)
        })
        .await
        .map_err(|_| "HTTP drain timeout".to_string())?
    }
}
impl Drop for HttpServer {
    fn drop(&mut self) {
        self.begin_shutdown();
        if let Some(finish) = self.finish.take() {
            let _ = finish.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
