//! Candidate finder for `edit` failure diagnostics.
//!
//! Rust port of the matcher layer of `pi-semantic-edit` (`src/domain/*.ts`), the
//! drop-in `edit` replacement authored by k3-2o / K2:
//! <https://github.com/k3-2o/pi-semantic-edit>.
//!
//! ```text
//! pi-semantic-edit — Copyright (c) 2026 K2 — MIT License
//! ```
//!
//! **Nothing here edits a file.** Applying is [`crate::edit_apply`], and
//! it only accepts a text that is present verbatim in the file, or that matches
//! after [`crate::edit_diff::normalize_for_fuzzy_match`] equalizes
//! trailing whitespace and typographic quotes/dashes on both sides — the same
//! two-step contract as upstream Pi. Every pass of the ported chain would happily
//! replace a text with different indentation, different escapes, or different
//! middle lines, and the TS package resolved ambiguity by auto-expanding context
//! until one occurrence stood out. Both are decisions the model should make, not
//! the tool, so those paths are gone.
//!
//! What is kept, and what it is used for (TS module in parentheses):
//!
//! | Rust           | TS                     | now used for                                   |
//! |----------------|------------------------|------------------------------------------------|
//! | [`similarity`] | `domain/similarity.ts` | LCS ratio (2·LCS/(\|a\|+\|b\|))                 |
//! | [`passes`]     | `domain/passes.ts`     | the 10 candidate-finding strategies            |
//! | [`chain`]      | `domain/chain.ts`      | pass order, occurrence line positions          |
//! | [`closest`]    | `domain/closest.ts`    | nearest region when no pass matches at all     |
//!
//! [`find_candidates`] collects the regions a failing `oldText` is *probably*
//! about, and [`crate::edit_diagnostics`] describes how each one differs
//! from what the model sent, quoting the real text so it can be copied back into
//! `oldText`. The passes are therefore hints, never a match: their output may be
//! arbitrarily far from the query.
//!
//! Removed with the applying path: `domain/apply.ts` (splice + overlap/no-op
//! checks), `domain/editor.ts` (resolve-against-original, auto-expand,
//! disproportionate-span guard), `domain/uniqueness.ts` (folded into
//! [`crate::edit_diagnostics`]), `domain/coherence.ts` (whole-file brace
//! and indent warnings — replaced by a per-edit brace-balance delta in
//! [`crate::edit_apply`]), and `domain/errors.ts` (replaced by the
//! compiler-style diagnostics). `domain/parser.ts` (aider SEARCH/REPLACE input)
//! and `pi/render.ts` were never ported.
//!
//! This copy comes from `rpi` (`crates/rpi-tools/src/tools/semantic_edit/`, tag
//! `v0.9.4-aia.2`, MIT) with only the crate-internal `use` paths adjusted.

pub mod chain;
pub mod closest;
pub mod passes;
pub mod similarity;

use crate::edit_diff::normalize_to_lf;

/// A region of the file that a failing `oldText` may have meant.
///
/// `text` is copied verbatim from the original content (LF-normalized, BOM
/// already stripped), so it can be quoted back to the model unmodified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Which strategy found it (`line_trimmed`, `closest-candidate`, ...).
    pub pass_name: &'static str,
    /// 1-indexed, inclusive.
    pub start_line: usize,
    /// 1-indexed, inclusive.
    pub end_line: usize,
    pub text: String,
}

impl Candidate {
    pub fn line_count(&self) -> usize {
        self.end_line - self.start_line + 1
    }

    /// `line 12` or `lines 12-15`.
    pub fn lines_phrase(&self) -> String {
        lines_phrase(self.start_line, self.end_line)
    }
}

/// `line 12` or `lines 12-15`.
pub fn lines_phrase(start_line: usize, end_line: usize) -> String {
    if start_line >= end_line {
        format!("line {start_line}")
    } else {
        format!("lines {start_line}-{end_line}")
    }
}

/// Cap on how many candidates are reported; past a handful the list stops being
/// a hint and starts being noise.
pub const MAX_CANDIDATES: usize = 3;

/// A pass result taller than this reached into a neighbouring block: too far
/// from the query's own shape to be a useful hint.
fn max_candidate_lines(old_lines: usize) -> usize {
    old_lines * 3 + 3
}

/// Find the regions a failing `oldText` probably refers to.
///
/// Passes run in their original order and each distinct span they offer becomes
/// a candidate; when they offer nothing, [`closest::find_closest_candidate`]
/// scans for the nearest region instead. Candidates are deduplicated by line
/// range and sorted so that the one closest to the query's shape comes first:
/// spans of comparable height before oversized ones, then by LCS similarity.
///
/// An empty or blank `oldText` yields no candidates (there is nothing to look
/// for).
pub fn find_candidates(original: &str, old_text: &str) -> Vec<Candidate> {
    let orig = normalize_to_lf(original);
    let old = normalize_to_lf(old_text);
    if old.trim().is_empty() {
        return Vec::new();
    }
    let old_lines = old.matches('\n').count() + 1;
    let orig_lines: Vec<&str> = orig.split('\n').collect();

    let mut found: Vec<Candidate> = Vec::new();
    for (pass_name, span) in chain::pass_spans(&orig, &old) {
        if span.matches('\n').count() + 1 > max_candidate_lines(old_lines) {
            continue;
        }
        let Some(range) = line_range_of_span(&orig_lines, &orig, &span) else {
            continue;
        };
        push_unique(&mut found, pass_name, range, &orig_lines);
    }

    if found.is_empty() {
        if let Some(closest) =
            closest::find_closest_candidate(&orig, &old, closest::DEFAULT_MAX_CANDIDATES)
        {
            push_unique(
                &mut found,
                closest.pass_name,
                (closest.start_line, closest.end_line),
                &orig_lines,
            );
        }
    }

    // Prefer the query's own height, then the most similar text. Stable so the
    // pass order still breaks ties.
    found.sort_by(|a, b| {
        let a_height = a.line_count().abs_diff(old_lines);
        let b_height = b.line_count().abs_diff(old_lines);
        let a_sim = similarity::similarity(&old, &a.text);
        let b_sim = similarity::similarity(&old, &b.text);
        a_height.cmp(&b_height).then_with(|| {
            b_sim
                .partial_cmp(&a_sim)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    found.truncate(MAX_CANDIDATES);
    found
}

fn push_unique(
    found: &mut Vec<Candidate>,
    pass_name: &'static str,
    (start_line, end_line): (usize, usize),
    orig_lines: &[&str],
) {
    if start_line == 0 || end_line < start_line || end_line > orig_lines.len() {
        return;
    }
    if found
        .iter()
        .any(|c| c.start_line == start_line && c.end_line == end_line)
    {
        return;
    }
    found.push(Candidate {
        pass_name,
        start_line,
        end_line,
        text: orig_lines[start_line - 1..end_line].join("\n"),
    });
}

/// 1-indexed inclusive line range covered by `span` inside `content`.
fn line_range_of_span(lines: &[&str], content: &str, span: &str) -> Option<(usize, usize)> {
    let start = content.find(span)?;
    let start_line = content[..start].matches('\n').count() + 1;
    let last = start + span.len().saturating_sub(1);
    let end_line = content[..last].matches('\n').count() + 1;
    (start_line <= lines.len() && end_line <= lines.len()).then_some((start_line, end_line))
}

#[cfg(test)]
mod tests;
