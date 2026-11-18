//! Bridge wire protocol v3 — shared wire format between the pi-for-office task
//! pane (client) and this extension (server).
//!
//! Ported from `@dieulc/pi-office-protocol` (`protocol.ts`). The frame shapes,
//! validation rules, capability list and error wording match upstream so an
//! unchanged add-in build can attach to this server.
#![forbid(unsafe_code)]

use crate::catalog::Host;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default TCP port the bridge server listens on (loopback only).
pub const BRIDGE_DEFAULT_PORT: u16 = 38617;

/// Protocol version (v2 = document identity, v3 = streaming deltas + abort).
pub const BRIDGE_PROTOCOL_VERSION: u32 = 3;

/// Oldest client protocol version still accepted.
pub const MIN_SUPPORTED_PROTOCOL_VERSION: u32 = 2;

/// Tool-argument key that selects the target document. The server adds it to
/// every tool schema and strips it before forwarding a `tool_call`.
pub const OFFICE_TARGET_PARAM: &str = "document";

/// Server capabilities advertised in `welcome` and `GET /health`.
///
/// `client-abort` is deliberately absent: the rpi plugin ABI has no runtime
/// action that cancels the in-flight run, so a pane's Stop button degrades to
/// the documented v2 behaviour (the reply keeps arriving) instead of being
/// advertised and silently ignored.
pub const CAPABILITIES: [&str; 3] = ["http-health", "agent-deltas", "manual-lifecycle"];

/// Longest tool-result text forwarded to the model.
pub const MAX_TEXT_CHARS: usize = 50_000;
/// Largest serialized `details` payload accepted from a pane.
pub const MAX_DETAILS_BYTES: usize = 1_000_000;

/// Where a pane's document identity came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentSource {
    DocumentUrl,
    WpsActiveWorkbookFullName,
    Unknown,
}

impl DocumentSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            DocumentSource::DocumentUrl => "document.url",
            DocumentSource::WpsActiveWorkbookFullName => "wps.activeWorkbook.fullName",
            DocumentSource::Unknown => "unknown",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "document.url" => Some(DocumentSource::DocumentUrl),
            "wps.activeWorkbook.fullName" => Some(DocumentSource::WpsActiveWorkbookFullName),
            "unknown" => Some(DocumentSource::Unknown),
            _ => None,
        }
    }
}

/// Privacy-preserving document identity sent in `hello`.
#[derive(Clone, Debug)]
pub struct DocumentIdentity {
    pub id: Option<String>,
    pub name: Option<String>,
    pub source: DocumentSource,
}

/// Client → server: attach a task pane.
#[derive(Clone, Debug)]
pub struct Hello {
    pub protocol_version: u32,
    pub host: Host,
    pub client_name: String,
    pub pane_id: String,
    pub document: DocumentIdentity,
    /// Op ids this pane can execute; `None` for a legacy (0.2.x) client.
    pub ops: Option<Vec<String>>,
    pub catalog_version: Option<u32>,
}

/// A validated client frame.
#[derive(Clone, Debug)]
pub enum ClientMessage {
    Hello(Hello),
    Ping {
        ts: f64,
    },
    ToolResult {
        id: String,
        ok: bool,
        text: String,
        details: Option<Value>,
        error: Option<String>,
    },
    UserMessage {
        text: String,
    },
    Status {
        model: Option<String>,
        provider: Option<String>,
    },
    PaneState {
        focused: Option<bool>,
        visible: Option<bool>,
        /// Pane-reported focus timestamp; kept for parity with upstream.
        #[allow(dead_code)]
        active_at: Option<f64>,
    },
    Abort {
        prompt_id: Option<String>,
    },
}

/// True when `version` is a client protocol version this server accepts.
pub fn is_protocol_version_supported(version: i64) -> bool {
    version >= i64::from(MIN_SUPPORTED_PROTOCOL_VERSION)
        && version <= i64::from(BRIDGE_PROTOCOL_VERSION)
}

/// Validate one raw client frame. Error text matches upstream so the add-in's
/// troubleshooting hints stay accurate.
pub fn parse_client_message(raw: &str) -> Result<ClientMessage, String> {
    let data: Value = serde_json::from_str(raw)
        .map_err(|error| format!("bridge: message is not valid JSON ({error})"))?;
    if !data.is_object() {
        return Err("bridge: expected a JSON object message".into());
    }
    let message_type = data["type"]
        .as_str()
        .ok_or("bridge: message missing 'type'")?;
    match message_type {
        "hello" => parse_hello(&data),
        "pane_state" => {
            let focused = optional_bool(
                &data,
                "focused",
                "bridge: pane_state.focused must be a boolean",
            )?;
            let visible = optional_bool(
                &data,
                "visible",
                "bridge: pane_state.visible must be a boolean",
            )?;
            let active_at = match data.get("activeAt") {
                None | Some(Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_f64()
                        .filter(|number| number.is_finite())
                        .ok_or("bridge: pane_state.activeAt must be a finite number")?,
                ),
            };
            Ok(ClientMessage::PaneState {
                focused,
                visible,
                active_at,
            })
        }
        "abort" => {
            let prompt_id = match data.get("promptId") {
                None | Some(Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_str()
                        .ok_or("bridge: abort.promptId must be a string when present")?
                        .to_string(),
                ),
            };
            Ok(ClientMessage::Abort { prompt_id })
        }
        "ping" => Ok(ClientMessage::Ping {
            ts: data["ts"].as_f64().unwrap_or(0.0),
        }),
        "tool_result" => {
            let id = data["id"]
                .as_str()
                .ok_or("bridge: tool_result missing 'id'")?
                .to_string();
            Ok(ClientMessage::ToolResult {
                id,
                ok: data["ok"].as_bool().unwrap_or(false),
                text: data["text"].as_str().unwrap_or_default().to_string(),
                details: match data.get("details") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(value.clone()),
                },
                error: data["error"].as_str().map(str::to_owned),
            })
        }
        "user_message" => Ok(ClientMessage::UserMessage {
            text: data["text"].as_str().unwrap_or_default().to_string(),
        }),
        "status" => Ok(ClientMessage::Status {
            model: data["model"].as_str().map(str::to_owned),
            provider: data["provider"].as_str().map(str::to_owned),
        }),
        other => Err(format!("bridge: unknown client message type '{other}'")),
    }
}

fn optional_bool(data: &Value, key: &str, error: &str) -> Result<Option<bool>, String> {
    match data.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_bool().map(Some).ok_or_else(|| error.to_string()),
    }
}

fn parse_hello(data: &Value) -> Result<ClientMessage, String> {
    let host_raw = data["host"].as_str().unwrap_or_default();
    let host = Host::parse(host_raw)
        .ok_or_else(|| format!("bridge: hello with unknown host '{host_raw}'"))?;

    let document = data
        .get("document")
        .filter(|value| value.is_object())
        .ok_or("bridge: hello missing 'document' identity (protocol v2 requires it)")?;
    let id = match document.get("id") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .ok_or("bridge: hello.document.id must be a string or null")?
                .to_string(),
        ),
    };
    let name = match document.get("name") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .ok_or("bridge: hello.document.name must be a string or null")?
                .to_string(),
        ),
    };
    let source_raw = document["source"].as_str().unwrap_or_default();
    let source = DocumentSource::parse(source_raw)
        .ok_or_else(|| format!("bridge: hello.document.source is unknown '{source_raw}'"))?;

    let ops = match data.get("ops") {
        None | Some(Value::Null) => None,
        Some(Value::Array(values)) => {
            let mut ops = Vec::with_capacity(values.len());
            for value in values {
                ops.push(
                    value
                        .as_str()
                        .ok_or("bridge: hello with malformed 'ops' (expected string[])")?
                        .to_string(),
                );
            }
            Some(ops)
        }
        Some(_) => return Err("bridge: hello with malformed 'ops' (expected string[])".into()),
    };

    let catalog_version = match data.get("catalogVersion") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let number = value.as_f64().filter(|number| number.is_finite());
            match number {
                Some(number) if number.fract() == 0.0 && number >= 1.0 => Some(number as u32),
                Some(number) => {
                    return Err(format!(
                        "bridge: hello with malformed 'catalogVersion' ({})",
                        json!(number)
                    ))
                }
                None => {
                    return Err(format!(
                        "bridge: hello with malformed 'catalogVersion' ({value})"
                    ))
                }
            }
        }
    };

    let protocol_version = data["protocolVersion"].as_i64().unwrap_or(0);
    if !is_protocol_version_supported(protocol_version) {
        return Err(format!(
            "bridge: client protocol v{protocol_version} is not supported (server v{BRIDGE_PROTOCOL_VERSION}, accepting v{MIN_SUPPORTED_PROTOCOL_VERSION}–v{BRIDGE_PROTOCOL_VERSION})"
        ));
    }

    Ok(ClientMessage::Hello(Hello {
        protocol_version: protocol_version as u32,
        host,
        client_name: data["clientName"].as_str().unwrap_or_default().to_string(),
        pane_id: data["paneId"].as_str().unwrap_or_default().to_string(),
        document: DocumentIdentity { id, name, source },
        ops,
        catalog_version,
    }))
}

/* ── server → client frames ─────────────────────────────────────────── */

pub fn welcome(server_name: &str, server_version: &str, pi_version: Option<&str>) -> Value {
    json!({
        "type": "welcome",
        "protocolVersion": BRIDGE_PROTOCOL_VERSION,
        "piVersion": pi_version,
        "serverName": server_name,
        "serverVersion": server_version,
        "capabilities": CAPABILITIES,
    })
}

pub fn pong(ts: f64) -> Value {
    json!({"type": "pong", "ts": ts})
}

pub fn tool_call(id: &str, op_id: &str, args: &Value, document_id: Option<&str>) -> Value {
    let mut frame = json!({
        "type": "tool_call",
        "id": id,
        "tool": op_id,
        "args": args,
    });
    if let Some(document_id) = document_id {
        frame["documentId"] = json!(document_id);
    }
    frame
}

pub fn agent_delta(text: &str, prompt_id: Option<&str>, message_id: Option<&str>) -> Value {
    let mut frame = json!({"type": "agent_message", "kind": "delta", "text": text});
    if let Some(prompt_id) = prompt_id {
        frame["promptId"] = json!(prompt_id);
    }
    if let Some(message_id) = message_id {
        frame["messageId"] = json!(message_id);
    }
    frame
}

pub fn agent_final(text: &str, prompt_id: Option<&str>, message_id: Option<&str>) -> Value {
    let mut frame = json!({"type": "agent_message", "kind": "final", "text": text});
    if let Some(prompt_id) = prompt_id {
        frame["promptId"] = json!(prompt_id);
    }
    if let Some(message_id) = message_id {
        frame["messageId"] = json!(message_id);
    }
    frame
}

pub fn tool_activity(tool: &str, status: &str, summary: Option<&str>) -> Value {
    let mut frame = json!({"type": "tool_activity", "tool": tool, "status": status});
    if let Some(summary) = summary {
        frame["summary"] = json!(summary);
    }
    frame
}

pub fn error_frame(code: &str, message: &str) -> Value {
    json!({"type": "error", "code": code, "message": message})
}

/// Truncate a tool-result text the way upstream does.
pub fn truncate_text(text: &str) -> String {
    let chars = text.chars().count();
    if chars <= MAX_TEXT_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_TEXT_CHARS).collect();
    format!("{head}\n…[truncated: {} chars]", chars - MAX_TEXT_CHARS)
}

/// Bound a pane-supplied `details` payload by serialized size.
pub fn bound_details(details: Option<Value>) -> Option<Value> {
    let details = details?;
    let bytes = serde_json::to_vec(&details)
        .map(|raw| raw.len())
        .unwrap_or(0);
    if bytes > MAX_DETAILS_BYTES {
        return Some(json!({
            "truncated": true,
            "note": format!("details exceeded {MAX_DETAILS_BYTES} bytes"),
        }));
    }
    Some(details)
}

/// Monotonic id generator for prompt/tool-call correlation.
pub fn next_call_id(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    format!("{prefix}-{}-{count}", now_ms())
}

/// Milliseconds since the Unix epoch (0 when the clock is unreadable).
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|delta| delta.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello_frame() -> Value {
        json!({
            "type": "hello",
            "protocolVersion": 3,
            "host": "excel",
            "clientName": "pi-for-office",
            "paneId": "pane-1",
            "document": {"id": "url_sha256:abc", "name": "Book1.xlsx", "source": "document.url"},
            "ops": ["excel.read_range", "excel.get_overview"],
            "catalogVersion": 3
        })
    }

    #[test]
    fn validates_hello_identity_and_ops() {
        let ClientMessage::Hello(hello) = parse_client_message(&hello_frame().to_string()).unwrap()
        else {
            panic!("expected hello");
        };
        assert_eq!(hello.host, Host::Excel);
        assert_eq!(hello.protocol_version, 3);
        assert_eq!(hello.document.id.as_deref(), Some("url_sha256:abc"));
        assert_eq!(hello.ops.as_deref().unwrap().len(), 2);
        assert_eq!(hello.catalog_version, Some(3));

        let mut missing_document = hello_frame();
        missing_document.as_object_mut().unwrap().remove("document");
        assert!(parse_client_message(&missing_document.to_string())
            .unwrap_err()
            .contains("missing 'document' identity"));

        let mut bad_source = hello_frame();
        bad_source["document"]["source"] = json!("clipboard");
        assert!(parse_client_message(&bad_source.to_string())
            .unwrap_err()
            .contains("document.source is unknown"));

        let mut bad_ops = hello_frame();
        bad_ops["ops"] = json!([1, 2]);
        assert!(parse_client_message(&bad_ops.to_string())
            .unwrap_err()
            .contains("malformed 'ops'"));
    }

    #[test]
    fn rejects_protocol_v1_and_unknown_hosts() {
        let mut v1 = hello_frame();
        v1["protocolVersion"] = json!(1);
        assert!(parse_client_message(&v1.to_string())
            .unwrap_err()
            .contains("client protocol v1 is not supported"));

        let mut v2 = hello_frame();
        v2["protocolVersion"] = json!(2);
        assert!(parse_client_message(&v2.to_string()).is_ok());

        let mut host = hello_frame();
        host["host"] = json!("access");
        assert!(parse_client_message(&host.to_string())
            .unwrap_err()
            .contains("unknown host"));
    }

    #[test]
    fn parses_the_other_client_frames_and_rejects_unknown_types() {
        assert!(matches!(
            parse_client_message(r#"{"type":"ping","ts":7}"#).unwrap(),
            ClientMessage::Ping { ts } if (ts - 7.0).abs() < f64::EPSILON
        ));
        assert!(matches!(
            parse_client_message(r#"{"type":"pane_state","focused":true,"visible":false}"#)
                .unwrap(),
            ClientMessage::PaneState {
                focused: Some(true),
                visible: Some(false),
                ..
            }
        ));
        assert!(parse_client_message(r#"{"type":"pane_state","focused":1}"#).is_err());
        assert!(matches!(
            parse_client_message(r#"{"type":"abort"}"#).unwrap(),
            ClientMessage::Abort { prompt_id: None }
        ));
        assert!(parse_client_message(r#"{"type":"abort","promptId":3}"#).is_err());
        let _ = parse_client_message(r#"{"type":"tool_result","id":"c-1","ok":true,"text":"ok"}"#)
            .unwrap();
        assert!(parse_client_message(r#"{"type":"nope"}"#)
            .unwrap_err()
            .contains("unknown client message type 'nope'"));
        assert!(parse_client_message("not json")
            .unwrap_err()
            .contains("not valid JSON"));
    }

    #[test]
    fn truncates_huge_text_and_details() {
        let text = "汉".repeat(MAX_TEXT_CHARS + 5);
        let truncated = truncate_text(&text);
        assert!(truncated.contains("[truncated: 5 chars]"));
        assert_eq!(truncate_text("short"), "short");
        let big = bound_details(Some(json!({"blob": "x".repeat(MAX_DETAILS_BYTES)}))).unwrap();
        assert_eq!(big["truncated"], json!(true));
        assert!(bound_details(Some(json!({"ok": true}))).unwrap()["ok"]
            .as_bool()
            .unwrap());
    }

    #[test]
    fn call_ids_are_unique_and_prefixed() {
        let first = next_call_id("tool");
        let second = next_call_id("tool");
        assert!(first.starts_with("tool-"));
        assert_ne!(first, second);
    }
}
