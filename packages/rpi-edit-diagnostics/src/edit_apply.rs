//! Pre-flight + apply for the `edit` tool.
//!
//! Copied from `rpi` (`crates/rpi-tools/src/tools/edit_apply.rs`, tag
//! `v0.9.4-aia.2`, MIT — see `NOTICE`). Adaptations: the crate-internal `use`
//! paths, [`FailureCode`] replacing the host's `FileErrorCode` (a plugin reports
//! failure text instead of a host error), and splicing only the matched bytes
//! instead of rebuilding normalized lines.
//!
//! Every `edits[i]` is matched against the **original** file content (never
//! incrementally), and only the bytes each edit matched are replaced: a match
//! found after normalization is mapped back onto the original bytes, so another
//! edit's line and the text around a match keep their trailing whitespace and
//! characters. A text counts as matched when it occurs verbatim, or when
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
use crate::edit_diff::{fuzzy_find_text, normalize_for_fuzzy_match, normalize_to_lf};
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

/// A resolved edit: the text it matched, every span it covers in the original
/// content, and the replacement.
struct Plan {
    /// Position in the `edits[]` array.
    index: usize,
    /// The matched text (normalized when `normalized_match`).
    needle: String,
    /// Every occurrence in the original content (ascending, disjoint).
    spans: Vec<Replacement>,
    new_text: String,
    match_count: usize,
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

    // Probing the raw content decides, per edit, whether its text is literal or
    // only matches after normalization; the normalized view is built only then.
    let probes: Vec<Option<bool>> = normalized
        .iter()
        .enumerate()
        .map(|(index, (old, _))| {
            (!rejected(index)).then(|| fuzzy_find_text(content, old).used_fuzzy_match)
        })
        .collect();
    let view = probes
        .contains(&Some(true))
        .then(|| NormalizedView::new(content));

    let mut plans: Vec<Plan> = Vec::new();
    for (index, (old, new)) in normalized.iter().enumerate() {
        let Some(normalized_match) = probes[index] else {
            continue;
        };
        let normalized_view = view.as_ref().filter(|_| normalized_match);
        let (needle, matches) = match normalized_view {
            None => (old.clone(), find_all_spans(content, old)),
            Some(view) => {
                let matched = fuzzy_find_text(&view.text, old);
                if !matched.found {
                    failures.push(Failure {
                        index,
                        code: FailureCode::NotFound,
                        message: diag::not_found(path, index, old, content),
                    });
                    continue;
                }
                let needle =
                    view.text[matched.index..matched.index + matched.match_length].to_string();
                let matches = find_all_spans(&view.text, &needle);
                (needle, matches)
            }
        };
        if matches.is_empty() {
            failures.push(Failure {
                index,
                code: FailureCode::NotFound,
                message: diag::not_found(path, index, old, content),
            });
            continue;
        }
        if !edits[index].replace_all && matches.len() > 1 {
            let positions: Vec<usize> = matches
                .iter()
                .map(|&(start, _)| match normalized_view {
                    Some(view) => view.positions[start].line + 1,
                    None => line_of(content, start),
                })
                .collect();
            failures.push(Failure {
                index,
                code: FailureCode::Invalid,
                message: diag::duplicate(path, index, matches.len(), &positions),
            });
            continue;
        }
        let match_count = matches.len();
        let spans = match normalized_view {
            Some(view) => view.original_replacements(content.len(), &matches, new),
            None => matches
                .into_iter()
                .map(|(start, end)| Replacement {
                    start,
                    end,
                    text: new.clone(),
                })
                .collect(),
        };
        if spans
            .iter()
            .all(|span| content[span.start..span.end] == span.text)
        {
            failures.push(Failure {
                index,
                code: FailureCode::Invalid,
                message: diag::no_op(path, index),
            });
            continue;
        }
        plans.push(Plan {
            index,
            needle,
            spans,
            new_text: new.clone(),
            match_count,
            normalized_match,
        });
    }

    if let Some(failure) = overlap_failure(content, &plans, path) {
        failures.push(failure);
    }

    if !failures.is_empty() {
        return Err(ApplyFailure::new(failures));
    }

    let mut replacements: Vec<&Replacement> = plans.iter().flat_map(|plan| &plan.spans).collect();
    replacements.sort_by_key(|span| span.start);
    let new_content = splice(content, &replacements)?;

    if content == new_content {
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
            let (start_line, end_line) = span_line_range(content, &plan.spans);
            format!(
                "note: edits[{}] matched after normalizing trailing whitespace/quotes ({})",
                plan.index,
                lines_phrase(start_line, end_line)
            )
        })
        .collect::<Vec<_>>();
    let warnings = brace_warnings(content, &plans);

    Ok(AppliedEdits {
        base_content: content.to_string(),
        new_content,
        replacements: plans.iter().map(|plan| plan.match_count).sum(),
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
                .map(move |span| (plan_index, span.start, span.end))
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
            let delta = plan.match_count as i64 * (new_balance[kind] - old_balance[kind]);
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
    let mut spans: Vec<(usize, &Replacement)> = plans
        .iter()
        .enumerate()
        .flat_map(|(plan_index, plan)| plan.spans.iter().map(move |span| (plan_index, span)))
        .collect();
    spans.sort_by_key(|(_, span)| span.start);
    let mut ranges = vec![Vec::new(); plans.len()];
    let mut line_delta: i64 = 0;
    let mut cursor = 0;
    let mut base_line = 1;
    for (plan_index, span) in spans {
        base_line += base[cursor..span.start].matches('\n').count() as i64;
        cursor = span.start;
        let start_line = base_line + line_delta;
        let new_lines = span.text.matches('\n').count() as i64;
        ranges[plan_index].push((start_line as usize, (start_line + new_lines) as usize));
        line_delta += new_lines - base[span.start..span.end].matches('\n').count() as i64;
    }
    ranges
}

fn span_line_range(base: &str, spans: &[Replacement]) -> (usize, usize) {
    let start = line_of(base, spans[0].start);
    let last_byte = spans[spans.len() - 1].end.saturating_sub(1);
    (start, line_of(base, last_byte))
}

/// 1-based line of a byte offset; the offset need not be a char boundary.
fn line_of(text: &str, offset: usize) -> usize {
    let bytes = &text.as_bytes()[..offset.min(text.len())];
    bytes.iter().filter(|&&byte| byte == b'\n').count() + 1
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

/// Splice ascending, disjoint replacements; reject invalid spans before slicing.
fn splice(base: &str, replacements: &[&Replacement]) -> Result<String, ApplyFailure> {
    let mut out = String::with_capacity(base.len());
    let mut cursor = 0usize;
    for replacement in replacements {
        let gap = base.get(cursor..replacement.start);
        if replacement.start > replacement.end
            || base.get(replacement.start..replacement.end).is_none()
            || gap.is_none()
        {
            return Err(ApplyFailure {
                code: FailureCode::Invalid,
                message:
                    "error: invalid replacement spans: overlapping, unordered, or out of bounds"
                        .into(),
            });
        }
        out.push_str(gap.unwrap_or_default());
        out.push_str(&replacement.text);
        cursor = replacement.end;
    }
    out.push_str(&base[cursor..]);
    Ok(out)
}

/// [`normalize_for_fuzzy_match`] of the original content, plus the original
/// byte range each normalized char came from, so a match found in normalized
/// text replaces exactly the original bytes it covers.
struct NormalizedView {
    text: String,
    /// One `(start, end)` per char of `text`; `None` when normalizing char by
    /// char does not reproduce `text` (NFKC composed across chars).
    sources: Option<Vec<(usize, usize)>>,
    /// Constant-time normalized byte -> source-char index and line lookup.
    positions: Vec<NormalizedPosition>,
    line_starts: Vec<usize>,
    original_line_starts: Vec<usize>,
}

#[derive(Clone, Copy)]
struct NormalizedPosition {
    source_index: usize,
    line: usize,
}

struct MappedSpan {
    start: usize,
    end: usize,
    normalized_start: usize,
    normalized_end: usize,
}

impl NormalizedView {
    fn new(content: &str) -> Self {
        use unicode_normalization::UnicodeNormalization;
        let text = normalize_for_fuzzy_match(content);
        let mut chars: Vec<(char, usize, usize)> = Vec::with_capacity(content.len());
        // Trailing whitespace of the current line, dropped at its end like `trim_end`.
        let mut pending: Vec<(char, usize, usize)> = Vec::new();
        for (start, c) in content.char_indices() {
            let end = start + c.len_utf8();
            for n in c.to_string().nfkc() {
                let n = match n {
                    '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
                    '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
                    '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
                    | '\u{2212}' => '-',
                    other => other,
                };
                if n == '\n' {
                    pending.clear();
                    chars.push((n, start, end));
                } else if n.is_whitespace() {
                    pending.push((n, start, end));
                } else {
                    chars.append(&mut pending);
                    chars.push((n, start, end));
                }
            }
        }
        let rebuilt: String = chars.iter().map(|(c, _, _)| *c).collect();
        let sources = (rebuilt == text).then(|| {
            chars
                .into_iter()
                .map(|(_, start, end)| (start, end))
                .collect()
        });
        let mut positions = Vec::with_capacity(text.len() + 1);
        let mut line_starts = vec![0];
        for (source_index, (byte, ch)) in text.char_indices().enumerate() {
            positions.extend(
                std::iter::repeat(NormalizedPosition {
                    source_index,
                    line: line_starts.len() - 1,
                })
                .take(ch.len_utf8()),
            );
            if ch == '\n' {
                line_starts.push(byte + 1);
            }
        }
        positions.push(NormalizedPosition {
            source_index: text.chars().count(),
            line: line_starts.len() - 1,
        });
        let original_line_starts = std::iter::once(0)
            .chain(content.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        Self {
            text,
            sources,
            positions,
            line_starts,
            original_line_starts,
        }
    }

    /// The original-content replacement for the normalized span `[start, end)`.
    /// A span that begins or ends inside one char's expansion (e.g. half of
    /// `ﬁ`), or a view without sources, rewrites the span's whole lines from the
    /// normalized text instead, as upstream Pi does.
    fn original_span(&self, content_len: usize, start: usize, end: usize) -> MappedSpan {
        if let Some(sources) = &self.sources {
            let first = self.positions[start].source_index;
            let last = self.positions[end].source_index - 1;
            let starts_clean = first == 0 || sources[first - 1].0 != sources[first].0;
            let ends_clean = last + 1 == sources.len() || sources[last + 1].0 != sources[last].0;
            if starts_clean && ends_clean {
                return MappedSpan {
                    start: sources[first].0,
                    end: sources[last].1,
                    normalized_start: start,
                    normalized_end: end,
                };
            }
        }
        let first_line = self.positions[start].line;
        let last_line = self.positions[end].line;
        MappedSpan {
            start: self.original_line_starts[first_line],
            end: self
                .original_line_starts
                .get(last_line + 1)
                .map_or(content_len, |next| next - 1),
            normalized_start: self.line_starts[first_line],
            normalized_end: self
                .line_starts
                .get(last_line + 1)
                .map_or(self.text.len(), |next| next - 1),
        }
    }

    /// Merge overlapping source spans before rebuilding, so a fallback line is
    /// normalized only once and includes every match (including clean ones).
    fn original_replacements(
        &self,
        content_len: usize,
        matches: &[(usize, usize)],
        new_text: &str,
    ) -> Vec<Replacement> {
        let mut groups: Vec<(MappedSpan, usize, usize)> = Vec::new();
        for (index, &(start, end)) in matches.iter().enumerate() {
            let mut span = self.original_span(content_len, start, end);
            let mut from = index;
            // A later fallback can extend back over previously clean matches.
            while groups
                .last()
                .is_some_and(|(previous, _, _)| previous.end > span.start)
            {
                let (previous, first, _) = groups.pop().expect("last span");
                span.start = span.start.min(previous.start);
                span.end = span.end.max(previous.end);
                span.normalized_start = span.normalized_start.min(previous.normalized_start);
                span.normalized_end = span.normalized_end.max(previous.normalized_end);
                from = first;
            }
            groups.push((span, from, index + 1));
        }
        let mut replacements = Vec::with_capacity(groups.len());
        for (span, from, to) in groups {
            let mut text = String::new();
            let mut cursor = span.normalized_start;
            for &(start, end) in &matches[from..to] {
                text.push_str(&self.text[cursor..start]);
                text.push_str(new_text);
                cursor = end;
            }
            text.push_str(&self.text[cursor..span.normalized_end]);
            replacements.push(Replacement {
                start: span.start,
                end: span.end,
                text,
            });
        }
        replacements
    }
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
        // Only the matched bytes are replaced: the trailing spaces after the match
        // and the untouched line both stay.
        assert_eq!(outcome.new_content, "it is a file  \nkeep   \n");
    }

    #[test]
    fn normalized_replace_all_rewrites_only_the_matched_lines() {
        let outcome = apply_edits(
            "it\u{2019}s A  \nx\nit\u{2019}s A  \n",
            &[req_all("it's A", "ok")],
            "/f",
        )
        .expect("ok");
        assert_eq!(outcome.new_content, "ok  \nx\nok  \n");
        assert_eq!(outcome.replacements, 2);
    }

    #[test]
    fn a_normalized_edit_leaves_an_exact_edit_line_byte_identical() {
        let outcome = apply_edits(
            "it\u{2019}s here\nfoo  \n",
            &[req("it's here", "its here"), req("foo", "bar")],
            "/f",
        )
        .expect("ok");
        assert_eq!(outcome.new_content, "its here\nbar  \n");
        assert_eq!(outcome.notes.len(), 1, "got: {:?}", outcome.notes);
    }

    #[test]
    fn a_normalized_match_keeps_the_text_around_it_on_its_line() {
        let outcome = apply_edits(
            "say \u{201C}hi\u{201D} \u{2014} ok  \n",
            &[req("\"hi\"", "hello")],
            "/f",
        )
        .expect("ok");
        assert_eq!(outcome.new_content, "say hello \u{2014} ok  \n");
    }

    #[test]
    fn a_match_inside_one_chars_expansion_rewrites_its_line_from_the_normalized_text() {
        // NFKC expands U+FB01 to "fi"; a match ending between f and i cannot be
        // mapped onto whole original chars, so the matched line is rebuilt.
        let outcome =
            apply_edits("de\u{FB01}ne  \nkeep  \n", &[req("def", "DEF")], "/f").expect("ok");
        assert_eq!(outcome.new_content, "DEFine\nkeep  \n");
    }

    #[test]
    fn normalized_replace_all_merges_partial_expansions_on_one_line() {
        let outcome = apply_edits("deﬁne deﬁne\n", &[req_all("def", "DEF")], "/f")
            .expect("overlapping fallback lines must be rebuilt together");
        assert_eq!(outcome.new_content, "DEFine DEFine\n");
        assert_eq!(outcome.replacements, 2);
    }

    #[test]
    fn normalized_replace_all_maps_many_matches_in_a_large_file() {
        let content = "前 deﬁne deﬁne  \nkeep  \n".repeat(4_000);
        let outcome = apply_edits(&content, &[req_all("def", "DEF")], "/f").expect("ok");
        assert_eq!(
            outcome.new_content,
            "前 DEFine DEFine\nkeep  \n".repeat(4_000)
        );
        assert_eq!(outcome.replacements, 8_000);

        // Whole expansions map cleanly, retaining the original trailing spaces.
        let outcome = apply_edits(&content, &[req_all("define", "DEF")], "/f").expect("ok");
        assert_eq!(outcome.new_content, "前 DEF DEF  \nkeep  \n".repeat(4_000));
        assert_eq!(outcome.replacements, 8_000);
    }

    #[test]
    fn normalized_replace_all_merges_clean_matches_before_and_after_a_fallback() {
        let outcome = apply_edits(
            "def def deﬁne def  \nkeep  \n",
            &[req_all("ｄｅｆ", "(")],
            "/f",
        )
        .expect("ok");
        assert_eq!(outcome.new_content, "( ( (ine (\nkeep  \n");
        assert_eq!(outcome.replacements, 4);
        assert_eq!(outcome.warnings.len(), 1);
        assert!(outcome.warnings[0].contains("+4 '(' (line 1)"));
    }

    #[test]
    fn normalized_replace_all_merges_composed_fallback_lines() {
        // Cross-character NFKC composition disables per-character sources.
        let outcome = apply_edits(
            "e\u{301} e\u{301}  \nkeep  \n",
            &[req_all("é", "E\n")],
            "/f",
        )
        .expect("ok");
        assert_eq!(outcome.new_content, "E\n E\n\nkeep  \n");
        assert_eq!(outcome.replacements, 2);
    }

    #[test]
    fn partial_expansion_without_replace_all_still_reports_every_occurrence() {
        let failure =
            apply_edits("deﬁne deﬁne\n", &[req("def", "DEF")], "/f").expect_err("ambiguous");
        assert!(failure
            .message
            .contains("occurs 2 times in /f (line 1 (2 times))"));
    }

    #[test]
    fn splice_rejects_overlapping_unordered_and_invalid_spans() {
        let span = |start, end| Replacement {
            start,
            end,
            text: "X".into(),
        };
        for replacements in [
            vec![span(0, 3), span(2, 4)],
            vec![span(3, 4), span(0, 2)],
            vec![span(3, 2)],
            vec![span(0, 6)],
            vec![span(1, 2)], // Inside é's UTF-8 encoding.
        ] {
            let spans = replacements.iter().collect::<Vec<_>>();
            let failure = splice("éabc", &spans).expect_err("invalid spans must not panic");
            assert_eq!(failure.code, FailureCode::Invalid);
            assert!(failure.message.contains("invalid replacement spans"));
        }
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
