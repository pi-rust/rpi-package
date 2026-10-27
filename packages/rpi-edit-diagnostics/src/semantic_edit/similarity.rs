//! Byte/grapheme-level LCS similarity — port of `src/domain/similarity.ts`
//! (`pi-semantic-edit`, MIT, (c) 2026 K2).
//!
//! Mirrors `difflib.SequenceMatcher.ratio`: `2 * LCS / (len_a + len_b)`.
//! The TS version counts UTF-16 code units; this one counts `char`s, which is
//! the closest Rust equivalent (the ratio itself is unaffected for ASCII and
//! only differs for astral-plane text, where TS counts surrogate halves).

/// `2 * LCS(a, b) / (len(a) + len(b))` in `0.0..=1.0`.
pub fn similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    (2.0 * lcs_length(&a, &b) as f64) / (a.len() + b.len()) as f64
}

/// Longest common subsequence length (space-optimized DP, one row kept).
fn lcs_length(a: &[char], b: &[char]) -> usize {
    let n = b.len();
    let mut prev = vec![0usize; n + 1];
    let mut curr = vec![0usize; n + 1];
    for i in 1..=a.len() {
        for j in 1..=n {
            curr[j] = if a[i - 1] == b[j - 1] {
                prev[j - 1] + 1
            } else {
                curr[j - 1].max(prev[j])
            };
        }
        std::mem::swap(&mut prev, &mut curr);
        curr.fill(0);
    }
    prev[n]
}
