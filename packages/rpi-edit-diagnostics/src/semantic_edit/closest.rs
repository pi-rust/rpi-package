//! Nearest region when no pass matches — port of `src/domain/closest.ts`
//! (`pi-semantic-edit`, MIT, (c) 2026 K2).
//!
//! Diagnostic use only. The anchor floor (not any pass' threshold) is the gate:
//! this finds the nearest real text so the model can compare its `oldText`
//! against it.

use crate::edit_diff::normalize_to_lf;
use crate::semantic_edit::similarity::similarity;

/// Anchor floor for the first-line similarity. Mirrors `ANCHOR_FLOOR`.
pub const ANCHOR_FLOOR: f64 = 0.3;

/// TS default for [`find_closest_candidate`]'s `max_candidates`.
pub const DEFAULT_MAX_CANDIDATES: usize = 50;

/// A near-miss region: `start_line..=end_line` (1-indexed) plus the LCS ratio
/// against the query. Mirrors TS `ClosestCandidate`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClosestCandidate {
    pub pass_name: &'static str,
    pub similarity: f64,
    pub candidate: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// Mirrors `findClosestCandidate`. `max_candidates` caps the anchor scan.
pub fn find_closest_candidate(
    original: &str,
    old_content: &str,
    max_candidates: usize,
) -> Option<ClosestCandidate> {
    let orig = normalize_to_lf(original);
    let old = normalize_to_lf(old_content);
    let old = old.trim();
    if old.is_empty() {
        return None;
    }

    let old_lines: Vec<&str> = old.split('\n').collect();
    let first_line = old_lines[0].trim();
    if first_line.is_empty() {
        return None;
    }

    let orig_lines: Vec<&str> = orig.split('\n').collect();

    let mut anchors: Vec<usize> = Vec::new();
    for (i, line) in orig_lines.iter().enumerate() {
        if anchors.len() >= max_candidates {
            break;
        }
        let line_sim = similarity(line.trim(), first_line);
        if line_sim >= ANCHOR_FLOOR {
            anchors.push(i);
        }
    }

    if anchors.is_empty() {
        return None;
    }

    let mut best: Option<(usize, usize, f64)> = None;
    for start in anchors {
        let min_end = start + old_lines.len() - 1;
        let max_end = (start + old_lines.len() * 2).min(orig_lines.len());
        let mut end = min_end;
        while end < max_end {
            let candidate = orig_lines[start..=end].join("\n").trim().to_string();
            let sim = similarity(old, &candidate);
            match best {
                Some((_, _, best_sim)) if sim <= best_sim => {}
                _ => best = Some((start, end, sim)),
            }
            end += 1;
        }
    }

    let (start, end, sim) = best?;
    let actual = orig_lines[start..=end].join("\n");
    Some(ClosestCandidate {
        pass_name: "closest-candidate",
        similarity: sim,
        candidate: actual,
        start_line: start + 1,
        end_line: end + 1,
    })
}
