use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use std::{path::PathBuf, sync::Arc};
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
    /// Idle timeout for live native hosts, in seconds. Zero/None disables it.
    pub session_timeout_seconds: Option<u64>,
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
            registry: rpi_extcore::registry::default_path(),
            session_timeout_seconds: Some(30 * 60),
        }
    }
}

#[derive(Clone)]
struct FrontendState {
    config: Arc<AppConfig>,
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
    let asset_root = asset_root(&config);
    let files = ServeDir::new(asset_root.join("assets"));
    let files = if config.assets.is_none() {
        files.fallback(get(crate::assets::asset))
    } else {
        files.fallback(get(|| async { StatusCode::NOT_FOUND }))
    };
    let frontend = Router::new()
        .route("/", get(index))
        .nest_service("/assets", files)
        .route("/favicon.svg", get(favicon))
        .with_state(FrontendState {
            config: Arc::new(config.clone()),
        });
    let http = rpi_httpcore::AppConfig {
        service_name: "rpi-web",
        project: config.project.clone(),
        projects: config.projects.clone(),
        projects_root: config.projects_root.clone(),
        rpi_bin: config.rpi_bin.clone(),
        bind: config.bind.clone(),
        port: config.port,
        allowed_hosts: config.allowed_hosts.clone(),
        session_dir: config.session_dir.clone(),
        registry: config.registry.clone(),
        session_timeout_seconds: config.session_timeout_seconds,
        admission_limit: 64,
    };
    rpi_httpcore::serve_until(
        http,
        frontend,
        move |info| {
            println!(
                "{}",
                crate::startup::page(info.address, &config, &info.hosts, info.password_enabled)
            )
        },
        stop,
    )
    .await
    .map_err(|error| error as Box<dyn std::error::Error>)
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
async fn index(State(state): State<FrontendState>) -> Html<String> {
    // Read the bundle manifest on each request so a frontend rebuild does not
    // require recompiling the Rust server. The embedded copy is the fallback
    // for a binary shipped without its `assets/` directory.
    let path = asset_root(&state.config).join("index.html");
    let html = tokio::fs::read_to_string(path)
        .await
        .unwrap_or_else(|_| crate::assets::index_html().to_string());
    Html(html)
}

async fn favicon(State(state): State<FrontendState>) -> axum::response::Response {
    match tokio::fs::read(asset_root(&state.config).join("favicon.svg")).await {
        Ok(bytes) => ([(axum::http::header::CONTENT_TYPE, "image/svg+xml")], bytes).into_response(),
        Err(_) if state.config.assets.is_none() => crate::assets::favicon().await,
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}
