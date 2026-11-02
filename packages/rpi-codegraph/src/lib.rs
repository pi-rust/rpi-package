#![forbid(unsafe_code)]

use rpi_plugin_sdk::handles::ToolHandles;
use std::ffi::c_void;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rpi_plugin_sdk::{
    FreeStringFn, StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};

const SESSION_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_OUTPUT_CHARS: usize = 25_000;
const MAX_DIAGNOSTIC_CHARS: usize = 1_000;

static HANDLES: ToolHandles<Drive> = ToolHandles::new();

struct Drive {
    tool: &'static str,
    params: Value,
    completed: bool,
}

struct ToolSpec {
    name: &'static str,
    description: String,
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

/// Resolve the `codegraph` executable path. On Windows, npm/Scoop shims
/// don't resolve reliably via `Command::new("codegraph")`, so we ask
/// PowerShell to find the native application shim.
#[cfg(windows)]
fn resolve_codegraph_cmd() -> Result<Command, String> {
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass",
            "-Command",
            "(Get-Command codegraph -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source",
        ])
        .output()
        .map_err(|e| format!("failed to locate codegraph: {e}"))?;
    if !output.status.success() {
        return Err("codegraph command not found; install @colbymchenry/codegraph".into());
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return Err("codegraph command not found; install @colbymchenry/codegraph".into());
    }
    Ok(Command::new(path))
}

#[cfg(not(windows))]
fn resolve_codegraph_cmd() -> Result<Command, String> {
    Ok(Command::new("codegraph"))
}

/// If the project has no `.codegraph/` directory, run `codegraph init` to
/// create the initial index automatically.
fn ensure_initialized(root: &Path) -> Result<(), String> {
    if root.join(".codegraph").is_dir() {
        return Ok(());
    }
    let mut cmd = resolve_codegraph_cmd()?;
    let output = cmd
        .args(["init"])
        .arg(root)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run codegraph init: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("codegraph init failed: {stderr}"));
    }
    Ok(())
}

/// Run `codegraph sync` to pick up any file changes since the last index.
fn sync_index(root: &Path) -> Result<(), String> {
    let mut cmd = resolve_codegraph_cmd()?;
    let output = cmd
        .args(["sync"])
        .arg(root)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run codegraph sync: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("codegraph sync failed: {stderr}"));
    }
    Ok(())
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
    // Auto-init if no index exists, then sync to pick up recent changes.
    ensure_initialized(&root)?;
    sync_index(&root)?;
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
    HANDLES.insert(Drive {
        tool,
        params: serde_json::from_str(&text).unwrap_or(Value::Null),
        completed: false,
    })
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
    let Some(entry) = HANDLES.get(handle) else {
        return StepResult::err(StbString::from_string(
            "invalid or destroyed tool handle".into(),
        ));
    };
    let mut drive = entry.lock();
    if entry.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("codegraph request cancelled".into()));
    }
    if drive.completed {
        return StepResult::err(StbString::from_string(
            "codegraph polled after completion".into(),
        ));
    }
    drive.completed = true;
    match call_codegraph(drive.tool, &drive.params, &entry.cancelled) {
        Ok(text) => StepResult::done(StbString::from_string(
            json!({"content":[{"type":"text","text":text}]}).to_string(),
        )),
        Err(error) => StepResult::err(StbString::from_string(error)),
    }
}

extern "C" fn cancel(handle: StepHandle) {
    HANDLES.cancel(handle);
}

extern "C" fn destroy(handle: StepHandle) {
    drop(HANDLES.remove(handle));
}

use rpi_plugin_sdk::callbacks::free_string;

fn parameters(mut properties: serde_json::Map<String, Value>, required: &[&str]) -> String {
    properties.insert(
        "projectPath".into(),
        json!({
            "type": "string",
            "description": "Optional absolute project path; omitted or blank defaults to the current working directory."
        }),
    );
    // Extra host metadata (including __rpi) must remain accepted.
    let mut schema =
        json!({"type":"object", "properties": properties, "additionalProperties": true});
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema.to_string()
}

fn description(purpose: &str) -> String {
    format!("{purpose} Requires codegraph CLI. Every call (including status) auto-initializes if needed and syncs, writing the project index. Text truncates at 25,000 characters plus a marker. If unavailable or incomplete, use read/grep/find to verify.")
}

fn tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "codegraph_search",
            description: description("Search indexed symbols by name; returns compact locations, not source. Use node for one definition or explore for an area."),
            parameters: parameters(json!({
                "query":{"type":"string","description":"Required symbol name or partial name to search (for example signIn or AuthService)."},
                "kind":{"type":"string","description":"Optional symbol-kind filter; omitted searches all indexed kinds.","enum":["function","method","class","interface","type","variable","route","component"]},
                "limit":{"type":"integer","description":"Optional maximum results requested; default hint 10. Upstream applies its own limits; the plugin does not clamp or inject defaults.","default":10}
            }).as_object().unwrap().clone(), &["query"]),
            execute: execute_search,
        },
        ToolSpec {
            name: "codegraph_callers",
            description: description("Find indexed functions/methods that call a symbol. Use file to disambiguate names; dynamic dispatch or unresolved calls may be missing."),
            parameters: parameters(json!({
                "symbol":{"type":"string","description":"Required definition name to find callers for."},
                "file":{"type":"string","description":"Optional definition path or suffix to disambiguate same-named symbols; omitted lets upstream resolve matches."},
                "limit":{"type":"integer","description":"Optional requested caller count; default hint 20. Upstream applies its own caps.","default":20}
            }).as_object().unwrap().clone(), &["symbol"]),
            execute: execute_callers,
        },
        ToolSpec {
            name: "codegraph_callees",
            description: description("Find indexed functions/methods called by a symbol. Use file to disambiguate names; this is a static relationship view, not a runtime trace."),
            parameters: parameters(json!({
                "symbol":{"type":"string","description":"Required definition name to find outgoing calls for."},
                "file":{"type":"string","description":"Optional definition path or suffix to disambiguate same-named symbols; omitted lets upstream resolve matches."},
                "limit":{"type":"integer","description":"Optional requested callee count; default hint 20. Upstream applies its own caps.","default":20}
            }).as_object().unwrap().clone(), &["symbol"]),
            execute: execute_callees,
        },
        ToolSpec {
            name: "codegraph_impact",
            description: description("Estimate the dependency impact radius of changing a symbol before refactoring. Depth bounds traversal; results do not prove all affected code is covered."),
            parameters: parameters(json!({
                "symbol":{"type":"string","description":"Required definition name whose dependents should be analyzed."},
                "file":{"type":"string","description":"Optional definition path or suffix to disambiguate same-named symbols; omitted lets upstream resolve matches."},
                "depth":{"type":"integer","description":"Optional requested dependency traversal levels; default hint 2. Upstream applies its own caps; deeper is not proof of completeness.","default":2}
            }).as_object().unwrap().clone(), &["symbol"]),
            execute: execute_impact,
        },
        ToolSpec {
            name: "codegraph_explore",
            description: description("Explore a question, symbol, file or flow; returns relevant source grouped by file and relationships. Prefer specific code terms, and narrow the query if source is trimmed."),
            parameters: parameters(json!({
                "query":{"type":"string","description":"Required question, symbol/file names or short code terms identifying the area or flow; specific terms help relevance."},
                "maxFiles":{"type":"integer","description":"Optional requested source-file budget; registered default hint 8. Omission is forwarded unchanged, so the installed upstream version chooses its actual default and caps.","default":8}
            }).as_object().unwrap().clone(), &["query"]),
            execute: execute_explore,
        },
        ToolSpec {
            name: "codegraph_node",
            description: description("Two modes: supply symbol for definition details/relationships (includeCode for source), or file without symbol for line-numbered file source (symbolsOnly for outline). Supply at least symbol or file. File/line disambiguate symbol definitions; file mode supports offset/limit. Availability depends on the installed upstream version."),
            parameters: parameters(json!({
                "symbol":{"type":"string","description":"Symbol mode: definition name. Required in use unless file is supplied for file mode; omit for whole-file source."},
                "includeCode":{"type":"boolean","description":"Symbol mode: request source (default hint false); containers may return an outline. Ignored in file mode.","default":false},
                "file":{"type":"string","description":"File mode: path or basename to read when symbol is omitted. Symbol mode: optional definition path/suffix to disambiguate."},
                "offset":{"type":"integer","description":"File mode only: optional 1-based starting line; omission starts at the beginning. Upstream controls normalization."},
                "limit":{"type":"integer","description":"File mode only: optional requested maximum lines; installed upstream defaults apply (locally inspected version defaults to 2000 lines with a separate character budget), then plugin text truncation applies."},
                "symbolsOnly":{"type":"boolean","description":"File mode only: return symbol map/dependents instead of source; default hint false.","default":false},
                "line":{"type":"integer","description":"Symbol mode only: optional 1-based definition line to disambiguate with file; not a file read offset."}
            }).as_object().unwrap().clone(), &[]),
            execute: execute_node,
        },
        ToolSpec {
            name: "codegraph_status",
            description: description("Report index health, file/node/edge counts and any pending sync information AFTER automatic init and sync. This is not a read-only pre-sync inspection and may fail during preparation."),
            parameters: parameters(serde_json::Map::new(), &[]),
            execute: execute_status,
        },
        ToolSpec {
            name: "codegraph_files",
            description: description("Show the indexed file tree, optionally filtered by directory/glob with format, metadata and depth controls. Not a complete filesystem listing: unindexed or unsupported files can be absent."),
            parameters: parameters(json!({
                "path":{"type":"string","description":"Optional directory filter within the project, for example src/components; omitted includes all indexed paths."},
                "pattern":{"type":"string","description":"Optional glob filter such as **/*.test.ts; interpreted by upstream, not the shell."},
                "format":{"type":"string","description":"Optional output layout: tree (hierarchical default hint), flat (list), grouped (by language).","enum":["tree","flat","grouped"],"default":"tree"},
                "includeMetadata":{"type":"boolean","description":"Optional language/symbol-count metadata; default hint true.","default":true},
                "maxDepth":{"type":"integer","description":"Optional maximum directory depth; omission leaves depth unlimited upstream, but response text remains bounded."}
            }).as_object().unwrap().clone(), &[]),
            execute: execute_files,
        },
    ]
}
rpi_plugin_sdk::export_plugin!(|api| {
    let Some(register) = api.register_tool else {
        return 1;
    };
    for spec in tool_specs() {
        let schema = Box::new(rpi_plugin_sdk::StableToolSchema {
            name: StbString::from_string(spec.name.into()),
            description: StbString::from_string(spec.description),
            parameters: StbString::from_string(spec.parameters),
        });
        let rc = register(&*schema, spec.execute, poll, cancel, destroy, free_string);
        drop(schema);
        if rc != 0 {
            return rc;
        }
    }
    0
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_schemas_preserve_shape_optional_fields_and_guidance() {
        let specs = tool_specs();
        assert_eq!(specs.len(), 8);
        let mut names = std::collections::HashSet::new();
        for spec in specs {
            assert!(names.insert(spec.name));
            let schema: Value = serde_json::from_str(&spec.parameters)
                .unwrap_or_else(|error| panic!("invalid schema for {}: {error}", spec.name));
            assert_eq!(schema["type"], "object");
            assert_eq!(schema["additionalProperties"], true, "accept host __rpi");
            let properties = schema["properties"].as_object().unwrap();
            for (name, property) in properties {
                assert!(
                    property["description"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty()),
                    "{}.{name}",
                    spec.name
                );
                assert!(property["type"].is_string());
            }
            let required = schema
                .get("required")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let expected = match spec.name {
                "codegraph_search" | "codegraph_explore" => vec![json!("query")],
                "codegraph_callers" | "codegraph_callees" | "codegraph_impact" => {
                    vec![json!("symbol")]
                }
                _ => vec![],
            };
            assert_eq!(required, expected, "optional fields stay optional");
            for field in &required {
                assert!(properties.contains_key(field.as_str().unwrap()));
            }
            let project_guidance = properties["projectPath"]["description"].as_str().unwrap();
            assert!(project_guidance.contains("absolute"));
            assert!(project_guidance.contains("current working directory"));
            assert!(project_guidance.len() <= 120);
            for text in [
                "codegraph CLI",
                "Every call",
                "including status",
                "auto-initializes",
                "syncs",
                "writing",
                "25,000",
                "unavailable",
                "read/grep/find",
            ] {
                assert!(
                    spec.description.contains(text),
                    "{} missing {text}",
                    spec.name
                );
            }
            match spec.name {
                "codegraph_search" => {
                    assert_eq!(properties["limit"]["default"], 10);
                    assert_eq!(properties["kind"]["enum"].as_array().unwrap().len(), 8);
                }
                "codegraph_callers" | "codegraph_callees" => {
                    assert_eq!(properties["limit"]["default"], 20)
                }
                "codegraph_impact" => assert_eq!(properties["depth"]["default"], 2),
                "codegraph_explore" => assert_eq!(properties["maxFiles"]["default"], 8),
                "codegraph_node" => {
                    assert!(spec.description.contains("at least symbol or file"));
                    assert_eq!(properties["includeCode"]["default"], false);
                    assert_eq!(properties["symbolsOnly"]["default"], false);
                    assert!(properties["offset"]["description"]
                        .as_str()
                        .unwrap()
                        .contains("1-based"));
                }
                "codegraph_status" => {
                    assert_eq!(properties.len(), 1);
                    assert!(spec.description.contains("not a read-only pre-sync"));
                }
                "codegraph_files" => {
                    assert_eq!(
                        properties["format"]["enum"],
                        json!(["tree", "flat", "grouped"])
                    );
                    assert_eq!(properties["format"]["default"], "tree");
                    assert_eq!(properties["includeMetadata"]["default"], true);
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn shared_guidance_stays_concise() {
        let guidance = description("");
        assert!((200..=300).contains(&guidance.trim().len()));
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
