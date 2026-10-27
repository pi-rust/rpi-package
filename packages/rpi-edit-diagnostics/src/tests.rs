//! Plugin-level coverage of the `edit` tool, ported from `rpi`'s
//! `crates/rpi-tools/tests/{edit_semantic,edit_fuzzy}.rs` and run against real
//! files in a temporary directory instead of the host's in-memory environment.
//!
//! The contract under test is the fork's: exact text, or the upstream
//! `normalize_for_fuzzy_match` layer (trailing whitespace, typographic
//! quotes/dashes/spaces) — nothing else. An `oldText` with different
//! indentation, escapes or middle lines never edits the file, an ambiguous
//! `oldText` is never resolved by guessing, and every failing edit is reported in
//! one round trip with line numbers, the kind of difference, and the real text
//! to copy.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};

use crate::{parse_input, resolve_path, run_tool, DESCRIPTION, PARAMETERS};

/// Minimal temp directory; removed on drop so a failing test leaves no files.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let unique = format!(
            "rpi-edit-diagnostics-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Write `content` (bytes, so BOM/CRLF fixtures stay exact) as `name`.
    fn file(&self, name: &str, content: &[u8]) -> PathBuf {
        let path = self.path.join(name);
        fs::write(&path, content).expect("write fixture");
        path
    }

    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.path)
            .expect("read temp dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Result text of a successful call.
fn result_of(params: Value) -> Value {
    let raw = run_tool(&params).expect("edit should succeed");
    serde_json::from_str(&raw).expect("tool result json")
}

fn result_text(params: Value) -> String {
    result_of(params)["content"][0]["text"]
        .as_str()
        .expect("text content")
        .to_string()
}

/// One `differing lines` entry, laid out the way the reports do it:
/// `<line> <sign> <label padded to 11><text>`.
fn differing_entry(line: usize, sign: &str, label: &str, text: &str) -> String {
    format!("{line:>5} {sign} {label:<11}{text}")
}

/// Fail an edit and return the model-facing message, asserting that the file is
/// byte-identical afterwards.
///
/// The edit names the file relatively, the way the fork's tests do: the host
/// injects `__rpi.cwd`, and the diagnostics quote the path the model wrote.
fn failed_edit(tag: &str, name: &str, original: &[u8], edits: Value) -> String {
    let dir = TempDir::new(tag);
    let file = dir.file(name, original);
    let message = run_tool(&json!({
        "path": name,
        "edits": edits,
        "__rpi": { "cwd": dir.path().to_string_lossy() }
    }))
    .expect_err("the edit must fail");
    assert_eq!(
        fs::read(&file).expect("read back"),
        original,
        "the file must be untouched after a failed edit"
    );
    message
}

// --- Matching is exact, plus the upstream normalization layer only ----------

#[test]
fn indentation_only_difference_is_reported_with_lines_and_help_text() {
    let message = failed_edit(
        "indent",
        "f.rs",
        b"fn a() {\n        let x = 1;\n}\n",
        json!([{
            "oldText": "fn a() {\n    let x = 1;\n}",
            "newText": "fn a() {\n    let x = 2;\n}"
        }]),
    );

    assert!(
        message.contains("error: edits[0].oldText not found in f.rs"),
        "got: {message}"
    );
    assert!(
        message.contains(
            "  --> lines 1-3 are the closest match: only indentation differs \
             (file uses 8 spaces, oldText 4 spaces)"
        ),
        "got: {message}"
    );
    // Differing lines, with file line numbers.
    let expected_old = format!(
        "  {}",
        differing_entry(2, "-", "oldText:", "    let x = 1;")
    );
    let expected_file = format!(
        "  {}",
        differing_entry(2, "+", "file:", "        let x = 1;")
    );
    assert!(
        message.contains(&expected_old),
        "missing {expected_old:?} in: {message}"
    );
    assert!(
        message.contains(&expected_file),
        "missing {expected_file:?} in: {message}"
    );
    // The current text, verbatim, ready to be copied into oldText.
    assert!(
        message.contains("  | fn a() {\n  |         let x = 1;\n  | }"),
        "got: {message}"
    );
}

#[test]
fn middle_line_difference_names_the_differing_line() {
    let message = failed_edit(
        "middle",
        "f.rs",
        b"fn main() {\n    let x = 1;\n    println!(\"x\");\n}\n",
        json!([{
            "oldText": "fn main() {\n    let x = 2;\n    println!(\"x\");\n}",
            "newText": "fn main() {\n    let x = 3;\n    println!(\"x\");\n}"
        }]),
    );

    assert!(
        message.contains(
            "lines 1-4 are the closest match: first and last lines match, line 2 differs"
        ),
        "got: {message}"
    );
    assert!(
        message.contains(&format!(
            "  {}",
            differing_entry(2, "-", "oldText:", "    let x = 2;")
        )),
        "got: {message}"
    );
    assert!(
        message.contains(&format!(
            "  {}",
            differing_entry(2, "+", "file:", "    let x = 1;")
        )),
        "got: {message}"
    );
    assert!(
        message.contains("  |     println!(\"x\");"),
        "the help must quote the real text, got: {message}"
    );
}

#[test]
fn escape_sequence_difference_is_named() {
    let message = failed_edit(
        "escape",
        "f.txt",
        b"first\tsecond\n",
        json!([{ "oldText": "first\\tsecond", "newText": "third" }]),
    );

    assert!(
        message.contains("the closest match: only escape sequences differ"),
        "got: {message}"
    );
}

#[test]
fn unrelated_old_text_points_at_the_first_line_and_asks_for_a_reread() {
    // The file is too short for any candidate window and nothing in the chain
    // matches: the only useful fact left is where the first line occurs.
    let message = failed_edit(
        "no-candidate",
        "f.rs",
        b"fn main() {\n    let total = compute();\n}\n",
        json!({
            "oldText": "fn main() {\nlet total = compute();\nreport(total);\nassert!(total > 0);\nreturn total;\nfinalize(total);",
            "newText": "fn main() {\n    report(compute());\n}"
        }),
    );

    assert!(
        message.contains("  --> no text in the file is similar to oldText"),
        "got: {message}"
    );
    assert!(
        message.contains("oldText's first line \"fn main() {\" appears at line 1"),
        "got: {message}"
    );
    assert!(
        message.contains("read the file again and copy its current text into oldText"),
        "got: {message}"
    );
}

#[test]
fn long_regions_are_pointed_at_instead_of_quoted() {
    let content: String = (1..=80).map(|i| format!("    call_{i}();\n")).collect();
    let old: String = (1..=40)
        .map(|i| format!("    call_{}();\n", i + 100))
        .collect();
    let message = failed_edit(
        "long-region",
        "f.rs",
        content.as_bytes(),
        json!([{ "oldText": old, "newText": "replaced();" }]),
    );

    assert!(
        message.contains("is too long to quote here"),
        "got: {message}"
    );
    assert!(message.contains("offset 1, limit 40"), "got: {message}");
}

// --- Ambiguity is reported, never resolved by guessing ----------------------

#[test]
fn ambiguous_old_text_reports_every_line_number() {
    let message = failed_edit(
        "ambiguous",
        "f.txt",
        b"x\nold\ny\nold\nx\n",
        json!([{ "oldText": "old", "newText": "new" }]),
    );

    assert!(
        message.contains("error: edits[0].oldText occurs 2 times in f.txt (lines 2, 4)"),
        "got: {message}"
    );
    assert!(
        message.contains("or set replaceAll: true to change every occurrence"),
        "got: {message}"
    );
}

#[test]
fn ambiguous_edit_is_not_saved_by_surrounding_context() {
    // Context would single out the last "foo" upstream (auto-expand); here the
    // tool refuses to decide.
    let message = failed_edit(
        "ambiguous-context",
        "f.txt",
        b"foo\nfoo\nbar\n",
        json!([{ "oldText": "foo", "newText": "FOO" }]),
    );

    assert!(
        message.contains("edits[0].oldText occurs 2 times in f.txt (lines 1, 2)"),
        "got: {message}"
    );
}

#[test]
fn several_occurrences_on_one_line_read_as_a_count() {
    let message = failed_edit(
        "same-line",
        "edit.txt",
        b"foo foo foo",
        json!([{ "oldText": "foo", "newText": "bar" }]),
    );

    assert!(
        message.contains("error: edits[0].oldText occurs 3 times in edit.txt (line 1 (3 times))"),
        "got: {message}"
    );
}

#[test]
fn replace_all_replaces_every_occurrence() {
    let dir = TempDir::new("replace-all");
    let file = dir.file("f.txt", b"old\nkeep\nold\nold\n");
    let text = result_text(json!({
        "path": file.to_string_lossy(),
        "edits": [{ "oldText": "old", "newText": "new", "replaceAll": true }]
    }));

    assert_eq!(
        text,
        format!("Successfully replaced 3 block(s) in {}.", file.display())
    );
    assert_eq!(fs::read(&file).unwrap(), b"new\nkeep\nnew\nnew\n");
}

// --- All failing edits are reported in one round trip -----------------------

#[test]
fn every_failing_edit_is_reported_together() {
    let message = failed_edit(
        "multi-failure",
        "f.rs",
        b"fn a() {\n        let x = 1;\n}\n\nold\nmid\nold\n",
        json!([
            {
                "oldText": "fn a() {\n    let x = 1;\n}",
                "newText": "fn a() {\n    let x = 2;\n}"
            },
            { "oldText": "old", "newText": "new" },
            { "oldText": "nothing like this", "newText": "x" }
        ]),
    );

    for expected in [
        "error: edits[0].oldText not found in f.rs",
        "error: edits[1].oldText occurs 2 times in f.rs (lines 5, 7)",
        "error: edits[2].oldText not found in f.rs",
    ] {
        assert!(
            message.contains(expected),
            "missing {expected:?} in: {message}"
        );
    }
}

#[test]
fn overlapping_edits_name_both_regions() {
    let message = failed_edit(
        "overlap",
        "f.txt",
        b"one\ntwo\nthree\n",
        json!([
            { "oldText": "one\ntwo\n", "newText": "ONE\nTWO\n" },
            { "oldText": "two\nthree\n", "newText": "TWO\nTHREE\n" }
        ]),
    );

    assert!(
        message.contains("error: edits[0] (lines 1-2) and edits[1] (lines 2-3) overlap in f.txt"),
        "got: {message}"
    );
    assert!(
        message.contains("merge them into one edit"),
        "got: {message}"
    );
}

#[test]
fn empty_and_unnecessary_edits_are_one_liners() {
    let empty = failed_edit(
        "empty-old-text",
        "f.txt",
        b"alpha\n",
        json!([{ "oldText": "", "newText": "x" }]),
    );
    assert_eq!(
        empty,
        "error: edits[0].oldText is empty in f.txt - provide the exact text to replace"
    );

    let no_op = failed_edit(
        "no-op",
        "f.txt",
        b"alpha\n",
        json!([{ "oldText": "alpha", "newText": "alpha" }]),
    );
    assert_eq!(
        no_op,
        "error: edits[0].oldText and newText are identical in f.txt - this edit would change nothing"
    );
}

// --- Success path: notes, warnings, diffs, BOM/CRLF -------------------------

#[test]
fn normalized_match_succeeds_with_a_note() {
    let dir = TempDir::new("note");
    // The file uses a typographic apostrophe and trailing spaces.
    let file = dir.file("f.txt", "it\u{2019}s a file  \nkeep\n".as_bytes());
    let text = result_text(json!({
        "path": file.to_string_lossy(),
        "edits": [{ "oldText": "it's a file", "newText": "it is a file" }]
    }));

    assert_eq!(
        text,
        format!(
            "Successfully replaced 1 block(s) in {}.\n\
             note: edits[0] matched after normalizing trailing whitespace/quotes (line 1)",
            file.display()
        )
    );
    assert_eq!(
        fs::read(&file).unwrap(),
        "it is a file\nkeep\n".as_bytes(),
        "untouched lines keep their trailing spaces"
    );
}

#[test]
fn brace_balance_change_produces_a_warning() {
    let dir = TempDir::new("warning");
    let file = dir.file("f.rs", b"fn a() {\n    body();\n}\n");
    let text = result_text(json!({
        "path": file.to_string_lossy(),
        "edits": [{ "oldText": "fn a() {\n    body();\n}", "newText": "body();\n}" }]
    }));

    assert_eq!(
        text,
        format!(
            "Successfully replaced 1 block(s) in {}.\n\
             warning: edits[0] changes brace balance by -1 '}}' (lines 1-2)",
            file.display()
        )
    );
    assert_eq!(fs::read(&file).unwrap(), b"body();\n}\n");
}

#[test]
fn balanced_edit_produces_no_warning() {
    let dir = TempDir::new("no-warning");
    let file = dir.file("f.rs", b"let a = 1;\n");
    let text = result_text(json!({
        "path": file.to_string_lossy(),
        "edits": [{ "oldText": "let a = 1;", "newText": "let a = compute(1, 2);" }]
    }));

    assert_eq!(
        text,
        format!("Successfully replaced 1 block(s) in {}.", file.display())
    );
}

#[test]
fn replaces_disjoint_edits_and_returns_the_display_diff() {
    let dir = TempDir::new("disjoint");
    let file = dir.file("edit.txt", b"alpha\nbeta\ngamma\ndelta\n");
    let result = result_of(json!({
        "path": file.to_string_lossy(),
        "edits": [
            { "oldText": "alpha\n", "newText": "ALPHA\n" },
            { "oldText": "gamma\n", "newText": "GAMMA\n" },
        ]
    }));

    assert_eq!(
        result["content"][0]["text"].as_str().unwrap(),
        format!("Successfully replaced 2 block(s) in {}.", file.display())
    );
    // The display diff lists the old line before the new one for a replacement.
    let diff = result["details"]["diff"].as_str().expect("diff");
    assert_eq!(
        diff, "-1 alpha\n+1 ALPHA\n 2 beta\n-3 gamma\n+3 GAMMA\n 4 delta",
        "got: {diff}"
    );
    let patch = result["details"]["patch"].as_str().expect("patch");
    assert!(patch.contains("+ALPHA"), "got: {patch}");
    assert!(patch.contains("-gamma"), "got: {patch}");
    assert_eq!(fs::read(&file).unwrap(), b"ALPHA\nbeta\nGAMMA\ndelta\n");
}

#[test]
fn normalized_match_preserves_bom_and_crlf() {
    let dir = TempDir::new("bom-crlf");
    let file = dir.file(
        "f.txt",
        "\u{FEFF}old line\r\nkeep\r\nold line\r\n".as_bytes(),
    );
    result_text(json!({
        "path": file.to_string_lossy(),
        "edits": [{ "oldText": "old line", "newText": "new line", "replaceAll": true }]
    }));

    assert_eq!(
        fs::read(&file).unwrap(),
        "\u{FEFF}new line\r\nkeep\r\nnew line\r\n".as_bytes()
    );
}

#[test]
fn trailing_whitespace_only_difference_matches_and_notes_it() {
    let dir = TempDir::new("trailing");
    let file = dir.file("f.txt", "it\u{2019}s A  \nx\nit\u{2019}s A  \n".as_bytes());
    let text = result_text(json!({
        "path": file.to_string_lossy(),
        "edits": [{ "oldText": "it's A", "newText": "ok", "replaceAll": true }]
    }));

    assert!(
        text.contains("note: edits[0] matched after normalizing"),
        "{text}"
    );
    assert_eq!(fs::read(&file).unwrap(), "ok\nx\nok\n".as_bytes());
}

// --- Argument handling ------------------------------------------------------

#[test]
fn accepts_single_edit_object_and_json_string_edits() {
    let dir = TempDir::new("arg-shapes");
    let file = dir.file("single.txt", b"alpha\nbeta\n");

    result_text(json!({
        "path": file.to_string_lossy(),
        "edits": { "oldText": "alpha", "newText": "ALPHA" }
    }));
    assert_eq!(fs::read(&file).unwrap(), b"ALPHA\nbeta\n");

    result_text(json!({
        "path": file.to_string_lossy(),
        "edits": "{\"oldText\":\"beta\",\"newText\":\"BETA\",\"replaceAll\":true}"
    }));
    assert_eq!(fs::read(&file).unwrap(), b"ALPHA\nBETA\n");
}

#[test]
fn legacy_top_level_old_and_new_text_are_folded_into_edits() {
    let dir = TempDir::new("legacy");
    let file = dir.file("f.txt", b"one\none\n");
    let text = result_text(json!({
        "path": file.to_string_lossy(),
        "oldText": "one",
        "newText": "two",
        "replaceAll": true
    }));

    assert_eq!(
        text,
        format!("Successfully replaced 2 block(s) in {}.", file.display())
    );
    assert_eq!(fs::read(&file).unwrap(), b"two\ntwo\n");
}

#[test]
fn relative_paths_resolve_against_the_host_cwd_and_unknown_keys_are_ignored() {
    let dir = TempDir::new("host-cwd");
    dir.file("nested.txt", b"before\n");
    let text = result_text(json!({
        "path": "nested.txt",
        "edits": [{ "oldText": "before", "newText": "after" }],
        // The host injects this; extra keys must never fail parameter parsing.
        "__rpi": { "cwd": dir.path().to_string_lossy(), "sessionId": "session-a" },
        "unexpectedKey": { "anything": [1, 2, 3] }
    }));

    assert_eq!(text, "Successfully replaced 1 block(s) in nested.txt.");
    assert_eq!(fs::read(dir.path().join("nested.txt")).unwrap(), b"after\n");
}

#[test]
fn absolute_paths_ignore_the_host_cwd() {
    let elsewhere = TempDir::new("elsewhere");
    let dir = TempDir::new("absolute");
    let file = dir.file("f.txt", b"x\n");
    let text = result_text(json!({
        "path": file.to_string_lossy(),
        "edits": [{ "oldText": "x", "newText": "y" }],
        "__rpi": { "cwd": elsewhere.path().to_string_lossy() }
    }));

    assert!(text.ends_with(&format!("in {}.", file.display())), "{text}");
    assert_eq!(fs::read(&file).unwrap(), b"y\n");
}

#[test]
fn relative_paths_without_a_host_cwd_fall_back_to_the_process_directory() {
    let resolved = resolve_path(None, "rel/file.txt");
    let expected = std::env::current_dir().unwrap().join("rel/file.txt");
    assert_eq!(resolved, expected);
    assert_eq!(
        resolve_path(None, "/abs/file.txt"),
        PathBuf::from("/abs/file.txt")
    );
}

#[test]
fn invalid_input_is_rejected_before_touching_the_file() {
    assert!(parse_input(&json!({ "path": "f", "edits": [] })).is_err());
    assert!(parse_input(&json!({ "path": "", "edits": [] })).is_err());
    assert!(parse_input(&json!({ "edits": [{ "oldText": "a", "newText": "b" }] })).is_err());
    assert!(parse_input(&json!({ "path": "f", "edits": [{ "oldText": "a" }] })).is_err());
    assert!(parse_input(&json!({ "path": "f", "edits": "not json" })).is_err());
    assert!(parse_input(&json!("nope")).is_err());
    assert!(parse_input(&json!({ "path": "f" })).is_err());
}

// --- Filesystem behavior ----------------------------------------------------

#[test]
fn the_write_leaves_no_temporary_file_behind() {
    let dir = TempDir::new("atomic");
    let file = dir.file("f.txt", b"one\n");
    result_text(json!({
        "path": file.to_string_lossy(),
        "edits": [{ "oldText": "one", "newText": "two" }]
    }));

    assert_eq!(dir.entries(), vec!["f.txt".to_string()]);
    assert_eq!(fs::read(&file).unwrap(), b"two\n");
}

#[test]
fn missing_path_becomes_a_read_error_naming_the_file() {
    let dir = TempDir::new("missing");
    let error = run_tool(&json!({
        "path": "absent.txt",
        "edits": [{ "oldText": "a", "newText": "b" }],
        "__rpi": { "cwd": dir.path().to_string_lossy() }
    }))
    .expect_err("missing file must fail");

    assert!(
        error.starts_with("Could not read file: absent.txt."),
        "{error}"
    );
}

// --- The tool's description states the contract the model can rely on --------

#[test]
fn description_documents_exactness_and_replace_all() {
    assert!(
        DESCRIPTION.contains("Copy each oldText from read output, preserving whitespace"),
        "got: {DESCRIPTION}"
    );
    for relaxed in ["tolerates", "fuzzy", "indentation drift", "10-pass"] {
        assert!(
            !DESCRIPTION.contains(relaxed),
            "description must not promise relaxed matching ({relaxed}): {DESCRIPTION}"
        );
    }
}

#[test]
fn schema_is_the_fork_edit_input() {
    let schema: Value = serde_json::from_str(PARAMETERS).expect("schema is valid JSON");
    assert_eq!(schema["required"], json!(["path", "edits"]));
    assert_eq!(
        schema["properties"]["edits"]["items"]["required"],
        json!(["oldText", "newText"])
    );
    let replace_all = &schema["properties"]["edits"]["items"]["properties"]["replaceAll"];
    assert_eq!(replace_all["type"], json!("boolean"));
    let description = |field: &str| {
        schema["properties"]["edits"]["items"]["properties"][field]["description"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    assert!(description("replaceAll").contains("Replace every occurrence of oldText"));
    assert!(description("oldText").contains("exactly once in the file unless replaceAll is set"));
}
