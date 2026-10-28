//! The 10-pass fuzzy matcher chain — port of `src/domain/passes.ts`
//! (`pi-semantic-edit`, MIT, (c) 2026 K2).
//!
//! Passes 1-9 are the OpenDev chain; pass 10 (`unicode_normalized`) is the
//! package's own addition (NFKC covers NBSP/ligatures but not typographic
//! quotes/dashes, hence the punctuation map).
//!
//! Diagnostic use only: these passes propose candidate regions for a failing
//! `oldText`, they never decide that an edit matches (see the [module
//! docs](super)).
//!
//! SAFETY INVARIANT: a pass returns only text that is present in `original`
//! (`original.contains(actual)`) — never text that is not in the file. The
//! chain re-checks this before reporting a candidate.

use crate::semantic_edit::similarity::similarity;

/// A pass: `(original, old_content) -> matched original substring`.
pub type PassFind = fn(&str, &str) -> Option<String>;

// --- Pass 1: Simple (exact match) ---

/// Mirrors `simpleFind`.
pub fn simple_find(original: &str, old_content: &str) -> Option<String> {
    original
        .contains(old_content)
        .then(|| old_content.to_string())
}

// --- Pass 2: LineTrimmed ---

/// Mirrors `lineTrimmedFind`.
pub fn line_trimmed_find(original: &str, old_content: &str) -> Option<String> {
    let old_lines: Vec<&str> = old_content.split('\n').collect();
    let old_trimmed: Vec<&str> = old_lines.iter().map(|l| l.trim()).collect();

    if old_trimmed.is_empty() || old_trimmed.iter().all(|l| l.is_empty()) {
        return None;
    }

    let original_lines: Vec<&str> = original.split('\n').collect();

    for i in 0..original_lines.len() {
        if original_lines[i].trim() != old_trimmed[0] {
            continue;
        }
        if i + old_trimmed.len() > original_lines.len() {
            continue;
        }
        let all_match = old_trimmed
            .iter()
            .enumerate()
            .all(|(j, old_ln)| original_lines[i + j].trim() == *old_ln);
        if all_match {
            let actual = original_lines[i..i + old_trimmed.len()].join("\n");
            if original.contains(&actual) {
                return Some(actual);
            }
        }
    }
    None
}

// --- Pass 3: BlockAnchor (first/last lines anchor, middle scored) ---

/// Mirrors `blockAnchorFind`.
pub fn block_anchor_find(original: &str, old_content: &str) -> Option<String> {
    let old_lines: Vec<&str> = old_content.split('\n').collect();
    if old_lines.len() < 3 {
        return None;
    }

    let first_trimmed = old_lines[0].trim();
    let last_trimmed = old_lines[old_lines.len() - 1].trim();
    let middle_old: Vec<&str> = old_lines[1..old_lines.len() - 1]
        .iter()
        .map(|l| l.trim())
        .collect();

    let original_lines: Vec<&str> = original.split('\n').collect();
    let mut candidates: Vec<(usize, usize, f64)> = Vec::new();

    for i in 0..original_lines.len() {
        if original_lines[i].trim() != first_trimmed {
            continue;
        }
        let window_end = (i + old_lines.len() * 2).min(original_lines.len());
        let mut end_idx = i + old_lines.len() - 1;
        while end_idx < window_end {
            if original_lines[end_idx].trim() != last_trimmed {
                end_idx += 1;
                continue;
            }
            let middle_orig: Vec<&str> = original_lines[i + 1..end_idx]
                .iter()
                .map(|l| l.trim())
                .collect();

            let sim = if middle_old.is_empty() && middle_orig.is_empty() {
                1.0
            } else if middle_old.is_empty() || middle_orig.is_empty() {
                end_idx += 1;
                continue;
            } else {
                similarity(&middle_old.join("\n"), &middle_orig.join("\n"))
            };
            candidates.push((i, end_idx, sim));
            end_idx += 1;
        }
    }

    if candidates.is_empty() {
        return None;
    }

    // --- Threshold: 0.0 for a single candidate, 0.3 when multiple exist ---
    let threshold = if candidates.len() == 1 { 0.0 } else { 0.3 };
    let mut best = candidates[0];
    for c in &candidates {
        if c.2 > best.2 {
            best = *c;
        }
    }
    if best.2 < threshold {
        return None;
    }

    let actual = original_lines[best.0..=best.1].join("\n");
    original.contains(&actual).then_some(actual)
}

// --- Pass 4: WhitespaceNormalized ---

fn ws_normalize(s: &str) -> String {
    s.split('\n')
        .map(|ln| ln.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Mirrors `whitespaceNormalizedFind`.
pub fn whitespace_normalized_find(original: &str, old_content: &str) -> Option<String> {
    let norm_old = ws_normalize(old_content);
    let original_lines: Vec<&str> = original.split('\n').collect();
    let old_line_count = old_content.split('\n').count();

    for i in 0..original_lines.len() {
        let end_max = (i + old_line_count + 2).min(original_lines.len());
        let mut j = i + old_line_count - 1;
        while j <= end_max {
            if j > original_lines.len() {
                break;
            }
            let candidate = original_lines[i..j].join("\n");
            if ws_normalize(&candidate) == norm_old && original.contains(&candidate) {
                return Some(candidate);
            }
            j += 1;
        }
    }
    None
}

// --- Pass 5: IndentationFlexible ---

/// Mirrors `indentationFlexibleFind`.
pub fn indentation_flexible_find(original: &str, old_content: &str) -> Option<String> {
    let old_stripped: Vec<&str> = old_content
        .split('\n')
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    if old_stripped.is_empty() {
        return None;
    }

    let original_lines: Vec<&str> = original.split('\n').collect();

    for i in 0..original_lines.len() {
        if original_lines[i].trim() != old_stripped[0] {
            continue;
        }
        let mut matched_indices: Vec<usize> = Vec::new();
        let mut j = 0usize;
        let search_end = (i + old_stripped.len() * 3).min(original_lines.len());
        let mut k = i;
        while k < search_end {
            if j >= old_stripped.len() {
                break;
            }
            let orig_line = original_lines[k];
            if orig_line.trim().is_empty() {
                k += 1;
                continue;
            }
            if orig_line.trim() == old_stripped[j] {
                matched_indices.push(k);
                j += 1;
            } else {
                break;
            }
            k += 1;
        }

        if j == old_stripped.len() && !matched_indices.is_empty() {
            let start = matched_indices[0];
            let end = matched_indices[matched_indices.len() - 1] + 1;
            let actual = original_lines[start..end].join("\n");
            if original.contains(&actual) {
                return Some(actual);
            }
        }
    }
    None
}

// --- Pass 6: EscapeNormalized ---

fn unescape(s: &str) -> String {
    // --- Rust `str::replace` replaces ALL; JS `String.replace` only the first,
    // so the TS port uses replaceAll — same result here ---
    s.replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\\\", "\\")
        .replace("\\\"", "\"")
        .replace("\\'", "'")
}

/// Mirrors `escapeNormalizedFind`.
pub fn escape_normalized_find(original: &str, old_content: &str) -> Option<String> {
    let unescaped = unescape(old_content);
    if unescaped == old_content {
        return None;
    }
    original.contains(&unescaped).then_some(unescaped)
}

// --- Pass 7: TrimmedBoundary ---

/// Mirrors `trimmedBoundaryFind`.
pub fn trimmed_boundary_find(original: &str, old_content: &str) -> Option<String> {
    let trimmed = old_content.trim();
    if trimmed == old_content {
        return None;
    }

    if original.contains(trimmed) {
        return Some(trimmed.to_string());
    }

    // --- Fallback: first/last content lines as contains-anchors ---
    let old_lines: Vec<&str> = old_content.split('\n').collect();
    let first_content = old_lines[0].trim();
    let last_content = old_lines[old_lines.len() - 1].trim();

    if first_content.is_empty() || last_content.is_empty() {
        return None;
    }

    let original_lines: Vec<&str> = original.split('\n').collect();
    for i in 0..original_lines.len() {
        if !original_lines[i].contains(first_content) {
            continue;
        }
        let end = (i + old_lines.len() + 2).min(original_lines.len());
        for j in (i + 1)..end {
            if !original_lines[j].contains(last_content) {
                continue;
            }
            let candidate = original_lines[i..=j].join("\n");
            if original.contains(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

// --- Pass 8: ContextAware ---

/// Mirrors `contextAwareFind`.
pub fn context_aware_find(original: &str, old_content: &str) -> Option<String> {
    let old_lines: Vec<&str> = old_content.split('\n').collect();
    if old_lines.len() < 2 {
        return None;
    }

    let first_ctx = old_lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim());
    let last_ctx = old_lines
        .iter()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim());
    let (Some(first_ctx), Some(last_ctx)) = (first_ctx, last_ctx) else {
        return None;
    };

    let original_lines: Vec<&str> = original.split('\n').collect();

    let starts: Vec<usize> = original_lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim().contains(first_ctx))
        .map(|(i, _)| i)
        .collect();

    if starts.is_empty() {
        return None;
    }

    let mut best_match: Option<String> = None;
    let mut best_sim = 0.0f64;

    for start in starts {
        let search_end = (start + old_lines.len() * 2).min(original_lines.len());
        for end in (start + 1)..search_end {
            if original_lines[end].trim().contains(last_ctx) {
                let candidate = original_lines[start..=end].join("\n");
                let sim = similarity(old_content.trim(), candidate.trim());
                if sim > best_sim && sim > 0.5 {
                    best_sim = sim;
                    best_match = Some(candidate);
                }
                break; // --- first end anchor per start only ---
            }
        }
    }

    best_match.filter(|m| original.contains(m))
}

// --- Pass 9: MultiOccurrence (trimmed line-by-line, last resort) ---

/// Mirrors `multiOccurrenceFind`.
pub fn multi_occurrence_find(original: &str, old_content: &str) -> Option<String> {
    let trimmed = old_content.trim();
    if trimmed.is_empty() {
        return None;
    }

    let original_lines: Vec<&str> = original.split('\n').collect();
    let trimmed_lines: Vec<&str> = trimmed.split('\n').collect();

    if trimmed_lines.len() > original_lines.len() {
        return None;
    }

    for i in 0..=(original_lines.len() - trimmed_lines.len()) {
        let candidate_lines = &original_lines[i..i + trimmed_lines.len()];
        let all_match = candidate_lines
            .iter()
            .zip(trimmed_lines.iter())
            .all(|(a, t)| a.trim() == t.trim());
        if all_match {
            let candidate = candidate_lines.join("\n");
            if original.contains(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

// --- Pass 10: UnicodeNormalized (the package's addition) — NFKC + punct map ---
//
// NFKC covers NBSP/ligatures but NOT typographic quotes/dashes (they have no
// compatibility decomposition), so PUNCT_MAP covers those. Runs after the nine
// OpenDev passes for parity.

fn punct_map(c: char) -> Option<&'static str> {
    Some(match c {
        '\u{2018}' => "'",  // ‘ left single quote
        '\u{2019}' => "'",  // ’ right single quote
        '\u{201A}' => "'",  // ‚ single low-9 quote
        '\u{201B}' => "'",  // ‛ single high-reversed-9 quote
        '\u{201C}' => "\"", // “ left double quote
        '\u{201D}' => "\"", // ” right double quote
        '\u{201E}' => "\"", // „ double low-9 quote
        '\u{201F}' => "\"", // ‟ double high-reversed-9 quote
        '\u{2013}' => "-",  // – en dash
        '\u{2014}' => "-",  // — em dash
        _ => return None,
    })
}

/// Normalize a single char: NFKC, then the punctuation map.
fn normalize_char(c: char) -> String {
    use unicode_normalization::UnicodeNormalization;
    let mut out = String::new();
    for n in c.to_string().nfkc() {
        match punct_map(n) {
            Some(repl) => out.push_str(repl),
            None => out.push(n),
        }
    }
    out
}

/// NFKC + punctuation map over the whole string. Mirrors `normalizeText`.
pub fn normalize_text(s: &str) -> String {
    s.chars().map(normalize_char).collect()
}

/// Normalize `s` and record, for every normalized char, the byte offset of the
/// source char it came from (plus a final sentinel of `s.len()`), so a match in
/// normalized space can be mapped back onto the **original** bytes. This
/// replaces TS `mapNormalizedIndex`, which walks UTF-16 code units.
fn normalize_with_offsets(s: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(s.len());
    let mut offsets: Vec<usize> = Vec::new();
    for (idx, c) in s.char_indices() {
        let start = out.len();
        out.push_str(&normalize_char(c));
        for _ in out[start..].chars() {
            offsets.push(idx);
        }
    }
    offsets.push(s.len());
    (out, offsets)
}

fn is_ascii(s: &str) -> bool {
    s.is_ascii()
}

/// Mirrors `unicodeNormalizedFind`.
pub fn unicode_normalized_find(original: &str, old_content: &str) -> Option<String> {
    let norm_old = normalize_text(old_content);
    // --- Skip when the query is already normalized and the file is pure ASCII;
    // non-ASCII files may hold ligatures/quotes worth normalizing ---
    if norm_old == old_content && is_ascii(original) {
        return None;
    }
    let (norm_orig, offsets) = normalize_with_offsets(original);
    let idx = norm_orig.find(&norm_old)?;
    let start_char = norm_orig[..idx].chars().count();
    let end_char = start_char + norm_old.chars().count();
    let start = *offsets.get(start_char)?;
    let end = *offsets.get(end_char)?;
    let actual = original.get(start..end)?;
    original.contains(actual).then(|| actual.to_string())
}
