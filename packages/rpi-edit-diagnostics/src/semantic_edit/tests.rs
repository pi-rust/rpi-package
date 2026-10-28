//! Unit tests for the (now diagnostics-only) candidate finder. One case per
//! strategy, plus the properties the failure reports rely on: candidate line
//! ranges are real file lines, the reported text is the file's own text, and an
//! unrelated `oldText` yields no candidate at all.

use crate::semantic_edit::chain::{find_occurrence_positions, pass_spans};
use crate::semantic_edit::closest::find_closest_candidate;
use crate::semantic_edit::passes::{
    block_anchor_find, escape_normalized_find, indentation_flexible_find, line_trimmed_find,
    multi_occurrence_find, unicode_normalized_find, whitespace_normalized_find,
};
use crate::semantic_edit::similarity::similarity;
use crate::semantic_edit::{find_candidates, lines_phrase, MAX_CANDIDATES};

fn first_candidate(content: &str, old: &str) -> crate::semantic_edit::Candidate {
    find_candidates(content, old)
        .into_iter()
        .next()
        .expect("a candidate")
}

// --- Passes still answer with the *original* text (they feed the reports) ---

#[test]
fn line_trimmed_pass_offers_the_indented_original() {
    let original = "fn a() {\n        let x = 1;\n}\n";
    assert_eq!(
        line_trimmed_find(original, "fn a() {\n    let x = 1;\n}"),
        Some("fn a() {\n        let x = 1;\n}".to_string())
    );
}

#[test]
fn indentation_flexible_pass_skips_blank_lines() {
    let original = "    fn foo() {\n\n        body();\n    }\n";
    assert_eq!(
        indentation_flexible_find(original, "fn foo() {\nbody();"),
        Some("    fn foo() {\n\n        body();".to_string())
    );
}

#[test]
fn block_anchor_pass_offers_a_region_with_a_different_middle() {
    let original = "fn main() {\n    let x = 1;\n    println!(\"x\");\n}\n";
    let old = "fn main() {\n    let x = 2;\n    println!(\"x!\");\n}";
    let actual = block_anchor_find(original, old).expect("anchor");
    assert_eq!(
        actual,
        "fn main() {\n    let x = 1;\n    println!(\"x\");\n}"
    );
}

#[test]
fn whitespace_normalized_pass_collapses_runs() {
    let original = "let a  =  1; let b = 2;";
    let old = "let a = 1; let b = 2;";
    assert_eq!(
        whitespace_normalized_find(original, old),
        Some(original.to_string())
    );
}

#[test]
fn escape_normalized_pass_unescapes_sequences() {
    assert_eq!(
        escape_normalized_find("a\tb\n", "a\\tb"),
        Some("a\tb".to_string())
    );
}

#[test]
fn multi_occurrence_pass_matches_trimmed_lines() {
    let original = "  alpha\n\n  beta\n";
    assert_eq!(
        multi_occurrence_find(original, "alpha\n\nbeta"),
        Some("  alpha\n\n  beta".to_string())
    );
    assert_eq!(multi_occurrence_find(original, "gamma"), None);
}

#[test]
fn unicode_normalized_pass_maps_typographic_chars_back_to_the_original() {
    let original = "it\u{2019}s \u{FB01}ne\n";
    assert_eq!(
        unicode_normalized_find(original, "it's fine"),
        Some("it\u{2019}s \u{FB01}ne".to_string())
    );
}

// --- pass_spans: every strategy's offer, deduplicated, verified ---

#[test]
fn pass_spans_keeps_pass_order_and_drops_duplicates() {
    let original = "fn a() {\n    let x = 1;\n}\n";
    let spans = pass_spans(original, "fn a() {\n  let x = 1;\n}");
    assert_eq!(spans[0].0, "line_trimmed");
    assert_eq!(spans[0].1, "fn a() {\n    let x = 1;\n}");
    // The same span from a later pass is not repeated.
    let count = spans
        .iter()
        .filter(|(_, span)| *span == "fn a() {\n    let x = 1;\n}")
        .count();
    assert_eq!(count, 1, "got: {spans:?}");
}

#[test]
fn pass_spans_returns_nothing_for_unrelated_text() {
    assert!(pass_spans("alpha\nbeta\n", "zzzz\nyyyy").is_empty());
}

// --- find_candidates: what the diagnostics quote ---

#[test]
fn candidates_are_file_regions_with_real_line_numbers() {
    let content = "fn a() {\n        let x = 1;\n}\n\nfn b() {\n    let y = 2;\n}\n";
    let candidate = first_candidate(content, "fn b() {\n  let y = 2;\n}");
    assert_eq!((candidate.start_line, candidate.end_line), (5, 7));
    assert_eq!(candidate.text, "fn b() {\n    let y = 2;\n}");
    assert_eq!(candidate.lines_phrase(), "lines 5-7");
}

#[test]
fn candidate_text_is_the_original_bytes_not_the_query() {
    let content = "it\u{2019}s a file\n";
    let candidate = first_candidate(content, "it's a file");
    assert_eq!(candidate.text, "it\u{2019}s a file");
}

#[test]
fn middle_line_difference_yields_the_same_height_region() {
    let content = "fn main() {\n    let x = 1;\n    println!(\"x\");\n}\n";
    let old = "fn main() {\n    let x = 2;\n    println!(\"x!\");\n}";
    let candidate = first_candidate(content, old);
    assert_eq!((candidate.start_line, candidate.end_line), (1, 4));
}

#[test]
fn unrelated_text_has_no_candidates() {
    assert!(find_candidates("alpha\nbeta\n", "zzzz\nyyyy").is_empty());
    assert!(find_candidates("alpha\nbeta\n", "").is_empty());
    assert!(find_candidates("alpha\nbeta\n", "   ").is_empty());
}

#[test]
fn candidates_are_capped_and_never_duplicated() {
    let content = "old\nold\nold\nold\nold\n";
    let candidates = find_candidates(content, "old");
    assert!(candidates.len() <= MAX_CANDIDATES, "got: {candidates:?}");
    for pair in candidates.windows(2) {
        assert_ne!(
            (pair[0].start_line, pair[0].end_line),
            (pair[1].start_line, pair[1].end_line)
        );
    }
}

#[test]
fn closest_candidate_looks_past_the_passes() {
    // Nothing in the chain matches this indentation-and-content change, but the
    // similarity scan still points at the right line.
    let content = "fn main() {\n    let value = 42;\n}\n";
    let closest = find_closest_candidate(content, "let value = 43;", 50).expect("closest");
    assert_eq!(closest.start_line, 2);
    assert_eq!(closest.candidate, "    let value = 42;");
    assert!(
        closest.similarity > 0.8,
        "similarity: {}",
        closest.similarity
    );
}

#[test]
fn occurrence_positions_are_one_indexed_lines() {
    assert_eq!(
        find_occurrence_positions("dup\nmid\ndup\n", "dup"),
        vec![1, 3]
    );
    assert_eq!(
        find_occurrence_positions("dup\nmid\n", ""),
        Vec::<usize>::new()
    );
}

#[test]
fn line_phrases_read_naturally() {
    assert_eq!(lines_phrase(12, 12), "line 12");
    assert_eq!(lines_phrase(12, 15), "lines 12-15");
}

#[test]
fn similarity_is_the_lcs_ratio() {
    assert_eq!(similarity("abcd", "abcd"), 1.0);
    assert_eq!(similarity("abcd", ""), 0.0);
    assert_eq!(similarity("abcd", "abce"), 0.75);
}
