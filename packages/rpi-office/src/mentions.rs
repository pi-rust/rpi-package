//! `#document` autocomplete for the prompt editor.
//!
//! Ported from the upstream `document-mentions.ts`: typing `#` opens the list of
//! documents attached to the bridge, and accepting an entry inserts `#<name>`
//! (or `#<id>` when two open documents share a name) which the server already
//! understands as a `document` target.
//!
//! Pure item construction — the host slot, the pane snapshot and the editor
//! insertion live in `lib.rs`.
#![forbid(unsafe_code)]

use crate::bridge::Pane;
use serde_json::{json, Value};

/// Most candidates offered for one query (the host caps a list at 50).
pub const MAX_ITEMS: usize = 20;

/// The `#` token the caret sits in, as the text after the `#`.
///
/// A `#` only starts a mention at the beginning of the line or right after
/// whitespace, so `a#b` and `C#` never trigger the document list. The needle may
/// be empty (just typed `#`) and may contain spaces (`#Budget 2024`).
pub fn mention_needle(line_prefix: &str) -> Option<&str> {
    let start = line_prefix.rfind('#')?;
    if start > 0 {
        let previous = line_prefix[..start].chars().next_back()?;
        if !previous.is_whitespace() {
            return None;
        }
    }
    Some(&line_prefix[start + 1..])
}

/// Completion items for the documents attached to the bridge.
///
/// Focused documents come first, then the rest by display name. The inserted
/// text keeps the leading `#` and sets `insertSpace: false`, so the caret lands
/// directly after the mention instead of after a space.
pub fn document_items(panes: &[Pane], needle: &str) -> Vec<Value> {
    let needle = needle.trim().to_lowercase();
    let mut candidates: Vec<&Pane> = panes
        .iter()
        .filter(|pane| {
            if needle.is_empty() {
                return true;
            }
            let name = pane.document_label().to_lowercase();
            name.contains(&needle) || pane.document_ref().to_lowercase().contains(&needle)
        })
        .collect();
    candidates.sort_by(|a, b| {
        b.focused
            .cmp(&a.focused)
            .then_with(|| a.document_label().cmp(&b.document_label()))
    });

    candidates
        .into_iter()
        .take(MAX_ITEMS)
        .map(|pane| {
            // Two *different* documents sharing a name cannot be told apart by
            // name, so those mentions insert the id instead. Match the
            // resolver's case-insensitive name comparison so completion cannot
            // suggest an ambiguous `#name`.
            let name_key = pane.document_name.as_deref().map(str::to_lowercase);
            let same_name = name_key
                .as_deref()
                .map(|name| {
                    panes
                        .iter()
                        .filter(|other| {
                            other.target_id != pane.target_id
                                && other
                                    .document_name
                                    .as_deref()
                                    .map(str::to_lowercase)
                                    .as_deref()
                                    == Some(name)
                        })
                        .count()
                })
                .unwrap_or(0);
            let identity = if same_name > 0 {
                pane.document_ref()
            } else {
                pane.document_name.as_deref().unwrap_or(pane.document_ref())
            };
            let windows = panes
                .iter()
                .filter(|other| other.target_id == pane.target_id)
                .count();
            let mut description = pane.host.label().to_string();
            if pane.focused {
                description.push_str(" · focused");
            }
            if windows > 1 {
                description.push_str(&format!(" · {windows} windows"));
            }
            json!({
                "text": format!("#{identity}"),
                "label": pane.document_label(),
                "description": description,
                "insertSpace": false,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Host;

    fn pane(pane_id: &str, host: Host, name: Option<&str>, id: Option<&str>) -> Pane {
        Pane {
            conn: 1,
            host,
            pane_id: pane_id.into(),
            client_name: "add-in".into(),
            protocol_version: 3,
            connected_at: 1,
            last_seen: 1,
            model: None,
            provider: None,
            ops: None,
            catalog_version: Some(3),
            ops_ignored: 0,
            document_id: id.map(str::to_owned),
            document_name: name.map(str::to_owned),
            document_source: "document.url".into(),
            target_id: id.unwrap_or(pane_id).to_string(),
            focused: false,
            focus_updated_at: 0,
            visible: true,
        }
    }

    #[test]
    fn only_a_bare_hash_opens_a_mention() {
        assert_eq!(mention_needle("#"), Some(""));
        assert_eq!(mention_needle("#Bud"), Some("Bud"));
        assert_eq!(mention_needle("sum #Bud"), Some("Bud"));
        assert_eq!(mention_needle("line one\n  #Bu"), Some("Bu"));
        assert_eq!(mention_needle("#Budget 24").map(str::len), Some(9));
        assert_eq!(mention_needle("no hash here"), None);
        assert_eq!(
            mention_needle("a#b"),
            None,
            "mid-word hash stays plain text"
        );
        assert_eq!(mention_needle("C#"), None);
    }

    #[test]
    fn items_filter_by_name_and_id_and_prefix_the_hash() {
        let panes = vec![
            pane("p1", Host::Excel, Some("Budget.xlsx"), Some("id-1")),
            pane("p2", Host::Word, Some("Report.docx"), Some("id-2")),
        ];
        let all = document_items(&panes, "");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0]["text"], json!("#Budget.xlsx"));
        assert_eq!(all[0]["insertSpace"], json!(false));
        assert_eq!(all[0]["description"], json!("Excel"));

        let filtered = document_items(&panes, "rep");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0]["text"], json!("#Report.docx"));
        assert_eq!(filtered[0]["description"], json!("Word"));

        let by_id = document_items(&panes, "id-1");
        assert_eq!(by_id.len(), 1);
        assert_eq!(by_id[0]["label"], json!("Budget.xlsx"));
    }

    #[test]
    fn focused_first_and_duplicate_names_insert_the_id() {
        let mut focused = pane("p1", Host::Excel, Some("Book.xlsx"), Some("id-1"));
        focused.focused = true;
        let other = pane("p2", Host::Excel, Some("Aardvark.xlsx"), Some("id-2"));
        let items = document_items(&[other.clone(), focused.clone()], "");
        assert_eq!(items[0]["label"], json!("Book.xlsx"));
        assert_eq!(items[0]["description"], json!("Excel · focused"));

        let mut twin = pane("p3", Host::Excel, Some("Book.xlsx"), Some("id-3"));
        twin.focused = false;
        let items = document_items(&[focused, twin], "");
        assert_eq!(items.len(), 2);
        assert!(items
            .iter()
            .all(|item| item["text"].as_str().unwrap().starts_with("#id-")));
    }

    #[test]
    fn duplicate_names_are_detected_case_insensitively() {
        let first = pane("p1", Host::Excel, Some("Book.xlsx"), Some("id-1"));
        let second = pane("p2", Host::Excel, Some("book.xlsx"), Some("id-2"));
        let items = document_items(&[first, second], "");
        assert_eq!(items.len(), 2);
        assert!(
            items
                .iter()
                .all(|item| item["text"].as_str().unwrap().starts_with("#id-")),
            "case-insensitive duplicate names must insert ids: {items:?}"
        );
    }

    #[test]
    fn unnamed_documents_fall_back_to_the_routing_id() {
        let items = document_items(&[pane("p1", Host::PowerPoint, None, None)], "");
        assert_eq!(items[0]["text"], json!("#p1"));
        assert_eq!(items[0]["label"], json!("Untitled (p1)"));
    }

    #[test]
    fn at_most_twenty_items_are_offered() {
        let panes: Vec<Pane> = (0..30)
            .map(|index| {
                pane(
                    &format!("p{index}"),
                    Host::Excel,
                    Some(&format!("Book{index}.xlsx")),
                    Some(&format!("id-{index}")),
                )
            })
            .collect();
        assert_eq!(document_items(&panes, "").len(), MAX_ITEMS);
    }

    #[test]
    fn no_match_returns_no_items() {
        let panes = vec![pane("p1", Host::Excel, Some("Budget.xlsx"), Some("id-1"))];
        assert!(document_items(&panes, "zzz").is_empty());
    }
}
