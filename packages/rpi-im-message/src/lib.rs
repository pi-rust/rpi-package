use feishu_sdk::core::{noop_logger, Config, FEISHU_BASE_URL, LARK_BASE_URL};
use feishu_sdk::event::models::MessageEvent;
use feishu_sdk::event::{
    Event, EventDispatcher, EventDispatcherConfig, EventHandler, EventHandlerResult,
};
use feishu_sdk::ws::{StreamClient, StreamConfig};
use feishu_sdk::Client;
use rpi_plugin_sdk::{
    register_entrypoint, EventTag, FreeStringFn, PluginApiVt, RuntimeActionFn, RuntimeActionId,
    StablePluginEvent, StableToolSchema, StbString, StbStringRef, StepHandle, StepResult,
    ToolPartialCb,
};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use std::env;
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const EVENT_TYPE_MESSAGE: &str = "im.message.receive_v1";
const MAX_MESSAGE_BYTES: usize = 1_048_576;
const DEFAULT_AUTO_REPLY_TIMEOUT_SECONDS: u64 = 600;
const SERVER_FLAG: &str = "im-message-server";
const PROFILE_FLAG: &str = "im-profile";

#[derive(Clone, Copy)]
struct StartupContext {
    runtime_action: RuntimeActionFn,
    free_string: FreeStringFn,
    host_user_data: *mut std::ffi::c_void,
}

// The host callback is supplied during plugin registration and remains valid
// for the lifetime of the loaded extension. It is called from the Feishu
// runtime thread after registration, so the opaque host pointer must be
// carried alongside the function pointers.
unsafe impl Send for StartupContext {}
unsafe impl Sync for StartupContext {}

static HOST_RUNTIME: OnceLock<StartupContext> = OnceLock::new();
static PRINT_PROCESS_LOCK: Mutex<()> = Mutex::new(());
static ACK_REACTION_COUNTER: AtomicU64 = AtomicU64::new(0);

const ACK_REACTIONS: [(&str, &str); 3] = [("了解", "OK"), ("敲键盘", "Typing"), ("冲！", "JIAYI")];

#[derive(Clone, Debug)]
struct Profile {
    name: String,
    provider: String,
    domain: String,
    app_id: String,
    app_secret: String,
    allow_chats: Vec<String>,
    mention_required: bool,
    auto_reconnect: bool,
    reconnect_interval: Duration,
    max_queue_size: usize,
    default_conversation_id: Option<String>,
    auto_reply: bool,
    auto_reply_model: Option<String>,
    auto_reply_timeout: Duration,
    ack_reaction: bool,
}

impl Profile {
    fn from_value(name: &str, value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| format!("profile {name} must be an object"))?;
        let provider = string_field(object, "provider", "feishu")?;
        if provider != "feishu" {
            return Err(format!("profile {name} provider must be feishu"));
        }
        let domain = string_field(object, "domain", "feishu")?;
        if domain != "feishu" && domain != "lark" {
            return Err("domain must be feishu or lark".into());
        }
        let transport = string_field(object, "transport", "long_connection")?;
        if transport != "long_connection" {
            return Err("transport must be long_connection".into());
        }

        let app_id = resolve_value_or_env(object, "appId", "appIdEnv")?
            .ok_or_else(|| format!("profile {name} requires appId or appIdEnv"))?;
        let app_secret = resolve_app_secret(object)?;

        let allow_chats = object
            .get("allowChats")
            .map(|value| string_array(value, "allowChats"))
            .transpose()?
            .unwrap_or_default();
        if allow_chats.len() > 256 {
            return Err("allowChats cannot contain more than 256 entries".into());
        }
        let mention_required = bool_field(object, "mentionRequired", false)?;
        let auto_reconnect = bool_field(object, "autoReconnect", true)?;
        let reconnect_seconds = u64_field(object, "reconnectIntervalSeconds", 5, 1, 300)?;
        let max_queue_size = u64_field(object, "maxQueueSize", 256, 1, 4096)? as usize;
        let default_conversation_id = object
            .get("defaultConversationId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned);
        let auto_reply = bool_field(object, "autoReply", false)?;
        let auto_reply_model = object
            .get("autoReplyModel")
            .or_else(|| object.get("model"))
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| "autoReplyModel must be a non-empty string".to_string())
            })
            .transpose()?;
        let auto_reply_timeout_seconds = u64_field(
            object,
            "autoReplyTimeoutSeconds",
            DEFAULT_AUTO_REPLY_TIMEOUT_SECONDS,
            1,
            3600,
        )?;
        let ack_reaction = bool_field(object, "ackReaction", true)?;

        Ok(Self {
            name: name.to_owned(),
            provider,
            domain,
            app_id,
            app_secret,
            allow_chats,
            mention_required,
            auto_reconnect,
            reconnect_interval: Duration::from_secs(reconnect_seconds),
            max_queue_size,
            default_conversation_id,
            auto_reply,
            auto_reply_model,
            auto_reply_timeout: Duration::from_secs(auto_reply_timeout_seconds),
            ack_reaction,
        })
    }
}

fn resolve_app_secret(object: &Map<String, Value>) -> Result<String, String> {
    let has_direct_secret = object.contains_key("appSecret");
    let has_env_secret = object.contains_key("appSecretEnv");
    if has_direct_secret && has_env_secret {
        return Err("configure either appSecret or appSecretEnv, not both".into());
    }
    if let Some(value) = object.get("appSecret") {
        return value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| "appSecret must be a non-empty string".into());
    }

    let secret_env = object
        .get("appSecretEnv")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("RPI_FEISHU_APP_SECRET");
    let app_secret = env::var(secret_env)
        .map_err(|_| format!("environment variable {secret_env} is not set"))?;
    if app_secret.trim().is_empty() {
        return Err(format!("environment variable {secret_env} is empty"));
    }
    Ok(app_secret)
}

fn string_field(object: &Map<String, Value>, key: &str, default: &str) -> Result<String, String> {
    match object.get(key) {
        None => Ok(default.to_owned()),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(value.clone()),
        Some(_) => Err(format!("{key} must be a non-empty string")),
    }
}

fn resolve_value_or_env(
    object: &Map<String, Value>,
    value_key: &str,
    env_key: &str,
) -> Result<Option<String>, String> {
    if let Some(value) = object.get(value_key) {
        return value
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("{value_key} must be a non-empty string"))
            .map(Some);
    }
    if let Some(value) = object.get(env_key) {
        let name = value
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("{env_key} must be a non-empty string"))?;
        return env::var(name)
            .map_err(|_| format!("environment variable {name} is not set"))
            .map(Some);
    }
    Ok(None)
}

fn bool_field(object: &Map<String, Value>, key: &str, default: bool) -> Result<bool, String> {
    match object.get(key) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("{key} must be a boolean")),
    }
}

fn u64_field(
    object: &Map<String, Value>,
    key: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, String> {
    match object.get(key) {
        None => Ok(default),
        Some(Value::Number(value)) => value
            .as_u64()
            .filter(|value| (*value >= min) && (*value <= max))
            .ok_or_else(|| format!("{key} must be an integer from {min} to {max}")),
        Some(_) => Err(format!("{key} must be an integer from {min} to {max}")),
    }
}

fn string_array(value: &Value, key: &str) -> Result<Vec<String>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{key} must be an array of strings"))?
        .iter()
        .map(|item| {
            item.as_str()
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("{key} must contain non-empty strings"))
        })
        .collect()
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
}

fn config_path() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("RPI_IM_CONFIG") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("RPI_IM_CONFIG must be an absolute path".into());
        }
        return Ok(path);
    }
    let project = env::current_dir()
        .map_err(|error| format!("failed to resolve current directory: {error}"))?
        .join(".rpi")
        .join("im.json");
    if project.is_file() {
        return Ok(project);
    }
    home_dir()
        .map(|home| home.join(".rpi").join("agent").join("im.json"))
        .ok_or_else(|| "cannot resolve home directory for ~/.rpi/agent/im.json".into())
}

fn load_profile(requested: Option<&str>) -> Result<(PathBuf, Profile), String> {
    let path = config_path()?;
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("could not read IM config {}: {error}", path.display()))?;
    if text.len() > MAX_MESSAGE_BYTES {
        return Err("IM config exceeds 1 MiB".into());
    }
    let root: Value =
        serde_json::from_str(&text).map_err(|error| format!("invalid IM config JSON: {error}"))?;
    let object = root
        .as_object()
        .ok_or_else(|| "IM config root must be an object".to_string())?;
    let profiles = object
        .get("profiles")
        .and_then(Value::as_object)
        .ok_or_else(|| "IM config requires a profiles object".to_string())?;
    let profile_name = requested
        .or_else(|| object.get("defaultProfile").and_then(Value::as_str))
        .ok_or_else(|| "profile is required when defaultProfile is not configured".to_string())?;
    let profile = profiles
        .get(profile_name)
        .ok_or_else(|| format!("IM profile {profile_name} was not found"))?;
    Ok((path, Profile::from_value(profile_name, profile)?))
}

struct RuntimeState {
    profile: Profile,
    queue: Mutex<VecDeque<Value>>,
    queue_ready: Condvar,
    status: Mutex<ServerStatus>,
    host_runtime: Option<StartupContext>,
}

#[derive(Clone, Debug)]
struct ServerStatus {
    state: String,
    last_error: Option<String>,
    dropped: u64,
}

impl RuntimeState {
    fn new(profile: Profile) -> Self {
        Self {
            profile,
            queue: Mutex::new(VecDeque::new()),
            queue_ready: Condvar::new(),
            status: Mutex::new(ServerStatus {
                state: "starting".into(),
                last_error: None,
                dropped: 0,
            }),
            host_runtime: HOST_RUNTIME.get().copied(),
        }
    }

    fn set_status(&self, state: &str, error: Option<String>) {
        if let Ok(mut status) = self.status.lock() {
            status.state = state.to_owned();
            status.last_error = error;
        }
        self.queue_ready.notify_all();
    }

    fn push_message(&self, message: Value) {
        if let Ok(mut queue) = self.queue.lock() {
            if queue.len() >= self.profile.max_queue_size {
                queue.pop_front();
                if let Ok(mut status) = self.status.lock() {
                    status.dropped = status.dropped.saturating_add(1);
                }
            }
            queue.push_back(message);
        }
        self.queue_ready.notify_all();
    }

    fn receive(&self, timeout: Duration) -> Result<Value, String> {
        let deadline = Instant::now() + timeout;
        let mut queue = self.queue.lock().map_err(|_| "message queue is poisoned")?;
        loop {
            if let Some(message) = queue.pop_front() {
                return Ok(message);
            }
            let state = self
                .status
                .lock()
                .map_err(|_| "server status is poisoned")?
                .clone();
            if state.state == "error" {
                return Err(state
                    .last_error
                    .unwrap_or_else(|| "IM server failed".into()));
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(json!({"type":"timeout","state":state.state}));
            };
            let (next_queue, result) = self
                .queue_ready
                .wait_timeout(queue, remaining)
                .map_err(|_| "message queue is poisoned")?;
            queue = next_queue;
            if result.timed_out() {
                let state = self
                    .status
                    .lock()
                    .map_err(|_| "server status is poisoned")?
                    .clone();
                return Ok(json!({"type":"timeout","state":state.state}));
            }
        }
    }

    fn status_value(&self, id: &str) -> Value {
        let status = self.status.lock().ok().map(|value| value.clone());
        let queue_len = self.queue.lock().map(|queue| queue.len()).unwrap_or(0);
        let status = status.unwrap_or(ServerStatus {
            state: "unknown".into(),
            last_error: Some("server status is unavailable".into()),
            dropped: 0,
        });
        json!({
            "id": id,
            "state": status.state,
            "provider": self.profile.provider,
            "profile": self.profile.name,
            "domain": self.profile.domain,
            "transport": "long_connection",
            "queueLength": queue_len,
            "maxQueueSize": self.profile.max_queue_size,
            "dropped": status.dropped,
            "lastError": status.last_error,
            "autoReply": self.profile.auto_reply,
            "autoReplyTimeoutSeconds": self.profile.auto_reply_timeout.as_secs(),
            "ackReaction": self.profile.ack_reaction,
            "capabilities": ["text", "markdown", "card", "reaction", "reply", "agent_auto_reply"]
        })
    }
}

fn invoke_runtime_action(
    runtime: &StartupContext,
    action: RuntimeActionId,
    args: &Value,
) -> Result<Value, String> {
    let args_json = serde_json::to_string(args).map_err(|error| error.to_string())?;
    let mut output = StbString::empty();
    let result = (runtime.runtime_action)(
        u32::from(action),
        StbStringRef::from_str(&args_json),
        &mut output,
        runtime.host_user_data,
    );
    let output_text = output.to_string_lossy();
    (runtime.free_string)(output);
    if result != 0 {
        if let Ok(value) = serde_json::from_str::<Value>(&output_text) {
            if let Some(error) = value.get("error").and_then(Value::as_str) {
                return Err(error.to_owned());
            }
        }
        return Err(if output_text.is_empty() {
            format!("runtime action {action:?} failed with status {result}")
        } else {
            output_text
        });
    }
    serde_json::from_str(&output_text)
        .map_err(|error| format!("runtime action returned invalid JSON: {error}"))
}

fn conversation_session_id(conversation_id: &str) -> String {
    let suffix = conversation_id
        .chars()
        .filter(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
        .take(96)
        .collect::<String>();
    if suffix.is_empty() {
        "im-feishu-unknown".into()
    } else {
        format!("im-feishu-{suffix}")
    }
}

fn random_ack_reaction() -> (&'static str, &'static str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or_default();
    let counter = ACK_REACTION_COUNTER.fetch_add(1, Ordering::Relaxed);
    ACK_REACTIONS[((now ^ counter) as usize) % ACK_REACTIONS.len()]
}

async fn send_reaction(
    client: &Client,
    message_id: &str,
    emoji_type: &str,
) -> Result<Value, String> {
    if message_id.trim().is_empty() {
        return Err("message_id is empty".into());
    }
    let response = client
        .operation("im.v1.message_reaction.create")
        .path_param("message_id", message_id)
        .body_json(&json!({
            "reaction_type": {"emoji_type": emoji_type}
        }))
        .map_err(|error| error.to_string())?
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status < 200 || response.status >= 300 {
        return Err(format!(
            "Feishu reaction API returned HTTP {}: {}",
            response.status,
            String::from_utf8_lossy(&response.body)
        ));
    }
    response.json_value().map_err(|error| error.to_string())
}

fn invoke_print_process(
    prompt: &str,
    conversation_id: &str,
    model: Option<&str>,
    timeout: Duration,
) -> Result<Value, String> {
    let _guard = PRINT_PROCESS_LOCK
        .lock()
        .map_err(|_| "rpi print process lock is poisoned".to_string())?;
    let session_id = conversation_session_id(conversation_id);
    let mut args = vec![
        "--print".to_owned(),
        "--no-extensions".to_owned(),
        "--timeout".to_owned(),
        timeout.as_secs().to_string(),
        "--session-id".to_owned(),
        session_id,
    ];
    if let Some(model) = model {
        args.push("--model".to_owned());
        args.push(model.to_owned());
    }
    args.push(prompt.to_owned());
    let mut child = ProcessCommand::new("rpi")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to start rpi print process: {error}"))?;
    let status = match wait_timeout::ChildExt::wait_timeout(&mut child, timeout)
        .map_err(|error| format!("failed waiting for rpi print process: {error}"))?
    {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "rpi print process timed out after {} seconds",
                timeout.as_secs()
            ));
        }
    };
    let output = child
        .wait_with_output()
        .map_err(|error| format!("failed reading rpi print process: {error}"))?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            format!("rpi print process exited with {status}")
        } else {
            stderr
        });
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if text.is_empty() {
        return Err("rpi print process returned an empty reply".into());
    }
    Ok(json!({"status": "completed", "text": text}))
}

fn invoke_print_with_retry(
    prompt: &str,
    conversation_id: &str,
    model: Option<&str>,
    timeout: Duration,
) -> Result<Value, String> {
    let mut last_error = None;
    for attempt in 0..3 {
        match invoke_print_process(prompt, conversation_id, model, timeout) {
            Ok(result) => return Ok(result),
            Err(error) if attempt < 2 && retryable_print_error(&error) => {
                let delay = Duration::from_secs(2 * (attempt + 1) as u64);
                eprintln!(
                    "rpi-im-message: fallback reply attempt {} failed ({}); retrying in {}s",
                    attempt + 1,
                    error,
                    delay.as_secs()
                );
                thread::sleep(delay);
                last_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| "rpi print process failed after retries".into()))
}

fn retryable_print_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("empty reply")
        || error.contains("throttl")
        || error.contains("rate limit")
        || error.contains("http transport error")
}

struct MessageHandler {
    state: Arc<RuntimeState>,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
}

impl EventHandler for MessageHandler {
    fn event_type(&self) -> &str {
        EVENT_TYPE_MESSAGE
    }

    fn handle(
        &self,
        event: Event,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = EventHandlerResult> + Send + '_>> {
        let state = Arc::clone(&self.state);
        let commands = self.commands.clone();
        Box::pin(async move {
            let Some(value) = event.event else {
                return Ok(None);
            };
            let message: MessageEvent = match serde_json::from_value(value) {
                Ok(message) => message,
                Err(error) => {
                    state.set_status(
                        "error",
                        Some(format!("invalid Feishu message event: {error}")),
                    );
                    return Ok(None);
                }
            };
            // Do not feed messages sent by the bot back into the Agent loop.
            if message.sender.sender_type.as_deref() == Some("app") {
                return Ok(None);
            }
            let chat_id = message.message.chat_id.clone().unwrap_or_default();
            if !state.profile.allow_chats.is_empty()
                && !state
                    .profile
                    .allow_chats
                    .iter()
                    .any(|allowed| allowed == &chat_id)
            {
                return Ok(None);
            }
            if state.profile.mention_required
                && message
                    .message
                    .mentions
                    .as_ref()
                    .map_or(true, Vec::is_empty)
            {
                return Ok(None);
            }
            let content = message.message.content.clone().unwrap_or_default();
            let text = extract_message_text(&message.message.message_type, &content);
            let message_id = message.message.message_id.clone().unwrap_or_default();
            eprintln!(
                "rpi-im-message: received message id={} chat={} text_len={}",
                message_id,
                chat_id,
                text.len()
            );
            if state.profile.ack_reaction && !message_id.trim().is_empty() {
                let (reaction_label, emoji_type) = random_ack_reaction();
                if commands
                    .send(Command::React {
                        message_id: message_id.clone(),
                        reaction_label,
                        emoji_type,
                    })
                    .is_err()
                {
                    eprintln!("rpi-im-message: failed to queue receive reaction");
                }
            }
            let sender_id = message.sender.sender_id.as_ref().and_then(|id| {
                id.open_id
                    .clone()
                    .or_else(|| id.user_id.clone())
                    .or_else(|| id.union_id.clone())
            });
            state.push_message(json!({
                "type": "message",
                "provider": "feishu",
                "messageId": message_id,
                "conversationId": chat_id,
                "conversationType": "chat",
                "senderId": sender_id,
                "messageType": message.message.message_type,
                "text": text,
                "content": content,
                "createTime": message.message.create_time,
                "rootId": message.message.root_id,
                "parentId": message.message.parent_id
            }));

            if state.profile.auto_reply && !text.trim().is_empty() {
                let Some(runtime) = state.host_runtime else {
                    eprintln!("rpi-im-message: autoReply is enabled but the rpi runtime bridge is unavailable");
                    return Ok(None);
                };
                let prompt = text.clone();
                let conversation_id = chat_id.clone();
                let process_conversation_id = conversation_id.clone();
                let process_model = state.profile.auto_reply_model.clone();
                let process_timeout = state.profile.auto_reply_timeout;
                let reply_commands = commands.clone();
                eprintln!(
                    "rpi-im-message: scheduling auto reply id={} chat={}",
                    message_id, conversation_id
                );
                tokio::spawn(async move {
                    let generated = tokio::task::spawn_blocking(move || {
                        match invoke_runtime_action(
                            &runtime,
                            RuntimeActionId::SendUserMessage,
                            &json!({"text": prompt.clone()}),
                        ) {
                            Ok(result) => Ok(result),
                            Err(error) if error.contains("before harness was built") => {
                                eprintln!(
                                    "rpi-im-message: host harness is not ready; using rpi print fallback model={}"
                                    , process_model.as_deref().unwrap_or("<rpi default>")
                                );
                                invoke_print_with_retry(
                                    &prompt,
                                    &process_conversation_id,
                                    process_model.as_deref(),
                                    process_timeout,
                                )
                            }
                            Err(error) => Err(error),
                        }
                    })
                    .await;
                    let generated = match generated {
                        Ok(result) => result,
                        Err(error) => {
                            eprintln!("rpi-im-message: auto reply task failed: {error}");
                            return;
                        }
                    };
                    match generated {
                        Ok(result) => {
                            let reply_text = result
                                .get("text")
                                .and_then(Value::as_str)
                                .filter(|value| !value.trim().is_empty());
                            if let Some(reply_text) = reply_text {
                                eprintln!(
                                    "rpi-im-message: generated auto reply id={} chars={}",
                                    message_id,
                                    reply_text.len()
                                );
                                let content = json!({"type": "text", "text": reply_text});
                                if let Err(error) = reply_commands.send(Command::SendAsync {
                                    conversation_id,
                                    content,
                                }) {
                                    eprintln!(
                                        "rpi-im-message: failed to queue auto reply: {error}"
                                    );
                                } else {
                                    eprintln!(
                                        "rpi-im-message: queued auto reply id={} for send",
                                        message_id
                                    );
                                }
                            } else {
                                eprintln!(
                                    "rpi-im-message: Agent completed without reply text: {result}"
                                );
                            }
                        }
                        Err(error) => {
                            eprintln!("rpi-im-message: Agent auto reply failed: {error}");
                        }
                    }
                });
            }
            Ok(None)
        })
    }
}

fn extract_message_text(message_type: &Option<String>, content: &str) -> String {
    if message_type.as_deref() == Some("text") {
        return serde_json::from_str::<Value>(content)
            .ok()
            .and_then(|value| value.get("text").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| content.to_owned());
    }
    content.to_owned()
}

enum Command {
    React {
        message_id: String,
        reaction_label: &'static str,
        emoji_type: &'static str,
    },
    Send {
        conversation_id: String,
        content: Value,
        reply: Sender<Result<Value, String>>,
    },
    SendAsync {
        conversation_id: String,
        content: Value,
    },
    Stop {
        reply: Sender<Result<(), String>>,
    },
}

struct Server {
    id: String,
    state: Arc<RuntimeState>,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
}

static SERVERS: OnceLock<Mutex<HashMap<String, Arc<Server>>>> = OnceLock::new();
static NEXT_SERVER_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn servers() -> &'static Mutex<HashMap<String, Arc<Server>>> {
    SERVERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn start_server(params: &Value) -> Result<Value, String> {
    let profile_name = params.get("profile").and_then(Value::as_str);
    let (config_file, profile) = load_profile(profile_name)?;
    let state = Arc::new(RuntimeState::new(profile.clone()));
    let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let command_sender_for_runtime = commands.clone();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let thread_state = Arc::clone(&state);
    let id = format!("rpi-im-{}", NEXT_SERVER_ID.fetch_add(1, Ordering::Relaxed));
    let thread_id = id.clone();
    let join = thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                thread_state
                    .set_status("error", Some(format!("failed to create runtime: {error}")));
                let _ = ready_tx.send(Err(error.to_string()));
                return;
            }
        };
        let result = runtime.block_on(run_server(
            thread_id,
            thread_state,
            profile,
            command_rx,
            command_sender_for_runtime,
            ready_tx,
        ));
        if let Err(error) = result {
            eprintln!("rpi-im-message server stopped: {error}");
        }
    });

    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            let _ = join.join();
            return Err(error);
        }
        Err(_) => return Err("timed out waiting for IM server startup".into()),
    }
    let server = Arc::new(Server {
        id: id.clone(),
        state,
        commands,
        join: Mutex::new(Some(join)),
    });
    let response = json!({
        "id": id,
        "state": "running",
        "profile": server.state.profile.name,
        "provider": server.state.profile.provider,
        "config": config_file,
        "transport": "long_connection",
        "autoReply": server.state.profile.auto_reply
    });
    servers()
        .lock()
        .map_err(|_| "IM server registry is poisoned")?
        .insert(server.id.clone(), server);
    Ok(response)
}

async fn run_server(
    _id: String,
    state: Arc<RuntimeState>,
    profile: Profile,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
    command_sender: tokio::sync::mpsc::UnboundedSender<Command>,
    ready: mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let base_url = if profile.domain == "lark" {
        LARK_BASE_URL
    } else {
        FEISHU_BASE_URL
    };
    let config = Config::builder(&profile.app_id, &profile.app_secret)
        .base_url(base_url)
        .build();
    let client = match Client::new(config) {
        Ok(client) => client,
        Err(error) => {
            return startup_failure(
                &state,
                &ready,
                format!("failed to create Feishu client: {error}"),
            );
        }
    };
    let dispatcher = EventDispatcher::new(EventDispatcherConfig::new(), noop_logger());
    dispatcher
        .register_handler(Box::new(MessageHandler {
            state: Arc::clone(&state),
            commands: command_sender,
        }))
        .await;
    let stream_config = StreamConfig::new()
        .auto_reconnect(profile.auto_reconnect)
        .reconnect_interval(profile.reconnect_interval)
        .reconnect_count(-1);
    let stream: StreamClient = match client
        .stream()
        .stream_config(stream_config)
        .event_dispatcher(dispatcher)
        .build()
    {
        Ok(stream) => stream,
        Err(error) => {
            return startup_failure(
                &state,
                &ready,
                format!("failed to build Feishu long connection: {error}"),
            );
        }
    };
    let mut stream_task = Box::pin(stream.start());
    state.set_status("running", None);
    let _ = ready.send(Ok(()));

    loop {
        tokio::select! {
            result = &mut stream_task => {
                let error = match result {
                    Ok(()) => "Feishu long connection closed".to_string(),
                    Err(error) => format!("Feishu long connection failed: {error}"),
                };
                state.set_status("error", Some(error));
                break;
            }
            command = commands.recv() => {
                match command {
                    Some(Command::React {
                        message_id,
                        reaction_label,
                        emoji_type,
                    }) => match send_reaction(&client, &message_id, emoji_type).await {
                        Ok(response) => eprintln!(
                            "rpi-im-message: receive reaction sent message={} reaction={} response={}",
                            message_id, reaction_label, response
                        ),
                        Err(error) => eprintln!(
                            "rpi-im-message: receive reaction failed message={} reaction={}: {}",
                            message_id, reaction_label, error
                        ),
                    },
                    Some(Command::Send { conversation_id, content, reply }) => {
                        let result = send_message(&client, &conversation_id, &content).await;
                        let _ = reply.send(result);
                    }
                    Some(Command::SendAsync { conversation_id, content }) => {
                        match send_message(&client, &conversation_id, &content).await {
                            Ok(response) => eprintln!(
                                "rpi-im-message: auto reply sent chat={} response={}",
                                conversation_id, response
                            ),
                            Err(error) => {
                                eprintln!("rpi-im-message: auto reply send failed: {error}");
                            }
                        }
                    }
                    Some(Command::Stop { reply }) => {
                        state.set_status("stopped", None);
                        let _ = reply.send(Ok(()));
                        break;
                    }
                    None => {
                        state.set_status("stopped", None);
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

fn startup_failure(
    state: &Arc<RuntimeState>,
    ready: &mpsc::SyncSender<Result<(), String>>,
    error: String,
) -> Result<(), String> {
    state.set_status("error", Some(error.clone()));
    let _ = ready.send(Err(error.clone()));
    Err(error)
}

async fn send_message(
    client: &Client,
    conversation_id: &str,
    content: &Value,
) -> Result<Value, String> {
    let content_type = content
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "content.type is required".to_string())?;
    let (msg_type, payload) = match content_type {
        "text" => {
            let text = content
                .get("text")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "content.text is required".to_string())?;
            ("text", json!({"text": text}))
        }
        "markdown" => {
            let text = content
                .get("text")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "content.text is required".to_string())?;
            (
                "post",
                json!({"zh_cn":{"title": content.get("title").and_then(Value::as_str).unwrap_or(""), "content":[[{"tag":"text","text":text}]]}}),
            )
        }
        "card" | "interactive" => {
            let card = content.get("card").cloned().unwrap_or_else(|| {
                let mut card = content.clone();
                if let Some(object) = card.as_object_mut() {
                    object.remove("type");
                }
                card
            });
            ("interactive", card)
        }
        _ => return Err("content.type must be one of: text, markdown, card".into()),
    };
    let serialized = serde_json::to_string(&payload).map_err(|error| error.to_string())?;
    if serialized.len() > MAX_MESSAGE_BYTES {
        return Err("message content exceeds 1 MiB".into());
    }
    let body = json!({
        "receive_id": conversation_id,
        "msg_type": msg_type,
        "content": serialized
    });
    let response = client
        .operation("im.v1.message.create")
        .query_param("receive_id_type", "chat_id")
        .body_json(&body)
        .map_err(|error| error.to_string())?
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status < 200 || response.status >= 300 {
        return Err(format!(
            "Feishu message API returned HTTP {}: {}",
            response.status,
            String::from_utf8_lossy(&response.body)
        ));
    }
    response.json_value().map_err(|error| error.to_string())
}

fn timeout_seconds(params: &Value) -> Result<Duration, String> {
    let seconds = params
        .get("timeoutSeconds")
        .and_then(Value::as_u64)
        .unwrap_or(30);
    if !(1..=300).contains(&seconds) {
        return Err("timeoutSeconds must be an integer from 1 to 300".into());
    }
    Ok(Duration::from_secs(seconds))
}

fn string_param(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{key} is required"))
}

fn server_by_id(id: &str) -> Result<Arc<Server>, String> {
    servers()
        .lock()
        .map_err(|_| "IM server registry is poisoned")?
        .get(id)
        .cloned()
        .ok_or_else(|| format!("IM server {id} was not found"))
}

fn stop_server(server: Arc<Server>) -> Result<(), String> {
    let (reply, receiver) = mpsc::channel();
    server
        .commands
        .send(Command::Stop { reply })
        .map_err(|_| "IM server command channel is closed".to_string())?;
    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "timed out stopping IM server")??;
    if let Ok(mut join) = server.join.lock() {
        if let Some(handle) = join.take() {
            let _ = handle.join();
        }
    }
    Ok(())
}

fn im_message_server(params: &Value) -> Result<Value, String> {
    match params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("start")
    {
        "start" => start_server(params),
        "list" => {
            let values = servers()
                .lock()
                .map_err(|_| "IM server registry is poisoned")?
                .values()
                .map(|server| server.state.status_value(&server.id))
                .collect::<Vec<_>>();
            Ok(Value::Array(values))
        }
        "status" => {
            let id = string_param(params, "serverId")?;
            let server = server_by_id(&id)?;
            Ok(server.state.status_value(&id))
        }
        "receive" => {
            let id = string_param(params, "serverId")?;
            let server = server_by_id(&id)?;
            server.state.receive(timeout_seconds(params)?)
        }
        "send" => {
            let id = string_param(params, "serverId")?;
            let server = server_by_id(&id)?;
            let conversation_id = params
                .get("conversationId")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .or_else(|| server.state.profile.default_conversation_id.clone())
                .ok_or_else(|| "conversationId is required".to_string())?;
            let content = params
                .get("content")
                .cloned()
                .ok_or_else(|| "content is required".to_string())?;
            let (reply, receiver) = mpsc::channel();
            server
                .commands
                .send(Command::Send {
                    conversation_id,
                    content,
                    reply,
                })
                .map_err(|_| "IM server command channel is closed".to_string())?;
            receiver
                .recv_timeout(timeout_seconds(params)?)
                .map_err(|_| "timed out sending Feishu message".to_string())?
        }
        "stop" => {
            let id = string_param(params, "serverId")?;
            let server = servers()
                .lock()
                .map_err(|_| "IM server registry is poisoned")?
                .remove(&id)
                .ok_or_else(|| format!("IM server {id} was not found"))?;
            stop_server(server)?;
            Ok(json!({"id": id, "state": "stopped"}))
        }
        _ => Err("action must be one of: start, status, list, receive, send, stop".into()),
    }
}

fn stop_all_servers() {
    let servers = match servers().lock() {
        Ok(mut registry) => registry
            .drain()
            .map(|(_, server)| server)
            .collect::<Vec<_>>(),
        Err(_) => return,
    };
    for server in servers {
        let _ = stop_server(server);
    }
}

fn cli_flag_value(context: &StartupContext, name: &str) -> Result<Value, String> {
    let args = json!({"name": name}).to_string();
    let mut output = StbString::empty();
    let result = (context.runtime_action)(
        u32::from(RuntimeActionId::GetCliFlag),
        StbStringRef::from_str(&args),
        &mut output,
        context.host_user_data,
    );
    let text = output.to_string_lossy();
    (context.free_string)(output);
    if result != 0 {
        return Err(if text.is_empty() {
            format!("host rejected CLI flag lookup for {name}")
        } else {
            text
        });
    }
    let response: Value = serde_json::from_str(&text)
        .map_err(|error| format!("host returned invalid CLI flag response: {error}"))?;
    Ok(response.get("value").cloned().unwrap_or(Value::Null))
}

fn cli_flag_enabled(value: &Value) -> bool {
    match value {
        Value::Bool(value) => *value,
        Value::String(value) => value.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn auto_start_from_cli(context: &StartupContext) -> ! {
    let enabled = match cli_flag_value(context, SERVER_FLAG) {
        Ok(value) => cli_flag_enabled(&value),
        Err(error) => {
            eprintln!("rpi-im-message: failed to read --{SERVER_FLAG}: {error}");
            std::process::exit(2);
        }
    };
    if !enabled {
        unreachable!("auto_start_from_cli called without --{SERVER_FLAG}");
    }

    let profile = match cli_flag_value(context, PROFILE_FLAG) {
        Ok(Value::String(value)) if !value.trim().is_empty() => Some(value),
        Ok(Value::Null) | Ok(Value::Bool(false)) => None,
        Ok(value) => {
            eprintln!(
                "rpi-im-message: --{PROFILE_FLAG} must be a non-empty profile name, got {value}"
            );
            std::process::exit(2);
        }
        Err(error) => {
            eprintln!("rpi-im-message: failed to read --{PROFILE_FLAG}: {error}");
            std::process::exit(2);
        }
    };
    let mut params = json!({"action": "start"});
    if let Some(profile) = profile {
        params["profile"] = Value::String(profile);
    }
    match start_server(&params) {
        Ok(response) => {
            eprintln!("rpi-im-message: server started: {response}");
            eprintln!("rpi-im-message: headless mode is running; press Ctrl+C to stop");
        }
        Err(error) => {
            eprintln!("rpi-im-message: failed to start server: {error}");
            std::process::exit(1);
        }
    }

    loop {
        thread::park();
    }
}

extern "C" fn on_session_shutdown(event: StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    if event.tag == EventTag::SessionShutdown {
        stop_all_servers();
    }
    0
}

struct Drive {
    params: Value,
    cancelled: AtomicBool,
    done: bool,
}

extern "C" fn execute(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let text = params.to_string_lossy();
    params.free_with(free);
    Box::into_raw(Box::new(Drive {
        params: serde_json::from_str(&text).unwrap_or(Value::Null),
        cancelled: AtomicBool::new(false),
        done: false,
    })) as StepHandle
}

extern "C" fn poll(
    handle: StepHandle,
    _: Option<ToolPartialCb>,
    _: *mut std::ffi::c_void,
) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null im message handle".into()));
    }
    let drive = unsafe { &mut *(handle as *mut Drive) };
    if drive.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string(
            "im message operation cancelled".into(),
        ));
    }
    if drive.done {
        return StepResult::err(StbString::from_string(
            "im message operation already completed".into(),
        ));
    }
    drive.done = true;
    match im_message_server(&drive.params) {
        Ok(value) => StepResult::done(StbString::from_string(
            json!({"content":[{"type":"text","text":value.to_string()}],"details":value})
                .to_string(),
        )),
        Err(error) => StepResult::err(StbString::from_string(error)),
    }
}

extern "C" fn cancel(handle: StepHandle) {
    if !handle.is_null() {
        unsafe {
            (&*(handle as *mut Drive))
                .cancelled
                .store(true, Ordering::SeqCst)
        };
    }
}

extern "C" fn destroy(handle: StepHandle) {
    if !handle.is_null() {
        unsafe { drop(Box::from_raw(handle as *mut Drive)) };
    }
}

extern "C" fn free_string(value: StbString) {
    if !value.is_empty() && !value.ptr.is_null() {
        unsafe {
            let bytes = std::slice::from_raw_parts(value.ptr as *const u8, value.len);
            let _ = Box::from_raw(bytes as *const [u8] as *mut [u8]);
        }
    }
}

#[no_mangle]
pub extern "C" fn rpi_plugin_register_v2(api: *const PluginApiVt, abi: u32) -> i32 {
    register_entrypoint(api, abi, |api| {
        let Some(register_tool) = api.register_tool else {
            return 1;
        };
        let schema = Box::new(StableToolSchema {
            name: StbString::from_string("im_message_server".into()),
            description: StbString::from_string(
                "Start and manage a Feishu/Lark SDK long-connection messaging server.".into(),
            ),
            parameters: StbString::from_string(
                r#"{"type":"object","properties":{"action":{"type":"string","enum":["start","status","list","receive","send","stop"]},"profile":{"type":"string"},"serverId":{"type":"string"},"conversationId":{"type":"string"},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":300},"content":{"type":"object","description":"Message content: {type:text|markdown|card,...}"}}}"#.into(),
            ),
        });
        let result = register_tool(&*schema, execute, poll, cancel, destroy, free_string);
        drop(schema);
        if let Some(register_flag) = api.register_flag {
            let _ = register_flag(
                StbStringRef::from_str(SERVER_FLAG),
                StbStringRef::from_str(
                    "Start the Feishu/Lark message server without opening the TUI",
                ),
            );
            let _ = register_flag(
                StbStringRef::from_str(PROFILE_FLAG),
                StbStringRef::from_str("Select the rpi-im-message profile for headless startup"),
            );
            let context = Box::new(StartupContext {
                runtime_action: api.runtime_action,
                free_string: api.free_string,
                host_user_data: api.user_data,
            });
            let _ = HOST_RUNTIME.set(*context);
            let context = Box::leak(context);
            if let Ok(value) = cli_flag_value(context, SERVER_FLAG) {
                if cli_flag_enabled(&value) {
                    auto_start_from_cli(context);
                }
            }
        }
        if let Some(register_event) = api.register_event_handler {
            let _ = register_event(
                EventTag::SessionShutdown,
                on_session_shutdown,
                std::ptr::null_mut(),
            );
        }
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_text_message_content() {
        assert_eq!(
            extract_message_text(&Some("text".into()), r#"{"text":"hello"}"#),
            "hello"
        );
    }

    #[test]
    fn rejects_invalid_profile_provider() {
        let value = json!({"provider":"slack"});
        assert!(Profile::from_value("test", &value).is_err());
    }

    #[test]
    fn accepts_direct_app_secret() {
        let profile = Profile::from_value(
            "test",
            &json!({"appId":"cli_test","appSecret":"secret-value"}),
        )
        .unwrap();
        assert_eq!(profile.app_secret, "secret-value");
        assert!(!profile.auto_reply);
    }

    #[test]
    fn accepts_auto_reply_config() {
        let profile = Profile::from_value(
            "test",
            &json!({
                "appId":"cli_test",
                "appSecret":"secret-value",
                "autoReply":true,
                "autoReplyModel":"huoshan-copy/deepseek-v4-flash-ga-260731"
            }),
        )
        .unwrap();
        assert!(profile.auto_reply);
        assert_eq!(
            profile.auto_reply_model.as_deref(),
            Some("huoshan-copy/deepseek-v4-flash-ga-260731")
        );
        assert_eq!(profile.auto_reply_timeout, Duration::from_secs(600));
        assert!(profile.ack_reaction);
    }

    #[test]
    fn allows_disabling_ack_reactions_and_custom_timeout() {
        let profile = Profile::from_value(
            "test",
            &json!({
                "appId":"cli_test",
                "appSecret":"secret-value",
                "ackReaction":false,
                "autoReplyTimeoutSeconds":120
            }),
        )
        .unwrap();
        assert!(!profile.ack_reaction);
        assert_eq!(profile.auto_reply_timeout, Duration::from_secs(120));
    }

    #[test]
    fn ack_reaction_uses_one_of_supported_choices() {
        let (label, emoji_type) = random_ack_reaction();
        assert!(ACK_REACTIONS
            .iter()
            .any(|choice| choice == &(label, emoji_type)));
    }

    #[test]
    fn derives_stable_conversation_session_id() {
        assert_eq!(
            conversation_session_id("oc_abc-123"),
            "im-feishu-oc_abc-123"
        );
        assert_eq!(conversation_session_id("!!!"), "im-feishu-unknown");
    }

    #[test]
    fn rejects_both_secret_sources() {
        let value = json!({
            "appId":"cli_test",
            "appSecret":"secret-value",
            "appSecretEnv":"RPI_FEISHU_APP_SECRET"
        });
        assert!(Profile::from_value("test", &value)
            .unwrap_err()
            .contains("either appSecret or appSecretEnv"));
    }

    #[test]
    fn recognizes_headless_server_flag_values() {
        assert!(cli_flag_enabled(&Value::Bool(true)));
        assert!(cli_flag_enabled(&Value::String("TRUE".into())));
        assert!(!cli_flag_enabled(&Value::Bool(false)));
        assert!(!cli_flag_enabled(&Value::String("yes".into())));
    }

    #[test]
    fn retries_transient_print_failures() {
        assert!(retryable_print_error(
            "rpi print process returned an empty reply"
        ));
        assert!(retryable_print_error(
            "http transport error: connection reset"
        ));
        assert!(!retryable_print_error(
            "rpi print process exited with exit code 2"
        ));
    }
}
