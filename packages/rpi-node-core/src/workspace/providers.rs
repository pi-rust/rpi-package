//! Native models.json/auth.json adapter. No resolver, network, shell or second
//! provider registry: new native hosts read these same documented agent files.
use super::resources::{conflict, invalid, plain_directory};
use super::{LocalError, LocalWorkspace, Path, PathBuf, WorkspaceError};
use fs2::FileExt;
use rpi_node_api::api::{
    CredentialLocation, NativeReleaseBlock, NativeReleaseReport, ProviderAction, ProviderCatalog,
    ProviderDefinition, ProviderEntry, ProviderMutation, ProviderReceipt, StoredCredential,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io::{Read, Write},
    sync::Arc,
};

const LIMIT: u64 = 2 * 1024 * 1024;
pub(super) struct Store {
    root: Option<PathBuf>,
    salt: [u8; 32],
    pub gate: Arc<tokio::sync::Mutex<()>>,
}
impl Store {
    pub fn new() -> Result<Self, LocalError> {
        let root = match std::env::var_os("RPI_CODING_AGENT_DIR") {
            Some(path) => Some(PathBuf::from(path)).filter(|p| p.is_absolute()),
            None => std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(".rpi/agent")),
        };
        let mut salt = [0; 32];
        getrandom::fill(&mut salt).map_err(|_| {
            LocalError::Initialization("provider revision entropy unavailable".into())
        })?;
        Ok(Self {
            root,
            salt,
            gate: Arc::new(tokio::sync::Mutex::new(())),
        })
    }
    fn root(&self) -> Result<&Path, WorkspaceError> {
        self.root.as_deref().ok_or_else(|| {
            LocalError::Unsupported(
                "set an absolute RPI_CODING_AGENT_DIR before starting the node".into(),
            )
            .into()
        })
    }
}
fn storage() -> WorkspaceError {
    LocalError::Service { kind: rpi_extcore::reply::ReplyKind::Unavailable,
        message: "provider storage unavailable; refresh before retrying; no credentials are included in diagnostics".into() }.into()
}
fn ordinary(meta: &std::fs::Metadata) -> bool {
    if !meta.is_file() || meta.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    true
}
fn read(root: &Path, name: &str) -> Result<Option<Vec<u8>>, WorkspaceError> {
    // Validate every existing ancestor even if the final directory is absent.
    for ancestor in root.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                plain_directory(ancestor, false).map_err(|_| storage())?;
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(storage()),
        }
    }
    let path = root.join(name);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(storage()),
    };
    if !ordinary(&meta) || meta.len() > LIMIT {
        return Err(storage());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| storage())?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| storage())?;
    if bytes.len() as u64 > LIMIT {
        return Err(storage());
    }
    Ok(Some(bytes))
}
fn uncomment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let (mut string, mut escape) = (false, false);
    while let Some(ch) = chars.next() {
        if !string && ch == '/' && chars.peek() == Some(&'/') {
            for c in chars.by_ref() {
                if c == '\n' {
                    out.push(c);
                    break;
                }
            }
            continue;
        }
        out.push(ch);
        if escape {
            escape = false;
        } else if string && ch == '\\' {
            escape = true;
        } else if ch == '"' {
            string = !string;
        }
    }
    out
}
// Reject duplicate keys rather than silently discarding unrelated definitions
// during an edit. serde_json::Value alone accepts last-key-wins input.
struct Unique(Value);
impl<'de> serde::Deserialize<'de> for Unique {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON without duplicate fields")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Unique, A::Error> {
                let mut out = Vec::new();
                while let Some(Unique(v)) = seq.next_element()? {
                    out.push(v);
                }
                Ok(Unique(Value::Array(out)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Unique, A::Error> {
                let mut out = Map::new();
                while let Some((k, Unique(v))) = map.next_entry::<String, Unique>()? {
                    if out.contains_key(&k) {
                        return Err(serde::de::Error::custom("duplicate configuration field"));
                    }
                    out.insert(k, v);
                }
                Ok(Unique(Value::Object(out)))
            }
        }
        d.deserialize_any(Visitor)
    }
}
fn document(bytes: Option<&[u8]>) -> Result<Value, WorkspaceError> {
    let Some(bytes) = bytes else {
        return Ok(json!({}));
    };
    let text = std::str::from_utf8(bytes).map_err(|_| storage())?;
    let Unique(value) = serde_json::from_str(&uncomment(text)).map_err(|_| storage())?;
    if !value.is_object() {
        return Err(storage());
    }
    Ok(value)
}
struct Snapshot {
    models: Value,
    auth: Value,
    revision: String,
}
fn snapshot(store: &Store) -> Result<Snapshot, WorkspaceError> {
    let root = store.root()?;
    let model_bytes = read(root, "models.json")?;
    let auth_bytes = read(root, "auth.json")?;
    let models = document(model_bytes.as_deref())?;
    let auth = document(auth_bytes.as_deref())?;
    if models
        .get("providers")
        .is_some_and(|p| !p.is_object() || p.as_object().is_some_and(|p| p.len() > 1024))
        || auth.as_object().is_some_and(|p| p.len() > 1024)
    {
        return Err(storage());
    }
    let mut h = Sha256::new();
    h.update(store.salt);
    for bytes in [&model_bytes, &auth_bytes] {
        h.update([u8::from(bytes.is_some())]);
        h.update(
            bytes
                .as_ref()
                .map_or(0u64, |b| b.len() as u64)
                .to_le_bytes(),
        );
        if let Some(b) = bytes {
            h.update(b);
        }
    }
    Ok(Snapshot {
        models,
        auth,
        revision: format!("{:x}", h.finalize()),
    })
}
fn id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        && s != "."
        && s != ".."
}
fn text_ok(s: &str, max: usize) -> bool {
    s.len() <= max && !s.chars().any(char::is_control)
}
fn validate(d: &ProviderDefinition) -> Result<(), WorkspaceError> {
    if !id_ok(&d.id) || !text_ok(&d.name, 160) || d.models.len() > 128 || d.base_url.len() > 2048 {
        return Err(invalid(
            "invalid provider identity/name/model count/endpoint length",
        ));
    }
    let url = url::Url::parse(&d.base_url)
        .map_err(|_| invalid("provider endpoint must be an absolute HTTP(S) URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || d.base_url
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(invalid(
            "provider endpoint rejects credentials, query, fragment and whitespace",
        ));
    }
    let mut ids = BTreeSet::new();
    for m in &d.models {
        if m.id.is_empty()
            || !text_ok(&m.id, 200)
            || m.id.trim() != m.id
            || !text_ok(&m.name, 200)
            || !ids.insert(&m.id)
            || m.context_window == Some(0)
            || m.max_tokens == Some(0)
            || m.context_window.is_some_and(|v| v > 100_000_000)
            || m.max_tokens.is_some_and(|v| v > 100_000_000)
            || m.context_window
                .zip(m.max_tokens)
                .is_some_and(|(c, t)| t > c)
        {
            return Err(invalid(
                "invalid or duplicate provider model; token bounds are 1..100000000 and maxTokens <= contextWindow",
            ));
        }
    }
    Ok(())
}
fn optional_type(raw: &Value, key: &str, check: fn(&Value) -> bool) -> bool {
    raw.get(key).is_none_or(|v| v.is_null() || check(v))
}
fn project_definition(id: &str, raw: &Value) -> Option<ProviderDefinition> {
    if !optional_type(raw, "name", Value::is_string) {
        return None;
    }
    let mut models = Vec::new();
    for m in raw
        .get("models")
        .map_or(Some(&[][..]), |v| v.as_array().map(Vec::as_slice))?
    {
        if !optional_type(m, "name", Value::is_string)
            || !optional_type(m, "reasoning", Value::is_boolean)
            || !optional_type(m, "contextWindow", Value::is_u64)
            || !optional_type(m, "maxTokens", Value::is_u64)
        {
            return None;
        }
        models.push(rpi_node_api::api::ProviderModel {
            id: m.get("id")?.as_str()?.into(),
            name: m
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            reasoning: m.get("reasoning").and_then(Value::as_bool).unwrap_or(false),
            context_window: m.get("contextWindow").and_then(Value::as_u64),
            max_tokens: m.get("maxTokens").and_then(Value::as_u64),
        });
    }
    let d = ProviderDefinition {
        id: id.into(),
        name: raw
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        api: serde_json::from_value(raw.get("api")?.clone()).ok()?,
        base_url: raw.get("baseUrl")?.as_str()?.into(),
        models,
    };
    validate(&d).ok()?;
    Some(d)
}
fn catalog(s: &Snapshot) -> ProviderCatalog {
    let defs = s.models.get("providers").and_then(Value::as_object);
    let ids: BTreeSet<_> = defs
        .into_iter()
        .flat_map(Map::keys)
        .chain(s.auth.as_object().into_iter().flat_map(Map::keys))
        .collect();
    let providers = ids
        .into_iter()
        .filter(|id| id_ok(id))
        .map(|id| {
            let raw = defs.and_then(|m| m.get(id));
            let definition = raw.and_then(|r| project_definition(id, r));
            ProviderEntry {
                id: id.clone(),
                configured: raw.is_some(),
                config_key_present: raw
                    .and_then(|v| v.get("apiKey"))
                    .is_some_and(|v| !v.is_null()),
                stored_credential: match s.auth.get(id) {
                    None => StoredCredential::None,
                    Some(v) => match v.get("type").and_then(Value::as_str) {
                        Some("api_key") => StoredCredential::ApiKey,
                        Some("oauth") => StoredCredential::OAuth,
                        _ => StoredCredential::Other,
                    },
                },
                diagnostic: (raw.is_some() && definition.is_none())
                    .then(|| "高级或旧版定义未映射到表单；原文保留且不允许表单覆盖。可在节点端编辑，或显式删除后重新创建。".into()),
                definition,
            }
        })
        .collect();
    ProviderCatalog {
        revision: s.revision.clone(),
        providers,
    }
}
fn merge_definition(old: Option<&Value>, d: &ProviderDefinition) -> Value {
    let mut raw = old
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let old_projection = old.and_then(|v| project_definition(&d.id, v));
    if old_projection.as_ref().is_none_or(|p| p.name != d.name) {
        if d.name.is_empty() {
            raw.as_object_mut().expect("provider object").remove("name");
        } else {
            raw["name"] = json!(d.name);
        }
    }
    raw["api"] = json!(d.api);
    raw["baseUrl"] = json!(d.base_url);
    let models: Vec<_> = d
        .models
        .iter()
        .map(|model| {
            let old_model = old
                .and_then(|v| v.get("models"))
                .and_then(Value::as_array)
                .and_then(|ms| ms.iter().find(|m| m["id"] == model.id));
            let mut raw_model = old_model
                .cloned()
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({}));
            let projected_model = old_projection
                .as_ref()
                .and_then(|p| p.models.iter().find(|m| m.id == model.id));
            let projected_json =
                projected_model.map(|m| serde_json::to_value(m).expect("model serialization"));
            for (k, v) in serde_json::to_value(model)
                .expect("provider model serialization")
                .as_object()
                .expect("model object")
            {
                // Keep absent/null/default spellings unchanged for unrelated edits.
                if projected_json.as_ref().and_then(|p| p.get(k)) == Some(v) {
                    continue;
                }
                if v.is_null() || (k == "name" && v == "") {
                    raw_model.as_object_mut().expect("model object").remove(k);
                } else {
                    raw_model[k] = v.clone();
                }
            }
            raw_model
        })
        .collect();
    raw["models"] = json!(models);
    raw
}
// Native expands config strings; escape user input as LITERAL, never !command or
// $ENV interpolation. No resolver is called by this application.
fn literal(key: &str) -> String {
    let escaped = key.replace('$', "$$");
    escaped
        .strip_prefix('!')
        .map_or_else(|| escaped.clone(), |s| format!("$!{s}"))
}
fn check_action(action: &ProviderAction) -> Result<(), WorkspaceError> {
    match action {
        ProviderAction::Upsert { definition } => validate(definition),
        ProviderAction::RemoveConfig { id } | ProviderAction::ClearCredential { id, .. } => {
            if id_ok(id) {
                Ok(())
            } else {
                Err(invalid("invalid provider identity"))
            }
        }
        ProviderAction::SetKey { id, key } => {
            if id_ok(id) && !key.is_empty() && text_ok(key, 8192) {
                Ok(())
            } else {
                Err(invalid(
                    "invalid provider identity or empty/oversized/control-character API key",
                ))
            }
        }
    }
}
fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}
fn write_one(
    store: &Store,
    before: &Snapshot,
    name: &str,
    value: &Value,
) -> Result<(), WorkspaceError> {
    let root = store.root()?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| storage())?;
    if bytes.len() as u64 > LIMIT {
        return Err(invalid("provider document exceeds 2 MiB"));
    }
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|_| storage())?;
    let stage = root.join(format!(
        ".rpi-work-provider-{:x}.tmp",
        u128::from_le_bytes(nonce)
    ));
    let result = (|| {
        let mut file = private_options().open(&stage).map_err(|_| storage())?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| storage())?;
        // Recheck BOTH files immediately before publication, including external edits.
        if snapshot(store)?.revision != before.revision {
            return Err(conflict(
                "provider files changed externally; refresh before retry",
            ));
        }
        std::fs::rename(&stage, root.join(name)).map_err(|_| storage())?;
        #[cfg(unix)]
        File::open(root)
            .and_then(|f| f.sync_all())
            .map_err(|_| storage())?;
        Ok(())
    })();
    let _ = std::fs::remove_file(stage);
    result
}
fn mutate(store: &Store, request: ProviderMutation) -> Result<ProviderReceipt, WorkspaceError> {
    check_action(&request.action)?;
    if request.expected_revision.len() != 64 {
        return Err(invalid(
            "provider revision required; refresh before editing",
        ));
    }
    let initial = snapshot(store)?;
    if initial.revision != request.expected_revision {
        return Err(conflict("provider catalog changed; refresh before editing"));
    }
    let root = store.root()?;
    plain_directory(root, true).map_err(|_| storage())?;
    let lock_path = root.join(".rpi-work-provider.lock");
    if let Ok(m) = std::fs::symlink_metadata(&lock_path) {
        if !ordinary(&m) {
            return Err(storage());
        }
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(&lock_path).map_err(|_| storage())?;
    lock.try_lock_exclusive()
        .map_err(|_| conflict("provider configuration is busy"))?;
    let before = snapshot(store)?;
    if before.revision != request.expected_revision {
        return Err(conflict("provider catalog changed; refresh before editing"));
    }
    let mut models = before.models.clone();
    let mut auth = before.auth.clone();
    let name = match request.action {
        ProviderAction::Upsert { definition } => {
            if models.get("providers").is_none() {
                models["providers"] = json!({});
            }
            let old = models["providers"].get(&definition.id);
            if old.is_some_and(|v| project_definition(&definition.id, v).is_none()) {
                return Err(conflict(
                    "existing provider definition cannot be safely projected; keep original bytes and edit on the node or explicitly delete before recreating",
                ));
            }
            models["providers"][&definition.id] = merge_definition(old, &definition);
            "models.json"
        }
        ProviderAction::RemoveConfig { id } => {
            if let Some(p) = models.get_mut("providers").and_then(Value::as_object_mut) {
                p.remove(&id);
            }
            "models.json"
        }
        ProviderAction::SetKey { id, key } => {
            auth[&id] = json!({"type":"api_key","key":literal(&key)});
            "auth.json"
        }
        ProviderAction::ClearCredential {
            id,
            location: CredentialLocation::Stored,
        } => {
            auth.as_object_mut().expect("auth object").remove(&id);
            "auth.json"
        }
        ProviderAction::ClearCredential {
            id,
            location: CredentialLocation::Config,
        } => {
            if let Some(p) = models
                .get_mut("providers")
                .and_then(|v| v.get_mut(&id))
                .and_then(Value::as_object_mut)
            {
                p.remove("apiKey");
            }
            "models.json"
        }
    };
    write_one(
        store,
        &before,
        name,
        if name == "models.json" {
            &models
        } else {
            &auth
        },
    )?;
    let after = snapshot(store)?;
    Ok(ProviderReceipt {
        catalog: catalog(&after),
        requires_restart: true,
    })
}
impl LocalWorkspace {
    async fn provider_io<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Store) -> Result<T, WorkspaceError> + Send + 'static,
    ) -> Result<T, WorkspaceError> {
        let lease = self.inner.lifecycle.clone().read_owned().await;
        drop(self.lock()?);
        let gate = self.inner.providers.gate.clone().lock_owned().await;
        let owner = self.clone();
        tokio::task::spawn_blocking(move || {
            let (_lease, _gate) = (lease, gate);
            work(&owner.inner.providers)
        })
        .await
        .map_err(|_| storage())?
    }
    pub async fn provider_catalog(&self) -> Result<ProviderCatalog, WorkspaceError> {
        self.provider_io(|s| snapshot(s).map(|v| catalog(&v))).await
    }
    pub async fn provider_mutate(
        &self,
        request: ProviderMutation,
    ) -> Result<ProviderReceipt, WorkspaceError> {
        self.provider_io(move |s| mutate(s, request)).await
    }
    pub async fn provider_release_hosts(&self) -> Result<NativeReleaseReport, WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        let services: Vec<_> = self.lock()?.services.values().cloned().collect();
        let mut report = NativeReleaseReport {
            released: vec![],
            blocked: vec![],
        };
        for service in services {
            let pid = rpi_extcore::workspace::Project::new(&service.config.project)
                .map_err(|_| storage())?
                .id;
            match service
                .release_idle_resources(pid.clone(), rpi_extcore::resources::ResourceScope::Global)
                .await
            {
                Ok(ids) => report.released.extend(ids),
                Err(_) => report.blocked.push(NativeReleaseBlock {
                    project_id: pid,
                    diagnostic: "宿主仍在使用、运行或待结算；未强制停止，请结算后显式重试。".into(),
                }),
            }
        }
        Ok(report)
    }
}
#[cfg(test)]
mod tests;
