//! Active office tool reconciliation.
//!
//! Ported from the upstream `active-tools.ts`: decide which
//! `office_<host>_<op>` tools the host exposes from the currently attached
//! panes. Pure functions, so the rules are testable without a session.
//!
//! Rules:
//!  - a pane advertising `ops` exposes exactly those ops' tools;
//!  - a legacy pane (no `ops`) exposes only the v1 op set;
//!  - tools for hosts that are no longer attached are deactivated;
//!  - the discovery tool is active whenever the bridge is on;
//!  - non-office tools in the current active set are preserved untouched.
#![forbid(unsafe_code)]

use crate::catalog::{catalog, Host};

/// The host-agnostic discovery tool, active only while the bridge is on.
pub const OFFICE_LIST_TOOL: &str = "office_list_documents";

/// What the server knows about a pane's capabilities.
#[derive(Clone, Debug)]
pub struct PaneCapability {
    pub host: Host,
    /// Advertised ops, or `None` for legacy panes (v1 set only).
    pub ops: Option<Vec<String>>,
}

/// True when the name is one of the catalog's `office_*` tools.
pub fn is_office_tool_name(name: &str) -> bool {
    catalog().entry_by_name(name).is_some()
}

/// True when the name is any tool this extension activates.
pub fn is_bridge_tool_name(name: &str) -> bool {
    name == OFFICE_LIST_TOOL || is_office_tool_name(name)
}

/// Tool names enabled by the given panes, in deterministic order.
pub fn active_office_tool_names(panes: &[PaneCapability]) -> Vec<String> {
    let catalog = catalog();
    let mut names: Vec<String> = Vec::new();
    for pane in panes {
        let fallback;
        let ops: &[String] = match &pane.ops {
            Some(ops) => ops,
            None => {
                fallback = catalog.legacy_v1_ops.clone();
                &fallback
            }
        };
        for op in ops {
            if let Some(entry) = catalog.entry_by_op(op) {
                if entry.host == pane.host && !names.iter().any(|name| name == &entry.name) {
                    names.push(entry.name.clone());
                }
            }
        }
    }
    names.sort();
    names
}

/// Lifecycle-aware reconciliation used by the extension.
///
/// When the bridge is off the whole bridge tool family is removed; when it is
/// on, the discovery tool plus exactly the ops the attached panes advertise are
/// added. Everything else in `current_active` is preserved.
pub fn reconcile_bridge_tool_activation(
    current_active: &[String],
    enabled: bool,
    panes: &[PaneCapability],
) -> Vec<String> {
    let mut next: Vec<String> = current_active
        .iter()
        .filter(|name| !is_bridge_tool_name(name))
        .cloned()
        .collect();
    if enabled {
        next.push(OFFICE_LIST_TOOL.to_string());
        next.extend(active_office_tool_names(panes));
    }
    // Deduplicate, then sort bridge tools last (matching upstream ordering).
    let mut seen: Vec<String> = Vec::new();
    for name in next {
        if !seen.iter().any(|kept| kept == &name) {
            seen.push(name);
        }
    }
    seen.sort_by(|a, b| {
        let a_office = is_bridge_tool_name(a);
        let b_office = is_bridge_tool_name(b);
        if a_office != b_office {
            return a_office.cmp(&b_office);
        }
        a.cmp(b)
    });
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn legacy_pane_activates_only_the_v1_op_set() {
        let panes = vec![PaneCapability {
            host: Host::Excel,
            ops: None,
        }];
        let active = active_office_tool_names(&panes);
        assert!(active.contains(&"office_excel_read_range".to_string()));
        assert!(active.contains(&"office_excel_get_overview".to_string()));
        assert!(!active.contains(&"office_excel_get_capabilities".to_string()));
        // Only Excel's v1 ops, never Word's.
        assert!(active.iter().all(|name| name.starts_with("office_excel_")));
    }

    #[test]
    fn advertised_ops_win_and_stay_scoped_to_the_host() {
        let panes = vec![PaneCapability {
            host: Host::Word,
            ops: Some(names(&["word.insert_text", "excel.read_range"])),
        }];
        let active = active_office_tool_names(&panes);
        assert_eq!(active, names(&["office_word_insert_text"]));
    }

    #[test]
    fn reconciliation_preserves_foreign_tools_and_is_idempotent() {
        let current = names(&["bash", "office_excel_read_range", "read"]);
        let off = reconcile_bridge_tool_activation(&current, false, &[]);
        assert_eq!(off, names(&["bash", "read"]));

        let panes = vec![PaneCapability {
            host: Host::Excel,
            ops: Some(names(&["excel.read_range"])),
        }];
        let on = reconcile_bridge_tool_activation(&off, true, &panes);
        assert_eq!(
            on,
            names(&["bash", "read", "office_excel_read_range", OFFICE_LIST_TOOL])
        );
        assert_eq!(reconcile_bridge_tool_activation(&on, true, &panes), on);
    }

    #[test]
    fn bridge_on_without_panes_exposes_only_the_discovery_tool() {
        let next = reconcile_bridge_tool_activation(&names(&["bash"]), true, &[]);
        assert_eq!(next, names(&["bash", OFFICE_LIST_TOOL]));
    }

    #[test]
    fn two_panes_of_the_same_host_merge_their_ops() {
        let panes = vec![
            PaneCapability {
                host: Host::Excel,
                ops: Some(names(&["excel.read_range"])),
            },
            PaneCapability {
                host: Host::Excel,
                ops: Some(names(&["excel.charts"])),
            },
        ];
        assert_eq!(
            active_office_tool_names(&panes),
            names(&["office_excel_charts", "office_excel_read_range"])
        );
    }

    #[test]
    fn tool_name_classification_matches_the_catalog() {
        assert!(is_office_tool_name("office_word_insert_text"));
        assert!(!is_office_tool_name("office_not_a_tool"));
        assert!(is_bridge_tool_name(OFFICE_LIST_TOOL));
        assert!(!is_bridge_tool_name("bash"));
    }
}
