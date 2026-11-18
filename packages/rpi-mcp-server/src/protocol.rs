//! MCP stdio server: JSON-RPC 2.0 framing, request dispatch and tool mapping.
//!
//! Protocol output is the only thing written to stdout; diagnostics go to
//! stderr. `tools/call` runs in its own task so a `notifications/cancelled`
//! frame can still be read and forwarded to the native run while it is active.

use rpi_extcore::host::{HostSession, SessionOptions};
use rpi_extcore::process::resolve_rpi_executable;
use rpi_extcore::run::RunOutcome;
use rpi_plugin_sdk::stdio::{InputLine, StdinLines};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch, Semaphore};
use tokio::task::JoinSet;

const SUPPORTED_PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
const SERVER_NAME: &str = "rpi-mcp-server";

/// Transport frame bound. MCP has no length header, so a frame is one line.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;
/// Concurrent `tools/call` ceiling; excess calls fail instead of queueing.
const MAX_CONCURRENT_CALLS: usize = 16;
/// Model-visible text bound per tool result.
const MAX_TOOL_TEXT: usize = 256 * 1024;
/// Queued outbound frames before the writer is considered stalled.
const OUTPUT_QUEUE: usize = 64;
/// How long a closed stdin keeps waiting for already-dispatched tool calls.
const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

#[derive(Debug, serde::Deserialize)]
struct Request {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Clone)]
pub struct ServerOptions {
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub bypass: bool,
    /// Explicit native session directory; `None` uses the native default.
    pub session_dir: Option<PathBuf>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            model: None,
            provider: None,
            bypass: false,
            session_dir: None,
        }
    }
}

impl ServerOptions {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            ..Self::default()
        }
    }
}

/// One MCP server instance bound to exactly one native session.
pub struct McpServer {
    session: Arc<HostSession>,
    state: Mutex<ServerState>,
}

#[derive(Default)]
struct ServerState {
    initialized: bool,
    /// Request id of the tool call that currently drives the native run.
    active: Option<String>,
}

impl McpServer {
    pub async fn open(options: ServerOptions) -> Result<Self, String> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let timings_dir = options.cwd.join(".rpi").join("mcp-timings");
        let session = HostSession::spawn_options(SessionOptions {
            executable: resolve_rpi_executable(),
            project: options.cwd.clone(),
            session_id,
            session_dir: options.session_dir,
            saved_session: None,
            fork_source: None,
            timings_dir,
            model: options.model,
            provider: options.provider,
            permission_gate: !options.bypass,
            bypass: options.bypass,
            resources: None,
        })
        .await?;
        Ok(Self::from_session(session))
    }

    /// Wrap an existing native session. The caller keeps ownership of its
    /// lifecycle; `close()` still shuts it down.
    pub fn from_session(session: Arc<HostSession>) -> Self {
        Self {
            session,
            state: Mutex::new(ServerState::default()),
        }
    }

    pub async fn close(&self) {
        self.session.shutdown().await;
    }

    fn initialized(&self) -> bool {
        self.state.lock().expect("server state").initialized
    }

    fn initialize(&self, params: Option<Value>) -> Value {
        let requested = params
            .as_ref()
            .and_then(|value| value.get("protocolVersion"))
            .and_then(Value::as_str);
        let protocol_version = requested
            .filter(|value| SUPPORTED_PROTOCOLS.contains(value))
            .unwrap_or(SUPPORTED_PROTOCOLS[0]);
        self.state.lock().expect("server state").initialized = true;
        json!({
            "protocolVersion": protocol_version,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
            "instructions": "Use rpi_prompt to run the rpi agent in the current workspace."
        })
    }

    /// Claim the native run for `id`, so a later cancellation can be matched.
    fn claim(&self, id: &Value) -> bool {
        let Some(key) = request_id_key(id) else {
            return false;
        };
        let mut state = self.state.lock().expect("server state");
        if state.active.is_some() {
            return false;
        }
        state.active = Some(key);
        true
    }

    fn release(&self) {
        self.state.lock().expect("server state").active = None;
    }

    /// Whether a cancellation frame targets the in-flight native run.
    fn take_cancelled(&self, id: &Value) -> bool {
        let Some(key) = request_id_key(id) else {
            return false;
        };
        let mut state = self.state.lock().expect("server state");
        if state.active.as_deref() == Some(key.as_str()) {
            state.active = None;
            return true;
        }
        false
    }

    /// Methods answered without owning the native run.
    fn handle_request(&self, method: &str, params: Option<Value>) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(self.initialize(params)),
            "ping" | "notifications/initialized" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tool_definitions()})),
            "tools/call" => {
                if !self.initialized() {
                    return Err(RpcError::invalid_request("initialize must complete first"));
                }
                // Parsed here so a malformed call fails before the native run.
                parse_tool_call(params).map(|call| call.name)?;
                Err(RpcError::invalid_request(
                    "tools/call is dispatched asynchronously",
                ))
            }
            other => Err(RpcError::method_not_found(other)),
        }
    }

    async fn call_tool(
        self: &Arc<Self>,
        id: &Value,
        params: Option<Value>,
        out: &mpsc::Sender<Value>,
    ) {
        let call = match self.initialized() {
            true => parse_tool_call(params),
            false => Err(RpcError::invalid_request("initialize must complete first")),
        };
        let call = match call {
            Ok(call) => call,
            Err(error) => {
                let _ = send(out, error_response(id.clone(), error)).await;
                return;
            }
        };
        if call.name == "rpi_prompt" && !self.claim(id) {
            let _ = send(
                out,
                error_response(
                    id.clone(),
                    RpcError::new(-32603, "another tool call is already driving the agent"),
                ),
            )
            .await;
            return;
        }
        let tracked = call.name == "rpi_prompt";
        let result = self.execute(call, out).await;
        if tracked {
            self.release();
        }
        let response = match result {
            Ok(value) => ok_response(id.clone(), tool_result(value, false)),
            Err(error) => ok_response(id.clone(), tool_result(json!({"error":error}), true)),
        };
        let _ = send(out, response).await;
    }

    async fn execute(
        self: &Arc<Self>,
        call: ToolCall,
        out: &mpsc::Sender<Value>,
    ) -> Result<Value, String> {
        let arguments = call.arguments;
        match call.name.as_str() {
            "rpi_prompt" => {
                let message = string_argument(&arguments, "message")?;
                self.prompt(message, out, call.progress_token).await
            }
            "rpi_state" => self.session.state().await,
            "rpi_available_models" => self.session.available_models().await,
            "rpi_thinking_levels" => self.session.available_thinking_levels().await,
            "rpi_set_model" => {
                let model = string_argument(&arguments, "model")?;
                self.session
                    .set_model(&model, arguments.get("provider").and_then(Value::as_str))
                    .await
                    .map(|_| json!({"updated":true}))
            }
            "rpi_set_thinking_level" => {
                let level = string_argument(&arguments, "level")?;
                self.session
                    .set_thinking_level(&level)
                    .await
                    .map(|_| json!({"updated":true}))
            }
            "rpi_abort" => self
                .session
                .abort_run()
                .await
                .map(|_| json!({"aborted":true})),
            // Not reachable: `parse_tool_call` rejects unknown tool names.
            other => Err(format!("unknown tool: {other}")),
        }
    }

    /// One prompt turn, settled by native acceptance plus settlement, with
    /// progress notifications when the client supplied a progress token.
    async fn prompt(
        &self,
        message: String,
        out: &mpsc::Sender<Value>,
        progress_token: Option<Value>,
    ) -> Result<Value, String> {
        let mut run = self.session.begin_prompt(message).await?;
        let mut text = String::new();
        let mut cancelled = false;
        loop {
            let event = tokio::select! {
                biased;
                _ = run.cancellation.changed(), if !cancelled => {
                    cancelled = true;
                    continue;
                }
                event = run.events.recv() => event,
            };
            let event = event.map_err(|error| format!("native event stream failed: {error}"))?;
            let progress = run.tracker.observe(&event, cancelled);
            for delta in progress.deltas {
                if delta["kind"] != "text" {
                    continue;
                }
                let Some(chunk) = delta["text"].as_str() else {
                    continue;
                };
                text.push_str(chunk);
                if let Some(token) = &progress_token {
                    let notification = json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/progress",
                        "params": {
                            "progressToken": token,
                            "progress": text.chars().count(),
                            "message": "agent output",
                        },
                    });
                    let _ = send(out, notification).await;
                }
            }
            let Some(outcome) = progress.outcome else {
                continue;
            };
            return match outcome {
                Ok(RunOutcome::Cancelled) => Err("prompt cancelled".into()),
                Ok(outcome) => Ok(json!({
                    "text": truncate(&text, MAX_TOOL_TEXT),
                    "outcome": format_outcome(outcome),
                })),
                Err(error) => Err(error),
            };
        }
    }
}

/// Serve MCP on the native session until stdin ends or `stop` fires.
pub async fn run(
    mut input: StdinLines,
    pending: Option<String>,
    stop: watch::Receiver<bool>,
    options: ServerOptions,
) -> i32 {
    let server = match McpServer::open(options).await {
        Ok(server) => Arc::new(server),
        Err(error) => {
            eprintln!("[rpi-mcp] failed to start native session: {error}");
            return 1;
        }
    };
    serve(
        server,
        move || input.poll_line(),
        pending,
        stop,
        tokio::io::stdout(),
    )
    .await
}

/// Transport-independent dispatcher. `poll` yields one stdin line at a time and
/// `out` receives serialized JSON-RPC frames, one per line.
pub async fn serve<W, F>(
    server: Arc<McpServer>,
    mut poll: F,
    pending: Option<String>,
    mut stop: watch::Receiver<bool>,
    out: W,
) -> i32
where
    W: AsyncWrite + Unpin + Send + 'static,
    F: FnMut() -> std::io::Result<InputLine>,
{
    let (sender, receiver) = mpsc::channel::<Value>(OUTPUT_QUEUE);
    let writer = tokio::spawn(writer_task(out, receiver));
    let permits = Arc::new(Semaphore::new(MAX_CONCURRENT_CALLS));
    let mut calls: JoinSet<()> = JoinSet::new();
    let mut pending = pending;
    let mut code = 0;
    // A half-closed client (`echo ... | server`) still reads stdout, so EOF
    // drains in-flight calls; an explicit stop or an I/O error aborts them.
    let mut eof = false;

    loop {
        while calls.try_join_next().is_some() {}
        if *stop.borrow() {
            break;
        }
        let line = match next_line(&mut pending, &mut poll, &mut stop).await {
            Ok(Some(line)) => line,
            Ok(None) => {
                eof = !*stop.borrow();
                break;
            }
            Err(error) => {
                eprintln!("[rpi-mcp] stdin error: {error}");
                code = 1;
                break;
            }
        };
        if line.len() > MAX_FRAME {
            let response = error_response(
                Value::Null,
                RpcError::new(-32600, format!("frame exceeds the {MAX_FRAME} byte limit")),
            );
            if send(&sender, response).await.is_err() {
                code = 1;
                break;
            }
            continue;
        }
        let request = match serde_json::from_str::<Request>(&line) {
            Ok(request) => request,
            Err(error) => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": RpcError::parse_error(&error),
                });
                if send(&sender, response).await.is_err() {
                    code = 1;
                    break;
                }
                continue;
            }
        };
        if request
            .jsonrpc
            .as_deref()
            .is_some_and(|value| value != "2.0")
        {
            if let Some(id) = request.id {
                let response =
                    error_response(id, RpcError::invalid_request("jsonrpc must be \"2.0\""));
                if send(&sender, response).await.is_err() {
                    code = 1;
                    break;
                }
            }
            continue;
        }

        let Some(id) = request.id else {
            // Notifications never carry a response.
            if request.method == "notifications/cancelled" {
                if let Some(target) = request
                    .params
                    .as_ref()
                    .and_then(|params| params.get("requestId"))
                    .filter(|value| server.take_cancelled(value))
                {
                    let _ = target;
                    if server.session.abort_run().await.is_err() {
                        eprintln!("[rpi-mcp] failed to abort the cancelled run");
                    }
                }
            }
            continue;
        };

        if request.method == "tools/call" {
            let permit = match Arc::clone(&permits).try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    let response =
                        error_response(id, RpcError::new(-32603, "too many concurrent tool calls"));
                    if send(&sender, response).await.is_err() {
                        code = 1;
                        break;
                    }
                    continue;
                }
            };
            let server = Arc::clone(&server);
            let sender = sender.clone();
            calls.spawn(async move {
                let _permit = permit;
                server.call_tool(&id, request.params, &sender).await;
            });
            continue;
        }

        let response = match server.handle_request(&request.method, request.params) {
            Ok(result) => ok_response(id, result),
            Err(error) => error_response(id, error),
        };
        if send(&sender, response).await.is_err() {
            code = 1;
            break;
        }
    }

    // Cancellation is the resource owner's job: settle or drop in-flight calls,
    // then the native session, then the writer.
    if eof {
        let drain = async { while calls.join_next().await.is_some() {} };
        if tokio::time::timeout(DRAIN_TIMEOUT, drain).await.is_err() {
            eprintln!("[rpi-mcp] in-flight calls did not settle; aborting them");
            calls.shutdown().await;
        }
    } else {
        calls.shutdown().await;
    }
    let _ = server.session.abort_run().await;
    server.close().await;
    drop(sender);
    if writer.await.map(|written| !written).unwrap_or(true) {
        code = 1;
    }
    code
}

async fn writer_task<W: AsyncWrite + Unpin>(
    mut out: W,
    mut receiver: mpsc::Receiver<Value>,
) -> bool {
    while let Some(value) = receiver.recv().await {
        let mut line = value.to_string();
        line.push('\n');
        if out.write_all(line.as_bytes()).await.is_err() || out.flush().await.is_err() {
            return false;
        }
    }
    true
}

/// Queue one outbound frame. `Err` means the client is gone.
async fn send(out: &mpsc::Sender<Value>, value: Value) -> Result<(), ()> {
    out.send(value).await.map_err(|_| ())
}

async fn next_line<F>(
    pending: &mut Option<String>,
    poll: &mut F,
    stop: &mut watch::Receiver<bool>,
) -> std::io::Result<Option<String>>
where
    F: FnMut() -> std::io::Result<InputLine>,
{
    loop {
        if let Some(line) = pending.take() {
            return Ok(Some(line));
        }
        match poll()? {
            InputLine::Line(line) => return Ok(Some(line)),
            InputLine::Eof => return Ok(None),
            InputLine::Pending => {
                tokio::select! {
                    _ = stop.changed() => {}
                    _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {}
                }
                if *stop.borrow() {
                    return Ok(None);
                }
            }
        }
    }
}

#[derive(Debug)]
struct ToolCall {
    name: String,
    arguments: Map<String, Value>,
    progress_token: Option<Value>,
}

fn parse_tool_call(params: Option<Value>) -> Result<ToolCall, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("params is required"))?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| RpcError::invalid_params("tools/call requires a tool name"))?;
    if !tool_definitions()
        .iter()
        .any(|tool| tool["name"].as_str() == Some(name))
    {
        return Err(RpcError::tool_not_found(name));
    }
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(arguments)) => arguments.clone(),
        Some(_) => return Err(RpcError::invalid_params("arguments must be an object")),
    };
    let progress_token = params
        .get("_meta")
        .and_then(|meta| meta.get("progressToken"))
        .filter(|value| value.is_string() || value.is_number())
        .cloned();
    Ok(ToolCall {
        name: name.to_string(),
        arguments,
        progress_token,
    })
}

fn string_argument(arguments: &Map<String, Value>, key: &str) -> Result<String, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{key} must be a non-empty string"))
}

fn request_id_key(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated]", &text[..end])
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

fn error_response(id: Value, error: RpcError) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":error})
}

fn tool_result(value: Value, is_error: bool) -> Value {
    let text = match &value {
        Value::String(text) => text.clone(),
        Value::Object(map) => map
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string()),
        _ => value.to_string(),
    };
    json!({
        "content": [{"type":"text","text":truncate(&text, MAX_TOOL_TEXT)}],
        "isError": is_error,
        "structuredContent": value,
    })
}

fn format_outcome(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Completed => "completed",
        RunOutcome::Cancelled => "cancelled",
        RunOutcome::TokenLimit => "token_limit",
    }
}

fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "rpi_prompt",
            "Run the rpi agent for one prompt and return its settled text.",
            json!({
                "type":"object",
                "properties":{"message":{"type":"string","description":"Prompt for the rpi agent."}},
                "required":["message"]
            }),
        ),
        tool(
            "rpi_state",
            "Read the current native rpi session state.",
            object_schema(),
        ),
        tool(
            "rpi_available_models",
            "List models available to the native host.",
            object_schema(),
        ),
        tool(
            "rpi_thinking_levels",
            "List thinking levels available to the native host.",
            object_schema(),
        ),
        tool(
            "rpi_set_model",
            "Change the native rpi session model.",
            json!({
                "type":"object",
                "properties":{"model":{"type":"string"},"provider":{"type":"string"}},
                "required":["model"]
            }),
        ),
        tool(
            "rpi_set_thinking_level",
            "Change the native rpi thinking level.",
            json!({
                "type":"object",
                "properties":{"level":{"type":"string"}},
                "required":["level"]
            }),
        ),
        tool(
            "rpi_abort",
            "Abort the active native rpi run.",
            object_schema(),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({"name":name,"description":description,"inputSchema":input_schema})
}

fn object_schema() -> Value {
    json!({"type":"object","properties":{}})
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(-32600, message)
    }
    fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(-32602, message)
    }
    fn method_not_found(method: &str) -> Self {
        Self::new(-32601, format!("method not found: {method}"))
    }
    fn tool_not_found(tool: &str) -> Self {
        Self::new(-32602, format!("unknown tool: {tool}"))
    }
    fn parse_error(error: &serde_json::Error) -> Self {
        Self::new(-32700, format!("parse error: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_catalog_has_stable_native_tools() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 7);
        assert_eq!(tools[0]["name"], "rpi_prompt");
        assert_eq!(tools[0]["inputSchema"]["required"], json!(["message"]));
        for tool in &tools {
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert!(!tool["description"].as_str().unwrap().is_empty());
        }
    }

    #[test]
    fn negotiates_supported_protocol_version() {
        assert!(SUPPORTED_PROTOCOLS.contains(&"2025-06-18"));
        assert!(SUPPORTED_PROTOCOLS.contains(&"2024-11-05"));
    }

    #[test]
    fn rejects_unknown_tools_and_bad_arguments() {
        let error = parse_tool_call(Some(json!({"name":"nope"}))).unwrap_err();
        assert_eq!(error.code, -32602);
        // Omitted arguments are an empty object; the tool validates them.
        let call = parse_tool_call(Some(json!({"name":"rpi_prompt"}))).unwrap();
        assert!(call.arguments.is_empty());
        let error = string_argument(&call.arguments, "message").unwrap_err();
        assert!(error.contains("non-empty string"), "{error}");
        let error = parse_tool_call(Some(json!({"name":"rpi_state","arguments":[]}))).unwrap_err();
        assert!(error.message.contains("object"));
    }

    #[test]
    fn accepts_structured_arguments_and_progress_token() {
        let call = parse_tool_call(Some(json!({
            "name":"rpi_prompt",
            "arguments":{"message":"hi"},
            "_meta":{"progressToken":"p1"}
        })))
        .unwrap();
        assert_eq!(call.name, "rpi_prompt");
        assert_eq!(call.arguments["message"], "hi");
        assert_eq!(call.progress_token, Some(json!("p1")));
    }

    #[test]
    fn request_ids_normalize_for_cancellation_matching() {
        assert_eq!(request_id_key(&json!(7)), Some("7".into()));
        assert_eq!(request_id_key(&json!("abc")), Some("abc".into()));
        assert_eq!(request_id_key(&json!(null)), None);
        assert_eq!(request_id_key(&json!({})), None);
    }

    #[test]
    fn truncation_keeps_utf8_boundaries() {
        let text = "你好世界";
        assert_eq!(truncate(text, text.len()), text);
        let cut = truncate(text, 7);
        assert!(cut.ends_with("\n[truncated]"));
        assert!(cut.len() < text.len() + 13);
    }

    #[test]
    fn tool_result_keeps_text_and_structured_content() {
        let result = tool_result(json!({"text":"done","outcome":"completed"}), false);
        assert_eq!(result["content"][0]["text"], "done");
        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["outcome"], "completed");
    }
}
