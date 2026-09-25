use std::ffi::c_void;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rpi_plugin_sdk::{
    register_entrypoint_unified, FreeStringFn, PluginApi, StbString, StbStringRef, StepHandle,
    StepResult, ToolPartialCb,
};
use serde_json::{json, Value};

const SESSION_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_OUTPUT_CHARS: usize = 25_000;
const MAX_DIAGNOSTIC_CHARS: usize = 1_000;

struct Drive {
    tool: &'static str,
    params: Value,
    cancelled: AtomicBool,
    completed: bool,
}

struct ToolSpec {
    name: &'static str,
    description: &'static str,
    parameters: String,
    execute: extern "C" fn(StbStringRef, StbString, Option<FreeStringFn>) -> StepHandle,
}

fn project_root(params: &Value) -> Result<PathBuf, String> {
    let requested = params
        .get("projectPath")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let root = match requested {
        Some(value) => PathBuf::from(value.trim()),
        None => std::env::current_dir()
            .map_err(|err| format!("cannot resolve current directory: {err}"))?,
    };
    if !root.is_absolute() {
        return Err("CodeGraph projectPath must be an absolute path".into());
    }
    if !root.is_dir() {
        return Err(format!(
            "CodeGraph projectPath is not an accessible directory: {}",
            root.display()
        ));
    }
    Ok(root)
}

#[cfg(windows)]
fn spawn_server(root: &Path) -> Result<Child, String> {
    // Command::new("codegraph") does not reliably resolve npm/Scoop .cmd shims
    // on Windows. Let PowerShell discover the native application shim.
    const SCRIPT: &str = concat!(
        "& { param([string]$ProjectPath) ",
        "$ErrorActionPreference = 'Stop'; ",
        "$cmd = Get-Command codegraph -CommandType Application -ErrorAction Stop | Select-Object -First 1; ",
        "if (-not $cmd) { throw 'codegraph command not found'; }; ",
        "& $cmd.Source serve --mcp --path $ProjectPath; exit $LASTEXITCODE; }"
    );
    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            SCRIPT,
        ])
        .arg(root)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to start CodeGraph MCP server: {err}"))
}

#[cfg(not(windows))]
fn spawn_server(root: &Path) -> Result<Child, String> {
    Command::new("codegraph")
        .args(["serve", "--mcp", "--path"])
        .arg(root)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            format!("failed to start CodeGraph MCP server: {err}; install @colbymchenry/codegraph")
        })
}

fn write_rpc(stdin: &mut ChildStdin, payload: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *stdin, payload)
        .map_err(|err| format!("failed to encode CodeGraph request: {err}"))?;
    stdin
        .write_all(b"\n")
        .and_then(|_| stdin.flush())
        .map_err(|err| format!("failed to write to CodeGraph MCP server: {err}"))
}

fn wait_for_response(
    receiver: &mpsc::Receiver<String>,
    id: u64,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<Value, String> {
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Err("codegraph request cancelled".into());
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(format!(
                "CodeGraph MCP session timed out after {} seconds; try `codegraph unlock`",
                SESSION_TIMEOUT.as_secs()
            ));
        }
        let wait = POLL_INTERVAL.min(deadline.saturating_duration_since(now));
        match receiver.recv_timeout(wait) {
            Ok(line) => {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                if let Some(error) = message.get("error") {
                    let text = error
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| error.to_string());
                    return Err(text);
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("CodeGraph MCP process closed before responding".into())
            }
        }
    }
}

fn response_text(result: &Value) -> Result<String, String> {
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(if text.is_empty() {
            "CodeGraph tool failed".into()
        } else {
            text
        });
    }
    let output = if text.is_empty() {
        result.to_string()
    } else {
        text
    };
    Ok(truncate_chars(&output, MAX_OUTPUT_CHARS))
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let byte_end = text
        .char_indices()
        .nth(max_chars)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    let prefix = &text[..byte_end];
    let cut = prefix
        .rfind('\n')
        .filter(|index| *index >= byte_end.saturating_mul(4) / 5)
        .unwrap_or(byte_end);
    format!(
        "{}\n\n... (output truncated to {max_chars} characters)",
        &text[..cut]
    )
}

fn sanitize_diagnostic(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len().min(MAX_DIAGNOSTIC_CHARS));
    for raw in text.lines() {
        let mut line = raw.replace('\u{1b}', "");
        for marker in ["TOKEN=", "SECRET=", "PASSWORD=", "API_KEY=", "APIKEY="] {
            if let Some(start) = line.to_ascii_uppercase().find(marker) {
                let value_start = start + marker.len();
                let value_end = line[value_start..]
                    .find(char::is_whitespace)
                    .map(|offset| value_start + offset)
                    .unwrap_or(line.len());
                line.replace_range(value_start..value_end, "[redacted]");
            }
        }
        if let Some(start) = line.to_ascii_lowercase().find("bearer ") {
            let value_start = start + "bearer ".len();
            let value_end = line[value_start..]
                .find(char::is_whitespace)
                .map(|offset| value_start + offset)
                .unwrap_or(line.len());
            line.replace_range(value_start..value_end, "[redacted]");
        }
        if !cleaned.is_empty() {
            cleaned.push('\n');
        }
        cleaned.push_str(&line);
        if cleaned.chars().count() >= MAX_DIAGNOSTIC_CHARS {
            return truncate_chars(&cleaned, MAX_DIAGNOSTIC_CHARS);
        }
    }
    cleaned
}

fn call_codegraph(tool: &str, params: &Value, cancelled: &AtomicBool) -> Result<String, String> {
    let root = project_root(params)?;
    let mut child = spawn_server(&root)?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "CodeGraph MCP stdin is unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "CodeGraph MCP stdout is unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "CodeGraph MCP stderr is unavailable".to_string())?;

    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let diagnostic = Arc::new(Mutex::new(String::new()));
    let diagnostic_writer = Arc::clone(&diagnostic);
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr).take(16 * 1024);
        let mut text = String::new();
        let _ = reader.read_to_string(&mut text);
        if let Ok(mut destination) = diagnostic_writer.lock() {
            *destination = text;
        }
    });

    let deadline = Instant::now() + SESSION_TIMEOUT;
    let operation = (|| {
        let path = root.to_string_lossy().replace('\\', "/");
        let root_uri = if cfg!(windows) {
            format!("file:///{}", path.trim_start_matches('/'))
        } else {
            format!("file://{path}")
        };
        write_rpc(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "rootUri": root_uri.clone(),
                    "workspaceFolders": [{"uri": root_uri, "name": root.file_name().and_then(|v| v.to_str()).unwrap_or("project")}],
                    "capabilities": {},
                    "clientInfo": {"name": "rpi-codegraph", "version": env!("CARGO_PKG_VERSION")}
                }
            }),
        )?;
        wait_for_response(&receiver, 1, deadline, cancelled)?;
        write_rpc(
            &mut stdin,
            &json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        )?;
        write_rpc(
            &mut stdin,
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":tool,"arguments":params}}),
        )?;
        let result = wait_for_response(&receiver, 2, deadline, cancelled)?;
        response_text(&result)
    })();

    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();

    operation.map_err(|message| {
        let stderr = diagnostic
            .lock()
            .ok()
            .map(|value| sanitize_diagnostic(&value))
            .unwrap_or_default();
        if stderr.is_empty() || message.contains(&stderr) {
            message
        } else {
            format!("{message}: {stderr}")
        }
    })
}

fn execute_tool(tool: &'static str, params: StbString, free: Option<FreeStringFn>) -> StepHandle {
    let text = params.to_string_lossy();
    params.free_with(free);
    Box::into_raw(Box::new(Drive {
        tool,
        params: serde_json::from_str(&text).unwrap_or(Value::Null),
        cancelled: AtomicBool::new(false),
        completed: false,
    })) as StepHandle
}

macro_rules! tool_execute {
    ($function:ident, $tool:literal) => {
        extern "C" fn $function(
            _: StbStringRef,
            params: StbString,
            free: Option<FreeStringFn>,
        ) -> StepHandle {
            execute_tool($tool, params, free)
        }
    };
}

tool_execute!(execute_search, "codegraph_search");
tool_execute!(execute_callers, "codegraph_callers");
tool_execute!(execute_callees, "codegraph_callees");
tool_execute!(execute_impact, "codegraph_impact");
tool_execute!(execute_explore, "codegraph_explore");
tool_execute!(execute_node, "codegraph_node");
tool_execute!(execute_status, "codegraph_status");
tool_execute!(execute_files, "codegraph_files");

extern "C" fn poll(handle: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null codegraph handle".into()));
    }
    let drive = unsafe { &mut *(handle as *mut Drive) };
    if drive.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("codegraph request cancelled".into()));
    }
    if drive.completed {
        return StepResult::err(StbString::from_string(
            "codegraph polled after completion".into(),
        ));
    }
    drive.completed = true;
    match call_codegraph(drive.tool, &drive.params, &drive.cancelled) {
        Ok(text) => StepResult::done(StbString::from_string(
            json!({"content":[{"type":"text","text":text}]}).to_string(),
        )),
        Err(error) => StepResult::err(StbString::from_string(error)),
    }
}

extern "C" fn cancel(handle: StepHandle) {
    if !handle.is_null() {
        unsafe { &*(handle as *mut Drive) }
            .cancelled
            .store(true, Ordering::SeqCst);
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
            let slice = std::slice::from_raw_parts(value.ptr as *const u8, value.len);
            let _ = Box::from_raw(slice as *const [u8] as *mut [u8]);
        }
    }
}

fn parameters(mut properties: serde_json::Map<String, Value>, required: &[&str]) -> String {
    properties.insert(
        "projectPath".into(),
        json!({
            "type": "string",
            "description": "Absolute path to an initialized CodeGraph project. Defaults to the current directory."
        }),
    );
    let mut schema = json!({"type":"object", "properties": properties});
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema.to_string()
}

fn tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "codegraph_search",
            description: "Search indexed code symbols by name. Returns compact locations only.",
            parameters: parameters(json!({
                "query":{"type":"string"},
                "kind":{"type":"string","enum":["function","method","class","interface","type","variable","route","component"]},
                "limit":{"type":"integer","default":10}
            }).as_object().unwrap().clone(), &["query"]),
            execute: execute_search,
        },
        ToolSpec {
            name: "codegraph_callers",
            description: "Find functions or methods that call an indexed symbol.",
            parameters: parameters(json!({
                "symbol":{"type":"string"}, "file":{"type":"string"},
                "limit":{"type":"integer","default":20}
            }).as_object().unwrap().clone(), &["symbol"]),
            execute: execute_callers,
        },
        ToolSpec {
            name: "codegraph_callees",
            description: "Find functions or methods called by an indexed symbol.",
            parameters: parameters(json!({
                "symbol":{"type":"string"}, "file":{"type":"string"},
                "limit":{"type":"integer","default":20}
            }).as_object().unwrap().clone(), &["symbol"]),
            execute: execute_callees,
        },
        ToolSpec {
            name: "codegraph_impact",
            description: "Analyze the dependency impact radius of changing an indexed symbol.",
            parameters: parameters(json!({
                "symbol":{"type":"string"}, "file":{"type":"string"},
                "depth":{"type":"integer","default":2}
            }).as_object().unwrap().clone(), &["symbol"]),
            execute: execute_impact,
        },
        ToolSpec {
            name: "codegraph_explore",
            description: "Explore a code area by question, symbol, file, or flow; returns bounded relevant source grouped by file.",
            parameters: parameters(json!({
                "query":{"type":"string"}, "maxFiles":{"type":"integer","default":8}
            }).as_object().unwrap().clone(), &["query"]),
            execute: execute_explore,
        },
        ToolSpec {
            name: "codegraph_node",
            description: "Get one symbol with source and relationships, or read one indexed file with optional line bounds.",
            parameters: parameters(json!({
                "symbol":{"type":"string"}, "includeCode":{"type":"boolean","default":false},
                "file":{"type":"string"}, "offset":{"type":"integer"}, "limit":{"type":"integer"},
                "symbolsOnly":{"type":"boolean","default":false}, "line":{"type":"integer"}
            }).as_object().unwrap().clone(), &[]),
            execute: execute_node,
        },
        ToolSpec {
            name: "codegraph_status",
            description: "Report CodeGraph index health, counts, and pending synchronization.",
            parameters: parameters(serde_json::Map::new(), &[]),
            execute: execute_status,
        },
        ToolSpec {
            name: "codegraph_files",
            description: "Get the indexed project file tree with optional path, glob, format, metadata, and depth filters.",
            parameters: parameters(json!({
                "path":{"type":"string"}, "pattern":{"type":"string"},
                "format":{"type":"string","enum":["tree","flat","grouped"],"default":"tree"},
                "includeMetadata":{"type":"boolean","default":true}, "maxDepth":{"type":"integer"}
            }).as_object().unwrap().clone(), &[]),
            execute: execute_files,
        },
    ]
}

#[no_mangle]
pub extern "C" fn rpi_plugin_register(api: *const PluginApi) -> i32 {
    unsafe { register_entrypoint_unified(api, |api| {
        let Some(register) = api.register_tool else {
            return 1;
        };
        for spec in tool_specs() {
            let schema = Box::new(rpi_plugin_sdk::StableToolSchema {
                name: StbString::from_string(spec.name.into()),
                description: StbString::from_string(spec.description.into()),
                parameters: StbString::from_string(spec.parameters),
            });
            let rc = register(&*schema, spec.execute, poll, cancel, destroy, free_string);
            drop(schema);
            if rc != 0 {
                return rc;
            }
        }
        0
    }) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_are_valid_json() {
        for spec in tool_specs() {
            serde_json::from_str::<Value>(spec.parameters)
                .unwrap_or_else(|error| panic!("invalid schema for {}: {error}", spec.name));
        }
    }

    #[test]
    fn extracts_text_and_rejects_mcp_errors() {
        let result = json!({"content":[{"type":"text","text":"one"},{"type":"image","data":"x"},{"type":"text","text":"two"}]});
        assert_eq!(response_text(&result).unwrap(), "one\ntwo");
        assert!(
            response_text(&json!({"isError":true,"content":[{"type":"text","text":"bad"}]}))
                .is_err()
        );
    }

    #[test]
    fn truncates_on_unicode_boundaries() {
        let text = "你".repeat(30);
        let output = truncate_chars(&text, 10);
        assert!(output.starts_with(&"你".repeat(10)));
        assert!(output.contains("output truncated"));
    }

    #[test]
    fn redacts_common_secrets() {
        let output = sanitize_diagnostic("failed TOKEN=abc Bearer xyz API_KEY=hidden");
        assert!(!output.contains("abc"));
        assert!(!output.contains("xyz"));
        assert!(!output.contains("hidden"));
    }
}
