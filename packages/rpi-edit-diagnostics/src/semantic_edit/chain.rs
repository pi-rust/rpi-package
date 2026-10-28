//! Pass ordering + occurrence positions — port of `src/domain/chain.ts`
//! (`pi-semantic-edit`, MIT, (c) 2026 K2).
//!
//! Diagnostic use only: [`pass_spans`] asks every pass for a region, it does not
//! decide anything. See the [module docs](self) for why.

use crate::edit_diff::normalize_to_lf;
use crate::semantic_edit::passes::{
    block_anchor_find, context_aware_find, escape_normalized_find, indentation_flexible_find,
    line_trimmed_find, multi_occurrence_find, simple_find, trimmed_boundary_find,
    unicode_normalized_find, whitespace_normalized_find, PassFind,
};

/// One entry of the chain. Mirrors TS `Replacer`.
pub struct Replacer {
    pub name: &'static str,
    pub find: PassFind,
}

/// Pass order — the single source of truth. Mirrors `REPLACER_CHAIN`.
pub const REPLACER_CHAIN: [Replacer; 10] = [
    Replacer {
        name: "simple",
        find: simple_find,
    },
    Replacer {
        name: "line_trimmed",
        find: line_trimmed_find,
    },
    Replacer {
        name: "block_anchor",
        find: block_anchor_find,
    },
    Replacer {
        name: "whitespace_normalized",
        find: whitespace_normalized_find,
    },
    Replacer {
        name: "indentation_flexible",
        find: indentation_flexible_find,
    },
    Replacer {
        name: "escape_normalized",
        find: escape_normalized_find,
    },
    Replacer {
        name: "trimmed_boundary",
        find: trimmed_boundary_find,
    },
    Replacer {
        name: "context_aware",
        find: context_aware_find,
    },
    Replacer {
        name: "multi_occurrence",
        find: multi_occurrence_find,
    },
    Replacer {
        name: "unicode_normalized",
        find: unicode_normalized_find,
    },
];

/// Every distinct region the passes offer for `old_content`, in pass order,
/// tagged with the pass that produced it.
///
/// Each span is the *original* text (a pass never invents content) and is
/// re-verified here against `original`; empty or absent spans are dropped. When
/// the text is present verbatim the first entry is the exact match, i.e. the
/// text occurs more than once ([`find_occurrence_positions`] then says where).
pub fn pass_spans(original: &str, old_content: &str) -> Vec<(&'static str, String)> {
    let orig = normalize_to_lf(original);
    let old = normalize_to_lf(old_content);
    let mut spans: Vec<(&'static str, String)> = Vec::new();
    for replacer in REPLACER_CHAIN.iter() {
        let Some(actual) = (replacer.find)(&orig, &old) else {
            continue;
        };
        if actual.is_empty() || !orig.contains(actual.as_str()) {
            continue;
        }
        if spans.iter().any(|(_, existing)| *existing == actual) {
            continue;
        }
        spans.push((replacer.name, actual));
    }
    spans
}

/// 1-indexed line numbers of every occurrence of `needle` in `haystack`
/// (overlapping occurrences included, matching TS `indexOf(needle, idx + 1)`).
///
/// Used to report *where* a still-ambiguous `oldText` occurs. `haystack` may be
/// fuzzy-normalized: normalization never adds or removes line breaks, so the
/// numbers are the file's own.
pub fn find_occurrence_positions(haystack: &str, needle: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    if needle.is_empty() {
        return positions;
    }
    let mut search_pos = 0usize;
    while search_pos <= haystack.len() {
        let Some(idx) = haystack[search_pos..].find(needle).map(|i| search_pos + i) else {
            break;
        };
        // --- newlines before idx + 1 ---
        positions.push(haystack[..idx].matches('\n').count() + 1);
        search_pos = idx + 1;
        // Advance to the next char boundary when the needle can start mid-char.
        while search_pos < haystack.len() && !haystack.is_char_boundary(search_pos) {
            search_pos += 1;
        }
    }
    positions
}
