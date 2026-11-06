//! A desktop owns its private loopback node instead of attaching to port 13030.
use crate::{NodeConfig, NodeManager};
use fs2::FileExt;
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    net::TcpListener,
    path::{Path, PathBuf},
    time::Duration,
};

/// Native agent process configuration. `work-node` resolves and describes the
/// executable; `work-client` owns the protocol reader and child lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeAgentProtocol {
    RpiRpc,
    Acp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeAgentSpec {
    pub protocol: NativeAgentProtocol,
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub project: PathBuf,
}

impl NativeAgentSpec {
    #[must_use]
    pub fn rpi_rpc(executable: impl Into<PathBuf>, project: impl Into<PathBuf>) -> Self {
        Self {
            protocol: NativeAgentProtocol::RpiRpc,
            executable: executable.into(),
            args: vec!["--mode".into(), "rpc".into()],
            project: project.into(),
        }
    }

    #[must_use]
    pub fn acp(executable: impl Into<PathBuf>, project: impl Into<PathBuf>) -> Self {
        Self {
            protocol: NativeAgentProtocol::Acp,
            executable: executable.into(),
            args: vec!["--acp".into()],
            project: project.into(),
        }
    }
}

pub struct DesktopNode {
    // Drop the child before releasing the catalog lock.
    _manager: NodeManager,
    _lock: File,
    address: String,
    password: String,
    rpi: PathBuf,
    project: PathBuf,
}

impl DesktopNode {
    /// The node binary comes from the application bundle or an installed runtime.
    pub fn start(
        executable: &Path,
        rpi: &Path,
        project: &Path,
        projects_root: &Path,
        data: &Path,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(data).map_err(|_| "无法创建本机节点数据目录。")?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(data.join("node.lock"))
            .map_err(|_| "无法打开本机节点锁。")?;
        lock.try_lock_exclusive()
            .map_err(|_| "本机节点已由另一个 RPI Work 窗口使用，请返回已打开的窗口。")?;
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|_| "无法分配本机节点端口。")?;
        let port = listener
            .local_addr()
            .map_err(|_| "无法读取本机节点端口。")?
            .port();
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| "无法生成本机节点认证信息。")?;
        let password = bytes
            .iter()
            .flat_map(|byte| {
                let hex = b"0123456789abcdef";
                [
                    char::from(hex[usize::from(byte >> 4)]),
                    char::from(hex[usize::from(byte & 15)]),
                ]
            })
            .collect::<String>();
        let mut config = NodeConfig::standalone(executable, project);
        config.port = port;
        config.rpi_bin = Some(rpi.to_owned());
        config.registry = Some(data.join("sessions.json"));
        let mut manager = NodeManager::new(config);
        // No inherited password/Host configuration may change this private child.
        // The random password is passed only in its environment, never CLI/history.
        let environment = [
            ("RPI_HTTP_PASSWORD".into(), password.clone().into()),
            ("RPI_WEB_PASSWORD".into(), password.clone().into()),
            (
                "RPI_HTTP_ALLOWED_HOSTS".into(),
                format!("127.0.0.1:{port}").into(),
            ),
            (
                "RPI_WEB_ALLOWED_HOSTS".into(),
                format!("127.0.0.1:{port}").into(),
            ),
        ];
        let arguments = [
            "--projects-root".into(),
            projects_root.as_os_str().to_owned(),
        ];
        // The HTTP protocol has no socket-activation API. If another process wins
        // this small bind gap, startup/login fails; we never adopt that process.
        drop(listener);
        manager
            .start_with(&arguments, &environment)
            .map_err(|_| "本机节点启动失败，请检查 rpi-http 运行时。")?;
        manager
            .wait_ready(Duration::from_secs(10))
            .map_err(|_| "本机节点未能就绪，请检查运行时版本和项目目录。")?;
        Ok(Self {
            _manager: manager,
            _lock: lock,
            address: format!("http://127.0.0.1:{port}/"),
            password,
            rpi: rpi.to_owned(),
            project: project.to_owned(),
        })
    }

    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    #[must_use]
    pub fn password(&self) -> &str {
        &self.password
    }

    #[must_use]
    pub fn native_agent(&self, protocol: NativeAgentProtocol) -> NativeAgentSpec {
        match protocol {
            NativeAgentProtocol::RpiRpc => NativeAgentSpec::rpi_rpc(&self.rpi, &self.project),
            NativeAgentProtocol::Acp => NativeAgentSpec::acp(&self.rpi, &self.project),
        }
    }
}

/// Explicit override, bundle sibling, then PATH. Never search unrelated workspaces.
#[must_use]
pub fn find_runtime(
    name: &str,
    override_path: Option<PathBuf>,
    application: &Path,
) -> Option<PathBuf> {
    find_runtime_on_path(name, override_path, application, std::env::var_os("PATH"))
}

/// The same lookup with an explicit PATH snapshot, for deterministic preparation.
pub(crate) fn find_runtime_on_path(
    name: &str,
    override_path: Option<PathBuf>,
    application: &Path,
    search_path: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(path) = override_path {
        return path.is_file().then_some(path);
    }
    let filename = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::iter::once(application.join(&filename))
        .chain(
            std::env::split_paths(&search_path.unwrap_or_default())
                .map(|directory| directory.join(&filename)),
        )
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_agent_specs_keep_protocol_args_out_of_the_ui_layer() {
        let directory = tempfile::tempdir().unwrap();
        let rpc = NativeAgentSpec::rpi_rpc(directory.path().join("rpi"), directory.path());
        assert_eq!(rpc.protocol, NativeAgentProtocol::RpiRpc);
        assert_eq!(rpc.args, ["--mode", "rpc"]);

        let acp = NativeAgentSpec::acp(directory.path().join("rpi"), directory.path());
        assert_eq!(acp.protocol, NativeAgentProtocol::Acp);
        assert_eq!(acp.args, ["--acp"]);
    }
    #[test]
    fn bundle_is_preferred_and_missing_explicit_runtime_does_not_fall_back() {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory
            .path()
            .join(format!("rpi-http{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&binary, "fixture").unwrap();
        assert_eq!(
            find_runtime("rpi-http", None, directory.path()),
            Some(binary)
        );
        assert!(
            find_runtime(
                "rpi-http",
                Some(directory.path().join("missing")),
                directory.path()
            )
            .is_none()
        );
    }

    fn assert_project_rename(http: &reqwest::blocking::Client, base: &str, project: &Path) {
        let url = |path: &str| format!("{base}{path}");
        let catalog: serde_json::Value = http
            .get(url("api/v1/projects"))
            .send()
            .unwrap()
            .json()
            .unwrap();
        let project_id = catalog["projects"][0]["id"].as_str().unwrap();
        let renamed: serde_json::Value = http
            .post(url(&format!("api/v1/projects/{project_id}/rename")))
            .header("x-rpi-web", "1")
            .json(&serde_json::json!({"name":"Renamed workspace"}))
            .send()
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(renamed["project"]["name"], "Renamed workspace");
        assert!(project.is_dir());
    }

    #[test]
    #[ignore = "requires RPI_WORK_NODE_TEST_BIN and an installed rpi runtime"]
    fn real_local_node_auth_ownership_and_restart() {
        let binary = std::env::var_os("RPI_WORK_NODE_TEST_BIN").unwrap();
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let data = directory.path().join("data");
        let node = DesktopNode::start(
            Path::new(&binary),
            Path::new("rpi"),
            &project,
            directory.path(),
            &data,
        )
        .unwrap();
        let http = reqwest::blocking::Client::builder()
            .cookie_store(true)
            .build()
            .unwrap();
        let url = |path: &str| format!("{}{path}", node.address());
        assert_eq!(
            http.get(url("api/v1/projects")).send().unwrap().status(),
            401
        );
        let state: serde_json::Value = http
            .post(url("api/v1/auth"))
            .header("x-rpi-web", "1")
            .json(&serde_json::json!({"password":node.password()}))
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(state["authenticated"], true);
        assert!(
            http.get(url("api/v1/projects"))
                .send()
                .unwrap()
                .status()
                .is_success()
        );
        assert!(
            http.get(url("api/v1/project-folders"))
                .send()
                .unwrap()
                .status()
                .is_success()
        );
        let nested: serde_json::Value = http
            .get(url("api/v1/project-folders"))
            .query(&[("path", "project")])
            .send()
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(nested["path"], "project");
        assert_project_rename(&http, node.address(), &project);
        assert_eq!(
            http.get(url("api/v1/project-folders"))
                .query(&[("path", project.to_string_lossy().as_ref())])
                .send()
                .unwrap()
                .status(),
            400
        );
        assert!(
            DesktopNode::start(
                Path::new(&binary),
                Path::new("rpi"),
                &project,
                directory.path(),
                &data
            )
            .is_err()
        );
        let endpoint = node
            .address()
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .parse()
            .unwrap();
        drop(node);
        assert!(
            std::net::TcpStream::connect_timeout(&endpoint, Duration::from_millis(200)).is_err()
        );
        let restarted = DesktopNode::start(
            Path::new(&binary),
            Path::new("rpi"),
            &project,
            directory.path(),
            &data,
        )
        .unwrap();
        assert_eq!(restarted.password().len(), 64);
    }
}
