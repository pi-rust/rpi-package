//! `/optchat import`: bring another tool's history into this session's memory.
//!
//! Upstream ships a parser per source. This module keeps the same idea but is
//! tolerant by design: a file is read as text, then as JSONL, then as one JSON
//! document, and every exchange found is taken as an imported note. Formats
//! recognised explicitly:
//!
//! - **Claude Code** — `{"type":"user"|"assistant","message":{…},"timestamp":…}`
//!   lines, as written under `~/.claude/projects/**/*.jsonl`;
//! - **ChatGPT** — a `conversations.json` export (`mapping` of message nodes);
//! - **Codex / OMP / anything else** — any JSON object that carries a role and
//!   text content, found by walking the document, plus plain text lines.
//!
//! Nothing is executed and no request is resumed: imported entries carry an
//! `[IMP <day>]` origin and are described as records in the injected pointer.

use crate::memory::{Kind, Origin};
use serde_json::Value;

/// One imported message: its kind, text and the date it was written.
#[derive(Debug)]
pub struct Imported {
    pub kind: Kind,
    pub text: String,
    pub date: Option<String>,
}

/// The source label written into each entry's origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Claude,
    Chatgpt,
    Jsonl,
    Text,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Claude => "claude-code",
            Format::Chatgpt => "chatgpt",
            Format::Jsonl => "jsonl",
            Format::Text => "text",
        }
    }

    /// `--format <name>`, or a guess from the file name and content.
    pub fn resolve(requested: Option<&str>, path: &str, text: &str) -> Option<Format> {
        if let Some(name) = requested {
            return match name.trim().to_ascii_lowercase().as_str() {
                "claude" | "claude-code" => Some(Format::Claude),
                "chatgpt" | "openai" => Some(Format::Chatgpt),
                "jsonl" | "json" | "codex" | "omp" => Some(Format::Jsonl),
                "text" | "md" | "markdown" => Some(Format::Text),
                _ => None,
            };
        }
        let lower = path.to_ascii_lowercase();
        if lower.ends_with("conversations.json") {
            return Some(Format::Chatgpt);
        }
        if text.trim_start().starts_with('[') {
            return Some(Format::Chatgpt);
        }
        if text
            .lines()
            .take(32)
            .any(|line| claude_line(line).is_some())
        {
            return Some(Format::Claude);
        }
        if text.lines().take(32).any(|line| {
            serde_json::from_str::<Value>(line.trim())
                .map(|value| value.is_object())
                .unwrap_or(false)
        }) {
            return Some(Format::Jsonl);
        }
        Some(Format::Text)
    }
}

/// The message a Claude Code JSONL line holds, if it holds one.
fn claude_line(line: &str) -> Option<Imported> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    let kind = match value.get("type").and_then(Value::as_str) {
        Some("user") => Kind::User,
        Some("assistant") => Kind::Talk,
        _ => return None,
    };
    let message = value.get("message").unwrap_or(&Value::Null);
    let text = text_of(message.get("content").unwrap_or(&Value::Null));
    if text.trim().is_empty() {
        return None;
    }
    Some(Imported {
        kind,
        text,
        date: value
            .get("timestamp")
            .and_then(Value::as_str)
            .map(day)
            .or_else(|| value.get("timestamp").and_then(Value::as_i64).map(day_ms)),
    })
}

/// Text of a content value: a string, blocks, or a nested message.
fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => {
            let mut parts: Vec<String> = Vec::new();
            for block in blocks {
                match block {
                    Value::String(text) => parts.push(text.clone()),
                    _ => {
                        let text = block
                            .get("text")
                            .and_then(Value::as_str)
                            .or_else(|| block.get("content").and_then(Value::as_str))
                            .unwrap_or_default();
                        if !text.trim().is_empty() {
                            parts.push(text.to_string());
                        }
                    }
                }
            }
            parts.join("\n")
        }
        Value::Object(map) => map
            .get("content")
            .map(text_of)
            .or_else(|| map.get("text").and_then(Value::as_str).map(str::to_string))
            // ChatGPT content: `{"content_type":"text","parts":["…"]}`.
            .or_else(|| map.get("parts").map(text_of))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn day(text: &str) -> String {
    text.chars().take(10).collect()
}

fn day_ms(ms: i64) -> String {
    day(&crate::memory::iso8601(ms))
}

/// A role name as memory reads it.
fn kind_of(role: &str) -> Option<Kind> {
    match role.to_ascii_lowercase().as_str() {
        "user" | "human" => Some(Kind::User),
        "assistant" | "ai" | "model" | "agent" => Some(Kind::Talk),
        "system" | "tool" | "function" => None,
        _ => None,
    }
}

/// Whatever a generic JSON object offers as one exchange.
fn generic(value: &Value) -> Option<Imported> {
    let map = value.as_object()?;
    // ChatGPT nodes wrap the exchange: `{"id":…,"message":{…},"parent":…}`.
    if let Some(message) = map.get("message") {
        if let Some(found) = generic(message) {
            return Some(found);
        }
    }
    let role = map
        .get("role")
        .and_then(Value::as_str)
        .or_else(|| {
            map.get("author")
                .and_then(|author| author.get("role"))
                .and_then(Value::as_str)
        })
        .or_else(|| map.get("type").and_then(Value::as_str))?;
    let kind = kind_of(role)?;
    let text = map
        .get("content")
        .map(text_of)
        .filter(|text| !text.trim().is_empty())
        .or_else(|| {
            map.get("message")
                .and_then(|message| message.get("content"))
                .map(text_of)
                .filter(|text| !text.trim().is_empty())
        })
        .or_else(|| {
            map.get("payload")
                .and_then(|payload| payload.get("content"))
                .map(text_of)
                .filter(|text| !text.trim().is_empty())
        })?;
    let date = map
        .get("create_time")
        .and_then(Value::as_f64)
        .map(|seconds| day_ms((seconds * 1000.0) as i64))
        .or_else(|| map.get("timestamp").and_then(Value::as_str).map(day));
    Some(Imported { kind, text, date })
}

/// Whether a document is a ChatGPT export (a mapping) rather than a flat log.
fn wrapped_or_mapping(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(wrapped_or_mapping),
        Value::Object(map) => map.contains_key("mapping") || map.contains_key("message"),
        _ => false,
    }
}

/// Walk a document collecting generic exchanges. Objects are visited in
/// insertion order where serde keeps it; the ChatGPT caller sorts by timestamp
/// anyway, which is the only reliable order for a mapping export.
fn walk(value: &Value, out: &mut Vec<Imported>, budget: usize, depth: usize) {
    if out.len() >= budget || depth > 32 {
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                walk(item, out, budget, depth + 1);
                if out.len() >= budget {
                    return;
                }
            }
        }
        Value::Object(map) => {
            if let Some(found) = generic(value) {
                out.push(found);
                return;
            }
            let _ = map;
            for child in value
                .as_object()
                .map(|map| map.values())
                .into_iter()
                .flatten()
            {
                walk(child, out, budget, depth + 1);
                if out.len() >= budget {
                    return;
                }
            }
        }
        _ => {}
    }
}

/// `create_time` of the exchange a value holds, directly or under `message`.
fn create_time(value: &Value) -> Option<f64> {
    let map = value.as_object()?;
    map.get("create_time")
        .and_then(Value::as_f64)
        .or_else(|| map.get("message").and_then(create_time))
}

/// Walk collecting timestamps too, so a mapping export can be ordered.
fn walk_timed(value: &Value, out: &mut Vec<(Option<f64>, Imported)>, budget: usize, depth: usize) {
    if out.len() >= budget || depth > 32 {
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                walk_timed(item, out, budget, depth + 1);
                if out.len() >= budget {
                    return;
                }
            }
        }
        Value::Object(map) => {
            let time = create_time(value);
            if let Some(found) = generic(value) {
                out.push((time, found));
                return;
            }
            for child in map.values() {
                walk_timed(child, out, budget, depth + 1);
                if out.len() >= budget {
                    return;
                }
            }
        }
        _ => {}
    }
}

/// Parse `text` as `format`, bounded by `budget` messages.
pub fn parse(format: Format, text: &str, budget: usize) -> Vec<Imported> {
    let mut out: Vec<Imported> = Vec::new();
    match format {
        Format::Claude => {
            for line in text.lines() {
                if out.len() >= budget {
                    break;
                }
                if let Some(item) = claude_line(line) {
                    out.push(item);
                }
            }
        }
        Format::Chatgpt | Format::Jsonl => {
            // A whole document first: a JSON array or object (ChatGPT export,
            // a Codex session file). Falling back to JSONL keeps a per-line file
            // working even when one line is a complete document of its own.
            let documents: Vec<Value> = match serde_json::from_str::<Value>(text) {
                Ok(document) => vec![document],
                Err(_) => text
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
                    .collect(),
            };
            if documents.iter().any(wrapped_or_mapping) {
                let mut timed: Vec<(Option<f64>, Imported)> = Vec::new();
                for document in &documents {
                    walk_timed(document, &mut timed, budget, 0);
                }
                // A mapping export is ordered by timestamp, not by node id.
                timed.sort_by(|left, right| {
                    left.0
                        .unwrap_or(f64::MAX)
                        .partial_cmp(&right.0.unwrap_or(f64::MAX))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                for (time, mut item) in timed {
                    if item.date.is_none() {
                        item.date = time.map(|seconds| day_ms((seconds * 1000.0) as i64));
                    }
                    out.push(item);
                }
            } else {
                for document in &documents {
                    walk(document, &mut out, budget, 0);
                }
            }
        }
        Format::Text => {
            for line in text.lines() {
                if out.len() >= budget {
                    break;
                }
                let body = crate::import_line(line);
                if !body.trim().is_empty() {
                    out.push(Imported {
                        kind: Kind::Note,
                        text: body,
                        date: None,
                    });
                }
            }
        }
    }
    out
}

/// The origin stamped on an imported message.
pub fn origin(format: Format, conversation: &str, message: usize, date: Option<&str>) -> Origin {
    Origin {
        source: format.as_str().to_string(),
        conversation: conversation.to_string(),
        message: message.to_string(),
        title: date.unwrap_or_default().to_string(),
        project: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_lines_yield_user_and_assistant_messages() {
        let text = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"fix the parser"}]},"timestamp":"2026-02-03T04:05:06Z"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"done"}]},"timestamp":"2026-02-03T04:06:00Z"}
{"type":"summary","summary":"x"}"#;
        assert_eq!(
            Format::resolve(None, "abc.jsonl", text),
            Some(Format::Claude)
        );
        let items = parse(Format::Claude, text, 10);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, Kind::User);
        assert_eq!(items[0].text, "fix the parser");
        assert_eq!(items[0].date.as_deref(), Some("2026-02-03"));
        assert_eq!(items[1].kind, Kind::Talk);
        assert_eq!(items[1].text, "done");
    }

    #[test]
    fn a_chatgpt_export_walks_the_mapping_in_order() {
        let text = r#"[{"title":"t","mapping":{
            "a":{"message":{"author":{"role":"user"},"content":{"content_type":"text","parts":["hello"]},"create_time":1700000000.0}},
            "b":{"message":{"author":{"role":"assistant"},"content":{"content_type":"text","parts":["hi"]},"create_time":1700000001.0}}}}]"#;
        assert_eq!(
            Format::resolve(None, "conversations.json", text),
            Some(Format::Chatgpt)
        );
        let items = parse(Format::Chatgpt, text, 10);
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(items[0].kind, Kind::User);
        assert_eq!(items[1].kind, Kind::Talk);
        assert_eq!(items[1].date.as_deref(), Some("2023-11-14"));
    }

    #[test]
    fn generic_jsonl_and_plain_text_still_import() {
        let jsonl = "{\"role\":\"user\",\"content\":\"one\"}\n{\"role\":\"assistant\",\"content\":\"two\"}\n";
        assert_eq!(
            Format::resolve(None, "session.jsonl", jsonl),
            Some(Format::Jsonl)
        );
        let items = parse(Format::Jsonl, jsonl, 10);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, Kind::User);

        let text = "## Decisions\n- keep the log bounded\n\nnotes";
        assert_eq!(Format::resolve(None, "notes.md", text), Some(Format::Text));
        let items = parse(Format::Text, text, 10);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].text, "Decisions");
        assert_eq!(items[1].text, "- keep the log bounded");
        assert_eq!(items[0].kind, Kind::Note);
    }

    #[test]
    fn parse_is_bounded_and_an_unknown_format_is_refused() {
        let text = (0..50)
            .map(|i| format!("{{\"role\":\"user\",\"content\":\"m{i}\"}}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parse(Format::Jsonl, &text, 5).len(), 5);
        assert_eq!(Format::resolve(Some("nope"), "x", ""), None);
        assert_eq!(
            Format::resolve(Some("claude"), "x", ""),
            Some(Format::Claude)
        );
    }
}
