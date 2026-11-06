//! Remote-access panel and action protocol. Runtime ownership stays with the
//! supplied node. Custom UI is packaged independently of the client renderer.
use rpi_components::{Action, Bundle, Manifest, Panel};
use std::collections::BTreeMap;
pub fn bundle() -> Result<Bundle, String> {
    Bundle::new(
        Manifest {
            schema_version: 1,
            runtime: None,
            id: "rpi-http".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            title: "远程访问".into(),
            description: "让其他设备访问当前节点，共享已有工作空间和任务。".into(),
            panels: vec![Panel {
                id: "remote-access".into(),
                title: "远程访问".into(),
                slot: "settings".into(),
                entry: "index.html".into(),
                protocol: 1,
            }],
            actions: BTreeMap::from([
                (
                    "status".into(),
                    Action {
                        capability: "node.sharing.read".into(),
                        local_only: false,
                    },
                ),
                (
                    "issue-authorization".into(),
                    Action {
                        capability: "node.sharing.authorize".into(),
                        local_only: true,
                    },
                ),
                (
                    "configure".into(),
                    Action {
                        capability: "node.sharing.configure".into(),
                        local_only: true,
                    },
                ),
                (
                    "set-enabled".into(),
                    Action {
                        capability: "node.sharing.write".into(),
                        local_only: true,
                    },
                ),
            ]),
        },
        BTreeMap::from([("index.html".into(), include_str!("../ui/index.html").into())]),
    )
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Snapshot {
    pub listening: bool,
    pub enabled: bool,
    pub status: String,
    pub warning: String,
    pub public_url: String,
    pub app_url: String,
    pub address: String,
    pub can_configure: bool,
    #[serde(default)]
    pub address_hint: String,
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub web_hosted: bool,
    #[serde(default)]
    pub configuration: Option<Configuration>,
    #[serde(default)]
    pub restart_required: bool,
}
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Configuration {
    pub bind: String,
    pub port: u16,
    pub public_url: String,
}
impl Default for Configuration {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0".into(),
            port: 13031,
            public_url: String::new(),
        }
    }
}
// Never derive Debug/Serialize on a request retaining a new password.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Configure {
    pub configuration: Configuration,
    pub new_password: Option<String>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Authorization {
    pub ticket: String,
    pub expires_at: u64,
    pub node_id: String,
}
#[cfg(not(target_arch = "wasm32"))]
pub trait RemoteAccessOwner: Send + Sync {
    fn snapshot(&self) -> Snapshot;
    fn set_enabled(&self, enabled: bool) -> Result<(), String>;
    fn configure(&self, _request: Configure) -> Result<(), String> {
        Err("Configuration is unavailable".into())
    }
    fn issue_authorization(&self) -> Result<Authorization, String> {
        Err("Authorization is unavailable".into())
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub fn register(
    host: &rpi_components::host::Host,
    owner: std::sync::Arc<dyn RemoteAccessOwner>,
) -> Result<(), String> {
    let read = owner.clone();
    let authorize = owner.clone();
    let configure = owner.clone();
    host.register_capability(
        "node.sharing.configure",
        true,
        std::sync::Arc::new(move |args, local| {
            let request: Configure = serde_json::from_value(args)
                .map_err(|_| "Expected listener configuration and optional newPassword")?;
            configure.configure(request)?;
            let mut snapshot = configure.snapshot();
            snapshot.can_configure = local;
            serde_json::to_value(snapshot)
                .map_err(|_| "Cannot encode listener configuration".into())
        }),
    )?;
    host.register_capability(
        "node.sharing.authorize",
        true,
        std::sync::Arc::new(move |args, _| {
            if !args.as_object().is_some_and(serde_json::Map::is_empty) {
                return Err("Expected empty authorization arguments".into());
            }
            serde_json::to_value(authorize.issue_authorization()?)
                .map_err(|_| "Cannot encode authorization".into())
        }),
    )?;
    host.register_capability(
        "node.sharing.read",
        false,
        std::sync::Arc::new(move |args, local| {
            if !args.as_object().is_some_and(serde_json::Map::is_empty) {
                return Err("Expected empty status arguments".into());
            }
            let mut s = read.snapshot();
            s.can_configure = local;
            serde_json::to_value(s).map_err(|e| e.to_string())
        }),
    )?;
    host.register_capability(
        "node.sharing.write",
        true,
        std::sync::Arc::new(move |args, local| {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Set {
                enabled: bool,
            }
            let request: Set =
                serde_json::from_value(args).map_err(|_| "Expected enabled boolean")?;
            owner.set_enabled(request.enabled)?;
            let mut s = owner.snapshot();
            s.can_configure = local;
            serde_json::to_value(s).map_err(|e| e.to_string())
        }),
    )?;
    host.register_builtin(bundle()?)
}
