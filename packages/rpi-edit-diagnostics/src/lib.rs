//! `edit` — a diagnosing `edit` tool, shipped as a Rust-native plugin.
//!
//! The host lets an extension's tool shadow a built-in one of the same name, so
//! registering `edit` here replaces the built-in tool's implementation with
//! this package's copy of it. The tool matches like a compiler front end: a
//! text matches only when it is present verbatim, or when upstream Pi's
//! [`normalize_for_fuzzy_match`] equalizes trailing whitespace and typographic
//! quotes/dashes/spaces on both sides. Indentation, escapes and middle lines are
//! *not* tolerated, an ambiguous text is never resolved by picking an occurrence,
//! and nothing is written unless every edit resolves. A failure is reported the
//! way a compiler reports a bad token: all failing edits in one round trip, each
//! with the line numbers of the closest real text, what differs, and that text
//! verbatim to copy back into `oldText`.
//!
//! The behavior, the diagnostics text and the schema are the `rpi` fork's
//! (`crates/rpi-tools/src/tools/{edit,edit_apply,edit_diagnostics,edit_diff}.rs`
//! and `semantic_edit/`, MIT, (c) 2026 BigFish; the candidate finder inside
//! `semantic_edit` is a port of `pi-semantic-edit` by K2, MIT — see `NOTICE`).
//! [`semantic_edit`] only *suggests* candidate regions for the reports; it never
//! decides a match.
//!
//! Per call the pipeline is the fork's:
//!
//! ```text
//! lock canonical target → read file → strip BOM → detect line ending → normalize to LF
//!           → apply_edits (match all, all-or-nothing) → restore line endings + BOM
//!           → temp file + snapshot check + rename (or checked in-place hard-link write)
//! ```
//!
//! Success text is `Successfully replaced N block(s) in <path>.`, followed by one
//! `note:` line per edit that only matched after normalization and one
//! `warning:` line per edit that changes the net bracket balance.
//!
//! Differences from the fork's built-in tool: a relative `path` is resolved
//! against the host-injected `__rpi.cwd` (see
//! [`rpi_plugin_sdk::HOST_CONTEXT_KEY`]) with the process working directory as
//! fallback, and the write goes to a temporary file in the same directory that
//! is renamed over the target, so an interrupted write cannot truncate a file.
//! Calls serialize by canonical target path and check the original bytes again
//! immediately before committing, rejecting stale snapshots.
#![forbid(unsafe_code)]
// The SDK's `export_plugin!` macro expands to a `pub extern "C"` entry point that
// dereferences the host's `*const PluginApi`. That is the ABI contract of
// `rpi-plugin-sdk` 0.3.x (the host guarantees validity for the call), and the
// macro is the only place this pattern appears in this crate.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod edit_apply;
pub mod edit_diagnostics;
pub mod edit_diff;
pub mod semantic_edit;

use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{
    FreeStringFn, StableToolSchema, StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::c_void;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use edit_apply::{apply_edits, ApplyFailure, EditRequest, FailureCode};
use edit_diff::{
    detect_line_ending, generate_diff_string, generate_unified_patch, normalize_to_lf,
    restore_line_endings, strip_bom,
};

/// Registered tool name. The host resolves this before built-ins, so the
/// extension replaces `edit`.
const TOOL: &str = "edit";

/// Tool description, verbatim from `rpi`'s `edit` tool
/// (`crates/rpi-tools/src/tools/edit.rs`).
const DESCRIPTION: &str = "Edit a single file by exact text replacement. Copy each oldText from read output, preserving whitespace and excluding line numbers or truncation markers. All edits are matched against the same original file, not one after another; put changes to the same or nearby lines in one edit. A failed call changes nothing and reports every failing edit with the closest text in the file, what differs, and that text to copy. Fields are camelCase (oldText, newText).";

/// Parameter schema, the JSON form of the fork's `EditInput` / `ReplaceEditJson`
/// (camelCase wire shape; `replaceAll` optional). Extra keys stay allowed
/// because the host injects `__rpi`.
const PARAMETERS: &str = r#"{
  "type": "object",
  "properties": {
    "path": {
      "type": "string",
      "description": "Path to the file to edit (relative or absolute)"
    },
    "edits": {
      "type": "array",
      "description": "One or more replacements, applied together.",
      "items": {
        "type": "object",
        "properties": {
          "oldText": {
            "type": "string",
            "description": "Text to replace. It must occur exactly once in the file unless replaceAll is set (add surrounding lines until it does; a repeated statement or bare identifier is not enough), and must not overlap another edit's oldText."
          },
          "newText": {
            "type": "string",
            "description": "Replacement text."
          },
          "replaceAll": {
            "type": "boolean",
            "description": "Replace every occurrence of oldText. Default false."
          }
        },
        "required": ["oldText", "newText"]
      }
    }
  },
  "required": ["path", "edits"]
}"#;

// ---------------------------------------------------------------------------
// Tool input
// ---------------------------------------------------------------------------

/// One parsed call: the file to edit plus its replacement requests.
struct EditInput {
    /// The path exactly as the model wrote it — this is what the result text and
    /// the diagnostics name, matching the built-in tool.
    path: String,
    edits: Vec<EditRequest>,
}

/// Parse `{ path, edits[] }` out of the raw arguments, ignoring unknown keys.
///
/// Mirrors the fork's `prepareEditArguments` + `EditInput` validation: `edits`
/// may arrive as a JSON string or as a single edit object, legacy top-level
/// `oldText`/`newText` (plus `replaceAll`) are folded into `edits[]`, and
/// `replaceAll` is optional.
fn parse_input(params: &Value) -> Result<EditInput, String> {
    let Some(object) = params.as_object() else {
        return Err(
            "Edit tool input is invalid. Provide an object with a string path and an edits array."
                .into(),
        );
    };
    let path = object
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .ok_or("Edit tool input is invalid. path must be a non-empty string.")?
        .to_string();

    let has_edits_key = object.contains_key("edits");
    let items = edit_items(object).ok_or(
        "Edit tool input is invalid. edits must be an array of { oldText, newText } objects.",
    )?;
    if items.is_empty() {
        return Err(if has_edits_key {
            "Edit tool input is invalid. edits must contain at least one replacement."
        } else {
            "Edit tool input is invalid. edits must be an array of { oldText, newText } objects."
        }
        .into());
    }

    let mut edits = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let item = item.as_object();
        let old_text = item
            .and_then(|item| item.get("oldText"))
            .and_then(Value::as_str);
        let new_text = item
            .and_then(|item| item.get("newText"))
            .and_then(Value::as_str);
        let (Some(old_text), Some(new_text)) = (old_text, new_text) else {
            return Err(format!(
                "Edit tool input is invalid. edits[{index}] must provide string oldText and newText."
            ));
        };
        edits.push(EditRequest {
            old_text: old_text.to_string(),
            new_text: new_text.to_string(),
            replace_all: item
                .and_then(|item| item.get("replaceAll"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    Ok(EditInput { path, edits })
}

/// The edit objects the arguments carry, after the fork's argument folding.
fn edit_items(object: &serde_json::Map<String, Value>) -> Option<Vec<Value>> {
    let mut items = match object.get("edits") {
        None => Vec::new(),
        Some(Value::Array(items)) => items.clone(),
        Some(Value::String(text)) => match serde_json::from_str::<Value>(text).ok()? {
            Value::Array(items) => items,
            single if is_edit_object(&single) => vec![single],
            _ => return None,
        },
        Some(single) if is_edit_object(single) => vec![single.clone()],
        Some(_) => return None,
    };
    // Legacy top-level shape: `{ oldText, newText, replaceAll }`.
    if let (Some(old_text), Some(new_text)) = (
        object.get("oldText").and_then(Value::as_str),
        object.get("newText").and_then(Value::as_str),
    ) {
        let mut folded = json!({ "oldText": old_text, "newText": new_text });
        if let Some(flag) = object.get("replaceAll").and_then(Value::as_bool) {
            folded["replaceAll"] = Value::Bool(flag);
        }
        items.push(folded);
    }
    Some(items)
}

fn is_edit_object(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        object.get("oldText").is_some_and(Value::is_string)
            && object.get("newText").is_some_and(Value::is_string)
    })
}

/// Host-injected working directory (`__rpi.cwd`), when the host sent one.
fn host_cwd(params: &Value) -> Option<PathBuf> {
    params
        .get(rpi_plugin_sdk::HOST_CONTEXT_KEY)?
        .get("cwd")?
        .as_str()
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
}

/// Resolve `path` against `cwd`, falling back to the process working directory.
/// Absolute paths are used as given.
fn resolve_path(cwd: Option<&Path>, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match cwd {
        Some(cwd) => cwd.join(path),
        None => match std::env::current_dir() {
            Ok(current) => current.join(path),
            Err(_) => path.to_path_buf(),
        },
    }
}

// ---------------------------------------------------------------------------
// File IO
// ---------------------------------------------------------------------------

/// Locks are keyed by canonical target paths, so relative paths and symlinks
/// to the same target serialize too. Weak entries do not keep idle locks alive.
fn target_lock(target: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(target).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(target.to_path_buf(), Arc::downgrade(&lock));
    lock
}

fn write_error(path: &Path, error: std::io::Error) -> ApplyFailure {
    ApplyFailure {
        code: FailureCode::Invalid,
        message: format!("Could not write file: {}. {error}", path.display()),
    }
}

/// Compare the original bytes directly (no hash collisions), immediately before
/// committing. This detects outside writers, but is not a filesystem CAS: an
/// uncooperative writer can still race the check and the rename/in-place write.
fn check_snapshot(path: &Path, expected: &[u8]) -> Result<(), ApplyFailure> {
    let current = fs::read(path).map_err(|error| write_error(path, error))?;
    if current != expected {
        return Err(ApplyFailure {
            code: FailureCode::Invalid,
            message: format!(
                "error: file changed after it was read: {}; read it again, then retry edit",
                path.display()
            ),
        });
    }
    Ok(())
}

/// Write the edited text to the canonical target, preserving hard-link behavior.
/// The caller holds the target lock throughout reading, matching and writing.
fn write_file(path: &Path, content: &str, expected: &[u8]) -> Result<(), ApplyFailure> {
    if has_other_hard_links(path) {
        check_snapshot(path, expected)?;
        return fs::write(path, content).map_err(|error| write_error(path, error));
    }
    write_atomically(path, content, expected)
}

#[cfg(unix)]
fn has_other_hard_links(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).is_ok_and(|metadata| metadata.nlink() > 1)
}

/// Stable Rust exposes no link count on Windows; hard-linked files there are
/// replaced like any other file (README: Limitations).
#[cfg(not(unix))]
fn has_other_hard_links(_path: &Path) -> bool {
    false
}

/// Replace `path`'s contents through a temporary file in the same directory, so
/// the target is either the old file or the new one — never a partial write.
fn write_atomically(path: &Path, content: &str, expected: &[u8]) -> Result<(), ApplyFailure> {
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let temp = directory.join(temp_name(&name));

    let staged = (|| {
        let mut file = fs::File::create(&temp)?;
        file.write_all(content.as_bytes())?;
        file.flush()?;
        drop(file);
        // Keep the target's mode: a rename would otherwise fall back to the
        // process umask and could drop an executable bit.
        if let Ok(metadata) = fs::metadata(path) {
            let _ = fs::set_permissions(&temp, metadata.permissions());
        }
        Ok::<_, std::io::Error>(())
    })();
    let written = staged
        .map_err(|error| write_error(path, error))
        .and_then(|()| {
            check_snapshot(path, expected)?;
            fs::rename(&temp, path).map_err(|error| write_error(path, error))
        });

    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// `.<file>.rpi-edit-<pid>-<sequence>.tmp` — unique per process and call, so
/// concurrent edits of the same file do not share a staging path.
fn temp_name(name: &str) -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    format!(
        ".{name}.rpi-edit-{}-{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

// ---------------------------------------------------------------------------
// The tool
// ---------------------------------------------------------------------------

/// Run one `edit` call: returns the `AgentToolResult` JSON on success and the
/// model-facing error text on failure.
fn run_tool(params: &Value) -> Result<String, String> {
    let input = parse_input(params)?;
    let absolute = resolve_path(host_cwd(params).as_deref(), &input.path);
    let target = fs::canonicalize(&absolute)
        .map_err(|error| format!("Could not read file: {}. {error}", input.path))?;
    let lock = target_lock(&target);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let raw = fs::read_to_string(&target)
        .map_err(|error| format!("Could not read file: {}. {error}", input.path))?;

    let stripped = strip_bom(&raw);
    let bom = stripped.bom;
    let ending = detect_line_ending(stripped.text);
    let normalized = normalize_to_lf(stripped.text);

    // All-or-nothing: this either resolves every edit or returns the complete
    // diagnostic list without touching the file.
    let applied =
        apply_edits(&normalized, &input.edits, &input.path).map_err(|failure| failure.message)?;

    let final_content = format!(
        "{bom}{}",
        restore_line_endings(&applied.new_content, ending)
    );
    write_file(&target, &final_content, raw.as_bytes()).map_err(|failure| failure.message)?;

    let diff = generate_diff_string(&applied.base_content, &applied.new_content, 4);
    let patch = generate_unified_patch(&input.path, &applied.base_content, &applied.new_content);

    let mut text = format!(
        "Successfully replaced {} block(s) in {}.",
        applied.replacements, input.path
    );
    for note in &applied.notes {
        text.push('\n');
        text.push_str(note);
    }
    for warning in &applied.warnings {
        text.push('\n');
        text.push_str(warning);
    }

    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "details": {
            "diff": diff.diff,
            "patch": patch,
            "first_changed_line": diff.first_changed_line,
        }
    })
    .to_string())
}

// ---------------------------------------------------------------------------
// Plugin ABI glue
// ---------------------------------------------------------------------------

static HANDLES: ToolHandles<Drive> = ToolHandles::new();

struct Drive {
    params: Value,
    done: bool,
}

extern "C" fn execute(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let text = params.to_string_lossy();
    params.free_with(free);
    HANDLES.insert(Drive {
        params: serde_json::from_str(&text).unwrap_or(Value::Null),
        done: false,
    })
}

extern "C" fn poll(handle: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null edit handle".into()));
    }
    let Some(entry) = HANDLES.get(handle) else {
        return StepResult::err(StbString::from_string(
            "invalid or destroyed tool handle".into(),
        ));
    };
    let mut drive = entry.lock();
    if entry.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("edit cancelled".into()));
    }
    if drive.done {
        return StepResult::err(StbString::from_string(
            "edit polled after completion".into(),
        ));
    }
    drive.done = true;
    match run_tool(&drive.params) {
        Ok(result) => StepResult::done(StbString::from_string(result)),
        // The diagnostics are already model-facing text; return them unchanged.
        Err(message) => StepResult::err(StbString::from_string(message)),
    }
}

extern "C" fn cancel(handle: StepHandle) {
    HANDLES.cancel(handle);
}

extern "C" fn destroy(handle: StepHandle) {
    drop(HANDLES.remove(handle));
}

use rpi_plugin_sdk::callbacks::free_string;

rpi_plugin_sdk::export_plugin!(|api| {
    let Some(register) = api.register_tool else {
        return 1;
    };
    let schema = Box::new(StableToolSchema {
        name: StbString::from_string(TOOL.into()),
        description: StbString::from_string(DESCRIPTION.into()),
        parameters: StbString::from_string(PARAMETERS.into()),
    });
    let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
    drop(schema);
    rc
});

#[cfg(test)]
mod tests;
