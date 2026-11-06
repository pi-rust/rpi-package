//! Desktop runtime resolution, workspace ownership, and optional HTTP supervision.
#![allow(clippy::missing_errors_doc)]
//!
//! Native GUI/headless hosts retain `local::LocalRuntime` around the same RPC
//! workspace. `work-host` attaches its optional in-process HTTP adapter; this crate
//! does not start that listener. The compatibility manager owns only processes it
//! started; connecting to an existing node remains `work-client`'s responsibility.

use std::{
    ffi::OsString,
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;

pub mod desktop;
pub mod local;
#[cfg(not(target_arch = "wasm32"))]
pub mod runtime;
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
pub mod workspace;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeBackend {
    /// API-only standalone service, preferred for the Dioxus client.
    RpiHttp,
    /// API plus the bundled browser workspace.
    RpiWeb,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeMode {
    Standalone,
    Extension,
}

impl NodeBackend {
    #[must_use]
    pub fn executable(self) -> &'static str {
        match self {
            Self::RpiHttp => "rpi-http",
            Self::RpiWeb => "rpi-web",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeConfig {
    pub executable: PathBuf,
    pub mode: NodeMode,
    pub project: PathBuf,
    pub bind: String,
    pub port: u16,
    pub rpi_bin: Option<PathBuf>,
    pub assets: Option<PathBuf>,
    pub registry: Option<PathBuf>,
}

impl NodeConfig {
    #[must_use]
    pub fn standalone(executable: impl Into<PathBuf>, project: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            mode: NodeMode::Standalone,
            project: project.into(),
            bind: "127.0.0.1".into(),
            port: 13030,
            rpi_bin: None,
            assets: None,
            registry: None,
        }
    }

    #[must_use]
    pub fn for_backend(backend: NodeBackend, project: impl Into<PathBuf>) -> Self {
        Self::standalone(backend.executable(), project)
    }

    #[must_use]
    pub fn rpi_http(project: impl Into<PathBuf>) -> Self {
        Self::for_backend(NodeBackend::RpiHttp, project)
    }

    #[must_use]
    pub fn rpi_web(project: impl Into<PathBuf>) -> Self {
        Self::for_backend(NodeBackend::RpiWeb, project)
    }

    #[must_use]
    pub fn arguments(&self) -> Vec<OsString> {
        let mut args = Vec::new();
        if self.mode == NodeMode::Extension {
            args.push("--webserver".into());
        }
        args.extend(["--project".into(), self.project.as_os_str().to_owned()]);
        args.extend(["--bind".into(), self.bind.clone().into()]);
        args.extend(["--port".into(), self.port.to_string().into()]);
        if let Some(path) = &self.rpi_bin {
            args.extend(["--rpi-bin".into(), path.as_os_str().to_owned()]);
        }
        if let Some(path) = &self.assets {
            args.extend(["--assets".into(), path.as_os_str().to_owned()]);
        }
        if let Some(path) = &self.registry {
            args.extend(["--registry".into(), path.as_os_str().to_owned()]);
        }
        args
    }

    pub fn endpoint(&self) -> Result<SocketAddr, NodeError> {
        (self.bind.as_str(), self.port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| NodeError::InvalidEndpoint(self.bind.clone(), self.port))
    }
}

#[derive(Debug, Error)]
pub enum NodeError {
    #[error("node is already running")]
    AlreadyRunning,
    #[error("node is not managed by this process")]
    NotRunning,
    #[error("node endpoint {0}:{1} could not be resolved")]
    InvalidEndpoint(String, u16),
    #[error("node process exited before becoming ready: {0:?}")]
    Exited(Option<i32>),
    #[error("node did not become ready within {0:?}")]
    ReadyTimeout(Duration),
    #[error("I/O error while managing node: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug)]
pub struct NodeManager {
    config: NodeConfig,
    child: Option<Child>,
}

impl NodeManager {
    #[must_use]
    pub fn new(config: NodeConfig) -> Self {
        Self {
            config,
            child: None,
        }
    }

    #[must_use]
    pub fn config(&self) -> &NodeConfig {
        &self.config
    }

    pub fn start(&mut self) -> Result<(), NodeError> {
        self.start_with(&[], &[])
    }

    pub fn start_with(
        &mut self,
        extra_arguments: &[OsString],
        environment: &[(OsString, OsString)],
    ) -> Result<(), NodeError> {
        if self.child.is_some() {
            return Err(NodeError::AlreadyRunning);
        }
        let mut command = Command::new(&self.config.executable);
        command
            .args(self.config.arguments())
            .args(extra_arguments)
            .envs(environment.iter().cloned())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        self.child = Some(command.spawn()?);
        Ok(())
    }

    pub fn wait_ready(&mut self, timeout: Duration) -> Result<(), NodeError> {
        let endpoint = self.config.endpoint()?;
        let deadline = Instant::now() + timeout;
        if self.child.is_none() {
            return Err(NodeError::NotRunning);
        }
        loop {
            // Check the owned child before probing the socket. Otherwise an
            // unrelated process already listening on the configured port can
            // make a failed child look ready.
            if let Some(child) = &mut self.child
                && let Some(status) = child.try_wait()?
            {
                self.child = None;
                return Err(NodeError::Exited(status.code()));
            }
            if TcpStream::connect_timeout(&endpoint, Duration::from_millis(100)).is_ok() {
                // Re-check the child after the socket probe to close the small
                // race where a process exits while connect() is in flight.
                if let Some(child) = &mut self.child
                    && let Some(status) = child.try_wait()?
                {
                    self.child = None;
                    return Err(NodeError::Exited(status.code()));
                }
                return Ok(());
            }
            if self.child.is_none() {
                return Err(NodeError::NotRunning);
            }
            if Instant::now() >= deadline {
                return Err(NodeError::ReadyTimeout(timeout));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn stop(&mut self) -> Result<(), NodeError> {
        let Some(mut child) = self.child.take() else {
            return Err(NodeError::NotRunning);
        };
        child.kill()?;
        child.wait()?;
        Ok(())
    }

    pub fn is_running(&mut self) -> Result<bool, NodeError> {
        let Some(child) = &mut self.child else {
            return Ok(false);
        };
        Ok(child.try_wait()?.is_none())
    }

    pub fn managed_endpoint(&self) -> Result<SocketAddr, NodeError> {
        self.config.endpoint()
    }
}

impl Drop for NodeManager {
    fn drop(&mut self) {
        // Do not leave a locally-owned server behind when a desktop client is
        // closed. Errors are intentionally ignored during process teardown.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn rpi_http_is_the_default_api_backend_constructor() {
        let config = NodeConfig::rpi_http("D:/Projects/demo");
        assert_eq!(config.executable, Path::new("rpi-http"));
        assert_eq!(config.mode, NodeMode::Standalone);
        assert_eq!(config.port, 13030);
        assert_eq!(NodeBackend::RpiHttp.executable(), "rpi-http");
    }
    #[test]
    fn standalone_arguments_are_explicit_and_ordered() {
        let mut config = NodeConfig::standalone("rpi-web", "D:/Projects/demo");
        config.port = 4317;
        config.rpi_bin = Some(Path::new("C:/Tools/rpi.exe").into());
        assert_eq!(
            config.arguments(),
            vec![
                OsString::from("--project"),
                "D:/Projects/demo".into(),
                "--bind".into(),
                "127.0.0.1".into(),
                "--port".into(),
                "4317".into(),
                "--rpi-bin".into(),
                "C:/Tools/rpi.exe".into(),
            ]
        );
    }

    #[test]
    fn extension_mode_uses_webserver_switch() {
        let mut config = NodeConfig::standalone("rpi", ".");
        config.mode = NodeMode::Extension;
        assert_eq!(
            config.arguments().first(),
            Some(&OsString::from("--webserver"))
        );
    }

    #[test]
    fn manager_does_not_claim_external_processes() {
        let mut manager = NodeManager::new(NodeConfig::standalone("rpi-web", "."));
        assert!(!manager.is_running().unwrap());
        assert!(matches!(
            manager.wait_ready(Duration::from_millis(1)),
            Err(NodeError::NotRunning)
        ));
        assert!(matches!(manager.stop(), Err(NodeError::NotRunning)));
    }
}
