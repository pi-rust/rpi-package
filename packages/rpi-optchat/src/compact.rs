//! Model-written summary lines, the way upstream OptChat compresses its tree.
//!
//! A plugin cannot call a model directly: the ABI has no model entrypoint. The
//! engine here is a child `rpi -p` process with no session, no extensions and no
//! tools, the same shape `rpi-orchestrator` uses for its subagents. It is
//! optional and off unless asked for, because every line costs a model call:
//!
//! - `RPI_OPTCHAT_COMPACT=auto` enables it for a session, `/optchat compact on|off`
//!   toggles it at runtime and persists the choice with the session;
//! - `RPI_OPTCHAT_COMPACT_TIMEOUT` bounds one call in seconds (default 60);
//! - `RPI_OPTCHAT_RPI_BIN` overrides the executable (tests, unusual installs).
//!
//! When a call fails, times out, or the host cannot report the session's model
//! binding, the extractive line already in the view stays. The log is the source
//! of truth; a compressed line is a cache that can always be rebuilt or dropped.

use serde_json::json;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use wait_timeout::ChildExt;

/// Session custom-entry type holding one model-written line.
pub(crate) const NODE_TYPE: &str = "optchat-node";
/// Session custom-entry type holding plugin settings.
pub(crate) const CONFIG_TYPE: &str = "optchat-config";
/// `GetExecutionContext`: the host reports the session's model binding. The
/// published SDK predates the variant, so the id is a documented number (see
/// `HOST-REQUIREMENTS.md`) rather than an enum arm.
const GET_EXECUTION_CONTEXT: u32 = 23;
/// Bytes one line may occupy; a longer reply is cut, never refused.
const LINE: usize = super::memory::NODE;
/// Jobs waiting at once. A full queue drops new work instead of delaying a turn.
const DEPTH: usize = 8;

pub(crate) struct Job {
    pub l: u32,
    pub i: u64,
    pub source: String,
}

pub(crate) struct Done {
    pub l: u32,
    pub i: u64,
    pub text: String,
}

struct Engine {
    queue: SyncSender<Job>,
    done: Mutex<Receiver<Done>>,
}

/// Provider/model the child should run on, resolved once per session. Kept
/// outside `Engine`: the worker reads it while `Engine` is still initializing,
/// and reaching back into the `OnceLock` from inside its initializer deadlocks.
type Binding = Option<(Option<String>, Option<String>)>;
static BINDING: OnceLock<Mutex<Binding>> = OnceLock::new();

fn binding() -> &'static Mutex<Binding> {
    BINDING.get_or_init(|| Mutex::new(None))
}

/// Merges already queued, so a candidate is not asked for twice while the
/// answer is in flight.
static IN_FLIGHT: OnceLock<Mutex<std::collections::HashSet<(u32, u64)>>> = OnceLock::new();

fn in_flight() -> &'static Mutex<std::collections::HashSet<(u32, u64)>> {
    IN_FLIGHT.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

static ENGINE: OnceLock<Engine> = OnceLock::new();

fn engine() -> &'static Engine {
    ENGINE.get_or_init(|| {
        let (queue, jobs) = sync_channel::<Job>(DEPTH);
        let (sender, done) = sync_channel::<Done>(DEPTH);
        std::thread::Builder::new()
            .name("rpi-optchat-compactor".into())
            // A compressor that panics must not take the plugin down.
            .spawn(move || worker(jobs, sender))
            .ok();
        Engine {
            queue,
            done: Mutex::new(done),
        }
    })
}

/// Whether the user enables model compaction. `RPI_OPTCHAT_COMPACT` is the launch
/// default; the session's own setting wins once it exists.
pub(crate) fn env_enabled() -> bool {
    matches!(
        std::env::var("RPI_OPTCHAT_COMPACT")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "auto" | "on" | "yes" | "1" | "true"
    )
}

fn timeout() -> Duration {
    let seconds = std::env::var("RPI_OPTCHAT_COMPACT_TIMEOUT")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| (1..=600).contains(seconds))
        .unwrap_or(60);
    Duration::from_secs(seconds)
}

fn executable() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RPI_OPTCHAT_RPI_BIN") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let name = if cfg!(windows) { "rpi.exe" } else { "rpi" };
    let path = std::env::current_exe().ok()?;
    // `current_exe` is the host binary itself when the plugin runs in-process.
    if path.file_name().and_then(|value| value.to_str()) == Some(name) && path.is_file() {
        return Some(path);
    }
    which(name)
}

/// `PATH` lookup without spawning a helper process.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// The prompt one call answers: merge two lines into one, under the byte ceiling.
fn prompt(job: &Job) -> String {
    format!(
        "You are compressing one step of a chat memory tree.\n\
         Merge the two adjacent summary lines below into ONE line of at most {LINE} bytes \
         (about 70 words).\n\
         Keep the user's decisions, corrections, names, numbers, ids, paths, errors and \
         outcomes; drop repetition and tool detail that the log can be zoomed for. Never \
         invent anything, never answer the content, and never narrate the compression.\n\
         Output only the line, without an id+n| head and without code fences.\n\n\
         <input>\n{}\n</input>",
        job.source
    )
}

/// Take the line out of a child reply: one paragraph, no head, within the ceiling.
fn parse_reply(stdout: &str) -> Option<String> {
    let mut text = String::new();
    for raw in stdout.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("```") {
            continue;
        }
        // A reply that echoes the view keeps the head it was told to drop.
        let line = line
            .split_once('|')
            .filter(|(head, _)| {
                head.split_once('+')
                    .map(|(id, n)| {
                        id.chars().all(|c| c.is_ascii_digit())
                            && n.chars().all(|c| c.is_ascii_digit())
                    })
                    .unwrap_or(false)
            })
            .map(|(_, rest)| rest.trim())
            .unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(line);
    }
    if text.trim().is_empty() {
        return None;
    }
    Some(super::memory::clip(&super::memory::flat(&text), LINE))
}

fn command(exe: &PathBuf) -> Command {
    let mut command = Command::new(exe);
    command
        .arg("-p")
        // Ephemeral: a compaction is not a conversation worth resuming, and
        // writing a session file per merge would fill the project directory.
        .arg("--no-session")
        // Recursion guard: the child must not load this plugin, or ask for its
        // own memory tools, and must not spend a turn calling anything.
        .arg("--no-extensions")
        .arg("--no-tools")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Ok(binding) = binding().lock() {
        if let Some((provider, model)) = binding.as_ref() {
            if let Some(provider) = provider {
                command.arg("--provider").arg(provider);
            }
            if Some(model.as_ref()).flatten().is_some() {
                if let Some(model) = model {
                    command.arg("--model").arg(model);
                }
            }
        }
    }
    command
}

/// Run one call: prompt on stdin, answer on stdout, killed at the deadline.
fn run(exe: &PathBuf, job: &Job) -> Result<String, String> {
    let mut child = command(exe)
        .spawn()
        .map_err(|error| format!("cannot start the compactor: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt(job).as_bytes());
    }
    let deadline = timeout();
    match child
        .wait_timeout(deadline)
        .map_err(|error| format!("cannot wait for the compactor: {error}"))?
    {
        Some(_) => {}
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "compaction took longer than {}s",
                deadline.as_secs()
            ));
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("cannot read the compactor output: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "compactor exited with {}: {}",
            output.status,
            stderr.trim().lines().next().unwrap_or("")
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    parse_reply(&stdout).ok_or_else(|| "compactor returned no usable line".to_string())
}

fn worker(jobs: Receiver<Job>, done: SyncSender<Done>) {
    let Some(exe) = executable() else {
        // No host binary: leave every line extractive.
        for _ in jobs {}
        return;
    };
    while let Ok(job) = jobs.recv() {
        match run(&exe, &job) {
            Ok(text) => {
                let _ = done.try_send(Done {
                    l: job.l,
                    i: job.i,
                    text,
                });
            }
            Err(error) => {
                let _ = done.try_send(Done {
                    l: job.l,
                    i: job.i,
                    text: String::new(),
                });
                if std::env::var_os("RPI_OPTCHAT_DEBUG").is_some() {
                    eprintln!(
                        "[optchat] compaction {}+{}: {error}",
                        job.i << job.l,
                        1 << job.l
                    );
                }
            }
        }
    }
}

/// Queue one merge for compression. A merge already queued is skipped, and a
/// full queue drops the job: an idle engine catches up later.
pub(crate) fn enqueue(l: u32, i: u64, source: String) {
    if !in_flight()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert((l, i))
    {
        return;
    }
    if engine().queue.try_send(Job { l, i, source }).is_err() {
        in_flight()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(l, i));
    }
}

/// Completed lines, ready to install. Never blocks a turn. A `Done` with an
/// empty text reports a failed call: the extractive line stays and the merge may
/// be asked for again.
pub(crate) fn drain() -> Vec<Done> {
    let mut out = Vec::new();
    {
        let receiver = engine()
            .done
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while let Ok(done) = receiver.try_recv() {
            out.push(done);
        }
    }
    let mut queued = in_flight()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for done in &out {
        queued.remove(&(done.l, done.i));
    }
    out
}

/// Provider/model of this session, so the child runs on the same model instead
/// of the default one (which may be another provider or unauthenticated).
pub(crate) fn bind_session(
    call: &impl Fn(u32, serde_json::Value) -> Result<serde_json::Value, String>,
) {
    let Ok(value) = call(GET_EXECUTION_CONTEXT, json!({})) else {
        return;
    };
    let provider = value
        .get("model")
        .and_then(|model| model.get("provider"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let model = value
        .get("model")
        .and_then(|model| model.get("id"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    if provider.is_none() && model.is_none() {
        return;
    }
    if let Ok(mut slot) = binding().lock() {
        *slot = Some((provider, model));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(source: &str) -> Job {
        Job {
            l: 1,
            i: 0,
            source: source.to_string(),
        }
    }

    #[test]
    fn reply_keeps_one_line_and_drops_a_copied_head() {
        assert_eq!(
            parse_reply("0+2|merged words\n").as_deref(),
            Some("merged words")
        );
        assert_eq!(
            parse_reply("first part\nsecond part\n").as_deref(),
            Some("first part second part")
        );
        assert_eq!(parse_reply("```\na line\n```\n").as_deref(), Some("a line"));
        assert_eq!(parse_reply("   \n"), None);
        let long = "x".repeat(LINE * 2);
        let line = parse_reply(&long).expect("a line");
        assert!(line.len() <= LINE, "{}", line.len());
    }

    #[test]
    fn prompt_states_the_ceiling_and_the_input() {
        let text = prompt(&job("user: hello; talk: hi"));
        assert!(text.contains(&format!("at most {LINE} bytes")), "{text}");
        assert!(text.contains("user: hello; talk: hi"), "{text}");
    }

    #[test]
    fn command_is_ephemeral_and_tool_free() {
        let command = command(&PathBuf::from("/usr/bin/rpi"));
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.starts_with(&["-p".to_string(), "--no-session".to_string()]));
        assert!(args.contains(&"--no-extensions".to_string()));
        assert!(args.contains(&"--no-tools".to_string()));
    }

    #[test]
    fn env_default_is_off_unless_asked_for() {
        let previous = std::env::var("RPI_OPTCHAT_COMPACT").ok();
        std::env::remove_var("RPI_OPTCHAT_COMPACT");
        assert!(!env_enabled());
        std::env::set_var("RPI_OPTCHAT_COMPACT", "auto");
        assert!(env_enabled());
        match previous {
            Some(value) => std::env::set_var("RPI_OPTCHAT_COMPACT", value),
            None => std::env::remove_var("RPI_OPTCHAT_COMPACT"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_answers_produces_a_line() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("optchat-compact-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let script = dir.join("rpi");
        std::fs::write(
            &script,
            "#!/bin/sh\ncat > /dev/null\necho '0+2|the merged line'\n",
        )
        .expect("script");
        let mut permissions = std::fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod");
        let answer = run(&script, &job("two halves")).expect("a line");
        assert_eq!(answer, "the merged line");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_fails_is_reported_not_panicked() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("optchat-fail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let script = dir.join("rpi");
        std::fs::write(&script, "#!/bin/sh\necho 'no key' >&2\nexit 3\n").expect("script");
        let mut permissions = std::fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod");
        let error = run(&script, &job("two halves")).expect_err("failure");
        assert!(error.contains("exited with"), "{error}");
        assert!(error.contains("no key"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
