//! Feishu CardKit 2.0 rendering.
//!
//! Mirrors the Hermes (`hermes-lark-streaming`) channel strategy: keep the
//! model output as raw Markdown and hand it to Feishu inside a card `markdown`
//! element. Feishu parses the Markdown itself, so bold, italic, strikethrough,
//! links, lists, code blocks and tables all survive. Hand-converting Markdown
//! into `post` rich-text tags only ever supported a small subset and degraded
//! everything else into literal `*`, backticks and pipes.
//!
//! The preprocessing steps below are the ones Hermes applies before shipping
//! the text to the card API:
//!
//! * demote `#`..`######` headings to `####`/`#####` (Feishu only renders the
//!   deeper heading levels),
//! * escape "stray" `*` that Feishu's parser is more eager to pair than
//!   CommonMark (e.g. `2*4000+4*3000`),
//! * drop image refs that are not real Feishu `img_` keys,
//! * collapse runs of blank lines,
//! * split very long replies into several cards.

use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

/// Conservative per-card text budget, matching Hermes' `_MAX_CHUNK_CHARS`.
const MAX_CHUNK_CHARS: usize = 2400;

fn fenced_code_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"```[\s\S]*?```").expect("valid fenced-code regex"))
}

fn inline_code_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"`[^`]+`").expect("valid inline-code regex"))
}

fn image_ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"!\[([^\]]*)\]\(([^)\s]+)\)").expect("valid image regex"))
}

fn multi_newline_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\n{3,}").expect("valid newline regex"))
}

/// Build the CardKit 2.0 card that renders `markdown` natively.
pub fn markdown_card(markdown: &str) -> Value {
    json!({
        "schema": "2.0",
        "config": {"update_multi": true},
        "body": {
            "elements": [{
                "tag": "markdown",
                "content": markdown,
                "text_align": "left",
                "text_size": "normal_v2"
            }]
        }
    })
}

/// Preprocess `markdown` and turn it into one or more sendable cards.
pub fn render_markdown_cards(markdown: &str) -> Vec<Value> {
    let optimized = optimize_markdown_style(markdown);
    let escaped = escape_markdown_asterisks(&optimized);
    split_long_text(&escaped, MAX_CHUNK_CHARS)
        .into_iter()
        .map(|chunk| markdown_card(&chunk))
        .collect()
}

/// Hermes' `optimize_markdown_style`: protect code, demote headings, tidy up.
pub fn optimize_markdown_style(text: &str) -> String {
    // 1. Pull fenced code blocks out so later steps cannot rewrite them.
    let mut code_blocks: Vec<String> = Vec::new();
    let protected = fenced_code_re()
        .replace_all(text, |caps: &regex::Captures| {
            let index = code_blocks.len();
            code_blocks.push(caps[0].to_string());
            format!("___CB_{index}___")
        })
        .into_owned();

    // 2. Heading demotion, but only when the reply actually uses H1-H3.
    let mut output = if has_shallow_heading(&protected) {
        demote_headings(&protected)
    } else {
        protected
    };

    // 3. Put the code blocks back verbatim.
    for (index, block) in code_blocks.iter().enumerate() {
        output = output.replace(&format!("___CB_{index}___"), block);
    }

    // 4. Collapse 3+ newlines down to a paragraph break.
    let output = multi_newline_re().replace_all(&output, "\n\n").into_owned();

    // 5. Remove image references that are not Feishu image keys.
    strip_invalid_image_keys(&output)
}

fn has_shallow_heading(text: &str) -> bool {
    text.lines().any(|line| {
        let hashes = line.bytes().take_while(|byte| *byte == b'#').count();
        (1..=3).contains(&hashes)
            && line[hashes..].starts_with(' ')
            && !line[hashes..].trim().is_empty()
    })
}

fn demote_headings(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            output.push('\n');
        }
        let hashes = line.bytes().take_while(|byte| *byte == b'#').count();
        let rest = &line[hashes..];
        if (1..=6).contains(&hashes) && rest.starts_with(' ') {
            let body = rest.trim_start_matches(' ');
            if !body.is_empty() {
                // H1 -> ####, everything else -> #####.
                output.push_str(if hashes == 1 { "####" } else { "#####" });
                output.push(' ');
                output.push_str(body);
                continue;
            }
        }
        output.push_str(line);
    }
    output
}

fn strip_invalid_image_keys(text: &str) -> String {
    if !text.contains("![") {
        return text.to_owned();
    }
    image_ref_re()
        .replace_all(text, |caps: &regex::Captures| {
            if caps[2].starts_with("img_") {
                caps[0].to_string()
            } else {
                String::new()
            }
        })
        .into_owned()
}

/// Escape `*` that Feishu would mis-pair as emphasis.
///
/// Feishu's parser is more eager than CommonMark, so `2*4000+4*3000` turns
/// into unintentional italics. Code spans and real `**bold**` / `*italic*`
/// runs are protected; anything else is escaped with a backslash.
pub fn escape_markdown_asterisks(text: &str) -> String {
    if !text.contains('*') {
        return text.to_owned();
    }

    // 1. Collect protected ranges (fenced + inline code).
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for regex in [fenced_code_re(), inline_code_re()] {
        for found in regex.find_iter(text) {
            ranges.push((found.start(), found.end()));
        }
    }
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }

    // 2. Escape each unprotected segment, leaving code untouched.
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for (start, end) in merged {
        output.push_str(&escape_segment(&text[cursor..start]));
        output.push_str(&text[start..end]);
        cursor = end;
    }
    output.push_str(&escape_segment(&text[cursor..]));
    output
}

fn escape_segment(segment: &str) -> String {
    if !segment.contains('*') {
        return segment.to_owned();
    }
    let bytes = segment.as_bytes();

    // Runs of consecutive `*`.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'*' {
            let start = index;
            while index < bytes.len() && bytes[index] == b'*' {
                index += 1;
            }
            runs.push((start, index - start));
        } else {
            index += 1;
        }
    }

    let mut used = vec![false; runs.len()];

    // Pass 1: pair `**`/`***` runs (bold).
    let mut open = 0usize;
    while open < runs.len() {
        if runs[open].1 >= 2 && !used[open] {
            let mut close = open + 1;
            while close < runs.len() {
                if runs[close].1 >= 2 && !used[close] && valid_span(segment, &runs, open, close) {
                    used[open] = true;
                    used[close] = true;
                    open = close;
                    break;
                }
                close += 1;
            }
        }
        open += 1;
    }

    // Pass 2: pair single-`*` runs (italic).
    let mut open = 0usize;
    while open < runs.len() {
        if !used[open] {
            let mut close = open + 1;
            while close < runs.len() {
                if !used[close] && valid_italic(segment, &runs, open, close) {
                    used[open] = true;
                    used[close] = true;
                    open = close;
                    break;
                }
                close += 1;
            }
        }
        open += 1;
    }

    // Escape every `*` that is not part of a recognised emphasis span.
    let mut output = String::with_capacity(segment.len());
    let mut consumed = 0usize;
    for (run_index, (start, len)) in runs.iter().enumerate() {
        output.push_str(&segment[consumed..*start]);
        if used[run_index] {
            output.push_str(&segment[*start..*start + *len]);
        } else {
            for offset in 0..*len {
                let absolute = start + offset;
                let next = bytes.get(absolute + 1).copied();
                let previous = absolute.checked_sub(1).and_then(|i| bytes.get(i).copied());
                let escapes = matches!(next, Some(byte) if !is_space(byte) && byte != b'*')
                    && previous != Some(b'\\');
                if escapes {
                    output.push('\\');
                }
                output.push('*');
            }
        }
        consumed = start + len;
    }
    output.push_str(&segment[consumed..]);
    output
}

fn valid_span(segment: &str, runs: &[(usize, usize)], open: usize, close: usize) -> bool {
    let (open_start, open_len) = runs[open];
    let (close_start, _) = runs[close];
    if close_start <= open_start + open_len {
        return false;
    }
    let bytes = segment.as_bytes();
    matches!(bytes.get(open_start + open_len), Some(byte) if !is_space(*byte))
        && matches!(bytes.get(close_start - 1), Some(byte) if !is_space(*byte))
}

fn valid_italic(segment: &str, runs: &[(usize, usize)], open: usize, close: usize) -> bool {
    if !valid_span(segment, runs, open, close) {
        return false;
    }
    let (open_start, _) = runs[open];
    match open_start
        .checked_sub(1)
        .and_then(|i| segment.as_bytes().get(i))
    {
        Some(byte) if byte.is_ascii_alphanumeric() || *byte == b'_' => false,
        _ => true,
    }
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

/// Split oversized replies on paragraph/line boundaries, like Hermes does.
pub fn split_long_text(text: &str, limit: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return vec![text.to_owned()];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        if chars.len() - start <= limit {
            chunks.push(chars[start..].iter().collect());
            break;
        }
        let window_end = start + limit;
        let paragraph = chars[start..window_end]
            .windows(2)
            .rposition(|pair| pair == ['\n', '\n'])
            .map(|offset| start + offset + 2);
        let line = chars[start..window_end]
            .iter()
            .rposition(|ch| *ch == '\n')
            .map(|offset| start + offset + 1);
        let cut = paragraph
            .or(line)
            .filter(|cut| *cut - start >= limit / 2)
            .unwrap_or(window_end);

        chunks.push(chars[start..cut].iter().collect());
        start = cut;
        while start < chars.len() && chars[start] == '\n' {
            start += 1;
        }
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_cardkit_v2_markdown_card() {
        let card = markdown_card("# 标题\n\n**粗体**");
        assert_eq!(card["schema"], "2.0");
        assert_eq!(card["config"]["update_multi"], true);
        let element = &card["body"]["elements"][0];
        assert_eq!(element["tag"], "markdown");
        assert_eq!(element["content"], "# 标题\n\n**粗体**");
        assert_eq!(element["text_align"], "left");
        assert_eq!(element["text_size"], "normal_v2");
    }

    #[test]
    fn demotes_headings_to_feishu_levels() {
        assert_eq!(optimize_markdown_style("# 标题一"), "#### 标题一");
        assert_eq!(optimize_markdown_style("## 标题二"), "##### 标题二");
        // Hermes only rewrites headings when the reply also uses H1-H3.
        assert_eq!(optimize_markdown_style("###### 标题六"), "###### 标题六");
        assert_eq!(
            optimize_markdown_style("# 一\n\n###### 六"),
            "#### 一\n\n##### 六"
        );
    }

    #[test]
    fn keeps_headings_inside_code_blocks_intact() {
        let text = "```\n# not a heading\n```";
        assert_eq!(optimize_markdown_style(text), text);
    }

    #[test]
    fn escapes_stray_asterisks_but_keeps_emphasis() {
        assert_eq!(
            escape_markdown_asterisks("尺寸 2*4000+4*3000 毫米"),
            "尺寸 2\\*4000+4\\*3000 毫米"
        );
        assert_eq!(escape_markdown_asterisks("**粗体**"), "**粗体**");
        assert_eq!(escape_markdown_asterisks("*斜体*"), "*斜体*");
        assert_eq!(escape_markdown_asterisks("`a*b`"), "`a*b`");
    }

    #[test]
    fn drops_image_refs_without_a_feishu_key() {
        assert_eq!(
            optimize_markdown_style("前 ![x](https://example.com/a.png) 后"),
            "前  后"
        );
        assert_eq!(
            optimize_markdown_style("![x](img_v3_abc)"),
            "![x](img_v3_abc)"
        );
    }

    #[test]
    fn splits_long_replies_into_multiple_cards() {
        let text = "段落一。\n\n段落二。\n\n段落三。";
        let chunks = split_long_text(text, 8);
        assert!(chunks.len() > 1);
        assert_eq!(chunks.concat().replace('\n', ""), text.replace('\n', ""));
    }

    #[test]
    fn renders_markdown_into_sendable_cards() {
        let cards = render_markdown_cards("# 标题\n\n| a | b |\n| - | - |\n| 1 | 2 |");
        assert_eq!(cards.len(), 1);
        let content = cards[0]["body"]["elements"][0]["content"].as_str().unwrap();
        assert!(content.starts_with("#### 标题"));
        assert!(content.contains("| a | b |"));
    }
}
