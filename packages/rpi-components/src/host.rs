//! Node-owned package persistence and capability dispatch. No additional owner,
//! HTTP listener, session database or native plugin ABI is introduced.
use crate::{valid_id, Bundle, Call, InstallRequest, Installed, MAX_BUNDLE};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};
pub type Handler = Arc<dyn Fn(Value, bool) -> Result<Value, String> + Send + Sync>;
struct Capability {
    local_only: bool,
    handler: Handler,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    bundle: Bundle,
    grants: BTreeSet<String>,
}
pub struct Host {
    root: PathBuf,
    records: Mutex<BTreeMap<String, Record>>,
    builtins: Mutex<BTreeSet<String>>,
    handlers: Mutex<BTreeMap<String, Capability>>,
}
impl Host {
    pub fn open(root: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let mut records = BTreeMap::new();
        for entry in std::fs::read_dir(&root).map_err(|e| e.to_string())? {
            let e = entry.map_err(|e| e.to_string())?;
            if e.path().extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if !e.file_type().map_err(|e| e.to_string())?.is_file()
                || e.metadata().map_err(|e| e.to_string())?.len() > (MAX_BUNDLE + 16384) as u64
            {
                return Err("Invalid installed component file".into());
            }
            let r: Record =
                serde_json::from_slice(&std::fs::read(e.path()).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            r.bundle.validate()?;
            validate_grants(&r)?;
            if e.file_name().to_str() != Some(&format!("{}.json", r.bundle.manifest.id)) {
                return Err("Component filename mismatch".into());
            }
            records.insert(r.bundle.manifest.id.clone(), r);
        }
        Ok(Self {
            root,
            records: Mutex::new(records),
            builtins: Mutex::new(BTreeSet::new()),
            handlers: Mutex::new(BTreeMap::new()),
        })
    }
    pub fn register_capability(
        &self,
        name: &str,
        local_only: bool,
        handler: Handler,
    ) -> Result<(), String> {
        if !valid_id(name) {
            return Err("Invalid capability".into());
        }
        let mut h = self
            .handlers
            .lock()
            .map_err(|_| "Component host unavailable")?;
        if h.contains_key(name) {
            return Err("Capability already registered".into());
        }
        h.insert(
            name.into(),
            Capability {
                local_only,
                handler,
            },
        );
        Ok(())
    }
    pub fn register_builtin(&self, bundle: Bundle) -> Result<(), String> {
        bundle.validate()?;
        let id = bundle.manifest.id.clone();
        let grants = bundle
            .manifest
            .actions
            .values()
            .map(|a| a.capability.clone())
            .collect();
        let mut b = self
            .builtins
            .lock()
            .map_err(|_| "Component host unavailable")?;
        let mut r = self
            .records
            .lock()
            .map_err(|_| "Component host unavailable")?;
        if r.contains_key(&id) {
            return Err("Component identity already installed".into());
        }
        r.insert(id.clone(), Record { bundle, grants });
        b.insert(id);
        Ok(())
    }
    pub fn catalog(&self) -> Result<Vec<Installed>, String> {
        let b = self
            .builtins
            .lock()
            .map_err(|_| "Component host unavailable")?;
        let r = self
            .records
            .lock()
            .map_err(|_| "Component host unavailable")?;
        Ok(r.values()
            .map(|r| Installed {
                manifest: r.bundle.manifest.clone(),
                sha256: r.bundle.sha256.clone(),
                source: if b.contains(&r.bundle.manifest.id) {
                    "bundled"
                } else {
                    "installed"
                }
                .into(),
                enabled: true,
                grants: r.grants.clone(),
            })
            .collect())
    }
    pub fn install(&self, request: InstallRequest) -> Result<Installed, String> {
        request.bundle.validate()?;
        let id = request.bundle.manifest.id.clone();
        let record = Record {
            bundle: request.bundle,
            grants: request.grants,
        };
        validate_grants(&record)?;
        let b = self
            .builtins
            .lock()
            .map_err(|_| "Component host unavailable")?;
        let mut r = self
            .records
            .lock()
            .map_err(|_| "Component host unavailable")?;
        if b.contains(&id) || r.contains_key(&id) {
            return Err("Component already installed; remove before changing versions".into());
        }
        let mut file = tempfile::NamedTempFile::new_in(&self.root).map_err(|e| e.to_string())?;
        use std::io::Write;
        file.write_all(&serde_json::to_vec(&record).map_err(|e| e.to_string())?)
            .and_then(|()| file.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        file.persist_noclobber(self.root.join(format!("{id}.json")))
            .map_err(|e| e.to_string())?;
        let installed = Installed {
            manifest: record.bundle.manifest.clone(),
            sha256: record.bundle.sha256.clone(),
            source: "installed".into(),
            enabled: true,
            grants: record.grants.clone(),
        };
        r.insert(id, record);
        Ok(installed)
    }
    pub fn remove(&self, id: &str) -> Result<(), String> {
        if !valid_id(id) {
            return Err("Invalid component identity".into());
        }
        let b = self
            .builtins
            .lock()
            .map_err(|_| "Component host unavailable")?;
        let mut r = self
            .records
            .lock()
            .map_err(|_| "Component host unavailable")?;
        if b.contains(id) {
            return Err("Bundled component cannot be removed".into());
        }
        if !r.contains_key(id) {
            return Err("Component not installed".into());
        }
        std::fs::remove_file(self.root.join(format!("{id}.json"))).map_err(|e| e.to_string())?;
        r.remove(id);
        Ok(())
    }
    pub fn panel(&self, id: &str, panel: &str) -> Result<String, String> {
        let r = self
            .records
            .lock()
            .map_err(|_| "Component host unavailable")?;
        let r = r.get(id).ok_or("Component unavailable")?;
        let p = r
            .bundle
            .manifest
            .panels
            .iter()
            .find(|p| p.id == panel)
            .ok_or("Panel unavailable")?;
        r.bundle
            .files
            .get(&p.entry)
            .cloned()
            .ok_or("Panel resource missing".into())
    }
    pub fn call(&self, id: &str, call: Call, local: bool) -> Result<Value, String> {
        let cap = {
            let r = self
                .records
                .lock()
                .map_err(|_| "Component host unavailable")?;
            let r = r.get(id).ok_or("Component unavailable")?;
            let a = r
                .bundle
                .manifest
                .actions
                .get(&call.action)
                .ok_or("Action not declared")?;
            if a.local_only && !local {
                return Err("Action requires the local owner".into());
            }
            if !r.grants.contains(&a.capability) {
                return Err("Capability not granted".into());
            }
            a.capability.clone()
        };
        let handler = {
            let h = self
                .handlers
                .lock()
                .map_err(|_| "Component host unavailable")?;
            let c = h.get(&cap).ok_or("Capability unavailable")?;
            if c.local_only && !local {
                return Err("Capability requires the local owner".into());
            }
            c.handler.clone()
        };
        handler(call.arguments, local)
    }
}
fn validate_grants(r: &Record) -> Result<(), String> {
    let requested: BTreeSet<_> = r
        .bundle
        .manifest
        .actions
        .values()
        .map(|a| &a.capability)
        .collect();
    if r.grants.iter().any(|g| !requested.contains(g)) {
        return Err("Grant not requested by component".into());
    }
    Ok(())
}
