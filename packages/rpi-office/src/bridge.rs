//! Attached-pane registry and `document` target resolution.
//!
//! Ported from the upstream `bridge-server.ts` pane model. Pure: no sockets, no
//! tokio, no host calls, so the routing rules (and every error message the model
//! sees) are unit-testable.
#![forbid(unsafe_code)]

use crate::catalog::{catalog, Host};
use crate::protocol::{now_ms, Hello, OFFICE_TARGET_PARAM};
use serde_json::{json, Value};

/// A task pane attached over the loopback WebSocket.
#[derive(Clone, Debug)]
pub struct Pane {
    /// Connection identity; a reconnect gets a new one.
    pub conn: u64,
    pub host: Host,
    pub pane_id: String,
    pub client_name: String,
    /// Protocol version the pane declared in `hello` (v3+ receives deltas).
    pub protocol_version: u32,
    pub connected_at: u64,
    pub last_seen: u64,
    pub model: Option<String>,
    pub provider: Option<String>,
    /// Ops this pane advertised, normalized against the catalog. `None` means a
    /// legacy pane that advertises nothing (only the v1 op set is allowed).
    pub ops: Option<Vec<String>>,
    pub catalog_version: Option<u32>,
    /// Advertised ops dropped because this server does not know them.
    pub ops_ignored: u32,
    pub document_id: Option<String>,
    pub document_name: Option<String>,
    pub document_source: String,
    /// Routing key: the document id when known, else a per-connection id.
    pub target_id: String,
    pub focused: bool,
    pub focus_updated_at: u64,
    pub visible: bool,
}

impl Pane {
    /// Display name used in tables, status lines and error messages.
    pub fn document_label(&self) -> String {
        self.document_name
            .clone()
            .unwrap_or_else(|| format!("Untitled ({})", self.target_id))
    }

    /// The id a model can pass as `document`.
    pub fn document_ref(&self) -> &str {
        self.document_id.as_deref().unwrap_or(&self.target_id)
    }
}

/// The attached-pane registry.
#[derive(Default)]
pub struct Bridge {
    panes: Vec<Pane>,
}

impl Bridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) the pane for a connection that just sent `hello`.
    pub fn attach(&mut self, conn: u64, hello: &Hello) -> &Pane {
        let ops = normalize_advertised_ops(hello.host, hello.ops.as_deref());
        let ops_ignored = count_ignored_ops(hello.host, hello.ops.as_deref());
        let now = now_ms();
        let pane = Pane {
            conn,
            host: hello.host,
            pane_id: hello.pane_id.clone(),
            client_name: hello.client_name.clone(),
            protocol_version: hello.protocol_version,
            connected_at: now,
            last_seen: now,
            model: None,
            provider: None,
            ops,
            catalog_version: hello.catalog_version,
            ops_ignored,
            document_id: hello.document.id.clone(),
            document_name: hello.document.name.clone(),
            document_source: hello.document.source.as_str().to_string(),
            target_id: hello
                .document
                .id
                .clone()
                .unwrap_or_else(|| format!("ephemeral:{}", hello.pane_id)),
            focused: false,
            focus_updated_at: 0,
            visible: true,
        };
        // A pane reconnecting replaces any older registration of the same pane.
        self.panes
            .retain(|existing| existing.pane_id != pane.pane_id);
        self.panes.push(pane);
        self.panes.last().expect("just pushed")
    }

    /// Drop a connection. Returns true when a pane was registered for it.
    pub fn detach(&mut self, conn: u64) -> bool {
        let before = self.panes.len();
        self.panes.retain(|pane| pane.conn != conn);
        self.panes.len() != before
    }

    /// Panes ordered most-recent-connection first (the order `/health` reports).
    pub fn attached(&self) -> Vec<Pane> {
        let mut panes = self.panes.clone();
        panes.sort_by_key(|pane| std::cmp::Reverse(pane.connected_at));
        panes
    }

    pub fn pane(&self, conn: u64) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.conn == conn)
    }

    pub fn pane_by_id(&self, pane_id: &str) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.pane_id == pane_id)
    }

    pub fn touch(&mut self, conn: u64) {
        if let Some(pane) = self.panes.iter_mut().find(|pane| pane.conn == conn) {
            pane.last_seen = now_ms();
        }
    }

    pub fn set_status(&mut self, conn: u64, model: Option<&str>, provider: Option<&str>) {
        if let Some(pane) = self.panes.iter_mut().find(|pane| pane.conn == conn) {
            pane.last_seen = now_ms();
            if let Some(model) = model {
                pane.model = Some(model.to_string());
            }
            if let Some(provider) = provider {
                pane.provider = Some(provider.to_string());
            }
        }
    }

    /// Mark `conn` focused, demoting every other pane of the same host so at
    /// most one document per host app is "active" at a time.
    pub fn mark_focused(&mut self, conn: u64, visible: Option<bool>) {
        let Some(host) = self.pane(conn).map(|pane| pane.host) else {
            return;
        };
        let now = now_ms();
        for pane in self.panes.iter_mut() {
            if pane.conn != conn && pane.host == host && pane.focused {
                pane.focused = false;
            }
        }
        if let Some(pane) = self.panes.iter_mut().find(|pane| pane.conn == conn) {
            pane.focused = true;
            pane.focus_updated_at = now;
            if let Some(visible) = visible {
                pane.visible = visible;
            }
        }
    }

    pub fn set_focused(&mut self, conn: u64, focused: bool) {
        if let Some(pane) = self.panes.iter_mut().find(|pane| pane.conn == conn) {
            pane.focused = focused;
        }
    }

    pub fn set_visible(&mut self, conn: u64, visible: bool) {
        if let Some(pane) = self.panes.iter_mut().find(|pane| pane.conn == conn) {
            pane.visible = visible;
        }
    }

    /// Resolve a `document` selector to exactly one pane of `host`.
    ///
    /// Order: focused document → the only attached document → exact id → exact
    /// name → unique name substring. Ambiguity or no match is an actionable
    /// error listing the open documents instead of a guess.
    pub fn resolve(&self, host: Host, target: Option<&str>) -> Result<&Pane, String> {
        let panes: Vec<&Pane> = self.panes.iter().filter(|pane| pane.host == host).collect();
        if panes.is_empty() {
            return Err(format!(
                "office-bridge: no {host} workbook/document is attached. \
Open the document in the Office add-in (pi-for-office) to enable this tool.",
                host = host.as_str()
            ));
        }

        // A `#name` mention is a valid selector; normalize it away.
        let needle = target
            .unwrap_or_default()
            .trim()
            .trim_start_matches('#')
            .trim()
            .to_string();
        if needle.is_empty() || needle.eq_ignore_ascii_case("active") {
            return self.pick_focused_or_only(&panes, host, "active");
        }

        let exact_id: Vec<&Pane> = panes
            .iter()
            .copied()
            .filter(|pane| pane.target_id == needle || pane.document_id.as_deref() == Some(&needle))
            .collect();
        if !exact_id.is_empty() {
            return self.pick_focused_or_only(&exact_id, host, &needle);
        }

        let lower = needle.to_lowercase();
        let exact_name: Vec<&Pane> = panes
            .iter()
            .copied()
            .filter(|pane| {
                pane.document_name
                    .as_deref()
                    .map(str::to_lowercase)
                    .as_deref()
                    == Some(lower.as_str())
            })
            .collect();
        if !exact_name.is_empty() {
            let distinct: std::collections::HashSet<&str> = exact_name
                .iter()
                .map(|pane| pane.target_id.as_str())
                .collect();
            if distinct.len() > 1 {
                return Err(format!(
                    "office-bridge: \"{needle}\" matches several {host} documents with the same name. \
Open: {open}. Pass {param}=<id> to choose one.",
                    host = host.as_str(),
                    open = self.describe(host),
                    param = OFFICE_TARGET_PARAM
                ));
            }
            return self.pick_focused_or_only(&exact_name, host, &needle);
        }

        let partial: Vec<&Pane> = panes
            .iter()
            .copied()
            .filter(|pane| {
                pane.document_name
                    .as_deref()
                    .map(|name| name.to_lowercase().contains(&lower))
                    .unwrap_or(false)
            })
            .collect();
        if partial.len() == 1 {
            return Ok(partial[0]);
        }
        if partial.len() > 1 {
            return Err(format!(
                "office-bridge: \"{needle}\" matches several {host} documents. \
Open: {open}. Pass {param}=<exact name|id> or open the one you mean.",
                host = host.as_str(),
                open = self.describe(host),
                param = OFFICE_TARGET_PARAM
            ));
        }

        Err(format!(
            "office-bridge: no {host} document matches \"{needle}\". \
Open: {open}. Pass {param}=<name|id|\"active\"> (or mention #<name> in the prompt).",
            host = host.as_str(),
            open = self.describe(host),
            param = OFFICE_TARGET_PARAM
        ))
    }

    /// Pick the focused candidate, else the only one. Two windows of one
    /// document with no focus is ambiguous: fail instead of guessing a window.
    fn pick_focused_or_only<'a>(
        &self,
        candidates: &[&'a Pane],
        host: Host,
        needle: &str,
    ) -> Result<&'a Pane, String> {
        let mut focused: Vec<&&Pane> = candidates.iter().filter(|pane| pane.focused).collect();
        focused.sort_by_key(|pane| std::cmp::Reverse(pane.focus_updated_at));
        if let Some(pane) = focused.first() {
            return Ok(pane);
        }
        if candidates.len() == 1 {
            return Ok(candidates[0]);
        }
        Err(format!(
            "office-bridge: several {host} documents are open and none is focused, so \"{needle}\" \
is ambiguous. Open: {open}. Focus the document you mean, or pass {param}=<name|id>.",
            host = host.as_str(),
            open = self.describe(host),
            param = OFFICE_TARGET_PARAM
        ))
    }

    /// Human-readable list of the open documents of `host`.
    pub fn describe(&self, host: Host) -> String {
        let panes: Vec<&Pane> = self.panes.iter().filter(|pane| pane.host == host).collect();
        panes
            .iter()
            .map(|pane| {
                let windows = panes
                    .iter()
                    .filter(|other| other.target_id == pane.target_id)
                    .count();
                let mut notes: Vec<String> = Vec::new();
                if pane.focused {
                    notes.push("focused".into());
                }
                if windows > 1 {
                    notes.push(format!("{windows} windows"));
                }
                let suffix = if notes.is_empty() {
                    String::new()
                } else {
                    format!(", {}", notes.join(", "))
                };
                format!(
                    "\"{name}\" ({id}{suffix})",
                    name = pane.document_label(),
                    id = pane.document_ref()
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Split the bridge-only `document` selector out of a tool-call args record so
/// the pane never receives it.
pub fn split_target(args: &Value) -> (Option<String>, Value) {
    let mut rest = args.clone();
    let target = match rest.as_object_mut() {
        Some(object) => match object.remove(OFFICE_TARGET_PARAM) {
            Some(Value::String(target)) if !target.trim().is_empty() => Some(target),
            _ => None,
        },
        None => None,
    };
    (target, rest)
}

/// Keep only ops the server catalog knows and that belong to the pane's host.
/// `None` means the pane advertised nothing (legacy 0.2.x client).
fn normalize_advertised_ops(host: Host, advertised: Option<&[String]>) -> Option<Vec<String>> {
    let advertised = advertised?;
    if advertised.is_empty() {
        return None;
    }
    let catalog = catalog();
    let mut known: Vec<String> = Vec::new();
    for op in advertised {
        if let Some(entry) = catalog.entry_by_op(op) {
            if entry.host == host && !known.iter().any(|kept| kept == op) {
                known.push(op.clone());
            }
        }
    }
    if known.is_empty() {
        None
    } else {
        Some(known)
    }
}

fn count_ignored_ops(host: Host, advertised: Option<&[String]>) -> u32 {
    let Some(advertised) = advertised else {
        return 0;
    };
    let catalog = catalog();
    advertised
        .iter()
        .filter(|op| match catalog.entry_by_op(op) {
            Some(entry) => entry.host != host,
            None => true,
        })
        .count() as u32
}

/// Replaceable server metadata published on `/health`.
#[derive(Clone, Debug)]
pub struct HealthMeta {
    pub service: String,
    pub server_version: String,
    pub pi_version: Option<String>,
    pub port: u16,
    pub started_at: u64,
}

/// Build the `GET /health` JSON body from a pane snapshot.
pub fn health_json(all: &[Pane], meta: &HealthMeta) -> Value {
    let panes: Vec<Value> = all
        .iter()
        .map(|pane| {
            let windows = all
                .iter()
                .filter(|other| other.host == pane.host && other.target_id == pane.target_id)
                .count();
            json!({
                "host": pane.host.as_str(),
                "paneId": pane.pane_id,
                "clientName": pane.client_name,
                "document": {
                    "id": pane.document_id,
                    "name": pane.document_name,
                    "source": pane.document_source,
                },
                "targetId": pane.target_id,
                "focused": pane.focused,
                "visible": pane.visible,
                "windows": windows,
                "connectedAt": pane.connected_at,
                "lastSeen": pane.last_seen,
                "model": pane.model,
                "provider": pane.provider,
                "ops": pane.ops,
                "catalogVersion": pane.catalog_version,
                "protocolVersion": pane.protocol_version,
                "opsIgnoredCount": pane.ops_ignored,
            })
        })
        .collect();
    json!({
        "ok": true,
        "service": meta.service,
        "serverVersion": meta.server_version,
        "capabilities": crate::protocol::CAPABILITIES,
        "protocolVersion": crate::protocol::BRIDGE_PROTOCOL_VERSION,
        "piVersion": meta.pi_version,
        "port": meta.port,
        "startedAt": meta.started_at,
        "uptimeMs": now_ms().saturating_sub(meta.started_at),
        "catalogVersion": catalog().version,
        "owner": {
            "pid": std::process::id(),
            "sessionId": Value::Null,
            "client": "cli",
            "cwd": std::env::current_dir()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
        },
        "panes": panes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::DocumentIdentity;

    fn hello(host: Host, pane_id: &str, name: Option<&str>, id: Option<&str>) -> Hello {
        Hello {
            protocol_version: 3,
            host,
            client_name: "pane".into(),
            pane_id: pane_id.into(),
            document: DocumentIdentity {
                id: id.map(str::to_owned),
                name: name.map(str::to_owned),
                source: crate::protocol::DocumentSource::DocumentUrl,
            },
            ops: Some(vec![
                format!("{}.read_range", host.as_str()),
                format!("{}.get_overview", host.as_str()),
            ]),
            catalog_version: Some(3),
        }
    }

    #[test]
    fn attach_normalizes_ops_and_replaces_the_same_pane_id() {
        let mut bridge = Bridge::new();
        let mut frame = hello(Host::Excel, "pane-1", Some("Book1"), Some("url_sha256:a"));
        frame.ops = Some(vec![
            "excel.read_range".into(),
            "word.insert_text".into(),
            "excel.not_a_real_op".into(),
        ]);
        let pane = bridge.attach(1, &frame).clone();
        assert_eq!(
            pane.ops.as_deref(),
            Some(&["excel.read_range".to_string()][..])
        );
        assert_eq!(pane.ops_ignored, 2);
        assert_eq!(pane.target_id, "url_sha256:a");

        let mut reconnect = hello(Host::Excel, "pane-1", Some("Book2"), None);
        reconnect.protocol_version = 2;
        let pane = bridge.attach(2, &reconnect).clone();
        assert_eq!(pane.protocol_version, 2);
        assert_eq!(pane.target_id, "ephemeral:pane-1");
        assert_eq!(
            bridge.attached().len(),
            1,
            "the reconnect replaces the old pane"
        );
        // The replaced connection no longer owns a pane.
        assert!(!bridge.detach(1));
        assert!(bridge.detach(2));
    }

    #[test]
    fn legacy_panes_get_no_op_list() {
        let mut bridge = Bridge::new();
        let mut frame = hello(Host::Word, "pane-1", Some("Doc"), None);
        frame.ops = None;
        let pane = bridge.attach(1, &frame).clone();
        assert!(pane.ops.is_none());
        assert_eq!(pane.ops_ignored, 0);
    }

    #[test]
    fn resolves_focused_then_only_then_id_then_name() {
        let mut bridge = Bridge::new();
        bridge.attach(1, &hello(Host::Excel, "p1", Some("Budget"), Some("id-1")));
        bridge.attach(2, &hello(Host::Excel, "p2", Some("Forecast"), Some("id-2")));

        // Several open, none focused → ambiguous.
        let error = bridge.resolve(Host::Excel, None).unwrap_err();
        assert!(error.contains("none is focused"));

        bridge.mark_focused(2, None);
        assert_eq!(bridge.resolve(Host::Excel, None).unwrap().conn, 2);
        assert_eq!(bridge.resolve(Host::Excel, Some("active")).unwrap().conn, 2);
        assert_eq!(bridge.resolve(Host::Excel, Some("id-1")).unwrap().conn, 1);
        assert_eq!(
            bridge.resolve(Host::Excel, Some("#Forecast")).unwrap().conn,
            2
        );
        assert_eq!(bridge.resolve(Host::Excel, Some("budget")).unwrap().conn, 1);
        assert_eq!(bridge.resolve(Host::Excel, Some("cast")).unwrap().conn, 2);

        let error = bridge.resolve(Host::Excel, Some("nope")).unwrap_err();
        assert!(error.contains("no excel document matches"));
        assert!(error.contains("Budget"));
        let error = bridge.resolve(Host::Word, None).unwrap_err();
        assert!(error.contains("no word workbook/document is attached"));
    }

    #[test]
    fn two_windows_of_one_document_route_by_focus() {
        let mut bridge = Bridge::new();
        bridge.attach(1, &hello(Host::Excel, "p1", Some("Book"), Some("id-1")));
        bridge.attach(2, &hello(Host::Excel, "p2", Some("Book"), Some("id-1")));
        assert!(bridge
            .resolve(Host::Excel, None)
            .unwrap_err()
            .contains("ambiguous"));
        bridge.mark_focused(2, Some(true));
        assert_eq!(bridge.resolve(Host::Excel, None).unwrap().conn, 2);
        // Focusing one pane demotes the other pane of the same host.
        assert!(!bridge.pane(1).unwrap().focused);
    }

    #[test]
    fn same_name_on_distinct_documents_is_never_resolved_by_focus() {
        let mut bridge = Bridge::new();
        bridge.attach(1, &hello(Host::Word, "p1", Some("Report"), Some("id-1")));
        bridge.attach(2, &hello(Host::Word, "p2", Some("Report"), Some("id-2")));
        bridge.mark_focused(1, None);
        let error = bridge.resolve(Host::Word, Some("Report")).unwrap_err();
        assert!(error.contains("matches several word documents"));
    }

    #[test]
    fn split_target_never_forwards_the_selector() {
        let (target, rest) = split_target(&json!({"document": "id-1", "range": "A1:B2"}));
        assert_eq!(target.as_deref(), Some("id-1"));
        assert!(rest.get("document").is_none());
        assert_eq!(rest["range"], json!("A1:B2"));

        let (target, rest) = split_target(&json!({"document": 42, "sheet": "Sheet1"}));
        assert!(target.is_none());
        assert!(rest.get("document").is_none());

        let (target, _) = split_target(&json!({}));
        assert!(target.is_none());
    }

    #[test]
    fn health_reports_every_pane_and_the_owner() {
        let mut bridge = Bridge::new();
        bridge.attach(
            1,
            &hello(Host::PowerPoint, "p1", Some("Deck"), Some("id-1")),
        );
        bridge.attach(2, &hello(Host::PowerPoint, "p2", None, None));
        let meta = HealthMeta {
            service: "rpi-office".into(),
            server_version: "0.1.0".into(),
            pi_version: None,
            port: 38617,
            started_at: now_ms(),
        };
        let value = health_json(&bridge.attached(), &meta);
        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["service"], json!("rpi-office"));
        assert_eq!(value["protocolVersion"], json!(3));
        assert_eq!(value["catalogVersion"], json!(3));
        assert_eq!(value["panes"].as_array().unwrap().len(), 2);
        assert_eq!(value["owner"]["pid"], json!(std::process::id()));
        assert_eq!(value["panes"][0]["windows"], json!(1));
    }
}
