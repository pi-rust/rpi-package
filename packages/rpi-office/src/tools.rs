//! `office_*` tool registration and the execute → poll → cancel → destroy drive.
//!
//! Every catalog op becomes one tool whose execution is proxied to the pane that
//! owns the requested document. The proxy round-trip is asynchronous, so
//! `execute` starts it and `poll` reports the result without blocking the host.
//! The discovery tool (`office_list_documents`) answers from the pane registry.
//!
//! The ABI's `execute` callback does not carry the tool name, so each tool gets
//! its own trampoline generated from the catalog list below. A test keeps that
//! list and the committed catalog in sync.
#![forbid(unsafe_code)]

use crate::active_tools::OFFICE_LIST_TOOL;
use crate::bridge::Pane;
use crate::catalog::{self, CatalogEntry, Host};
use crate::protocol::now_ms;
use crate::server::{Server, ToolOutput};
use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{
    PluginApi, StableToolSchema, StbString, StbStringRef, StepHandle, StepResult, ToolCancelFn,
    ToolDestroyFn, ToolExecuteFn, ToolPartialCb, ToolPollFn,
};
use serde_json::{json, Value};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

/// How long one proxied Office.js call may take before the bridge gives up.
pub const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Marker the host injects into plugin tool arguments (never model-visible).
pub const HOST_CONTEXT_KEY: &str = "__rpi";

/// The error used when the bridge is not listening.
pub const OFFLINE_ERROR: &str =
    "office-bridge: server is not running. Run /office connect (or Connect in the Documents \
panel), then try again.";

/// Every tool this extension registers, in catalog order plus the discovery tool.
///
/// One `extern "C"` trampoline per entry (see the module docs). The names must
/// match `catalog/office-catalog.json` exactly; `registered_names_match_catalog`
/// enforces that.
macro_rules! office_tools {
    ($($fn_name:ident => $tool:literal),* $(,)?) => {
        $(
            // `execute_handler!` owns the raw-pointer half of the ABI: it turns
            // the call id into `&str` and adopts `params` as an `OwnedString`
            // that reclaims itself through the host's deallocator, so this crate
            // needs no `unsafe` for any of the 33 trampolines.
            rpi_plugin_sdk::execute_handler! {
                fn $fn_name(id, params) {
                    execute_named($tool, id, params)
                }
            }
        )*

        /// `(tool name, trampoline)` pairs in registration order.
        const EXECUTORS: &[(&str, ToolExecuteFn)] = &[
            $(($tool, $fn_name as ToolExecuteFn)),*
        ];
    };
}

office_tools! {
    tool_office_excel_get_overview => "office_excel_get_overview",
    tool_office_excel_read_range => "office_excel_read_range",
    tool_office_excel_write_cells => "office_excel_write_cells",
    tool_office_excel_fill_formula => "office_excel_fill_formula",
    tool_office_excel_search_workbook => "office_excel_search_workbook",
    tool_office_excel_modify_structure => "office_excel_modify_structure",
    tool_office_excel_format_cells => "office_excel_format_cells",
    tool_office_excel_conditional_format => "office_excel_conditional_format",
    tool_office_excel_charts => "office_excel_charts",
    tool_office_excel_trace_dependencies => "office_excel_trace_dependencies",
    tool_office_excel_explain_formula => "office_excel_explain_formula",
    tool_office_excel_view_settings => "office_excel_view_settings",
    tool_office_excel_comments => "office_excel_comments",
    tool_office_excel_workbook_history => "office_excel_workbook_history",
    tool_office_excel_get_capabilities => "office_excel_get_capabilities",
    tool_office_word_get_overview => "office_word_get_overview",
    tool_office_word_get_capabilities => "office_word_get_capabilities",
    tool_office_word_read_document => "office_word_read_document",
    tool_office_word_insert_text => "office_word_insert_text",
    tool_office_word_replace_text => "office_word_replace_text",
    tool_office_word_format_range => "office_word_format_range",
    tool_office_word_insert_blocks => "office_word_insert_blocks",
    tool_office_word_insert_table => "office_word_insert_table",
    tool_office_word_insert_page_break => "office_word_insert_page_break",
    tool_office_word_insert_image => "office_word_insert_image",
    tool_office_word_insert_hyperlink => "office_word_insert_hyperlink",
    tool_office_powerpoint_get_overview => "office_powerpoint_get_overview",
    tool_office_powerpoint_read_slide => "office_powerpoint_read_slide",
    tool_office_powerpoint_add_slide => "office_powerpoint_add_slide",
    tool_office_powerpoint_add_text_box => "office_powerpoint_add_text_box",
    tool_office_powerpoint_format_slide => "office_powerpoint_format_slide",
    tool_office_powerpoint_get_capabilities => "office_powerpoint_get_capabilities",
    tool_office_list_documents => "office_list_documents",
}

/// One in-flight (or immediately finished) tool call.
struct CallDrive {
    id: String,
    /// `None` when the call never started; `immediate` then holds the reason.
    receiver: Option<Receiver<Result<ToolOutput, String>>>,
    immediate: Option<String>,
    deadline: u64,
    cancel_sent: bool,
}

static HANDLES: ToolHandles<CallDrive> = ToolHandles::new();

/// Server accessor installed by the plugin root (keeps this module host-free).
pub type ServerLookup = fn() -> Option<std::sync::Arc<Server>>;

/// Provider of the attached panes, for the discovery tool.
pub type PaneLookup = fn() -> Vec<Pane>;

static SERVER_LOOKUP: std::sync::OnceLock<ServerLookup> = std::sync::OnceLock::new();
static PANE_LOOKUP: std::sync::OnceLock<PaneLookup> = std::sync::OnceLock::new();

/// Wire the tool drive to the plugin's bridge state. Idempotent per process.
pub fn install(server: ServerLookup, panes: PaneLookup) {
    let _ = SERVER_LOOKUP.set(server);
    let _ = PANE_LOOKUP.set(panes);
}

fn server() -> Option<std::sync::Arc<Server>> {
    SERVER_LOOKUP.get().and_then(|lookup| lookup())
}

fn panes() -> Vec<Pane> {
    PANE_LOOKUP.get().map(|lookup| lookup()).unwrap_or_default()
}

/// Strip the host-injected `__rpi` metadata from tool arguments.
pub fn strip_host_context(params: &mut Value) {
    if let Some(object) = params.as_object_mut() {
        object.remove(HOST_CONTEXT_KEY);
    }
}

/// Build the `AgentToolResult` JSON the host expects from `poll`.
pub fn tool_result_json(text: &str, details: Option<&Value>) -> String {
    let mut result = json!({"content": [{"type": "text", "text": text}]});
    if let Some(details) = details {
        result["details"] = details.clone();
    }
    result.to_string()
}

/// Build one tool's schema from a catalog entry (with the `document` selector).
pub fn catalog_schema(entry: &CatalogEntry) -> StableToolSchema {
    StableToolSchema {
        name: StbString::from_string(entry.name.clone()),
        description: StbString::from_string(entry.description.clone()),
        parameters: StbString::from_string(
            catalog::parameters_with_target(&entry.parameters).to_string(),
        ),
    }
}

/// The host-agnostic discovery tool schema.
pub fn list_schema() -> StableToolSchema {
    StableToolSchema {
        name: StbString::from_string(OFFICE_LIST_TOOL.to_string()),
        description: StbString::from_string(
            "List every Office document attached to the bridge (Excel, Word, PowerPoint) with its \
document id, name, host app, and whether it is focused. Use this to choose the `document` \
target for the office_* tools."
                .to_string(),
        ),
        parameters: StbString::from_string(
            json!({
                "type": "object",
                "properties": {
                    "host": {
                        "type": "string",
                        "enum": ["excel", "word", "powerpoint"],
                        "description": "Only list documents of this host app.",
                    }
                }
            })
            .to_string(),
        ),
    }
}

/// Render the discovery-tool table for `office_list_documents`.
pub fn list_documents_text(host: Option<Host>) -> String {
    let all = panes();
    let selected: Vec<&Pane> = all
        .iter()
        .filter(|pane| host.map(|host| pane.host == host).unwrap_or(true))
        .collect();
    if selected.is_empty() {
        return match host {
            None => "No Office documents are attached to the bridge.".to_string(),
            Some(host) => format!("No {} documents are attached to the bridge.", host.as_str()),
        };
    }
    let rows: Vec<String> = selected
        .iter()
        .map(|pane| {
            let windows = all
                .iter()
                .filter(|other| other.host == pane.host && other.target_id == pane.target_id)
                .count();
            format!(
                "| {} | `{}` | {} | {} | {} |",
                pane.document_label(),
                pane.document_ref(),
                pane.host.label(),
                if pane.focused { "yes" } else { "no" },
                windows
            )
        })
        .collect();
    [
        "| document | id | host | focused | windows |".to_string(),
        "|---|---|---|---|---|".to_string(),
    ]
    .into_iter()
    .chain(rows)
    .collect::<Vec<_>>()
    .join("\n")
}

/* ── ABI drive ─────────────────────────────────────────────────────── */

fn execute_named(
    tool: &str,
    _tool_call_id: &str,
    params: rpi_plugin_sdk::callbacks::OwnedString,
) -> StepHandle {
    let mut args: Value = serde_json::from_str(&params.to_string_lossy()).unwrap_or(Value::Null);
    strip_host_context(&mut args);
    if !args.is_object() {
        args = json!({});
    }
    let drive = if tool == OFFICE_LIST_TOOL {
        let host = args
            .get("host")
            .and_then(Value::as_str)
            .and_then(Host::parse);
        CallDrive::ready(list_documents_text(host))
    } else {
        match catalog::catalog().entry_by_name(tool) {
            Some(entry) => start_call(entry.host, &entry.op, args),
            None => CallDrive::failing("office-bridge: unknown office tool"),
        }
    };
    HANDLES.insert(drive)
}

fn start_call(host: Host, op: &str, args: Value) -> CallDrive {
    let id = crate::protocol::next_call_id("tool");
    let deadline = now_ms() + TOOL_CALL_TIMEOUT.as_millis() as u64;
    let Some(server) = server() else {
        return CallDrive::failing(OFFLINE_ERROR);
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let immediate = server
        .call(id.clone(), host, op.to_string(), args, tx)
        .err();
    CallDrive {
        id,
        receiver: Some(rx),
        immediate,
        deadline,
        cancel_sent: false,
    }
}

impl CallDrive {
    /// A drive that already has its answer (or its error).
    fn ready(text: String) -> Self {
        Self::with_result(Ok(ToolOutput {
            text,
            details: None,
        }))
    }

    fn failing(reason: &str) -> Self {
        Self {
            id: crate::protocol::next_call_id("tool"),
            receiver: None,
            immediate: Some(reason.to_string()),
            deadline: now_ms(),
            cancel_sent: false,
        }
    }

    fn with_result(result: Result<ToolOutput, String>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(result);
        Self {
            id: crate::protocol::next_call_id("tool"),
            receiver: Some(rx),
            immediate: None,
            deadline: now_ms() + TOOL_CALL_TIMEOUT.as_millis() as u64,
            cancel_sent: false,
        }
    }
}

extern "C" fn poll(
    handle: StepHandle,
    _partial: Option<ToolPartialCb>,
    _user_data: *mut std::ffi::c_void,
) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string(
            "office-bridge: null tool handle".into(),
        ));
    }
    let Some(entry) = HANDLES.get(handle) else {
        return StepResult::err(StbString::from_string(
            "office-bridge: invalid or destroyed tool handle".into(),
        ));
    };
    let cancelled = entry.cancelled.load(std::sync::atomic::Ordering::SeqCst);
    let mut drive = entry.lock();
    if cancelled {
        if !drive.cancel_sent {
            drive.cancel_sent = true;
            if let Some(server) = server() {
                let _ = server.cancel_call(&drive.id);
            }
        }
        return StepResult::err(StbString::from_string(
            "office-bridge: tool call aborted by the agent".into(),
        ));
    }
    if let Some(reason) = drive.immediate.take() {
        return StepResult::err(StbString::from_string(reason));
    }
    let Some(receiver) = drive.receiver.as_ref() else {
        return StepResult::err(StbString::from_string(
            "office-bridge: tool call has no result channel".into(),
        ));
    };
    match receiver.try_recv() {
        Ok(Ok(output)) => StepResult::done(StbString::from_string(tool_result_json(
            &output.text,
            output.details.as_ref(),
        ))),
        Ok(Err(error)) => StepResult::err(StbString::from_string(error)),
        Err(TryRecvError::Disconnected) => StepResult::err(StbString::from_string(
            "office-bridge: pane disconnected before answering the tool call".into(),
        )),
        Err(TryRecvError::Empty) => {
            if now_ms() > drive.deadline {
                if let Some(server) = server() {
                    let _ = server.cancel_call(&drive.id);
                }
                return StepResult::err(StbString::from_string(format!(
                    "office-bridge: tool call timed out after {}s",
                    TOOL_CALL_TIMEOUT.as_secs()
                )));
            }
            StepResult::pending(StbString::empty())
        }
    }
}

extern "C" fn cancel(handle: StepHandle) {
    HANDLES.cancel(handle);
}

extern "C" fn destroy(handle: StepHandle) {
    drop(HANDLES.remove(handle));
}

/// Register every catalog tool plus the discovery tool.
///
/// Returns 0 on success, otherwise the host's nonzero registration status.
pub fn register(api: &PluginApi) -> i32 {
    let Some(register_tool) = api.register_tool else {
        return 1;
    };
    // The host copies the schema strings and reclaims them through the
    // `plugin_free_string` below. Freeing them here as well is a double free
    // (observed as a libmalloc abort on macOS), so the `StableToolSchema` is
    // only dropped: it is `Copy` and owns nothing.
    for (name, execute) in EXECUTORS {
        let schema = match catalog::catalog().entry_by_name(name) {
            Some(entry) if *name != OFFICE_LIST_TOOL => catalog_schema(entry),
            _ => list_schema(),
        };
        let rc = register_tool(
            &schema,
            *execute,
            poll as ToolPollFn,
            cancel as ToolCancelFn,
            destroy as ToolDestroyFn,
            rpi_plugin_sdk::callbacks::free_string,
        );
        if rc != 0 {
            return rc;
        }
        register_tool_prompt(api, name);
    }
    0
}

/// Attach `promptSnippet` / `promptGuidelines` to an already-registered tool.
///
/// The metadata comes from the committed catalog, so the model-facing prompt
/// stays in lockstep with the add-in's op surface. Hosts without the slot (or
/// with an older `struct_size`) leave it `None` and simply get the schema.
fn register_tool_prompt(api: &PluginApi, name: &str) {
    let Some(register_prompt) = api.register_tool_prompt else {
        return;
    };
    let metadata = match catalog::catalog().entry_by_name(name) {
        Some(entry) => {
            let mut metadata = serde_json::Map::new();
            if let Some(snippet) = entry.prompt_snippet.as_deref() {
                if !snippet.trim().is_empty() {
                    metadata.insert("promptSnippet".into(), json!(snippet));
                }
            }
            let guidelines: Vec<String> = entry
                .prompt_guidelines
                .iter()
                .filter(|guideline| !guideline.trim().is_empty())
                .cloned()
                .collect();
            if !guidelines.is_empty() {
                metadata.insert("promptGuidelines".into(), json!(guidelines));
            }
            Value::Object(metadata)
        }
        None if name == OFFICE_LIST_TOOL => json!({
            "promptSnippet": "List the Office documents attached to the bridge"
        }),
        None => return,
    };
    if metadata
        .as_object()
        .map(serde_json::Map::is_empty)
        .unwrap_or(true)
    {
        return;
    }
    let encoded = metadata.to_string();
    let rc = register_prompt(
        StbStringRef::from_str(name),
        StbStringRef::from_str(&encoded),
    );
    if rc != 0 {
        eprintln!("[rpi-office] tool prompt metadata for {name} returned {rc}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_names_match_catalog() {
        let catalog = catalog::catalog();
        let mut expected: Vec<&str> = catalog
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        expected.push(OFFICE_LIST_TOOL);
        let actual: Vec<&str> = EXECUTORS.iter().map(|(name, _)| *name).collect();
        assert_eq!(actual, expected, "tool trampolines must follow the catalog");
        assert_eq!(EXECUTORS.len(), 33);
    }

    #[test]
    fn strips_the_host_context_key_only() {
        let mut params = json!({"range": "A1", "__rpi": {"cwd": "/tmp"}});
        strip_host_context(&mut params);
        assert!(params.get("__rpi").is_none());
        assert_eq!(params["range"], json!("A1"));
    }

    #[test]
    fn catalog_schema_adds_the_document_target_and_keeps_required_intact() {
        let entry = catalog::catalog()
            .entry_by_op("excel.read_range")
            .expect("entry");
        let schema = catalog_schema(entry);
        let parameters: Value =
            serde_json::from_str(&rpi_plugin_sdk::stb_text!(schema.parameters)).unwrap();
        assert_eq!(rpi_plugin_sdk::stb_text!(schema.name), entry.name);
        assert!(parameters["properties"]["document"].is_object());
        assert!(parameters["properties"]["range"].is_object());
        let required = parameters["required"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(!required.iter().any(|value| value == "document"));
    }

    #[test]
    fn list_schema_is_a_valid_object() {
        let schema = list_schema();
        let parameters: Value =
            serde_json::from_str(&rpi_plugin_sdk::stb_text!(schema.parameters)).unwrap();
        assert_eq!(rpi_plugin_sdk::stb_text!(schema.name), OFFICE_LIST_TOOL);
        assert_eq!(
            parameters["properties"]["host"]["enum"],
            json!(["excel", "word", "powerpoint"])
        );
    }

    #[test]
    fn tool_result_json_matches_the_host_contract() {
        let value: Value =
            serde_json::from_str(&tool_result_json("hello", Some(&json!({"a": 1})))).unwrap();
        assert_eq!(value["content"][0]["type"], json!("text"));
        assert_eq!(value["content"][0]["text"], json!("hello"));
        assert_eq!(value["details"]["a"], json!(1));
        let value: Value = serde_json::from_str(&tool_result_json("hello", None)).unwrap();
        assert!(value.get("details").is_none());
    }

    #[test]
    fn list_documents_text_reports_absence_and_host_filter() {
        // No panes are installed in unit tests.
        assert_eq!(
            list_documents_text(None),
            "No Office documents are attached to the bridge."
        );
        assert_eq!(
            list_documents_text(Some(Host::Excel)),
            "No excel documents are attached to the bridge."
        );
    }

    #[test]
    fn offline_execute_reports_the_connect_hint() {
        let drive = start_call(Host::Excel, "read_range", json!({"range": "A1"}));
        assert!(drive.receiver.is_none());
        let reason = drive.immediate.expect("offline reason");
        assert!(reason.contains("/office connect"));
    }

    #[test]
    fn ready_and_failing_drives_answer_on_the_first_poll() {
        let ready = CallDrive::ready("table".into());
        let handle = HANDLES.insert(ready);
        let result = poll(handle, None, std::ptr::null_mut());
        assert_eq!(result.tag, rpi_plugin_sdk::StepResultTag::Done);
        destroy(handle);

        let failing = CallDrive::failing("nope");
        let handle = HANDLES.insert(failing);
        let result = poll(handle, None, std::ptr::null_mut());
        assert_eq!(result.tag, rpi_plugin_sdk::StepResultTag::Err);
        destroy(handle);
    }
}
