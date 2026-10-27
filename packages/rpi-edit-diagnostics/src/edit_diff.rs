//! The diff / normalize helpers behind `edit` — copied from `rpi`
//! (`crates/rpi-tools/src/tools/edit_diff.rs`, MIT, see `NOTICE`).
//!
//! Mirrors `packages/agent/src/harness/tools/edit-diff.ts`: the two-step match
//! contract (exact text, then [`normalize_for_fuzzy_match`] on both sides),
//! BOM / line-ending handling, and the display diff / unified patch the tool
//! returns in its details.
//!
//! Copied subset: the fork's `apply_edits_to_normalized_content` family is not
//! here, because applying is [`crate::edit_apply`] — the fork's `edit` no longer
//! splices through this module. What remains is exactly what
//! [`crate::edit_apply`], [`crate::edit_diagnostics`] and the tool layer call.

use similar::{udiff::unified_diff, Algorithm, TextDiff};

pub struct BomResult<'a> {
    pub bom: &'static str,
    pub text: &'a str,
}

/// Strip a leading U+FEFF BOM. Mirrors `stripBom` (returns the BOM string +
/// the text without it).
pub fn strip_bom(content: &str) -> BomResult<'_> {
    const BOM: &str = "\u{FEFF}";
    if let Some(rest) = content.strip_prefix(BOM) {
        BomResult {
            bom: BOM,
            text: rest,
        }
    } else {
        BomResult {
            bom: "",
            text: content,
        }
    }
}

/// Detect the dominant line ending: `\r\n` or `\n`. Mirrors `detectLineEnding`.
/// Lone `\r` is treated as `\n` (the normalizer maps both to `\n`).
pub fn detect_line_ending(content: &str) -> &'static str {
    let first_crlf = content.find("\r\n");
    let first_lf = content.find('\n');
    match (first_crlf, first_lf) {
        (None, None) => "\n",
        (Some(_), None) => "\n",
        (Some(crlf), Some(lf)) => {
            if crlf <= lf {
                "\r\n"
            } else {
                "\n"
            }
        }
        (None, Some(_)) => "\n",
    }
}

/// `\r\n` and lone `\r` → `\n`. Mirrors `normalizeToLF`.
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// `\n` → `ending`. Mirrors `restoreLineEndings`.
pub fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

/// NFKC + per-line trimEnd + smart-quote/dash/space normalization. Mirrors
/// `normalizeForFuzzyMatch`.
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let nfkc: String = text.nfkc().collect();
    // Per-line trimEnd.
    let trimmed: String = nfkc
        .split('\n')
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = String::with_capacity(trimmed.len());
    for c in trimmed.chars() {
        match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => out.push('\''),
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => out.push('"'),
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => out.push('-'),
            '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
            | '\u{3000}' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

/// Result of [`fuzzy_find_text`].
#[derive(Clone)]
pub struct FuzzyMatchResult {
    pub found: bool,
    pub index: usize,
    pub match_length: usize,
    pub used_fuzzy_match: bool,
    /// The content to splice into (original when exact, normalized when fuzzy).
    pub content_for_replacement: String,
}

/// Find `old_text` in `content`: exact first, then fuzzy-normalized. Mirrors
/// `fuzzyFindText`.
pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatchResult {
    if let Some(idx) = content.find(old_text) {
        return FuzzyMatchResult {
            found: true,
            index: idx,
            match_length: old_text.len(),
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        };
    }
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old = normalize_for_fuzzy_match(old_text);
    if let Some(idx) = fuzzy_content.find(&fuzzy_old) {
        return FuzzyMatchResult {
            found: true,
            index: idx,
            match_length: fuzzy_old.len(),
            used_fuzzy_match: true,
            content_for_replacement: fuzzy_content,
        };
    }
    FuzzyMatchResult {
        found: false,
        index: usize::MAX,
        match_length: 0,
        used_fuzzy_match: false,
        content_for_replacement: content.to_string(),
    }
}

/// Split content into lines, each preserving its trailing `\n` (or the final
/// line without one). Mirrors `splitLinesWithEndings`. Shared with
/// [`crate::edit_apply`], which copies untouched lines from the original
/// content when an edit only matched after normalization.
pub fn split_lines_with_endings(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut acc = String::new();
    for c in content.chars() {
        acc.push(c);
        if c == '\n' {
            out.push(std::mem::take(&mut acc));
        }
    }
    if !acc.is_empty() {
        out.push(acc);
    }
    out
}

/// `(start, end)` byte-offset spans per line. Mirrors `getLineSpans`. Shared
/// with [`crate::edit_apply`].
pub fn get_line_spans(content: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0usize;
    for (i, c) in content.char_indices() {
        if c == '\n' {
            spans.push((start, i + 1));
            start = i + 1;
        }
    }
    if start < content.len() {
        spans.push((start, content.len()));
    }
    spans
}

/// `{ diff, first_changed_line }`. Mirrors `generateDiffString`.
pub struct DiffResult {
    pub diff: String,
    pub first_changed_line: Option<usize>,
}

/// A display-oriented diff (NOT a unified patch) with line-number prefixes.
/// Mirrors `generateDiffString` with a 4-line context window.
pub fn generate_diff_string(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> DiffResult {
    let parts = TextDiff::from_lines(old_content, new_content);
    let ops: Vec<_> = parts.ops().to_vec();
    let old_lines: Vec<&str> = old_content.split('\n').collect();
    let new_lines: Vec<&str> = new_content.split('\n').collect();
    let max_line_num = old_lines.len().max(new_lines.len());
    let line_num_width = max_line_num.to_string().len();

    let mut out: Vec<String> = Vec::new();
    let mut old_line_num = 1usize;
    let mut new_line_num = 1usize;
    let mut last_was_change = false;
    let mut first_changed_line: Option<usize> = None;

    for (i, op) in ops.iter().enumerate() {
        let tag = op.tag();
        // The raw lines for this op — sliced from the op ranges directly
        // (mirrors TS `part.value.split("\n")` + trailing-empty pop).
        let mut raw: Vec<&str> = match tag {
            similar::DiffTag::Equal | similar::DiffTag::Delete => {
                old_lines[op.old_range().start..op.old_range().end].to_vec()
            }
            _ => new_lines[op.new_range().start..op.new_range().end].to_vec(),
        };
        if raw.last().map(|s| s.is_empty()).unwrap_or(false) {
            raw.pop();
        }
        let raw_len = raw.len();

        let is_added = matches!(tag, similar::DiffTag::Insert | similar::DiffTag::Replace);
        let is_removed = matches!(tag, similar::DiffTag::Delete | similar::DiffTag::Replace);

        if is_added || is_removed {
            if first_changed_line.is_none() {
                first_changed_line = Some(new_line_num);
            }
            // A `Replace` op is the removed old lines followed by the added new
            // lines, as jsdiff reports them; the two ranges may differ in length.
            if is_removed {
                let mut removed = &old_lines[op.old_range().start..op.old_range().end];
                if removed.last().is_some_and(|s| s.is_empty()) {
                    removed = &removed[..removed.len() - 1];
                }
                for line in removed {
                    out.push(format!(
                        "-{:>width$} {}",
                        old_line_num,
                        line,
                        width = line_num_width
                    ));
                    old_line_num += 1;
                }
            }
            if is_added {
                for line in &raw {
                    out.push(format!(
                        "+{:>width$} {}",
                        new_line_num,
                        line,
                        width = line_num_width
                    ));
                    new_line_num += 1;
                }
            }
            last_was_change = true;
            let _ = i; // index only needed for context lookahead below
        } else {
            // Equal → context lines, show a few before/after changes.
            let next_is_change = i + 1 < ops.len()
                && matches!(
                    ops[i + 1].tag(),
                    similar::DiffTag::Insert | similar::DiffTag::Delete | similar::DiffTag::Replace
                );
            let has_leading_change = last_was_change;
            let has_trailing_change = next_is_change;

            if has_leading_change && has_trailing_change {
                if raw_len <= context_lines * 2 {
                    for line in &raw {
                        out.push(format!(
                            " {:>width$} {}",
                            old_line_num,
                            line,
                            width = line_num_width
                        ));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                } else {
                    let leading = &raw[..context_lines];
                    let trailing = &raw[raw_len - context_lines..];
                    let skipped = raw_len - leading.len() - trailing.len();
                    for line in leading {
                        out.push(format!(
                            " {:>width$} {}",
                            old_line_num,
                            line,
                            width = line_num_width
                        ));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                    out.push(format!(" {:>width$} ...", "", width = line_num_width));
                    old_line_num += skipped;
                    new_line_num += skipped;
                    for line in trailing {
                        out.push(format!(
                            " {:>width$} {}",
                            old_line_num,
                            line,
                            width = line_num_width
                        ));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                }
            } else if has_leading_change {
                let shown = if raw_len <= context_lines {
                    &raw[..]
                } else {
                    &raw[..context_lines]
                };
                let skipped = raw_len - shown.len();
                for line in shown {
                    out.push(format!(
                        " {:>width$} {}",
                        old_line_num,
                        line,
                        width = line_num_width
                    ));
                    old_line_num += 1;
                    new_line_num += 1;
                }
                if skipped > 0 {
                    out.push(format!(" {:>width$} ...", "", width = line_num_width));
                    old_line_num += skipped;
                    new_line_num += skipped;
                }
            } else if has_trailing_change {
                let skipped = raw_len.saturating_sub(context_lines);
                if skipped > 0 {
                    out.push(format!(" {:>width$} ...", "", width = line_num_width));
                    old_line_num += skipped;
                    new_line_num += skipped;
                }
                for line in &raw[skipped..] {
                    out.push(format!(
                        " {:>width$} {}",
                        old_line_num,
                        line,
                        width = line_num_width
                    ));
                    old_line_num += 1;
                    new_line_num += 1;
                }
            } else {
                // Skip these context lines entirely.
                old_line_num += raw_len;
                new_line_num += raw_len;
            }
            last_was_change = false;
        }
    }

    DiffResult {
        diff: out.join("\n"),
        first_changed_line,
    }
}

/// A unified-diff patch with 4 lines of context, file headers only (no
/// timestamps). Mirrors `generateUnifiedPatch` (via the `similar` crate, which
/// is the Rust equivalent of the npm `diff` package).
pub fn generate_unified_patch(path: &str, old_content: &str, new_content: &str) -> String {
    // `similar::udiff::unified_diff(alg, old, new, context_radius, header)`
    // where `header: Option<(&str,&str)>` carries the (old_label, new_label)
    // file headers (`--- ` / `+++ `). No timestamps, matching the TS port.
    unified_diff(
        Algorithm::Myers,
        old_content,
        new_content,
        4,
        Some((path, path)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_to_lf_collapses_crlf_and_cr() {
        assert_eq!(normalize_to_lf("a\r\nb\rc\n"), "a\nb\nc\n");
    }

    #[test]
    fn detect_line_ending_prefers_crlf_when_first() {
        assert_eq!(detect_line_ending("a\r\nb\nc"), "\r\n");
        assert_eq!(detect_line_ending("a\nb\r\nc"), "\n");
        assert_eq!(detect_line_ending("noslash"), "\n");
    }

    #[test]
    fn diff_string_shows_replaced_lines_old_then_new() {
        let old = "keep\nold one\nold two\nkeep\n";
        let new = "keep\nnew\nkeep\n";
        let diff = generate_diff_string(old, new, 4).diff;
        assert_eq!(diff, " 1 keep\n-2 old one\n-3 old two\n+2 new\n 4 keep");
    }

    #[test]
    fn strip_bom_removes_prefix() {
        let r = strip_bom("\u{FEFF}hello");
        assert_eq!(r.bom, "\u{FEFF}");
        assert_eq!(r.text, "hello");
        let r2 = strip_bom("hello");
        assert_eq!(r2.bom, "");
        assert_eq!(r2.text, "hello");
    }

    #[test]
    fn fuzzy_find_exact_then_smart_quotes() {
        let content = "it's a test";
        let m = fuzzy_find_text(content, "it's a test");
        assert!(m.found);
        assert!(!m.used_fuzzy_match);
        // Smart-quote variant.
        let m2 = fuzzy_find_text(content, "it\u{2019}s a test");
        assert!(m2.found);
        assert!(m2.used_fuzzy_match);
    }

    #[test]
    fn generate_unified_patch_has_headers() {
        let patch = generate_unified_patch("/x", "a\nb\n", "a\nB\n");
        assert!(patch.contains("--- /x"));
        assert!(patch.contains("+++ /x"));
        assert!(patch.contains("-b"));
        assert!(patch.contains("+B"));
    }
}
