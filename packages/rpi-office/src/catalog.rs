//! Office op catalog: the shared, server-authoritative tool surface.
//!
//! The catalog is generated from the upstream `@dieulc/pi-office-protocol`
//! TypeBox catalog (`office-catalog.ts`) and committed as JSON so the Rust
//! bridge and the pi-for-office task pane cannot drift: the add-in advertises
//! the op ids it can execute in `hello.ops`, and this side validates them
//! against exactly the same ids.
//!
//! Regenerating: see `catalog/README.md`.
#![forbid(unsafe_code)]

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

/// The committed catalog, generated from the upstream TypeScript source.
pub const CATALOG_JSON: &str = include_str!("../catalog/office-catalog.json");

/// Office host applications the add-in can attach.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Host {
    Excel,
    Word,
    PowerPoint,
}

impl Host {
    pub fn as_str(self) -> &'static str {
        match self {
            Host::Excel => "excel",
            Host::Word => "word",
            Host::PowerPoint => "powerpoint",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "excel" => Some(Host::Excel),
            "word" => Some(Host::Word),
            "powerpoint" => Some(Host::PowerPoint),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Host::Excel => "Excel",
            Host::Word => "Word",
            Host::PowerPoint => "PowerPoint",
        }
    }
}

/// One `office_<host>_<op>` tool definition.
///
/// Some fields are catalog metadata the current host has no slot for yet (the
/// tool-prompt registrar arrived after the pinned SDK), so they are kept for
/// parity, diagnostics and future use instead of being dropped.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct CatalogEntry {
    pub host: Host,
    /// Payload op id, namespaced by host: `excel.read_range`.
    pub op: String,
    /// Registered tool name: `office_excel_read_range`.
    pub name: String,
    pub label: String,
    pub description: String,
    pub prompt_snippet: Option<String>,
    pub prompt_guidelines: Vec<String>,
    /// JSON Schema for the arguments, without the bridge-only `document` target.
    pub parameters: Value,
    /// True when the legacy 0.2.x add-in can execute this op.
    pub legacy_v1: bool,
    pub since: u32,
}

impl CatalogEntry {
    /// `"<host>.<op>"`, the id the pane advertises and answers.
    pub fn op_id(&self) -> String {
        format!("{}.{}", self.host.as_str(), self.op)
    }
}

/// The parsed catalog plus the lookup indexes the bridge needs.
pub struct Catalog {
    pub version: u32,
    /// Protocol version the catalog was published with (informational here;
    /// the wire version lives in [`crate::protocol`]).
    #[allow(dead_code)]
    pub protocol_version: u32,
    pub entries: Vec<CatalogEntry>,
    pub legacy_v1_ops: Vec<String>,
    pub tool_names: Vec<String>,
    by_op: HashMap<String, usize>,
    by_name: HashMap<String, usize>,
}

impl Catalog {
    fn parse(raw: &str) -> Result<Self, String> {
        let doc: Value = serde_json::from_str(raw)
            .map_err(|error| format!("office catalog JSON is invalid: {error}"))?;
        let version = doc["catalogVersion"]
            .as_u64()
            .ok_or("office catalog is missing catalogVersion")? as u32;
        let protocol_version = doc["protocolVersion"]
            .as_u64()
            .ok_or("office catalog is missing protocolVersion")?
            as u32;
        let mut entries = Vec::new();
        let mut legacy_v1_ops = Vec::new();
        for entry in doc["entries"]
            .as_array()
            .ok_or("office catalog is missing entries")?
        {
            let host_raw = entry["host"].as_str().unwrap_or_default();
            let host = Host::parse(host_raw)
                .ok_or_else(|| format!("office catalog entry has unknown host '{host_raw}'"))?;
            let op = entry["op"]
                .as_str()
                .ok_or("office catalog entry is missing op")?
                .to_string();
            let name = entry["name"]
                .as_str()
                .ok_or("office catalog entry is missing name")?
                .to_string();
            let legacy_v1 = entry["legacyV1"].as_bool().unwrap_or(false);
            if legacy_v1 {
                legacy_v1_ops.push(format!("{}.{op}", host.as_str()));
            }
            entries.push(CatalogEntry {
                host,
                op,
                name,
                label: entry["label"].as_str().unwrap_or_default().to_string(),
                description: entry["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                prompt_snippet: entry["promptSnippet"].as_str().map(str::to_owned),
                prompt_guidelines: entry["promptGuidelines"]
                    .as_array()
                    .map(|list| {
                        list.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
                parameters: entry["parameters"].clone(),
                legacy_v1,
                since: entry["since"].as_u64().unwrap_or(1) as u32,
            });
        }
        let mut by_op = HashMap::new();
        let mut by_name = HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            by_op.insert(entry.op_id(), index);
            by_name.insert(entry.name.clone(), index);
        }
        let mut tool_names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
        legacy_v1_ops.sort();
        // Deterministic: the catalog order is the registration order upstream.
        tool_names.sort();
        Ok(Self {
            version,
            protocol_version,
            entries,
            legacy_v1_ops,
            tool_names,
            by_op,
            by_name,
        })
    }

    pub fn entry_by_op(&self, op_id: &str) -> Option<&CatalogEntry> {
        self.by_op.get(op_id).map(|index| &self.entries[*index])
    }

    pub fn entry_by_name(&self, name: &str) -> Option<&CatalogEntry> {
        self.by_name.get(name).map(|index| &self.entries[*index])
    }

    /// The host a registered tool name drives, or `None` for non-office tools.
    pub fn host_for_tool(&self, name: &str) -> Option<Host> {
        self.entry_by_name(name).map(|entry| entry.host)
    }

    /// True when the legacy (no capability list) pane may execute this op id.
    pub fn legacy_allows(&self, op_id: &str) -> bool {
        self.legacy_v1_ops.iter().any(|op| op == op_id)
    }
}

/// The process-wide catalog (parsed once).
pub fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        Catalog::parse(CATALOG_JSON).unwrap_or_else(|_error| {
            // A malformed committed catalog is a build-time mistake; keep the
            // plugin loadable (empty surface) instead of aborting the host.
            Catalog {
                version: 0,
                protocol_version: 0,
                entries: Vec::new(),
                legacy_v1_ops: Vec::new(),
                tool_names: Vec::new(),
                by_op: HashMap::new(),
                by_name: HashMap::new(),
            }
        })
    })
}

/// The optional per-tool document selector (protocol v2).
///
/// It is added to every bridge tool schema and stripped by the server before
/// the pane sees the call, so the shared catalog stays target-free.
pub fn document_target_schema() -> Value {
    json!({
        "type": "string",
        "description": "Target document: \"active\" (default) for the focused document, a document \
    id from office_list_documents, or a unique document name. Required when several documents of \
    this host are open and none is focused. A \"#name\" mention from the prompt is accepted.",
    })
}

/// Copy a catalog parameter schema and add the optional `document` property.
pub fn parameters_with_target(parameters: &Value) -> Value {
    if parameters["type"] != json!("object") {
        return parameters.clone();
    }
    let mut schema = parameters.clone();
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return schema;
    };
    properties.insert("document".into(), document_target_schema());
    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_matches_the_upstream_surface() {
        let catalog = catalog();
        assert_eq!(catalog.version, 3);
        assert_eq!(catalog.protocol_version, 3);
        assert_eq!(catalog.entries.len(), 32, "upstream catalog has 32 ops");
        let hosts = catalog
            .entries
            .iter()
            .filter(|e| e.host == Host::Excel)
            .count();
        assert_eq!(hosts, 15);
        assert_eq!(
            catalog
                .entries
                .iter()
                .filter(|e| e.host == Host::Word)
                .count(),
            11
        );
        assert_eq!(
            catalog
                .entries
                .iter()
                .filter(|e| e.host == Host::PowerPoint)
                .count(),
            6
        );
        assert_eq!(catalog.legacy_v1_ops.len(), 12);
        assert_eq!(
            catalog.host_for_tool("office_word_insert_text"),
            Some(Host::Word)
        );
        assert_eq!(catalog.host_for_tool("bash"), None);
        let entry = catalog
            .entry_by_op("excel.read_range")
            .expect("excel.read_range exists");
        assert_eq!(entry.name, "office_excel_read_range");
        assert!(entry.legacy_v1);
        assert!(!entry.description.is_empty());
        assert_eq!(entry.parameters["type"], json!("object"));
        assert!(entry.parameters["properties"]["range"].is_object());
    }

    #[test]
    fn document_target_is_optional_and_never_required() {
        let entry = catalog()
            .entry_by_op("excel.read_range")
            .expect("catalog entry");
        let with_target = parameters_with_target(&entry.parameters);
        assert!(with_target["properties"]["document"].is_object());
        assert_eq!(
            with_target["properties"]["range"],
            entry.parameters["properties"]["range"]
        );
        let required = with_target["required"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            !required.iter().any(|value| value == "document"),
            "document must stay optional"
        );
    }
}
