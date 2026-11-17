//! OptChat's memory core: an append-only chat log, a binary summary tree over
//! it, and the bounded view the tree renders.
//!
//! Ported from `jonaslsaa/pi-optchat` (`src/core/memory.ts`) without its
//! Pi/Node dependencies. This module owns no host or ABI types, so it can be
//! tested as plain Rust and stays reusable by any adapter.
//!
//! Deliberate differences from upstream, both recorded in the package README:
//!
//! - **Summary lines are extractive.** Upstream asks a model to compress every
//!   leaf and every merge; a plugin cannot block a turn on a nested model call,
//!   so a leaf keeps the head of its message and a merge keeps the head of both
//!   halves, cut at [`NODE`] bytes. That is the fallback upstream itself uses
//!   when a compressor refuses, and the log keeps every message whole, so
//!   `zoom(id, 1)` still returns the original text.
//! - **Summary nodes are not persisted.** The merge order is a pure function of
//!   the log, so a resumed session rebuilds the identical tree from its entries.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Bytes one summary line may occupy in a view.
pub const NODE: usize = 512;
/// Default view budget in bytes.
pub const VIEW: usize = 128_000;
/// Tool-output ceiling for a whole result.
pub const CAP: usize = 30_000;
/// The most bytes of one message the tree's leaf keeps (a huge paste must not
/// dominate the log).
pub const SOURCE: usize = 100_000;
/// The most characters one `zoom` page returns.
pub const PAGE: usize = 25_000;
/// Search hits per page.
pub const SEARCH_PAGE: usize = 20;
/// Bounded log length; capture stops here instead of growing without limit.
pub const MAX_ENTRIES: usize = 20_000;

const SNIPPET: usize = 200;
const UNBUILT: &str = "(not summarized yet: zoom it)";
const VIEW_OPEN: &str = "<chat>\n";
const VIEW_CLOSE: &str = "</chat>";
const VIEW_FRAME_BYTES: usize = VIEW_OPEN.len() + VIEW_CLOSE.len();

/// A message's kind, as the model reads it in a view line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The user's words.
    User,
    /// The agent's reply.
    Talk,
    /// A tool call.
    Tool,
    /// A tool result.
    Echo,
    /// A memory recorded outside the chat (note, imported history).
    Note,
    /// A subagent's report.
    Work,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::User => "user",
            Kind::Talk => "talk",
            Kind::Tool => "tool",
            Kind::Echo => "echo",
            Kind::Note => "note",
            Kind::Work => "work",
        }
    }
}

/// Marks an entry as imported history: the summary treats it as historical, not
/// as a new request. `source` is a label the importer chooses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Origin {
    pub source: String,
    pub conversation: String,
    pub message: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// One logged message, whole.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub i: usize,
    pub kind: Kind,
    pub text: String,
    /// ISO-8601 UTC, e.g. `2026-02-03T04:05:06Z`.
    pub date: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

/// A node of the summary tree: `l` levels up, covering `2^l` messages from `i`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Part {
    pub l: u32,
    pub i: u64,
}

impl Part {
    pub fn start(self) -> u64 {
        self.i << self.l
    }

    pub fn end(self) -> u64 {
        self.start() + (1u64 << self.l)
    }

    pub fn len(self) -> u64 {
        1u64 << self.l
    }

    /// `start+N`, the line's head.
    pub fn label(self) -> String {
        format!("{}+{}", self.start(), self.len())
    }
}

/// View and log statistics for `/optchat status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    pub messages: usize,
    pub lines: usize,
    pub bytes: usize,
}

/// The head a message shows as in a view line, leaf and search: its kind, after
/// `[IMP <day>]` for imported history.
pub fn head(entry: &Entry) -> String {
    match &entry.origin {
        Some(_) => format!("[IMP {}] {}", short_date(&entry.date), entry.kind.as_str()),
        None => entry.kind.as_str().to_string(),
    }
}

/// `2026-02-03` from an ISO-8601 date.
pub fn short_date(date: &str) -> &str {
    date.get(..10).unwrap_or(date)
}

/// Newlines as spaces: a view line stays one line.
pub fn flat(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = false;
    for ch in text.chars() {
        if ch == '\r' || ch == '\n' {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
            continue;
        }
        last_space = ch == ' ';
        out.push(ch);
    }
    out
}

/// The longest prefix of `text` within `max` bytes, cut on a character boundary.
pub fn head_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// `text` clipped to `max` bytes with a trailing ellipsis, never splitting a
/// character.
pub fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    if max <= '…'.len_utf8() {
        return String::new();
    }
    let mut out = head_bytes(text, max - '…'.len_utf8()).to_string();
    out.push('…');
    out
}

/// The leaf line for one message: its kind and the head of its words.
fn leaf_text(entry: &Entry) -> String {
    let body = flat(&entry.text);
    clip(&format!("{}: {}", head(entry), body.trim()), NODE)
}

/// A merge of two adjacent lines, balanced so neither half is dropped.
fn merge_text(left: &str, right: &str) -> String {
    const MARKER: &str = " ⋯ ";
    let budget = NODE.saturating_sub(MARKER.len());
    if budget < 8 {
        return clip(&format!("{left}{MARKER}{right}"), NODE);
    }
    let half = budget / 2;
    let left = clip(left, half);
    let right = clip(right, budget - left.len());
    format!("{left}{MARKER}{right}")
}

/// The most due pair of sibling lines: as due as long ago it ended, in its own
/// line size; equal pairs go oldest first.
fn most_due(view: &[Part], total: u64) -> Option<usize> {
    let mut best = None;
    let mut due = f64::NEG_INFINITY;
    for j in 0..view.len().saturating_sub(1) {
        let (a, b) = (view[j], view[j + 1]);
        if a.l != b.l || a.i % 2 != 0 || b.i != a.i + 1 {
            continue;
        }
        let age = (total as f64 - (b.end() as f64 - 1.0)) / a.len() as f64;
        if age > due {
            due = age;
            best = Some(j);
        }
    }
    best
}

/// One page of `text`, measured in Unicode scalar values, never splitting a
/// character, and saying where the next one starts.
pub fn page(text: &str, offset: usize, limit: usize) -> Result<String, String> {
    let limit = limit.clamp(1, PAGE);
    let total = text.chars().count();
    if offset > total {
        return Err(format!(
            "This message has {total} characters; offset must be 0 to {total}."
        ));
    }
    let to_char = offset.saturating_add(limit).min(total);
    let from = text
        .char_indices()
        .nth(offset)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len());
    let to = text
        .char_indices()
        .nth(to_char)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len());
    let more = if to_char < total {
        format!("; go on with offset {to_char}")
    } else {
        String::new()
    };
    Ok(format!(
        "{}\n[characters {offset}-{to_char} of {total}{more}]",
        &text[from..to]
    ))
}

/// The chat log, its summary tree and its view.
#[derive(Debug)]
pub struct Memory {
    entries: Vec<Entry>,
    tree: HashMap<(u32, u64), String>,
    view: Vec<Part>,
    measured: usize,
    merging: bool,
    budget: usize,
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl Memory {
    pub fn new() -> Self {
        Self::with_budget(VIEW)
    }

    pub fn with_budget(budget: usize) -> Self {
        Self {
            entries: Vec::new(),
            tree: HashMap::new(),
            view: Vec::new(),
            measured: 0,
            merging: false,
            budget: budget.max(NODE * 2),
        }
    }

    /// Rebuild memory from saved entries. Indices are normalized to the log
    /// order: a branch that lost or duplicated an entry (a hand-edited file, a
    /// stale import) still yields a consistent tree instead of refusing to open.
    pub fn load(mut entries: Vec<Entry>) -> Self {
        entries.sort_by_key(|entry| entry.i);
        entries.dedup_by_key(|entry| entry.i);
        let mut memory = Self::new();
        for (index, mut entry) in entries.into_iter().enumerate() {
            entry.i = index;
            memory.push_entry(entry);
            memory.fit(memory.entries.len() as u64, false);
        }
        memory.fit(memory.entries.len() as u64, false);
        memory
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, i: u64) -> Option<&Entry> {
        self.entries.get(usize::try_from(i).ok()?)
    }

    pub fn stats(&self) -> Stats {
        Stats {
            messages: self.entries.len(),
            lines: self.view.len(),
            bytes: self.view_bytes(),
        }
    }

    /// Build the next entry without mutating the log. Callers that need durable
    /// semantics can persist this value before committing it with
    /// [`Self::append_prepared`].
    pub fn prepare(
        &self,
        kind: Kind,
        text: String,
        date: String,
        receipt: Option<String>,
        origin: Option<Origin>,
    ) -> Entry {
        Entry {
            i: self.entries.len(),
            kind,
            text: head_bytes(&text, SOURCE).to_string(),
            date,
            receipt,
            origin,
        }
    }

    /// Commit an entry previously returned by [`Self::prepare`].
    pub fn append_prepared(&mut self, entry: Entry) -> Result<Entry, String> {
        if entry.i != self.entries.len() {
            return Err(format!(
                "cannot append message {} to a log with {} messages",
                entry.i,
                self.entries.len()
            ));
        }
        self.push_entry(entry.clone());
        self.fit(self.entries.len() as u64, false);
        Ok(entry)
    }

    /// Append one message and fold the view if it outgrew its budget.
    #[cfg(test)]
    pub fn record(
        &mut self,
        kind: Kind,
        text: String,
        date: String,
        receipt: Option<String>,
        origin: Option<Origin>,
    ) -> Entry {
        self.append_prepared(self.prepare(kind, text, date, receipt, origin))
            .expect("a freshly prepared entry must append at the end of the log")
    }

    /// Merge as far as the tree allows: the smallest view that still tiles the
    /// log. `/optchat fit` exposes this to the user; the automatic sawtooth stays
    /// inside its budget without collapsing history on its own.
    pub fn collapse(&mut self) {
        let total = self.entries.len() as u64;
        while let Some(index) = most_due(&self.view, total) {
            self.merge_at(index);
        }
        self.merging = false;
    }

    fn push_entry(&mut self, entry: Entry) {
        let part = Part {
            l: 0,
            i: entry.i as u64,
        };
        self.tree.insert((0, part.i), leaf_text(&entry));
        self.entries.push(entry);
        self.view.push(part);
        self.measured += self.line_size(part);
    }

    fn line_size(&self, part: Part) -> usize {
        let head = format!("{}|\n", part.label()).len();
        let body = self
            .tree
            .get(&(part.l, part.i))
            .map(String::len)
            .unwrap_or(UNBUILT.len());
        head + body
    }

    fn view_bytes(&self) -> usize {
        VIEW_FRAME_BYTES + self.measured + usize::from(self.view.is_empty())
    }

    /// Grow a line per message and, once over the budget or when forced, merge
    /// the most due pairs in one batch down to half of it.
    fn fit(&mut self, total: u64, force: bool) {
        let low = self.budget / 2;
        if force || self.view_bytes() > self.budget {
            self.merging = true;
        }
        while self.merging && self.view_bytes() > low {
            let Some(index) = most_due(&self.view, total) else {
                break;
            };
            self.merge_at(index);
        }
        if self.view_bytes() <= low {
            self.merging = false;
        }
    }

    fn merge_at(&mut self, index: usize) {
        let (left, right) = (self.view[index], self.view[index + 1]);
        let parent = Part {
            l: left.l + 1,
            i: left.i / 2,
        };
        let left_text = self.text_of(left).to_string();
        let right_text = self.text_of(right).to_string();
        let before = self.line_size(left) + self.line_size(right);
        self.tree
            .insert((parent.l, parent.i), merge_text(&left_text, &right_text));
        self.view.splice(index..index + 2, [parent]);
        self.measured = self.measured.saturating_sub(before) + self.line_size(parent);
    }

    fn text_of(&self, part: Part) -> &str {
        self.tree
            .get(&(part.l, part.i))
            .map(String::as_str)
            .unwrap_or(UNBUILT)
    }

    /// The view line of a node, `id+n|text`.
    pub fn render(&self, part: Part) -> String {
        format!("{}|{}", part.label(), self.text_of(part))
    }

    /// The line covering message `at`.
    pub fn covering(&self, at: u64) -> Option<Part> {
        let mut lo = 0;
        let mut hi = self.view.len() as isize - 1;
        while lo <= hi {
            let mid = ((lo + hi) / 2) as usize;
            let part = self.view[mid];
            if part.end() <= at {
                lo = mid as isize + 1;
            } else if part.start() > at {
                hi = mid as isize - 1;
            } else {
                return Some(part);
            }
        }
        None
    }

    /// The whole view, oldest first, in `<chat>` tags.
    pub fn view_text(&self) -> String {
        let mut out = String::with_capacity(self.view_bytes());
        out.push_str(VIEW_OPEN);
        for part in &self.view {
            out.push_str(&self.render(*part));
            out.push('\n');
        }
        if self.view.is_empty() {
            out.push('\n');
        }
        out.push_str(VIEW_CLOSE);
        out
    }

    /// The newest `count` view lines, for a terminal that cannot show 128 KB.
    pub fn view_tail(&self, count: usize) -> String {
        let count = count.max(1);
        let skip = self.view.len().saturating_sub(count);
        let mut out = String::new();
        if skip > 0 {
            out.push_str(&format!("[{skip} older lines omitted]\n"));
        }
        for part in &self.view[skip..] {
            out.push_str(&self.render(*part));
            out.push('\n');
        }
        if self.view.is_empty() {
            out.push_str("(memory is empty)\n");
        }
        out
    }

    /// Open the line `id+n` into what it was made of: `n = 1` gives message
    /// `id` whole (paged), a larger power of two gives its two halves.
    pub fn zoom(&self, id: u64, n: u32, offset: usize, limit: usize) -> Result<String, String> {
        if n == 0 || !n.is_power_of_two() {
            return Err(format!(
                "n must be a power of two (1, 2, 4, 8, ...), not {n}"
            ));
        }
        if id % u64::from(n) != 0 {
            return Err(format!(
                "id must be a multiple of n: {id} is not a multiple of {n}"
            ));
        }
        let part = Part {
            l: n.trailing_zeros(),
            i: id / u64::from(n),
        };
        if part.l == 0 {
            let entry = self.get(id).ok_or_else(|| {
                format!(
                    "No message {id}: memory holds messages 0..{}",
                    self.entries.len()
                )
            })?;
            return Ok(format!(
                "{id} · {} · {}\n{}",
                short_date(&entry.date),
                head(entry),
                page(&entry.text, offset, limit)?
            ));
        }
        if !self.tree.contains_key(&(part.l, part.i)) {
            return Err(format!(
                "No summary line {}: memory holds {} messages in {} view lines",
                part.label(),
                self.entries.len(),
                self.view.len()
            ));
        }
        let left = Part {
            l: part.l - 1,
            i: part.i * 2,
        };
        let right = Part {
            l: part.l - 1,
            i: part.i * 2 + 1,
        };
        Ok(format!(
            "{}\n{}\n{}\n\nzoom({}, {}) opens either half; zoom({}, 1) opens a message whole.",
            self.render(part),
            self.render(left),
            self.render(right),
            left.start(),
            left.len(),
            left.start()
        ))
    }

    /// One page of hits: id, the view line holding it, its date and a snippet
    /// around the first match, newest first.
    pub fn search(&self, text: &str, before: Option<u64>) -> String {
        let needle = text.trim();
        if needle.is_empty() {
            return "Search needs some text to look for.".to_string();
        }
        let lower = needle.to_lowercase();
        let hits: Vec<&Entry> = self
            .entries
            .iter()
            .rev()
            .filter(|entry| before.map(|seq| (entry.i as u64) < seq).unwrap_or(true))
            .filter(|entry| find_ci(&entry.text, &lower).is_some())
            .collect();
        let older = if before.is_some() { "older " } else { "" };
        if hits.is_empty() {
            return format!("No {older}messages contain \"{needle}\".");
        }
        let total = hits.len();
        let page = &hits[..total.min(SEARCH_PAGE)];
        let mut out = format!(
            "{total} {older}{} \"{needle}\", newest first:\n",
            if total == 1 {
                "message contains"
            } else {
                "messages contain"
            }
        );
        let mut lines = Vec::with_capacity(page.len());
        for entry in page {
            let at = find_ci(&entry.text, &lower).unwrap_or(0);
            let snippet = snippet(&entry.text, at);
            let line = self.covering(entry.i as u64);
            let inside = line
                .filter(|part| part.l > 0)
                .map(|part| format!(" (in {})", part.label()))
                .unwrap_or_default();
            lines.push(format!(
                "{}{inside} · {} · {}: {}{snippet}{}",
                entry.i,
                short_date(&entry.date),
                head(entry),
                if at > 0 { "…" } else { "" },
                if at + SNIPPET < entry.text.len() {
                    "…"
                } else {
                    ""
                }
            ));
        }
        out.push_str(&lines.join("\n"));
        if total > page.len() {
            let last = page.last().expect("a non-empty page");
            out.push_str(&format!(
                "\nOlder matches: search again with before: {}.",
                last.i
            ));
        }
        out
    }
}

/// Byte index of the first case-insensitive match of `needle_lower` in `text`.
fn find_ci(text: &str, needle_lower: &str) -> Option<usize> {
    if needle_lower.is_empty() {
        return Some(0);
    }
    let mut lower = String::with_capacity(text.len());
    let mut map: Vec<(usize, usize)> = Vec::new();
    for (original, ch) in text.char_indices() {
        map.push((lower.len(), original));
        for folded in ch.to_lowercase() {
            lower.push(folded);
        }
    }
    let at = lower.find(needle_lower)?;
    map.iter()
        .rev()
        .find(|(folded, _)| *folded <= at)
        .map(|(_, original)| *original)
        .or(Some(0))
}

/// Around `at`, at most [`SNIPPET`] bytes, flattened.
fn snippet(text: &str, at: usize) -> String {
    let mut from = at.saturating_sub(SNIPPET / 4);
    while from > 0 && !text.is_char_boundary(from) {
        from -= 1;
    }
    if text.len() <= SNIPPET {
        return flat(text);
    }
    let window = head_bytes(&text[from..], SNIPPET);
    flat(window)
}

/// `2026-02-03T04:05:06Z` from epoch milliseconds.
pub fn iso8601(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to a civil date.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(i: usize, kind: Kind, text: &str) -> Entry {
        Entry {
            i,
            kind,
            text: text.to_string(),
            date: iso8601(1_700_000_000_000 + i as i64 * 1000),
            receipt: None,
            origin: None,
        }
    }

    fn memory(entries: Vec<Entry>) -> Memory {
        Memory::load(entries)
    }

    #[test]
    fn formats_and_parses_iso_dates() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_700_000_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(iso8601(-86_400_000), "1969-12-31T00:00:00Z");
        assert_eq!(short_date("2023-11-14T22:13:20Z"), "2023-11-14");
    }

    #[test]
    fn clip_and_head_never_split_a_character() {
        let text = "汉字很";
        assert_eq!(head_bytes(text, 3), "汉");
        assert_eq!(head_bytes(text, 4), "汉");
        assert_eq!(head_bytes(text, 0), "");
        assert_eq!(clip(text, 3), "");
        // Two bytes cannot hold one three-byte character plus an ellipsis.
        assert_eq!(clip(text, 5), "…");
        assert_eq!(clip(text, 6), "汉…");
        assert!(clip(&"a".repeat(100), NODE).len() <= NODE);
        assert_eq!(clip("short", NODE), "short");
        // A four-byte emoji is never halved.
        let emoji = "🚀🚀🚀";
        assert_eq!(head_bytes(emoji, 5), "🚀");
        assert_eq!(clip(emoji, 5), "…");
    }

    #[test]
    fn flat_keeps_one_line_and_collapses_newline_runs() {
        assert_eq!(flat("a\r\n\nb"), "a b");
        assert_eq!(flat("a b"), "a b");
        assert_eq!(flat("行一\n行二"), "行一 行二");
    }

    #[test]
    fn page_uses_character_offsets_for_multibyte_text() {
        let first = page("汉字abc", 1, 2).unwrap();
        assert!(first.starts_with("字a\n"), "{first}");
        assert!(
            first.contains("characters 1-3 of 5; go on with offset 3"),
            "{first}"
        );
        assert_eq!(page("汉字abc", 5, 1).unwrap(), "\n[characters 5-5 of 5]");
        assert!(page("汉字abc", 6, 1).is_err());
    }

    #[test]
    fn prepared_entries_are_not_visible_until_committed() {
        let mut m = Memory::new();
        let entry = m.prepare(Kind::Note, "durable".into(), iso8601(0), None, None);
        assert_eq!(m.len(), 0);
        assert_eq!(m.append_prepared(entry.clone()).unwrap().i, 0);
        assert_eq!(m.entries()[0].text, "durable");
        assert!(m.append_prepared(entry).is_err());
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn zoom_returns_a_message_whole_and_pages_it() {
        let mut m = Memory::new();
        m.record(Kind::User, "hello".into(), iso8601(0), None, None);
        let text = m.zoom(0, 1, 0, PAGE).unwrap();
        assert!(text.contains("user"), "{text}");
        assert!(text.contains("hello"), "{text}");
        assert!(text.contains("0 · 1970-01-01"), "{text}");

        let long = "x".repeat(PAGE + 10);
        m.record(Kind::Talk, long, iso8601(1000), None, None);
        let first = m.zoom(1, 1, 0, PAGE).unwrap();
        assert!(
            first.contains(&format!("go on with offset {PAGE}")),
            "{first}"
        );
        let second = m.zoom(1, 1, PAGE, PAGE).unwrap();
        assert!(second.contains("characters "), "{second}");
        assert!(m.zoom(1, 1, PAGE * 2 + 100, PAGE).is_err());
    }

    #[test]
    fn zoom_rejects_non_powers_and_misaligned_ids() {
        let m = memory(vec![entry(0, Kind::User, "a"), entry(1, Kind::User, "b")]);
        assert!(m.zoom(0, 0, 0, PAGE).is_err());
        assert!(m.zoom(0, 3, 0, PAGE).is_err());
        assert!(m.zoom(1, 2, 0, PAGE).is_err());
        assert!(m.zoom(9, 1, 0, PAGE).is_err());
    }

    #[test]
    fn merge_tree_folds_the_view_under_its_budget_and_zooms_into_halves() {
        let mut m = Memory::with_budget(NODE * 4);
        for i in 0..64 {
            m.record(
                Kind::User,
                format!("message number {i} {}", "x".repeat(60)),
                iso8601(i * 1000),
                None,
                None,
            );
        }
        let stats = m.stats();
        assert!(stats.lines < 64, "{stats:?}");
        assert!(stats.bytes <= NODE * 4, "{stats:?}");

        // The view always tiles the log: the last line ends at the last message.
        assert_eq!(m.view.last().expect("a line").end(), 64);
        assert!(m.view_text().contains('|'));

        // The oldest block is a merge we can open into its two halves.
        let block = m.view[0];
        if block.l > 0 {
            let opened = m.zoom(block.start(), 1 << block.l, 0, PAGE).unwrap();
            assert!(opened.contains(&format!("{}+{}|", block.start(), block.len())));
            let left = Part {
                l: block.l - 1,
                i: block.i * 2,
            };
            assert!(opened.contains(&left.label()), "{opened}");
        }
        // Every message stays readable whole regardless of folding.
        assert!(m
            .zoom(63, 1, 0, PAGE)
            .unwrap()
            .contains("message number 63"));
    }

    #[test]
    fn collapse_merges_everything_reachable() {
        let mut m = Memory::with_budget(VIEW);
        for i in 0..8 {
            m.record(Kind::Talk, format!("m{i}"), iso8601(i * 1000), None, None);
        }
        m.collapse();
        assert_eq!(m.stats().lines, 1);
        assert_eq!(m.view[0], Part { l: 3, i: 0 });
        assert!(m.view_text().starts_with(VIEW_OPEN));

        // A log whose length is not a power of two keeps the remainder as lines.
        let mut m = Memory::with_budget(VIEW);
        for i in 0..11 {
            m.record(Kind::Talk, format!("m{i}"), iso8601(i * 1000), None, None);
        }
        m.collapse();
        assert_eq!(m.stats().lines, 3);
        assert_eq!(m.view[0], Part { l: 3, i: 0 });
        assert_eq!(m.view[1], Part { l: 1, i: 4 });
        assert_eq!(m.view[2], Part { l: 0, i: 10 });
        // Every message is still readable whole after collapsing.
        assert!(m.zoom(10, 1, 0, PAGE).unwrap().contains("m10"));
    }

    #[test]
    fn line_sizes_stay_within_the_node_ceiling() {
        let mut m = Memory::with_budget(NODE * 4);
        for i in 0..40 {
            m.record(Kind::User, "中".repeat(400), iso8601(i * 1000), None, None);
        }
        for part in &m.view {
            let text = m.text_of(*part);
            assert!(
                text.len() <= NODE,
                "{} bytes for {}",
                text.len(),
                part.label()
            );
            assert!(!text.is_empty());
        }
    }

    #[test]
    fn search_finds_hits_newest_first_and_continues_older() {
        let m = memory(vec![
            entry(0, Kind::User, "fix the parser bug"),
            entry(1, Kind::Talk, "unrelated"),
            entry(2, Kind::Echo, "parser bug fixed again"),
        ]);
        let first = m.search("PARSER BUG", None);
        assert!(first.contains("2 "), "{first}");
        assert!(first.contains("0 "), "{first}");
        assert!(!first.contains("1 "), "{first}");
        let older = m.search("parser bug", Some(2));
        assert!(older.contains("older message"), "{older}");
        assert!(older.contains("0 "), "{older}");
        assert!(m
            .search("nothing at all", None)
            .contains("No messages contain"));
    }

    #[test]
    fn search_snippets_are_bounded_and_survive_multibyte_text() {
        let mut text = "汉".repeat(400);
        text.push_str("needle");
        let m = memory(vec![entry(0, Kind::Note, &text)]);
        let page = m.search("needle", None);
        assert!(page.contains("needle"), "{page}");
        assert!(page.len() < 1_000, "{page}");
    }

    #[test]
    fn load_normalizes_indices_and_rebuilds_the_same_view() {
        let entries = vec![
            entry(7, Kind::User, "third"),
            entry(3, Kind::User, "first"),
            entry(5, Kind::Talk, "second"),
            entry(5, Kind::Talk, "second"),
        ];
        let m = Memory::load(entries);
        assert_eq!(m.len(), 3);
        assert_eq!(m.entries()[0].text, "first");
        assert_eq!(m.entries()[2].i, 2);
        assert_eq!(m.view.len(), 3);
        let reloaded = Memory::load(m.entries().to_vec());
        assert_eq!(reloaded.view_text(), m.view_text());
    }

    #[test]
    fn covering_finds_the_line_holding_a_message() {
        let mut m = Memory::with_budget(NODE * 4);
        for i in 0..32 {
            m.record(Kind::Talk, format!("m{i}"), iso8601(i * 1000), None, None);
        }
        for i in 0..32u64 {
            let part = m.covering(i).expect("covered");
            assert!(part.start() <= i && i < part.end(), "{part:?} {i}");
            assert!(part.len().is_power_of_two());
        }
        assert!(m.covering(32).is_none());
    }

    #[test]
    fn imported_entries_are_marked_in_the_view_head() {
        let origin = Origin {
            source: "import".into(),
            conversation: "notes.md".into(),
            message: "4".into(),
            title: String::new(),
            project: None,
        };
        let mut m = Memory::new();
        m.record(
            Kind::Note,
            "remembered".into(),
            "2024-05-06T07:08:09Z".into(),
            None,
            Some(origin),
        );
        assert!(m.view_text().contains("[IMP 2024-05-06] note: remembered"));
    }

    #[test]
    fn record_caps_one_message_at_the_source_ceiling() {
        let mut m = Memory::new();
        m.record(Kind::User, "a".repeat(SOURCE + 10), iso8601(0), None, None);
        assert_eq!(m.entries()[0].text.len(), SOURCE);
        assert!(m.view_text().len() < NODE * 4);
    }
}
