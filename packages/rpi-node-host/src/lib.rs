//! GUI-independent ownership of the local RPC workspace and its HTTP entrance.
//!
//! Native desktop/service hosts only. Call `start` once per process and retain
//! the guard for the entire application lifetime. Web/Android are remote clients.
#![allow(clippy::missing_errors_doc)]

use rpi_node_core::workspace::LocalWorkspace;
use std::sync::Arc;
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};
type NodeConnection = Arc<LocalWorkspace>;
use rpi_node_core::local::LocalRuntime;

mod components;
mod headless;
pub mod http;
mod ready_output;
mod shutdown;
pub use headless::{run_extension, run_headless, run_headless_with};
pub use ready_output::report_error;

/// Explicit launch configuration; credentials are intentionally not Debug/Serialize.
#[derive(Default)]
pub struct LaunchOverrides {
    pub rpi_bin: Option<std::path::PathBuf>,
    pub data: Option<std::path::PathBuf>,
    pub http: http::LaunchOverrides,
    /// Parent/child capability, never part of public ready output or argv.
    pub control_capability: Option<String>,
}

static STARTED: AtomicBool = AtomicBool::new(false);
static CONNECTION: OnceLock<NodeConnection> = OnceLock::new();
static STARTUP_ERROR: OnceLock<String> = OnceLock::new();

/// Owns the data lock and HTTP executor for exactly one local workspace.
#[must_use = "retain this guard until application shutdown"]
pub struct LocalGuard {
    lock: Option<LocalRuntime>,
    http: Option<rpi_http_node::HttpServer>,
    control: Option<rpi_http_node::HttpServer>,
    client: NodeConnection,
    node_id: String,
}

fn claim_start(started: &AtomicBool) -> Result<(), String> {
    started
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| "The local host can only be started once per process".into())
}

/// Start the shared owner. HTTP startup failure is exposed by `http::sharing()`;
/// the GUI may still use local RPC. Headless mode treats HTTP failure as fatal.
/// A second call is rejected before resolving paths or creating an owner; even a
/// failed first attempt consumes the process's single startup opportunity.
#[must_use]
pub fn start() -> Option<LocalGuard> {
    start_with(LaunchOverrides::default())
}

pub fn start_with(options: LaunchOverrides) -> Option<LocalGuard> {
    let result = claim_start(&STARTED).and_then(|()| {
        let runtime = LocalRuntime::prepare_with(options.rpi_bin, options.data)?;
        let client = Arc::new(
            LocalWorkspace::new(runtime.rpi.clone(), runtime.data.clone())
                .map_err(|error| error.to_string())?,
        );
        let data = runtime.data.clone();
        let mut guard = LocalGuard {
            lock: Some(runtime),
            http: None,
            control: None,
            client,
            node_id: String::new(),
        };
        // Construct the guard first so an unwinding startup still retains the
        // data lock through cleanup, even before CONNECTION is published.
        guard.node_id = http::node_identity(&data)?;
        guard.http = http::start(&guard.client, &data, &options.http);
        if let Some(capability) = options.control_capability {
            let node_id = guard.node_id.clone();
            guard.control = Some(rpi_http_node::HttpServer::start_control(
                guard.client.clone(),
                rpi_http_node::ServerConfig {
                    bind: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
                    password: capability.clone(),
                    allowed_hosts: vec![],
                    node_id,
                    assets: None,
                    web_proxy: false,
                    allow_sharing: true,
                },
                capability,
            )?);
        }
        if let Err(error) = components::register(&guard.client) {
            eprintln!("Component registration failed: {error}");
        }
        let _ = CONNECTION.set(guard.client.clone());
        Ok(guard)
    });
    match result {
        Ok(guard) => Some(guard),
        Err(error) => {
            let _ = STARTUP_ERROR.set(error);
            None
        }
    }
}

/// The exact local facade passed to the HTTP server, never a second owner.
pub fn client() -> Option<NodeConnection> {
    CONNECTION.get().cloned()
}

pub fn startup_error() -> Option<&'static str> {
    STARTUP_ERROR.get().map(String::as_str)
}

impl LocalGuard {
    #[must_use]
    pub fn node_id(&self) -> &str {
        &self.node_id
    }
    #[must_use]
    pub fn control_address(&self) -> Option<String> {
        self.control
            .as_ref()
            .map(|server| format!("http://{}/", server.address()))
    }
    /// Compatibility entry point for GUI lifecycle hooks. Failures are reported
    /// to stderr; service callers must use `checked_shutdown` and propagate Err.
    pub fn shutdown(self) {
        if let Err(error) = self.checked_shutdown() {
            eprintln!("{error}");
        }
    }

    /// Wait at most ten seconds, without driving an executor on itself.
    /// Success means HTTP drained, workspace children stopped, HTTP executor
    /// dropped, then the `LocalRuntime` lock released, in exactly that order.
    /// On timeout the cleanup worker keeps ownership until cleanup completes;
    /// on executor/drain failure or panic ownership is retained until exit.
    pub fn checked_shutdown(self) -> Result<(), String> {
        shutdown::run_retaining(self, std::time::Duration::from_secs(10), |guard| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| "Unable to create shutdown executor; data lock retained until exit")?;
            runtime.block_on(async {
                if let Some(server) = guard.http.as_mut() {
                    server.drain().await?;
                }
                if let Some(server) = guard.control.as_mut() {
                    server.drain().await?;
                }
                guard.client.shutdown().await;
                Ok::<(), String>(())
            })?;
            // Do not drop the HTTP runtime while workspace children can still
            // depend on tasks running there. On drain failure retain both.
            drop(guard.http.take());
            drop(guard.control.take());
            drop(runtime);
            drop(guard.lock.take());
            http::mark_stopped();
            Ok(())
        })
    }
}

impl Drop for LocalGuard {
    fn drop(&mut self) {
        if let Some(lock) = self.lock.take() {
            Self {
                lock: Some(lock),
                http: self.http.take(),
                control: self.control.take(),
                client: self.client.clone(),
                node_id: self.node_id.clone(),
            }
            .shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_start_claim_can_succeed_even_after_failure_or_stop() {
        let started = AtomicBool::new(false);
        assert!(claim_start(&started).is_ok());
        assert!(claim_start(&started).is_err());
        assert!(claim_start(&started).is_err());
    }

    #[test]
    fn simultaneous_start_claims_cannot_create_two_owners() {
        let started = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let first = scope.spawn(|| claim_start(&started).is_ok());
            let second = scope.spawn(|| claim_start(&started).is_ok());
            assert_ne!(first.join().unwrap(), second.join().unwrap());
        });
    }
}
