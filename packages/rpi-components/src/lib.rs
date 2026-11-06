//! Component UI packages are separate from native executable extensions.
//! Clients mount custom documents; node-owned capabilities implement actions.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
pub const MAX_BUNDLE: usize = 2 * 1024 * 1024;
pub const UI_PROTOCOL: u32 = 1;
pub const UI_BRIDGE: &str = include_str!("../ui-host/bridge.js");
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub title: String,
    pub description: String,
    pub panels: Vec<Panel>,
    pub actions: BTreeMap<String, Action>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<NativeRuntime>,
}
/// A native node provider travels with its custom UI. Installing UI alone never
/// executes this code; native clients require a separate, explicit trust action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeRuntime {
    pub entry_flag: String,
    pub artifacts: BTreeMap<String, NativeArtifact>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeArtifact {
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
}
impl NativeRuntime {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.entry_flag)
            || !self
                .entry_flag
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic())
            || self.artifacts.is_empty()
            || self.artifacts.len() > 16
        {
            return Err("Invalid native runtime descriptor".into());
        }
        for (target, a) in &self.artifacts {
            let url = url::Url::parse(&a.url).map_err(|_| "Invalid native artifact URL")?;
            if !valid_id(target)
                || a.bytes == 0
                || a.bytes > 256 * 1024 * 1024
                || a.sha256.len() != 64
                || !a
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err("Invalid native artifact URL/digest/size".into());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Panel {
    pub id: String,
    pub title: String,
    pub slot: String,
    pub entry: String,
    pub protocol: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Action {
    pub capability: String,
    #[serde(default)]
    pub local_only: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Bundle {
    pub manifest: Manifest,
    pub files: BTreeMap<String, String>,
    pub sha256: String,
}
impl Bundle {
    pub fn new(manifest: Manifest, files: BTreeMap<String, String>) -> Result<Self, String> {
        let sha256 = digest(&manifest, &files)?;
        let b = Self {
            manifest,
            files,
            sha256,
        };
        b.validate()?;
        Ok(b)
    }
    pub fn validate(&self) -> Result<(), String> {
        let m = &self.manifest;
        if let Some(runtime) = &m.runtime {
            runtime.validate()?;
        }
        let version: Vec<_> = m.version.split('.').collect();
        if m.schema_version != 1
            || !valid_id(&m.id)
            || version.len() != 3
            || version.iter().any(|v| {
                v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) || v.parse::<u32>().is_err()
            })
            || m.title.is_empty()
            || m.title.len() > 160
            || m.description.len() > 2048
            || m.panels.is_empty()
            || m.panels.len() > 16
            || m.actions.len() > 32
            || self.files.is_empty()
            || self.files.len() > 32
            || serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > MAX_BUNDLE
        {
            return Err("Invalid component identity/schema/size".into());
        }
        if self.files.keys().any(|p| {
            p.len() > 160 || !p.ends_with(".html") || p.split('/').any(|part| !valid_id(part))
        }) {
            return Err("UI entries must be portable relative HTML paths".into());
        }
        let mut ids = BTreeSet::new();
        for p in &m.panels {
            if !valid_id(&p.id)
                || !ids.insert(&p.id)
                || p.title.is_empty()
                || p.title.len() > 160
                || p.slot != "settings"
                || p.protocol != UI_PROTOCOL
                || !self.files.contains_key(&p.entry)
            {
                return Err("Unsupported component panel".into());
            }
        }
        if m.actions
            .iter()
            .any(|(id, a)| !valid_id(id) || !valid_id(&a.capability))
        {
            return Err("Invalid component action".into());
        }
        if self.sha256 != digest(m, &self.files)? {
            return Err("Component digest mismatch".into());
        }
        Ok(())
    }
}
fn digest(m: &Manifest, files: &BTreeMap<String, String>) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(m, files)).map_err(|e| e.to_string())?)
    ))
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Installed {
    pub manifest: Manifest,
    pub sha256: String,
    pub source: String,
    pub enabled: bool,
    pub grants: BTreeSet<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallRequest {
    pub bundle: Bundle,
    pub grants: BTreeSet<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Call {
    pub action: String,
    pub arguments: serde_json::Value,
}
#[cfg(not(target_arch = "wasm32"))]
pub mod host;
