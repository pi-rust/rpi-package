//! Pre-flight + apply for the `edit` tool.
//!
//! Copied from `rpi` (`crates/rpi-tools/src/tools/edit_apply.rs`, tag
//! `v0.9.4-aia.2`, MIT — see `NOTICE`); the only adaptations are the
//! crate-internal `use` paths and [`FailureCode`] replacing the host's
//! `FileErrorCode`, because a plugin reports failure text instead of a host
//! error.
//!
//! Every `edits[i]` is matched against the **original** file content (never
//! incrementally). A text counts as matched when it occurs verbatim, or when
//! [`normalize_for_fuzzy_match`] makes both sides equal — the same two-step
//! contract as upstream Pi's `applyEditsToNormalizedContent`
//! ([`crate::edit_diff`]), which only equalizes trailing whitespace and
//! typographic quotes/dashes/space characters. Nothing else is fuzziness: a text
//! with different indentation, different escapes or different middle lines does
//! not match, and an ambiguous text is never resolved by picking one occurrence.
//!
//! All failures are collected before anything is written, so the model gets the
//! whole list in one round trip ([`crate::edit_diagnostics`]) and the file
//! is left untouched. On success the result carries
//!
//! * a `note:` per edit that only matched after normalization (a fact about the
//!   match, so the model knows its `oldText` was not literal);
//! * a `warning:` per edit whose replacement changes the net bracket balance
//!   (`{}`, `()`, `[]`), compared before/after within that edit only. A balanced
//!   edit — however unusual — produces no warning at all.

use crate::edit_diagnostics as diag;
use crate::edit_diff::{
    fuzzy_find_text, get_line_spans, normalize_for_fuzzy_match, normalize_to_lf,
    split_lines_with_endings,
};
use crate::semantic_edit::lines_phrase;

/// Coarse failure class, the plugin's stand-in for `rpi`'s `FileErrorCode`.
///
/// The fork maps these onto the host's `FileError`; a plugin reports the message
/// itself, so only the not-found/invalid distinction survives — enough for a
/// caller (or a test) to tell "the text is not in this file" from "the request
/// cannot be applied as written".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCode {
    NotFound,
    Invalid,
}

/// One requested replacement. Mirrors the `edits[]` entry of the tool input.
#[derive(Debug, Clone)]
pub struct EditRequest {
    pub old_text: String,
    pub new_text: String,
    /// Replace every occurrence instead of requiring the text to be unique.
    pub replace_all: bool,
}

/// Outcome of a successful (and therefore fully applied) edit call.
#[derive(Debug, Clone)]
pub struct AppliedEdits {
    /// The content the edits were matched against (LF-normalized, BOM stripped).
    pub base_content: String,
    pub new_content: String,
    /// Total spans replaced (`replaceAll` can replace many for one edit).
    pub replacements: usize,
    /// `note:` lines: facts about how the text was matched.
    pub notes: Vec<String>,
    /// `warning:` lines: non-blocking observations about the replacements.
    pub warnings: Vec<String>,
}

/// Every failing edit, already rendered as model-facing text.
#[derive(Debug)]
pub struct ApplyFailure {
    pub code: FailureCode,
    pub message: String,
}

impl ApplyFailure {
    fn new(failures: Vec<Failure>) -> Self {
        let code = if failures.iter().any(|f| f.code == FailureCode::NotFound) {
            FailureCode::NotFound
        } else {
            FailureCode::Invalid
        };
        Self {
            code,
            message: diag::join(failures.into_iter().map(|f| (f.index, f.message)).collect()),
        }
    }
}

struct Failure {
    index: usize,
    code: FailureCode,
    message: String,
}

/// A resolved edit: the text as it exists in the write base, every span it
/// covers there, and the replacement.
struct Plan {
    /// Position in the `edits[]` array.
    index: usize,
    /// The matched text, taken from the base content.
    needle: String,
    /// Byte spans of every occurrence (ascending, disjoint).
    spans: Vec<(usize, usize)>,
    new_text: String,
    /// Matched only after normalization, i.e. `oldText` was not literal.
    normalized_match: bool,
}

/// Match every edit against `content`, then apply them all or nothing.
pub fn apply_edits(
    content: &str,
    edits: &[EditRequest],
    path: &str,
) -> Result<AppliedEdits, ApplyFailure> {
    let normalized: Vec<(String, String)> = edits
        .iter()
        .map(|e| (normalize_to_lf(&e.old_text), normalize_to_lf(&e.new_text)))
        .collect();

    let mut failures: Vec<Failure> = Vec::new();
    for (index, (old, new)) in normalized.iter().enumerate() {
        if old.is_empty() {
            failures.push(Failure {
                index,
                code: FailureCode::Invalid,
                message: diag::empty_old_text(path, index),
            });
        } else if old == new {
            failures.push(Failure {
                index,
                code: FailureCode::Invalid,
                message: diag::no_op(path, index),
            });
        }
    }
    // An edit that already failed validation is not matched, but the edits after
    // it still are: every problem is reported in one round trip.
    let rejected = |index: usize| failures.iter().any(|f| f.index == index);

    // Probing the raw content decides whether the normalization layer is in play
    // at all. Upstream Pi then matches (and splices) against the normalized
    // content, keeping untouched lines byte-identical.
    let probes: Vec<Option<bool>> = normalized
        .iter()
        .enumerate()
        .map(|(index, (old, _))| {
            (!rejected(index)).then(|| fuzzy_find_text(content, old).used_fuzzy_match)
        })
        .collect();
    let any_normalized = probes.contains(&Some(true));
    let base = if any_normalized {
        normalize_for_fuzzy_match(content)
    } else {
        content.to_string()
    };

    let mut plans: Vec<Plan> = Vec::new();
    for (index, (old, new)) in normalized.iter().enumerate() {
        let Some(normalized_match) = probes[index] else {
            continue;
        };
        let matched = fuzzy_find_text(&base, old);
        if !matched.found {
            failures.push(Failure {
                index,
                code: FailureCode::NotFound,
                message: diag::not_found(path, index, old, content),
            });
            continue;
        }
        let needle = base[matched.index..matched.index + matched.match_length].to_string();
        let spans = find_all_spans(&base, &needle);
        if spans.is_empty() {
            failures.push(Failure {
                index,
                code: FailureCode::NotFound,
                message: diag::not_found(path, index, old, content),
            });
            continue;
        }
        if !edits[index].replace_all && spans.len() > 1 {
            let positions: Vec<usize> = spans
                .iter()
                .map(|(start, _)| line_of(&base, *start))
                .collect();
            failures.push(Failure {
                index,
                code: FailureCode::Invalid,
                message: diag::duplicate(path, index, spans.len(), &positions),
            });
            continue;
        }
        plans.push(Plan {
            index,
            needle,
            spans,
            new_text: new.clone(),
            normalized_match,
        });
    }

    if let Some(failure) = overlap_failure(&base, &plans, path) {
        failures.push(failure);
    }

    if !failures.is_empty() {
        return Err(ApplyFailure::new(failures));
    }

    let replacements: Vec<Replacement> = plans
        .iter()
        .flat_map(|plan| {
            plan.spans.iter().map(move |(start, end)| Replacement {
                start: *start,
                end: *end,
                text: plan.new_text.clone(),
            })
        })
        .collect();
    let new_content = if any_normalized {
        splice_preserving_untouched_lines(content, &base, &replacements)
    } else {
        splice(&base, &replacements)
    };

    if base == new_content {
        return Err(ApplyFailure::new(vec![Failure {
            index: 0,
            code: FailureCode::Invalid,
            message: diag::no_op(path, 0),
        }]));
    }

    let notes = plans
        .iter()
        .filter(|plan| plan.normalized_match)
        .map(|plan| {
            let (start_line, end_line) = span_line_range(&base, &plan.spans);
            format!(
                "note: edits[{}] matched after normalizing trailing whitespace/quotes ({})",
                plan.index,
                lines_phrase(start_line, end_line)
            )
        })
        .collect::<Vec<_>>();
    let warnings = brace_warnings(&base, &plans);

    Ok(AppliedEdits {
        base_content: content.to_string(),
        new_content,
        replacements: replacements.len(),
        notes,
        warnings,
    })
}

/// Report the first two edits whose resolved spans overlap. `replaceAll` spans
/// of one edit never overlap each other, so only cross-edit pairs matter.
fn overlap_failure(base: &str, plans: &[Plan], path: &str) -> Option<Failure> {
    let mut spans: Vec<(usize, usize, usize)> = plans
        .iter()
        .enumerate()
        .flat_map(|(plan_index, plan)| {
            plan.spans
                .iter()
                .map(move |(start, end)| (plan_index, *start, *end))
        })
        .collect();
    spans.sort_by_key(|(_, start, _)| *start);
    for pair in spans.windows(2) {
        let (a_plan, a_start, a_end) = pair[0];
        let (b_plan, b_start, b_end) = pair[1];
        if a_plan == b_plan || a_end <= b_start {
            continue;
        }
        let a_index = plans[a_plan].index;
        let b_index = plans[b_plan].index;
        return Some(Failure {
            index: a_index.min(b_index),
            code: FailureCode::Invalid,
            message: diag::overlap(
                path,
                (a_index, line_of(base, a_start), line_of(base, a_end - 1)),
                (b_index, line_of(base, b_start), line_of(base, b_end - 1)),
            ),
        });
    }
    None
}

/// `warning: edits[2] changes brace balance by -1 '}' (lines 30-34)` for every
/// edit whose replacement shifts the net balance of `{}`, `()` or `[]`.
fn brace_warnings(base: &str, plans: &[Plan]) -> Vec<String> {
    let new_ranges = new_line_ranges(base, plans);
    let mut warnings: Vec<String> = Vec::new();
    for (plan_index, plan) in plans.iter().enumerate() {
        let old_balance = bracket_balance(&plan.needle);
        let new_balance = bracket_balance(&plan.new_text);
        for (kind, (open, close)) in BRACKETS.iter().enumerate() {
            let delta = plan.spans.len() as i64 * (new_balance[kind] - old_balance[kind]);
            if delta == 0 {
                continue;
            }
            let bracket = if delta > 0 { *open } else { *close };
            let ranges = &new_ranges[plan_index];
            let lines = match ranges.len() {
                0 => String::from("no lines"),
                1 => lines_phrase(ranges[0].0, ranges[0].1),
                _ => format!(
                    "{}, and {} more",
                    lines_phrase(ranges[0].0, ranges[0].1),
                    ranges.len() - 1
                ),
            };
            warnings.push(format!(
                "warning: edits[{}] changes brace balance by {delta:+} '{bracket}' ({lines})",
                plan.index
            ));
        }
    }
    warnings
}

const BRACKETS: [(char, char); 3] = [('{', '}'), ('(', ')'), ('[', ']')];

/// `opens - closes` per bracket kind.
fn bracket_balance(text: &str) -> [i64; 3] {
    let mut balance = [0i64; 3];
    for ch in text.chars() {
        for (kind, (open, close)) in BRACKETS.iter().enumerate() {
            if ch == *open {
                balance[kind] += 1;
            }
            if ch == *close {
                balance[kind] -= 1;
            }
        }
    }
    balance
}

/// Line ranges (in the new content) covered by every occurrence of every plan.
fn new_line_ranges(base: &str, plans: &[Plan]) -> Vec<Vec<(usize, usize)>> {
    let mut spans: Vec<(usize, usize)> = plans
        .iter()
        .enumerate()
        .flat_map(|(plan_index, plan)| {
            plan.spans
                .iter()
                .map(move |(start, _)| (plan_index, *start))
        })
        .collect();
    spans.sort_by_key(|(_, start)| *start);
    let mut ranges = vec![Vec::new(); plans.len()];
    let mut line_delta: i64 = 0;
    for (plan_index, start) in spans {
        let start_line = line_of(base, start) as i64 + line_delta;
        let new_lines = plans[plan_index].new_text.matches('\n').count() as i64;
        ranges[plan_index].push((start_line as usize, (start_line + new_lines) as usize));
        line_delta += new_lines - plans[plan_index].needle.matches('\n').count() as i64;
    }
    ranges
}

fn span_line_range(base: &str, spans: &[(usize, usize)]) -> (usize, usize) {
    let start = line_of(base, spans[0].0);
    let last_byte = spans[spans.len() - 1].1.saturating_sub(1);
    (start, line_of(base, last_byte))
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].matches('\n').count() + 1
}

/// Byte spans of every occurrence of `needle` (overlapping included).
fn find_all_spans(content: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut from = 0usize;
    while from <= content.len() {
        let Some(idx) = content[from..].find(needle).map(|i| from + i) else {
            break;
        };
        spans.push((idx, idx + needle.len()));
        from = idx + needle.len();
    }
    spans
}

struct Replacement {
    start: usize,
    end: usize,
    text: String,
}

/// Splice every replacement in ascending order.
fn splice(base: &str, replacements: &[Replacement]) -> String {
    let mut sorted: Vec<&Replacement> = replacements.iter().collect();
    sorted.sort_by_key(|r| r.start);
    let mut out = String::with_capacity(base.len());
    let mut cursor = 0usize;
    for replacement in sorted {
        out.push_str(&base[cursor..replacement.start]);
        out.push_str(&replacement.text);
        cursor = replacement.end;
    }
    out.push_str(&base[cursor..]);
    out
}

/// Splice into the normalized base but copy untouched lines from `original`, the
/// way upstream `applyReplacementsPreservingUnchangedLines` does: normalizing the
/// base must not rewrite lines the edits did not touch.
fn splice_preserving_untouched_lines(
    original: &str,
    base: &str,
    replacements: &[Replacement],
) -> String {
    let original_lines = split_lines_with_endings(original);
    let base_lines = get_line_spans(base);
    if original_lines.len() != base_lines.len() {
        return splice(base, replacements);
    }

    let mut sorted: Vec<&Replacement> = replacements.iter().collect();
    sorted.sort_by_key(|r| r.start);
    let mut groups: Vec<(usize, usize, Vec<&Replacement>)> = Vec::new();
    for replacement in sorted {
        let (start_line, end_line) =
            replacement_line_range(&base_lines, replacement.start, replacement.end);
        if let Some(last) = groups.last_mut() {
            if start_line < last.1 {
                last.1 = last.1.max(end_line);
                last.2.push(replacement);
                continue;
            }
        }
        groups.push((start_line, end_line, vec![replacement]));
    }

    let mut out = String::with_capacity(original.len());
    let mut copied = 0usize;
    for (start_line, end_line, group) in &groups {
        out.push_str(&original_lines[copied..*start_line].join(""));
        let group_start = base_lines[*start_line].0;
        let group_end = base_lines[*end_line - 1].1;
        let local: Vec<Replacement> = group
            .iter()
            .map(|r| Replacement {
                start: r.start - group_start,
                end: r.end - group_start,
                text: r.text.clone(),
            })
            .collect();
        out.push_str(&splice(&base[group_start..group_end], &local));
        copied = *end_line;
    }
    out.push_str(&original_lines[copied..].join(""));
    out
}

/// Line range covering `[start, end)`, `end_line` exclusive. Mirrors
/// `getReplacementLineRange` (upstream `edit-diff.ts`).
fn replacement_line_range(lines: &[(usize, usize)], start: usize, end: usize) -> (usize, usize) {
    let mut start_line = 0usize;
    for (i, span) in lines.iter().enumerate() {
        if start >= span.0 && start < span.1 {
            start_line = i;
            break;
        }
        start_line = i + 1;
    }
    let mut end_line = start_line + 1;
    while end_line < lines.len() && lines[end_line - 1].1 < end {
        end_line += 1;
    }
    (start_line, end_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(old: &str, new: &str) -> EditRequest {
        EditRequest {
            old_text: old.to_string(),
            new_text: new.to_string(),
            replace_all: false,
        }
    }

    fn req_all(old: &str, new: &str) -> EditRequest {
        EditRequest {
            replace_all: true,
            ..req(old, new)
        }
    }

    #[test]
    fn exact_edit_replaces_and_reports_no_notes() {
        let outcome = apply_edits("a\nb\n", &[req("b", "B")], "/f").expect("ok");
        assert_eq!(outcome.new_content, "a\nB\n");
        assert_eq!(outcome.replacements, 1);
        assert!(outcome.notes.is_empty());
        assert!(outcome.warnings.is_empty());
    }

    #[test]
    fn replace_all_replaces_every_occurrence() {
        let outcome = apply_edits("foo\nbar\nfoo\n", &[req_all("foo", "baz")], "/f").expect("ok");
        assert_eq!(outcome.new_content, "baz\nbar\nbaz\n");
        assert_eq!(outcome.replacements, 2);
    }

    #[test]
    fn multiple_occurrences_without_replace_all_fail() {
        let err = apply_edits("foo\nbar\nfoo\n", &[req("foo", "baz")], "/f").expect_err("dup");
        assert_eq!(err.code, FailureCode::Invalid);
        assert!(
            err.message
                .contains("error: edits[0].oldText occurs 2 times in /f (lines 1, 3)"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn normalized_match_is_noted() {
        let outcome = apply_edits(
            "it\u{2019}s fine\n",
            &[req("it's fine", "it is fine")],
            "/f",
        )
        .expect("ok");
        assert_eq!(outcome.new_content, "it is fine\n");
        assert_eq!(outcome.notes.len(), 1);
        assert!(
            outcome.notes[0].contains("matched after normalizing"),
            "got: {:?}",
            outcome.notes
        );
    }

    #[test]
    fn indentation_difference_does_not_match() {
        let err = apply_edits(
            "fn a() {\n        let x = 1;\n}\n",
            &[req("fn a() {\n    let x = 1;\n}", "y")],
            "/f",
        )
        .expect_err("indentation must not be tolerated");
        assert_eq!(err.code, FailureCode::NotFound);
        assert!(
            err.message.contains("only indentation differs"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn brace_balance_change_warns() {
        let outcome = apply_edits(
            "fn a() {\n    body();\n}\n",
            &[req("fn a() {\n    body();\n}", "body();\n}")],
            "/f",
        )
        .expect("ok");
        assert_eq!(outcome.warnings.len(), 1);
        assert!(
            outcome.warnings[0].contains("changes brace balance by -1 '}'"),
            "got: {:?}",
            outcome.warnings
        );
    }

    #[test]
    fn normalization_does_not_rewrite_untouched_lines() {
        let outcome = apply_edits(
            "it\u{2019}s a file  \nkeep   \n",
            &[req("it's a file", "it is a file")],
            "/f",
        )
        .expect("ok");
        // The matched line is rewritten; the untouched one keeps its trailing
        // spaces even though the base content was normalized.
        assert_eq!(outcome.new_content, "it is a file\nkeep   \n");
    }

    #[test]
    fn normalized_replace_all_rewrites_only_the_matched_lines() {
        let outcome = apply_edits(
            "it\u{2019}s A  \nx\nit\u{2019}s A  \n",
            &[req_all("it's A", "ok")],
            "/f",
        )
        .expect("ok");
        assert_eq!(outcome.new_content, "ok\nx\nok\n");
        assert_eq!(outcome.replacements, 2);
    }

    #[test]
    fn balanced_edit_produces_no_warning() {
        let outcome = apply_edits(
            "let a = 1;\n",
            &[req("let a = 1;", "let a = compute(1, 2);")],
            "/f",
        )
        .expect("ok");
        assert!(outcome.warnings.is_empty(), "got: {:?}", outcome.warnings);
    }

    #[test]
    fn overlapping_edits_are_reported_with_line_ranges() {
        let err = apply_edits(
            "one\ntwo\nthree\n",
            &[req("one\ntwo\n", "A"), req("two\nthree\n", "B")],
            "/f",
        )
        .expect_err("overlap");
        assert!(
            err.message
                .contains("edits[0] (lines 1-2) and edits[1] (lines 2-3) overlap in /f"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn a_validation_failure_does_not_hide_later_failures() {
        let err = apply_edits("alpha\nbeta\n", &[req("", "x"), req("nope", "y")], "/f")
            .expect_err("both fail");
        assert!(
            err.message
                .contains("error: edits[0].oldText is empty in /f"),
            "got: {}",
            err.message
        );
        assert!(
            err.message
                .contains("error: edits[1].oldText not found in /f"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn edits_are_matched_against_the_original_not_incrementally() {
        let outcome = apply_edits("a\nb\n", &[req("a", "x\nb"), req("b", "c")], "/f");
        // The second edit's "b" is the original one; the first edit introduces a
        // second "b", so both are matched against the original snapshot and
        // replaced in one pass.
        let outcome = outcome.expect("ok");
        assert_eq!(outcome.new_content, "x\nb\nc\n");
    }
}
