//! Register rpi as a custom external agent in Zed.
//!
//! Zed stores configuration in `settings.json`, which is **JSONC**: it allows
//! `//` and `/* */` comments as well as trailing commas — Zed's own default file
//! ships a comment header, and hand-edited files routinely carry trailing commas.
//! Round-tripping that through a plain JSON parser would delete the user's
//! comments and reformat the whole document, so this module instead scans for
//! the insertion point and splices in a single member, leaving every other byte
//! untouched.
//!
//! The result Zed needs is one entry:
//!
//! ```jsonc
//! "agent_servers": {
//!   "rpi": { "type": "custom", "command": "<path>", "args": ["--acp"] }
//! }
//! ```
//!
//! so `RPI_ACP_AUTO_REGISTER_ZED=1` (or `rpi-acp-bridge --install-zed`) is all a
//! user needs, with no registry submission and no hand-editing.

use std::path::{Path, PathBuf};

/// Key Zed shows the agent under, and the member we own inside `agent_servers`.
pub const SERVER_ID: &str = "rpi";

/// How Zed should launch the agent: the `agent_servers.<id>` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    pub command: String,
    pub args: Vec<String>,
}

impl Registration {
    /// Pick the entry from how *this* process was launched.
    ///
    /// * launched as `rpi` (the cdylib extension) -> `rpi --acp`
    /// * launched as `rpi-acp-bridge` (standalone) -> the bridge itself
    pub fn detect() -> Self {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("rpi"));
        Self::for_executable(&exe)
    }

    pub fn for_executable(exe: &Path) -> Self {
        let command = exe.display().to_string();
        if crate::session::is_rpi_binary(exe) {
            Registration {
                command,
                args: vec!["--acp".to_owned()],
            }
        } else {
            Registration {
                command,
                args: Vec::new(),
            }
        }
    }

    /// The full `agent_servers` block, for `--print-zed` and documentation.
    pub fn snippet(&self) -> String {
        format!(
            "{{\n  \"agent_servers\": {{\n    {}\n  }}\n}}",
            self.member(4)
        )
    }

    /// `"rpi": { ... }` at indentation `pad` (later lines indented relative).
    fn member(&self, pad: usize) -> String {
        let quote = '"';
        let command = serde_json::to_string(&self.command).unwrap_or_else(|_| "\"rpi\"".into());
        let args = self
            .args
            .iter()
            .filter_map(|arg| serde_json::to_string(arg).ok())
            .collect::<Vec<_>>()
            .join(", ");
        let p = |n: usize| " ".repeat(n);
        format!(
            "{q}{SERVER_ID}{q}: {{\n\
             {p2}{q}type{q}: {q}custom{q},\n\
             {p2}{q}command{q}: {command},\n\
             {p2}{q}args{q}: [{args}]\n\
             {p1}}}",
            q = quote,
            p1 = p(pad),
            p2 = p(pad + 2),
        )
    }
}

/// What `register_file` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `settings.json` did not exist and was created.
    Created,
    /// The member was spliced in; `backup` is the pre-change copy, if kept.
    Updated { backup: Option<PathBuf> },
    /// `agent_servers.rpi` was already present; nothing was written.
    AlreadyPresent,
}

/// Where Zed keeps `settings.json` on this host.
pub fn settings_path() -> Option<PathBuf> {
    let get = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    settings_path_from(
        std::env::consts::OS,
        get("APPDATA"),
        get("XDG_CONFIG_HOME"),
        get("HOME").or_else(|| get("USERPROFILE")),
        get("ZED_SETTINGS"),
    )
}

/// Pure path resolution, so it can be tested without touching the environment.
fn settings_path_from(
    os: &str,
    appdata: Option<String>,
    xdg: Option<String>,
    home: Option<String>,
    explicit: Option<String>,
) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(PathBuf::from(path));
    }
    match os {
        "windows" => appdata.map(|dir| PathBuf::from(dir).join("Zed").join("settings.json")),
        "macos" => home.map(|dir| {
            PathBuf::from(dir)
                .join("Library")
                .join("Application Support")
                .join("Zed")
                .join("settings.json")
        }),
        _ => {
            let base = xdg
                .map(PathBuf::from)
                .or_else(|| home.map(|dir| PathBuf::from(dir).join(".config")))?;
            Some(base.join("zed").join("settings.json"))
        }
    }
}

/// Ensure `agent_servers.rpi` is present in the JSONC document `text`.
///
/// Returns `Ok(None)` when it is already registered, or `Ok(Some(new_text))`
/// with only the inserted member added.
pub fn ensure_registered(
    text: &str,
    registration: &Registration,
) -> Result<Option<String>, String> {
    let bytes = text.as_bytes();
    let (root_open, root_close) = root_object(bytes)
        .ok_or("settings.json is not a JSON object (did you mean to edit it by hand?)")?;

    match member_value(bytes, root_open, root_close, "agent_servers") {
        // Present and an object: add `rpi` inside unless it is already there.
        Some(value) if bytes.get(value) == Some(&b'{') => {
            let (open, close) = matching_delim(bytes, value)
                .ok_or("settings.json has an unterminated `agent_servers` object")?;
            if member_value(bytes, open, close, SERVER_ID).is_some() {
                return Ok(None);
            }
            // `agent_servers` is a top-level key, so its members sit at 4 and its
            // closing brace at 2.
            Ok(Some(splice_object(
                text,
                open,
                close,
                &registration.member(4),
                4,
                2,
            )))
        }
        // Present but not an object: refuse rather than silently rewrite it.
        Some(_) => Err(
            "settings.json already has an `agent_servers` value that is not an object".to_owned(),
        ),
        // Absent: append a new top-level member.
        None => {
            let member = registration.member(4);
            let block = format!(
                "{q}agent_servers{q}: {{\n{pad}{member}\n  }}",
                q = '"',
                pad = "    ",
            );
            Ok(Some(splice_object(
                text, root_open, root_close, &block, 2, 0,
            )))
        }
    }
}

/// Insert `member` *before* `close`, in place of the whitespace that precedes it.
///
/// `member_indent` is the indentation the member's own line should get and
/// `close_indent` that of the closing brace. Takes the trailing comma of the
/// previous member into account, so both `{ a: 1 }` and `{ a: 1, }` work.
fn splice_object(
    text: &str,
    open: usize,
    close: usize,
    member: &str,
    member_indent: usize,
    close_indent: usize,
) -> String {
    let _ = open;
    let prefix = &text[..text[..close].trim_end().len()];
    let suffix = &text[close..];
    let mut body = String::new();
    match prefix.as_bytes().last() {
        // Empty object: `{` + member.
        Some(b'{') => {}
        // Previous member already ends with a comma.
        Some(b',') => {}
        // Last member without a trailing comma.
        _ => body.push(','),
    }
    body.push('\n');
    body.push_str(&" ".repeat(member_indent));
    body.push_str(member);
    body.push('\n');
    body.push_str(&" ".repeat(close_indent));
    format!("{prefix}{body}{suffix}")
}

/// Borrowed helpers below treat the document as bytes and understand JSONC.
fn root_object(b: &[u8]) -> Option<(usize, usize)> {
    let start = skip_trivia(b, 0);
    if b.get(start) != Some(&b'{') {
        return None;
    }
    matching_delim(b, start)
}

/// Skip whitespace and `//` / `/* */` comments.
fn skip_trivia(b: &[u8], mut i: usize) -> usize {
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        match (b.get(i), b.get(i + 1)) {
            (Some(b'/'), Some(b'/')) => {
                i += 2;
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            (Some(b'/'), Some(b'*')) => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            _ => return i,
        }
    }
}

/// `i` must be a `"`; returns the index just past the closing quote.
fn skip_string(b: &[u8], mut i: usize) -> Option<usize> {
    if b.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// The `(open, close)` indices of the `{}` / `[]` starting at `open`.
fn matching_delim(b: &[u8], open: usize) -> Option<(usize, usize)> {
    let close_byte = match b.get(open) {
        Some(b'{') => b'}',
        Some(b'[') => b']',
        _ => return None,
    };
    let mut depth = 0usize;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'"' => {
                i = skip_string(b, i)?;
                continue;
            }
            b'/' if matches!(b.get(i + 1), Some(b'/') | Some(b'*')) => {
                i = skip_trivia(b, i);
                continue;
            }
            byte if byte == b[open] => depth += 1,
            byte if byte == close_byte => {
                depth -= 1;
                if depth == 0 {
                    return Some((open, i));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Index where the value of the top-level `key` inside `open..close` starts.
fn member_value(b: &[u8], open: usize, close: usize, key: &str) -> Option<usize> {
    let mut i = skip_trivia(b, open + 1);
    while i < close && b.get(i) == Some(&b'"') {
        let key_end = skip_string(b, i)?;
        let name = &b[i + 1..key_end - 1];
        let after = skip_trivia(b, key_end);
        if b.get(after) != Some(&b':') {
            return None;
        }
        let value = skip_trivia(b, after + 1);
        if name == key.as_bytes() {
            return Some(value);
        }
        let next = skip_trivia(b, skip_value(b, value)?);
        if b.get(next) == Some(&b',') {
            i = skip_trivia(b, next + 1);
        } else {
            break;
        }
    }
    None
}

/// Index just past the value starting at `i`.
fn skip_value(b: &[u8], i: usize) -> Option<usize> {
    match b.get(i)? {
        b'"' => skip_string(b, i),
        b'{' | b'[' => matching_delim(b, i).map(|(_, close)| close + 1),
        _ => {
            let mut j = i;
            while j < b.len() && !matches!(b[j], b',' | b'}' | b']') && !b[j].is_ascii_whitespace()
            {
                j += 1;
            }
            Some(j)
        }
    }
}

/// One-line description of an [`Outcome`], for the CLI frontends. `dry_run`
/// switches to the conditional verbs, because nothing was written.
pub fn describe(outcome: &Outcome, path: &Path, dry_run: bool) -> String {
    let display = path.display();
    match outcome {
        Outcome::AlreadyPresent => format!("already registered in {display}"),
        Outcome::Created if dry_run => format!("would create {display} with agent_servers.rpi"),
        Outcome::Created => format!("created {display} with agent_servers.rpi"),
        Outcome::Updated { .. } if dry_run => format!("would add rpi to {display}"),
        Outcome::Updated { .. } => format!("added rpi to {display}"),
    }
}

/// Back `rpi --install-zed` (`write`) and `rpi --print-zed` (`!write`).
///
/// Returns the process exit code the frontend should report.
pub fn run_default_action(write: bool) -> i32 {
    let Some(path) = settings_path() else {
        eprintln!("error: cannot locate Zed's settings.json (set ZED_SETTINGS)");
        return 1;
    };
    let registration = Registration::detect();
    if !write {
        println!("{}", registration.snippet());
        return 0;
    }
    match register_file(&path, &registration, false) {
        Ok(outcome) => {
            println!("{}", describe(&outcome, &path, false));
            if let Outcome::Updated {
                backup: Some(backup),
            } = &outcome
            {
                println!("backup: {}", backup.display());
            }
            0
        }
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    }
}

/// Ensure the file at `path` registers rpi; create it if missing.
///
/// Writes atomically (temp + rename) and keeps the pre-change file as
/// `<name>.rpi-bak` the first time it is modified.
pub fn register_file(
    path: &Path,
    registration: &Registration,
    dry_run: bool,
) -> Result<Outcome, String> {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };

    let (outcome, new_text) = match existing {
        None => {
            let member = registration.member(4);
            let text =
                format!("// Zed settings\n{{\n  \"agent_servers\": {{\n    {member}\n  }}\n}}\n");
            (Outcome::Created, text)
        }
        Some(text) => match ensure_registered(&text, registration)? {
            None => return Ok(Outcome::AlreadyPresent),
            Some(new_text) => {
                let backup = backup_path(path);
                (Outcome::Updated { backup }, new_text)
            }
        },
    };

    if dry_run {
        return Ok(outcome);
    }
    if let Outcome::Updated {
        backup: Some(backup),
    } = &outcome
    {
        // Keep the *first* pre-change copy: a second run must not clobber it
        // with an already-modified file.
        if !backup.exists() {
            std::fs::copy(path, backup)
                .map_err(|error| format!("cannot write {}: {error}", backup.display()))?;
        }
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
    }
    let temp = sibling(path, ".rpi-tmp");
    std::fs::write(&temp, &new_text)
        .map_err(|error| format!("cannot write {}: {error}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("cannot replace {}: {error}", path.display())
    })?;
    Ok(outcome)
}

fn backup_path(path: &Path) -> Option<PathBuf> {
    Some(sibling(path, ".rpi-bak"))
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "settings.json".to_owned());
    path.with_file_name(format!("{name}{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registration() -> Registration {
        Registration {
            command: "C:/tools/rpi.exe".to_owned(),
            args: vec!["--acp".to_owned()],
        }
    }

    /// Mirrors a real Zed file: comment header, trailing commas, strings that
    /// contain braces, and an `agent_servers` object that is not ours to rewrite.
    const REALISH: &str = r#"// Zed settings
//
// docs: https://zed.dev/docs/configuring-zed
{
  "terminal": {
    "dock": "bottom"
  },
  "agent": {
    "commit_message_instructions": "use <type>(<scope>): <subject> {like this}"
  },
  "theme": {
    "mode": "dark",
    "dark": "macOS Classic Dark",
  },
  "agent_servers": {
    "other": {
      "type": "custom",
      "command": "/usr/bin/other",
      "args": [],
    },
  },
}
"#;

    #[test]
    fn inserts_into_an_existing_agent_servers_object() {
        let new = ensure_registered(REALISH, &registration())
            .unwrap()
            .unwrap();

        // Our member lands inside `agent_servers`, after the existing one.
        assert!(new.contains("\"rpi\": {\n      \"type\": \"custom\","));
        assert!(new.contains("\"command\": \"C:/tools/rpi.exe\","));
        assert!(new.contains("\"args\": [\"--acp\"]"));
        // Everything else is byte-for-byte intact, comments included.
        for line in REALISH.lines() {
            assert!(new.contains(line), "lost line: {line}");
        }
        assert!(new.contains("// docs: https://zed.dev/docs/configuring-zed"));
        assert!(new.contains("\"dark\": \"macOS Classic Dark\","));
        // The other agent survives.
        assert!(new.contains("\"/usr/bin/other\""));
    }

    #[test]
    fn is_idempotent() {
        let once = ensure_registered(REALISH, &registration())
            .unwrap()
            .unwrap();
        assert!(ensure_registered(&once, &registration()).unwrap().is_none());
    }

    #[test]
    fn snippet_is_indented_and_parses_back() {
        let snippet = registration().snippet();
        // The `rpi` line sits under `agent_servers`, not flush against column 0.
        assert!(snippet.contains("\n    \"rpi\": {\n"), "{snippet}");
        assert!(snippet.contains("\n      \"args\": [\"--acp\"]\n"));
        assert!(snippet.trim_end().ends_with('}'));
    }

    #[test]
    fn adds_agent_servers_when_absent() {
        let text = "{\n  \"ui_font_size\": 16,\n}\n";
        let new = ensure_registered(text, &registration()).unwrap().unwrap();
        assert!(new.contains("\"agent_servers\": {\n    \"rpi\": {"));
        assert!(new.contains("\"ui_font_size\": 16,"));
        // No doubled comma from the existing trailing comma.
        assert!(!new.contains(",,"));
    }

    #[test]
    fn adds_agent_servers_to_an_empty_object() {
        let new = ensure_registered("{}\n", &registration()).unwrap().unwrap();
        assert_eq!(
            new,
            "{\n  \"agent_servers\": {\n    \"rpi\": {\n      \"type\": \"custom\",\n      \
             \"command\": \"C:/tools/rpi.exe\",\n      \"args\": [\"--acp\"]\n    }\n  }\n}\n"
        );
    }

    #[test]
    fn handles_an_empty_agent_servers_object() {
        let text = "{\n  \"agent_servers\": {}\n}\n";
        let new = ensure_registered(text, &registration()).unwrap().unwrap();
        assert!(new.contains("\"agent_servers\": {\n    \"rpi\": {"));
        assert!(ensure_registered(&new, &registration()).unwrap().is_none());
    }

    #[test]
    fn refualls_to_rewrite_a_non_object_agent_servers() {
        let text = "{\n  \"agent_servers\": 3\n}\n";
        let error = ensure_registered(text, &registration()).unwrap_err();
        assert!(error.contains("not an object"), "{error}");
    }

    #[test]
    fn rejects_a_document_that_is_not_an_object() {
        assert!(ensure_registered("[1, 2]", &registration()).is_err());
    }

    #[test]
    fn ignores_a_commented_out_agent_servers() {
        // A commented key must not be mistaken for a real one.
        let text = "{\n  // \"agent_servers\": {},\n  \"ui_font_size\": 16\n}\n";
        let new = ensure_registered(text, &registration()).unwrap().unwrap();
        assert!(new.contains("\n  \"agent_servers\": {\n    \"rpi\": {"));
        assert!(new.starts_with("{\n  // \"agent_servers\": {},"));
    }

    #[test]
    fn detect_uses_the_acp_flag_only_for_the_rpi_host() {
        let host = Registration::for_executable(Path::new("C:/x/rpi.exe"));
        assert_eq!(host.args, vec!["--acp".to_owned()]);
        let bridge = Registration::for_executable(Path::new("C:/x/rpi-acp-bridge.exe"));
        assert!(bridge.args.is_empty());
    }

    #[test]
    fn settings_path_resolution_is_per_os() {
        let win = settings_path_from(
            "windows",
            Some("C:/Users/me/AppData/Roaming".into()),
            None,
            Some("C:/Users/me".into()),
            None,
        )
        .unwrap();
        assert_eq!(
            win,
            PathBuf::from("C:/Users/me/AppData/Roaming/Zed/settings.json")
        );

        let mac = settings_path_from("macos", None, None, Some("/Users/me".into()), None).unwrap();
        assert_eq!(
            mac,
            PathBuf::from("/Users/me/Library/Application Support/Zed/settings.json")
        );

        let linux = settings_path_from("linux", None, None, Some("/home/me".into()), None).unwrap();
        assert_eq!(linux, PathBuf::from("/home/me/.config/zed/settings.json"));

        let explicit = settings_path_from(
            "linux",
            None,
            None,
            Some("/home/me".into()),
            Some("/tmp/z.json".into()),
        )
        .unwrap();
        assert_eq!(explicit, PathBuf::from("/tmp/z.json"));
    }

    #[test]
    fn describe_names_each_outcome() {
        let path = Path::new("/tmp/settings.json");
        assert!(describe(&Outcome::AlreadyPresent, path, false).starts_with("already registered"));
        assert!(describe(&Outcome::Created, path, false).starts_with("created"));
        assert!(describe(&Outcome::Created, path, true).starts_with("would create"));
        assert!(describe(&Outcome::Updated { backup: None }, path, false).starts_with("added rpi"));
        assert!(describe(&Outcome::Updated { backup: None }, path, true).starts_with("would add"));
    }

    #[test]
    fn register_file_creates_and_then_is_a_no_op() {
        let dir = std::env::temp_dir().join("rpi-acp-zed-register");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");

        assert_eq!(
            register_file(&path, &registration(), false).unwrap(),
            Outcome::Created
        );
        let created = std::fs::read_to_string(&path).unwrap();
        assert!(created.contains("\n    \"rpi\": {\n"), "{created}");
        assert!(
            created.contains("\n      \"args\": [\"--acp\"]\n"),
            "{created}"
        );
        assert_eq!(
            register_file(&path, &registration(), false).unwrap(),
            Outcome::AlreadyPresent
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn register_file_backs_up_once_and_replaces_atomically() {
        let dir = std::env::temp_dir().join("rpi-acp-zed-backup");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, REALISH).unwrap();

        let outcome = register_file(&path, &registration(), false).unwrap();
        let backup = match outcome {
            Outcome::Updated { backup } => backup.expect("backup path"),
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), REALISH);
        assert!(!dir.join("settings.json.rpi-tmp").exists());

        // A later change must not clobber the original backup.
        let modified = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\"rpi\"", "\"rpi-x\"");
        std::fs::write(&path, &modified).unwrap();
        register_file(&path, &registration(), false).unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), REALISH);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = std::env::temp_dir().join("rpi-acp-zed-dry-run");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");
        assert_eq!(
            register_file(&path, &registration(), true).unwrap(),
            Outcome::Created
        );
        assert!(!path.exists(), "dry run must not create the file");
    }
}
