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
        let (needle, spans) = match view.as_ref().filter(|_| normalized_match) {
            None => {
                let spans = find_all_spans(content, old)
                    .into_iter()
                    .map(|(start, end)| Replacement {
                        start,
                        end,
                        text: new.clone(),
                    })
                    .collect::<Vec<_>>();
                (old.clone(), spans)
            }
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
                let spans = find_all_spans(&view.text, &needle)
                    .into_iter()
                    .map(|(start, end)| view.original_replacement(content, start, end, new))
                    .collect::<Vec<_>>();
                (needle, spans)
            }
        };
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
                .map(|span| line_of(content, span.start))
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

    if let Some(failure) = overlap_failure(content, &plans, path) {
        failures.push(failure);
    }

    if !failures.is_empty() {
        return Err(ApplyFailure::new(failures));
    }

    let replacements: Vec<&Replacement> = plans.iter().flat_map(|plan| &plan.spans).collect();
    let new_content = splice(content, &replacements);

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
        .flat_map(|(plan_index, plan)| plan.spans.iter().map(move |span| (plan_index, span.start)))
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

/// Splice every replacement in ascending order.
fn splice(base: &str, replacements: &[&Replacement]) -> String {
    let mut sorted: Vec<&Replacement> = replacements.to_vec();
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

/// [`normalize_for_fuzzy_match`] of the original content, plus the original
/// byte range each normalized char came from, so a match found in normalized
/// text replaces exactly the original bytes it covers.
struct NormalizedView {
    text: String,
    /// One `(start, end)` per char of `text`; `None` when normalizing char by
    /// char does not reproduce `text` (NFKC composed across chars).
    sources: Option<Vec<(usize, usize)>>,
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
        Self { text, sources }
    }

    /// The original-content replacement for the normalized span `[start, end)`.
    /// A span that begins or ends inside one char's expansion (e.g. half of
    /// `ﬁ`), or a view without sources, rewrites the span's whole lines from the
    /// normalized text instead, as upstream Pi does.
    fn original_replacement(
        &self,
        content: &str,
        start: usize,
        end: usize,
        new_text: &str,
    ) -> Replacement {
        if let Some(sources) = &self.sources {
            let first = self.text[..start].chars().count();
            let last = first + self.text[start..end].chars().count() - 1;
            let starts_clean = first == 0 || sources[first - 1].0 != sources[first].0;
            let ends_clean = last + 1 == sources.len() || sources[last + 1].0 != sources[last].0;
            if starts_clean && ends_clean {
                return Replacement {
                    start: sources[first].0,
                    end: sources[last].1,
                    text: new_text.to_string(),
                };
            }
        }
        let line_start = self.text[..start].rfind('\n').map_or(0, |i| i + 1);
        let line_end = self.text[end..]
            .find('\n')
            .map_or(self.text.len(), |i| end + i);
        let first_line = self.text[..start].matches('\n').count();
        let last_line = self.text[..line_end].matches('\n').count();
        let line_starts: Vec<usize> = std::iter::once(0)
            .chain(content.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        let original_end = line_starts
            .get(last_line + 1)
            .map_or(content.len(), |next| next - 1);
        Replacement {
            start: line_starts[first_line],
            end: original_end,
            text: format!(
                "{}{new_text}{}",
                &self.text[line_start..start],
                &self.text[end..line_end]
            ),
        }
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
