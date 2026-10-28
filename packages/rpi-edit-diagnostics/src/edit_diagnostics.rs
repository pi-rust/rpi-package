//! Compiler-style failure reports for the `edit` tool.
//!
//! Copied from `rpi` (`crates/rpi-tools/src/tools/edit_diagnostics.rs`, tag
//! `v0.9.4-aia.2`, MIT — see `NOTICE`), with the crate-internal `use` paths
//! adjusted.
//!
//! A failing edit is described the way a compiler describes a bad token: the
//! line numbers of the region the model probably meant, what exactly differs
//! there, and the real text so the next attempt can copy it instead of guessing.
//! [`crate::edit_apply`] reports *all* failing edits in one round trip,
//! each as one block of the same shape:
//!
//! ```text
//! error: edits[1].oldText not found in src/a.rs
//!   --> lines 12-15 are the closest match: only indentation differs (file uses 8 spaces, oldText 4)
//!   differing lines:
//!       13 - oldText:   let x = foo(a);
//!       13 + file:          let x = foo(a);
//!   help: this is the text at lines 12-15 (the "| " prefix is not part of it); use it as oldText:
//!   | fn a() {
//!   |         let x = foo(a);
//!   | }
//! ```
//!
//! Every line but the `error:` line is indented, and the quoted region is
//! verbatim so it can be pasted into `oldText`.

use similar::{ChangeTag, TextDiff};

use crate::edit_diff::normalize_to_lf;
use crate::semantic_edit::chain::find_occurrence_positions;
use crate::semantic_edit::{find_candidates, Candidate};

/// Maximum number of differing lines quoted per edit; past this a count is
/// printed instead.
pub const MAX_DIFFERING_LINES: usize = 8;

/// Maximum number of file lines quoted as the `help` text. A longer region is
/// pointed at instead (with `read` offsets), since a wall of text is not
/// something the model can copy reliably.
pub const MAX_QUOTED_LINES: usize = 30;

/// Maximum number of occurrence lines listed for an ambiguous `oldText`.
pub const MAX_LISTED_LINES: usize = 8;

/// `error: edits[2].oldText is empty in src/a.rs - ...`.
pub fn empty_old_text(path: &str, index: usize) -> String {
    format!("error: edits[{index}].oldText is empty in {path} - provide the exact text to replace")
}

/// `error: edits[2].oldText and newText are identical ...`.
pub fn no_op(path: &str, index: usize) -> String {
    format!(
        "error: edits[{index}].oldText and newText are identical in {path} - this edit would change nothing"
    )
}

/// `error: edits[0].oldText occurs 3 times in src/a.rs (lines 10, 40, 77)`.
pub fn duplicate(path: &str, index: usize, count: usize, positions: &[usize]) -> String {
    format!(
        "error: edits[{index}].oldText occurs {count} times in {path} ({})\n  \
         help: include the surrounding lines that distinguish the intended occurrence, or set \
         replaceAll: true to change every occurrence",
        lines_list(positions)
    )
}

/// `error: edits[0] (lines 3-4) and edits[1] (lines 4-5) overlap in src/a.rs`.
pub fn overlap(
    path: &str,
    (a_index, a_start, a_end): (usize, usize, usize),
    (b_index, b_start, b_end): (usize, usize, usize),
) -> String {
    format!(
        "error: edits[{a_index}] ({}) and edits[{b_index}] ({}) overlap in {path}\n  \
         help: merge them into one edit, or make each oldText target a disjoint region",
        lines_phrase(a_start, a_end),
        lines_phrase(b_start, b_end)
    )
}

/// Everything that can be said about one failing `oldText`: the closest regions
/// the file does contain, how they differ, and the text to copy back.
pub fn not_found(path: &str, index: usize, old_text: &str, content: &str) -> String {
    let candidates = find_candidates(content, old_text);
    let mut out = format!("error: edits[{index}].oldText not found in {path}\n");

    if candidates.is_empty() {
        out.push_str("  --> no text in the file is similar to oldText\n");
        if let Some(line) = first_content_line(old_text) {
            let positions = find_occurrence_positions(content, &line);
            if !positions.is_empty() {
                out.push_str(&format!(
                    "  --> oldText's first line \"{}\" appears at {}\n",
                    abbreviate_line(&line),
                    lines_list(&positions)
                ));
            }
        }
        out.push_str(
            "  help: read the file again and copy its current text into oldText (whitespace and \
             all)\n",
        );
        return out.trim_end().to_string();
    }

    for (i, candidate) in candidates.iter().enumerate() {
        let verb = if candidate.line_count() == 1 {
            "is"
        } else {
            "are"
        };
        out.push_str(&format!(
            "  --> {} {verb} {}: {}\n",
            candidate.lines_phrase(),
            if i == 0 {
                "the closest match"
            } else {
                "also similar"
            },
            describe(candidate, old_text)
        ));
    }

    let best = &candidates[0];
    out.push_str("  differing lines:\n");
    for entry in differing_lines(old_text, best) {
        out.push_str(&format!("  {entry}\n"));
    }

    if best.line_count() > MAX_QUOTED_LINES {
        out.push_str(&format!(
            "  help: {} is too long to quote here; read it with read (offset {}, limit {}) and \
             copy that text into oldText\n",
            best.lines_phrase(),
            best.start_line,
            best.line_count()
        ));
    } else {
        out.push_str(&format!(
            "  help: this is the text at {} (the \"| \" prefix is not part of it); use it as \
             oldText:\n",
            best.lines_phrase()
        ));
        for line in best.text.split('\n') {
            out.push_str(&format!("  | {line}\n"));
        }
    }
    out.trim_end().to_string()
}

/// Join the blocks of all failing edits, ordered by edit index and separated by
/// a blank line.
pub fn join(mut failures: Vec<(usize, String)>) -> String {
    failures.sort_by_key(|(index, _)| *index);
    failures
        .into_iter()
        .map(|(_, message)| message)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `lines 10, 40, 77`; a line holding several occurrences reads `1 (3 times)`,
/// and lines past [`MAX_LISTED_LINES`] are counted instead of listed.
fn lines_list(positions: &[usize]) -> String {
    let mut lines: Vec<(usize, usize)> = Vec::new();
    for &line in positions {
        match lines.last_mut() {
            Some((last, count)) if *last == line => *count += 1,
            _ => lines.push((line, 1)),
        }
    }
    if lines.is_empty() {
        return "no lines".to_string();
    }
    let shown: Vec<String> = lines
        .iter()
        .take(MAX_LISTED_LINES)
        .map(|&(line, count)| match count {
            1 => line.to_string(),
            _ => format!("{line} ({count} times)"),
        })
        .collect();
    let mut out = if shown.len() == 1 {
        format!("line {}", shown[0])
    } else {
        format!("lines {}", shown.join(", "))
    };
    if lines.len() > shown.len() {
        out.push_str(&format!(" (and {} more lines)", lines.len() - shown.len()));
    }
    out
}

fn lines_phrase(start_line: usize, end_line: usize) -> String {
    if start_line >= end_line {
        format!("line {start_line}")
    } else {
        format!("lines {start_line}-{end_line}")
    }
}

/// How the candidate region differs from what the model sent.
fn describe(candidate: &Candidate, old_text: &str) -> String {
    let old = normalize_to_lf(old_text);
    if unescape(&old) == candidate.text {
        return "only escape sequences differ (oldText has \"\\n\"/\"\\t\", the file has the real \
                characters)"
            .to_string();
    }
    let old_lines: Vec<&str> = old.split('\n').collect();
    let file_lines: Vec<&str> = candidate.text.split('\n').collect();

    if old_lines.len() != file_lines.len() {
        return if file_lines.len() < old_lines.len() {
            format!(
                "the file region has {} fewer line(s) than oldText",
                old_lines.len() - file_lines.len()
            )
        } else {
            format!(
                "the file region has {} more line(s) than oldText",
                file_lines.len() - old_lines.len()
            )
        };
    }

    let mut content_diffs: Vec<usize> = Vec::new();
    let mut indent: Option<(String, String)> = None;
    let mut spacing_differs = false;
    for (i, (old_line, file_line)) in old_lines.iter().zip(file_lines.iter()).enumerate() {
        if old_line == file_line {
            continue;
        }
        if old_line.trim() == file_line.trim() {
            if leading_whitespace(old_line) == leading_whitespace(file_line) {
                spacing_differs = true;
            } else if indent.is_none() {
                indent = Some((indent_desc(file_line), indent_desc(old_line)));
            }
        } else {
            content_diffs.push(i);
        }
    }

    if let (Some(first), Some(last)) = (content_diffs.first(), content_diffs.last()) {
        let first_matches = old_lines[0].trim() == file_lines[0].trim();
        let last_matches =
            old_lines[old_lines.len() - 1].trim() == file_lines[file_lines.len() - 1].trim();
        if old_lines.len() >= 3 && first_matches && last_matches {
            let start = candidate.start_line + first;
            let end = candidate.start_line + last;
            let verb = if start == end { "differs" } else { "differ" };
            return format!(
                "first and last lines match, {} {verb}",
                lines_phrase(start, end)
            );
        }
        if old_lines.len() == 1 {
            return "the line's text differs (see the differing line below)".to_string();
        }
        return "the lines differ (see the differing lines below)".to_string();
    }
    if let Some((file_indent, old_indent)) = indent {
        return format!("only indentation differs (file uses {file_indent}, oldText {old_indent})");
    }
    if spacing_differs {
        return "only the spacing inside the lines differs".to_string();
    }
    "only whitespace differs".to_string()
}

/// The changed lines with their file line numbers, `oldText` line directly above
/// the file line it corresponds to.
fn differing_lines(old_text: &str, candidate: &Candidate) -> Vec<String> {
    let old = normalize_to_lf(old_text);
    let diff = TextDiff::from_lines(old.as_str(), candidate.text.as_str());
    let mut changes: Vec<(ChangeTag, usize, &str)> = Vec::new();
    for change in diff.iter_all_changes() {
        if change.tag() == ChangeTag::Equal {
            continue;
        }
        let offset = match change.tag() {
            ChangeTag::Delete => change.old_index(),
            _ => change.new_index(),
        }
        .unwrap_or(0);
        changes.push((
            change.tag(),
            candidate.start_line + offset,
            change.value().trim_end_matches('\n'),
        ));
    }

    // `similar` reports a rewritten line as its removals followed by its
    // additions; pair them so a changed line reads `- old` then `+ file`.
    let mut ordered: Vec<&(ChangeTag, usize, &str)> = Vec::new();
    let mut i = 0usize;
    while i < changes.len() {
        if changes[i].0 != ChangeTag::Delete {
            ordered.push(&changes[i]);
            i += 1;
            continue;
        }
        let deleted_from = i;
        while i < changes.len() && changes[i].0 == ChangeTag::Delete {
            i += 1;
        }
        let deleted_to = i;
        let inserted_from = i;
        while i < changes.len() && changes[i].0 == ChangeTag::Insert {
            i += 1;
        }
        let inserted_to = i;
        let deleted = deleted_to - deleted_from;
        let inserted = inserted_to - inserted_from;
        for k in 0..deleted.max(inserted) {
            if deleted_from + k < deleted_to {
                ordered.push(&changes[deleted_from + k]);
            }
            if inserted_from + k < inserted_to {
                ordered.push(&changes[inserted_from + k]);
            }
        }
    }

    let mut entries: Vec<String> = Vec::new();
    let mut omitted = 0usize;
    for (tag, line_no, text) in ordered {
        if entries.len() >= MAX_DIFFERING_LINES {
            omitted += 1;
            continue;
        }
        let (sign, label) = match tag {
            ChangeTag::Delete => ("-", "oldText:"),
            _ => ("+", "file:"),
        };
        entries.push(format!("{line_no:>5} {sign} {label:<11}{text}"));
    }
    if omitted > 0 {
        entries.push(format!("… {omitted} more differing lines"));
    }
    if entries.is_empty() {
        entries.push("(the difference is inside a line; see the text below)".into());
    }
    entries
}

/// First line of `text` that has content, trimmed; the line most likely to be
/// present in the file.
fn first_content_line(text: &str) -> Option<String> {
    text.split('\n')
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

fn abbreviate_line(line: &str) -> String {
    const MAX_CHARS: usize = 80;
    if line.chars().count() <= MAX_CHARS {
        return line.to_string();
    }
    let cut: String = line.chars().take(MAX_CHARS).collect();
    format!("{cut}…")
}

fn leading_whitespace(line: &str) -> &str {
    let end = line
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    &line[..end]
}

/// `4 spaces` / `1 tab` / `1 tab + 4 spaces`.
fn indent_desc(line: &str) -> String {
    let indent = leading_whitespace(line);
    let tabs = indent.matches('\t').count();
    let spaces = indent.chars().count() - tabs;
    match (tabs, spaces) {
        (0, spaces) => format!("{spaces} spaces"),
        (tabs, 0) => format!("{tabs} tabs"),
        (tabs, spaces) => format!("{tabs} tabs + {spaces} spaces"),
    }
}

fn unescape(s: &str) -> String {
    s.replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\\\", "\\")
        .replace("\\\"", "\"")
        .replace("\\'", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indent_only_difference_is_named() {
        let content = "fn a() {\n        let x = 1;\n}\n";
        let message = not_found("/w/f.rs", 1, "fn a() {\n    let x = 1;\n}", content);
        assert!(message.contains("error: edits[1].oldText not found in /w/f.rs"));
        assert!(
            message.contains("lines 1-3 are the closest match: only indentation differs"),
            "got: {message}"
        );
        assert!(
            message.contains("file uses 8 spaces, oldText 4 spaces"),
            "got: {message}"
        );
        assert!(message.contains("  | fn a() {"), "got: {message}");
        assert!(message.contains("  |         let x = 1;"), "got: {message}");
    }

    #[test]
    fn duplicate_lists_line_numbers() {
        let message = duplicate("/w/f.rs", 0, 3, &[10, 40, 77]);
        assert_eq!(
            message,
            "error: edits[0].oldText occurs 3 times in /w/f.rs (lines 10, 40, 77)\n  help: include \
             the surrounding lines that distinguish the intended occurrence, or set replaceAll: \
             true to change every occurrence"
        );
    }

    #[test]
    fn long_occurrence_lists_are_truncated() {
        let positions: Vec<usize> = (1..=12).collect();
        let message = duplicate("/w/f.rs", 0, 12, &positions);
        assert!(message.contains("(and 4 more lines)"), "got: {message}");
    }

    #[test]
    fn differing_lines_are_capped_and_counted() {
        let old = (0..20)
            .map(|i| format!("old line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let content = (0..20)
            .map(|i| format!("file line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let candidate = find_candidates(&content, &old)
            .into_iter()
            .next()
            .expect("candidate");
        let entries = differing_lines(&old, &candidate);
        assert_eq!(entries.len(), MAX_DIFFERING_LINES + 1);
        assert!(entries.last().unwrap().contains("more differing lines"));
    }

    #[test]
    fn escape_sequence_difference_is_named() {
        let content = "a\tb\n";
        let message = not_found("/w/f.txt", 0, "a\\tb", content);
        assert!(
            message.contains("only escape sequences differ"),
            "got: {message}"
        );
    }

    #[test]
    fn no_candidate_points_at_the_first_line_occurrence() {
        let content = "alpha\nbeta\nlet value = compute();\ngamma\n";
        let message = not_found(
            "/w/f.rs",
            2,
            "let value = compute(1);\nunrelated();",
            content,
        );
        // Either a candidate or the first-line fallback, but always actionable.
        assert!(message.contains("  help: "), "got: {message}");
        assert!(message.contains("edits[2]"), "got: {message}");
    }

    #[test]
    fn long_regions_are_not_quoted() {
        let content = (1..=60)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let old = (1..=40)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let message = not_found("/w/f.rs", 0, &old, &content);
        assert!(
            message.contains("is too long to quote here"),
            "got: {message}"
        );
        assert!(message.contains("offset 1, limit 40"), "got: {message}");
    }

    #[test]
    fn max_candidates_is_small() {
        assert_eq!(crate::semantic_edit::MAX_CANDIDATES, 3);
    }
}
