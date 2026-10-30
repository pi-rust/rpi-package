//! Official experimental session contracts, pinned in docs/architecture/acp-session-rfd-evidence.md.
//! The RFDs define agent capabilities, not a client negotiation field. The opt-in
//! below is explicitly private adapter policy, not a purported stable ACP key.
use super::*;
use rpi_extcore::host::SessionOptions;
use rpi_extcore::session_metadata::{ArchiveFilter, MetadataStore};

pub(super) const OPT_IN: &str = "rpi/unstable-session-rfds";
pub(super) const REVISION: &str = "1142c83bbfa9407f9f264d1e290ffafcd54eb79e";

pub(super) fn negotiated(params: &Value) -> bool {
    let pointer = if params.get("protocolVersion").and_then(Value::as_u64).is_some_and(|v| v >= 2) {
        "/capabilities/_meta"
    } else {
        "/clientCapabilities/_meta"
    };
    params.pointer(pointer).and_then(|m| m.get(OPT_IN)).and_then(Value::as_str) == Some(REVISION)
}
pub(super) fn advertise(result: &mut Value, enabled: bool, v2: bool) {
    if !enabled { return; }
    let cap = if v2 { &mut result["capabilities"]["session"] } else { &mut result["agentCapabilities"]["sessionCapabilities"] };
    cap["systemPrompt"] = json!({"mode":{"override":{}}});
    cap["fork"] = json!({});
    cap["setTitle"] = json!({});
    cap["archive"] = json!({});
    cap["_meta"][OPT_IN] = json!({"revision":REVISION,"negotiation":"private opt-in; wire fields follow official RFDs"});
}
pub(super) fn archive_filter(params: &Value, enabled: bool) -> Result<ArchiveFilter, String> {
    match params.get("archived").filter(|v| !v.is_null()) {
        None => Ok(ArchiveFilter::Exclude),
        Some(_) if !enabled => Err("archived requires explicit unstable session RFD negotiation".into()),
        Some(v) => match v.as_str() {
            Some("exclude") => Ok(ArchiveFilter::Exclude),
            Some("include") => Ok(ArchiveFilter::Include),
            Some("only") => Ok(ArchiveFilter::Only),
            _ => Err("archived must be exclude, include, only, or null".into()),
        }
    }
}
pub(super) fn normalized_title(params: &Value) -> Result<String, String> {
    let title = params.get("title").and_then(Value::as_str).ok_or("title must be a string")?;
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() || title.chars().count() > 256 { return Err("title must contain 1 to 256 characters after whitespace normalization".into()); }
    Ok(title)
}

pub(super) struct Context {
    pub sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    pub watchers: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    pub prompts: PromptTasks,
    pub requests: RequestTasks,
    pub writer: Arc<Writer>,
    pub client_tools: rpi_extcore::host::ClientToolOptions,
}

pub(super) async fn mutate(method: &str, params: Value, session_id: String, context: Context) -> Result<Value, RequestError> {
    let root = crate::history::session_root()?;
    let store = MetadataStore::new(&root)?;
    if method == "session/set_title" {
        let title = normalized_title(&params).map_err(RequestError::InvalidInput)?;
        store.update(&session_id, |metadata| metadata.title = Some(title.clone()))?;
        if context.sessions.lock().await.contains_key(&session_id) {
            context.writer.notify("session/update", json!({"sessionId":session_id,"update":{"sessionUpdate":"session_info_update","title":title}})).await;
        }
        return Ok(json!({"title":title}));
    }
    let archived = method == "session/archive";
    if archived {
        if context.sessions.lock().await.contains_key(&session_id) {
            context.writer.notify("session/update", json!({"sessionId":session_id,"update":{"sessionUpdate":"session_info_update","archived":true}})).await;
        }
        stop_live_session(&session_id, &context.sessions, &context.watchers, &context.prompts, &context.requests).await;
    }
    store.update(&session_id, |metadata| metadata.archived = archived)?;
    if !archived && context.sessions.lock().await.contains_key(&session_id) {
        context.writer.notify("session/update", json!({"sessionId":session_id,"update":{"sessionUpdate":"session_info_update","archived":false}})).await;
    }
    Ok(json!({}))
}

pub(super) fn validate_fork(params: &Value) -> Result<(PathBuf, Vec<PathBuf>), String> {
    let object = request_params(params, "session/fork")?;
    let cwd = PathBuf::from(required_string(object, "cwd")?);
    if !cwd.is_absolute() || !cwd.is_dir() { return Err("cwd must be an existing absolute directory".into()); }
    crate::mcp::parse(params, false, true)?;
    crate::system_prompt::parse(params, true)?;
    Ok((cwd, parse_additional_directories(params)?))
}

pub(super) async fn fork(params: Value, source_id: String, context: Context, mut cancelled: watch::Receiver<bool>) -> Result<Value, RequestError> {
    let (cwd, roots) = validate_fork(&params).map_err(RequestError::InvalidInput)?;
    let system_prompt = crate::system_prompt::parse(&params, true).map_err(RequestError::InvalidInput)?;
    let source = crate::history::saved_session(&source_id)?;
    // Snapshot live settings, not an adapter-side replacement transcript. Native
    // fork owns branch semantics; never submit a prompt to create a fork.
    let live = context.sessions.lock().await.get(&source_id).cloned();
    let settings = if let Some(source) = live { source.state().await? } else { json!({}) };
    let model = settings["model"]["id"].as_str().or_else(|| settings["model"].as_str()).map(str::to_owned);
    let provider = settings["model"]["provider"].as_str().or_else(|| settings["provider"].as_str()).map(str::to_owned);
    let root = crate::history::session_root()?;
    let options = SessionOptions {
        executable: crate::session::resolve_rpi_executable(), project: cwd.clone(), session_id: String::new(),
        session_dir: None, saved_session: None, fork_source: Some(source.to_string_lossy().into_owned()),
        timings_dir: root.join(".fork-timings"), model, provider,
        additional_directories: roots.clone(), permission_gate: !crate::env_flag("RPI_ACP_NO_PERMISSION_GATE"),
        bypass: crate::session::permission_bypass(),
    };
    let fork = rpi_extcore::session_fork::fork_saved_session(&root, options).await?;
    // Until registry insertion, failure/cancellation owns the newly published tree.
    struct SavedFork(PathBuf, bool);
    impl Drop for SavedFork { fn drop(&mut self) { if !self.1 { let _ = std::fs::remove_dir_all(&self.0); } } }
    let mut saved_owner = SavedFork(root.join(&fork.id), false);
    if *cancelled.borrow() { return Err(RequestError::Cancelled); }
    let servers = crate::mcp::parse(&params, context.writer.supports_v2(), true).map_err(RequestError::InvalidInput)?;
    let mut owner = OpeningSession::reserve(&context.writer)?;
    let (session, state) = open_session(&fork.id, cwd, roots.clone(), Some(&fork.file), context.client_tools, servers, &context.writer).await?;
    owner.0 = Some(session.clone());
    if let Some(prompt) = system_prompt.as_ref() {
        tokio::select! {
            biased;
            _ = wait_cancelled(&mut cancelled) => return Err(RequestError::Cancelled),
            result = session.configure_system_prompt(prompt) => result?,
        }
    }
    // The exact saved native branch supplies initial settings for offline forks.
    // For a live source preserve runtime config changes as well.
    if let Some(model) = settings["model"]["id"].as_str().or_else(|| settings["model"].as_str()) {
        session.set_model(model, settings["model"]["provider"].as_str().or_else(|| settings["provider"].as_str())).await?;
    }
    if let Some(level) = settings["thinkingLevel"].as_str() { session.set_thinking_level(level).await?; }
    if let Some(enabled) = settings["compactionEnabled"].as_bool() { session.set_compaction_enabled(enabled).await?; }
    let mut result = json!({"sessionId":fork.id});
    if let Ok(config) = config_options_for_capability(&session, &state, context.writer.supports_boolean_config()).await {
        result["configOptions"] = config["configOptions"].clone();
    }
    crate::history::save_additional_directories(&fork.id, &roots)?;
    if *cancelled.borrow_and_update() { return Err(RequestError::Cancelled); }
    context.sessions.lock().await.insert(fork.id.clone(), session.clone());
    owner.0.take(); saved_owner.1 = true;
    if let Some(watcher) = spawn_durable_update_watcher(session, context.writer, fork.id.clone()) {
        context.watchers.lock().await.insert(fork.id, watcher);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn negotiation_is_explicit_and_fields_are_gated() {
        assert!(!negotiated(&json!({"clientCapabilities":{"session":{"fork":{}}}})));
        assert!(!negotiated(&json!({"clientCapabilities":{"_meta":{OPT_IN:"older-revision"}}})));
        assert!(!negotiated(&json!({"clientCapabilities":{"_meta":{OPT_IN:true}}})));
        let params = json!({"clientCapabilities":{"_meta":{OPT_IN:REVISION}}});
        assert!(negotiated(&params));
        assert!(!negotiated(&json!({"protocolVersion":2,"clientCapabilities":{"_meta":{OPT_IN:REVISION}}})));
        assert!(!negotiated(&json!({"protocolVersion":1,"capabilities":{"_meta":{OPT_IN:REVISION}}})));
        assert!(negotiated(&json!({"protocolVersion":2,"capabilities":{"_meta":{OPT_IN:REVISION}}})));
        let mut response = json!({"agentCapabilities":{"sessionCapabilities":{}}});
        advertise(&mut response, false, false);
        assert!(response.pointer("/agentCapabilities/sessionCapabilities/fork").is_none());
        assert!(response.pointer("/agentCapabilities/sessionCapabilities/systemPrompt").is_none());
        advertise(&mut response, true, false);
        assert_eq!(response.pointer("/agentCapabilities/sessionCapabilities/systemPrompt"), Some(&json!({"mode":{"override":{}}})));
        let mut v2 = json!({"capabilities":{"session":{}}});
        advertise(&mut v2, true, true);
        assert_eq!(v2.pointer("/capabilities/session/systemPrompt"), Some(&json!({"mode":{"override":{}}})));
        assert!(response.pointer("/agentCapabilities/sessionCapabilities/archive").unwrap().is_object());
        assert!(archive_filter(&json!({"archived":"only"}), false).is_err());
        assert!(archive_filter(&json!({"archived":true}), true).is_err());
        assert_eq!(archive_filter(&json!({"archived":"only"}), true).unwrap(), ArchiveFilter::Only);
    }
    #[test]
    fn title_contract() {
        assert_eq!(normalized_title(&json!({"title":"  chosen\n title "})).unwrap(), "chosen title");
        for value in [json!(null), json!(true), json!("  "), json!("x".repeat(257))] {
            assert!(normalized_title(&json!({"title":value})).is_err());
        }
    }
}
