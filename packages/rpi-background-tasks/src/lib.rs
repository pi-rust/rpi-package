#![forbid(unsafe_code)]
// The SDK's safe export macro owns the raw-pointer ABI boundary.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

//! Background shell jobs plus a `jobs` inspector for the rpi Rust agent.
//!
//! Every job state change appends a self-contained `background-tasks` custom
//! snapshot so a supervising UI can tell whether a session still has work
//! running without reading a full transcript.

use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::process::ProcessTree;
use rpi_plugin_sdk::{
    EventHandlerFn, EventTag, FreeStringFn, PluginApi, RuntimeActionId, StableToolSchema,
    StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Custom entry type. Lowercase `background-tasks` is the contract external
/// supervisors match on; it is intentionally not the crate name.
const SNAPSHOT_ENTRY: &str = "background-tasks";

const BACKGROUND_TOOL: &str = "background";
const JOBS_TOOL: &str = "jobs";

/// Upper bound for the command text echoed back into the snapshot/summary.
const MAX_COMMAND_CHARS: usize = 2_000;
const MAX_LABEL_CHARS: usize = 200;
/// Retained output lines per job. Older lines are dropped so a chatty server
/// cannot grow the process table without bound.
const MAX_OUTPUT_LINES: usize = 500;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const DEFAULT_TAIL_LINES: usize = 40;

// Status vocabulary. Only these values are ever emitted; supervisors bucket
// them into active/done/failed and reject anything they do not recognize.
const STATUS_RUNNING: &str = "running";
const STATUS_COMPLETED: &str = "completed";
const STATUS_FAILED: &str = "failed";
const STATUS_INTERRUPTED: &str = "interrupted";
const STATUS_CANCELLED: &str = "cancelled";

type Runtime = PluginApi;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static TABLE: OnceLock<Mutex<Table>> = OnceLock::new();
/// Serializes snapshot appends so the JSONL order matches status order.
static APPEND_GATE: Mutex<()> = Mutex::new(());

/// Retained output of one job, bounded by both line and byte budgets.
#[derive(Default, Debug)]
struct Output {
    lines: Vec<String>,
    bytes: usize,
    /// Set when older output was dropped, so `tail` can say so.
    truncated: bool,
}

impl Output {
    fn push(&mut self, line: String) {
        let len = line.len() + 1;
        // A single oversized line is clipped rather than retained whole.
        let line = if len > MAX_OUTPUT_BYTES {
            self.truncated = true;
            let clipped: String = line.chars().take(MAX_OUTPUT_BYTES / 2).collect();
            clipped
        } else {
            line
        };
        self.bytes += line.len() + 1;
        self.lines.push(line);
        while self.lines.len() > MAX_OUTPUT_LINES || self.bytes > MAX_OUTPUT_BYTES {
            if self.lines.is_empty() {
                break;
            }
            self.bytes = self.bytes.saturating_sub(self.lines.remove(0).len() + 1);
            self.truncated = true;
        }
    }

    fn tail(&self, count: usize) -> Vec<String> {
        let start = self.lines.len().saturating_sub(count);
        self.lines[start..].to_vec()
    }
}

#[derive(Debug)]
struct Job {
    id: u64,
    kind: &'static str,
    label: String,
    command: String,
    pid: Option<u32>,
    status: &'static str,
    exit_code: Option<i32>,
    started_ms: u128,
    finished_ms: Option<u128>,
    output: Output,
    /// Kept so `jobs` action `kill` can signal and reap the process group.
    ///
    /// Cleared by `settle` once the leader exits, so it must not be the only
    /// record of the tree.
    child: Option<Arc<Mutex<Child>>>,
    /// Session that started this job, snapshotted at spawn and never revised.
    ///
    /// A job belongs to exactly one session for its whole life: `jobs` list,
    /// tail and kill only see rows whose owner matches the caller's session.
    /// A forked or cloned session is a different owner, so it cannot see or
    /// stop work that belongs to the session it was derived from.
    owner: String,
    /// Handle on the spawned tree, independent of the leader's lifetime.
    ///
    /// The SDK's `ProcessTree` is a Job Object on Windows and a process group
    /// on Unix. Both outlive the shell that was spawned: a dev server behind
    /// `npm run dev` keeps running after its `sh -c` wrapper exits, and the
    /// only way to reach it is through the tree handle.
    ///
    /// `taskkill /T` is not that mechanism — it walks parent pids, and a
    /// git-bash child is not on that chain, which is why it left processes
    /// alive when this plugin was reviewed on Windows.
    tree: Option<Arc<ProcessTree>>,
    cancel_requested: bool,
}

impl Job {
    /// Supervisor-facing shape. `status` is the field external readers key on;
    /// the rest is for humans reading the JSONL.
    fn snapshot(&self) -> Value {
        let mut value = json!({
            "id": self.id.to_string(),
            "kind": self.kind,
            "label": self.label,
            "command": self.command,
            "status": self.status,
        });
        if let Some(pid) = self.pid {
            value["pid"] = json!(pid);
        }
        if let Some(code) = self.exit_code {
            value["exitCode"] = json!(code);
        }
        value["startedAt"] = json!(self.started_ms);
        if let Some(finished) = self.finished_ms {
            value["finishedAt"] = json!(finished);
        }
        value
    }

    fn is_active(&self) -> bool {
        matches!(self.status, STATUS_RUNNING)
    }
}

#[derive(Debug, Default)]
struct Table {
    jobs: Vec<Job>,
    next_id: u64,
}

fn table() -> &'static Mutex<Table> {
    TABLE.get_or_init(|| Mutex::new(Table::default()))
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let clipped: String = text.chars().take(max).collect();
    format!("{clipped}…")
}

// ---------------------------------------------------------------------------
// Snapshot persistence
// ---------------------------------------------------------------------------

type RuntimeFailure = String;

fn call_runtime(id: RuntimeActionId, args: Value) -> Result<Value, RuntimeFailure> {
    let runtime = *RUNTIME
        .get()
        .ok_or_else(|| "background-tasks runtime is unavailable".to_string())?;
    let mut out = StbString::empty();
    let input_json = args.to_string();
    let input = StbStringRef::from_str(&input_json);
    let rc = (runtime.runtime_action)(id.into(), input, &mut out, runtime.user_data);
    let text = out.to_string_lossy();
    (runtime.free_string)(out);
    if rc != 0 {
        return Err(if text.is_empty() {
            "runtime action failed".into()
        } else {
            text
        });
    }
    if text.trim().is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_str(&text).map_err(|e| format!("invalid runtime response: {e}"))
    }
}

/// Full current job list, newest snapshot wins. Every field is recomputed from
/// memory so a later snapshot never depends on an earlier one.
/// Snapshot payload for **one session**.
///
/// The record is appended to a session's own transcript, so it must describe
/// only that session's jobs. Writing the whole table would put another
/// session's commands and pids into this session's durable history even though
/// the tool queries already hide them.
fn snapshot_json(table: &Table, owner: &str) -> Value {
    let jobs: Vec<Value> = table
        .jobs
        .iter()
        .filter(|job| job.owner == owner)
        .map(Job::snapshot)
        .collect();
    json!({"jobs": jobs})
}

/// Append the whole job table. A failure is reported to the caller but must not
/// abort the tool: the process is already running and killing it would be worse.
fn persist_snapshot(owner: &str) -> Result<(), String> {
    // Take the append gate **before** copying the table.
    //
    // The other order lets two writers interleave: A copies the table, B copies
    // a newer table, B appends, then A appends its older snapshot last — the
    // session's newest `background-tasks` record would then describe the older
    // state (a finished job showing as `running` again). Holding the gate
    // across both steps makes the append order match the state order.
    let _gate = APPEND_GATE.lock().unwrap_or_else(|p| p.into_inner());
    let payload = {
        let table = table().lock().unwrap_or_else(|p| p.into_inner());
        snapshot_json(&table, owner)
    };
    call_runtime(
        RuntimeActionId::AppendEntry,
        json!({"customType": SNAPSHOT_ENTRY, "data": payload}),
    )
    .map(|_| ())
}

// ---------------------------------------------------------------------------
// Process lifecycle
// ---------------------------------------------------------------------------

/// Run a helper and return its stdout, or `None` if it is missing or fails.
///
/// Used only for shell discovery (`where`, `which`), so a missing helper is an
/// ordinary "not found", not an error worth surfacing.
fn quiet_command(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

/// A bash executable plus the flag it takes for a command string.
///
/// Resolved once per process: probing spawns helper commands (`where`,
/// `which`), and `background` may be called in a tight loop.
struct ShellConfig {
    shell: std::path::PathBuf,
    args: Vec<String>,
}

/// Find bash the same way the host's `bash` tool does.
///
/// This mirrors `rpi-tools::os_env` so a command behaves identically whether it
/// runs through `bash` or through `background`: same shell, same POSIX syntax,
/// and the same refusal to fall back to something else.
///
/// Deliberately a **copy** of the host's probe: the plugin ABI exposes no shell
/// discovery, and the alternative — `cmd /C` on Windows — would mean `ls -la`
/// works through `bash` and fails through `background` in the same session.
/// Replace this with the SDK's own resolver if it ever exposes one.
#[cfg(windows)]
fn resolve_shell() -> Option<ShellConfig> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    // `var_os`, not `var`: a Windows profile path is not required to be UTF-8.
    for key in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(base) = std::env::var_os(key) {
            candidates.push(
                std::path::PathBuf::from(base)
                    .join("Git")
                    .join("bin")
                    .join("bash.exe"),
            );
        }
    }
    for candidate in &candidates {
        if candidate.is_file() {
            return Some(ShellConfig {
                shell: candidate.clone(),
                args: vec!["-c".to_string()],
            });
        }
    }
    // `where bash.exe` — the same fallback the host uses when git-bash is
    // installed somewhere unusual.
    let found = crate::quiet_command("where", &["bash.exe"])?;
    let first = found.lines().next().unwrap_or("").trim().to_string();
    if first.is_empty() {
        return None;
    }
    let path = std::path::PathBuf::from(first);
    path.is_file().then_some(ShellConfig {
        shell: path,
        args: vec!["-c".to_string()],
    })
}

/// Unix: `/bin/bash`, then `which bash`, then `sh`.
///
/// `sh` last rather than first: the host prefers bash, and bashisms are common
/// in the commands people background (`[[ ]]`, arrays). A POSIX-only `sh`
/// would reject them while the `bash` tool accepts them.
#[cfg(not(windows))]
fn resolve_shell() -> Option<ShellConfig> {
    let bash = std::path::PathBuf::from("/bin/bash");
    if bash.is_file() {
        return Some(ShellConfig {
            shell: bash,
            args: vec!["-c".to_string()],
        });
    }
    if let Some(found) = crate::quiet_command("which", &["bash"]) {
        let first = found.lines().next().unwrap_or("").trim().to_string();
        if !first.is_empty() {
            let path = std::path::PathBuf::from(first);
            if path.is_file() {
                return Some(ShellConfig {
                    shell: path,
                    args: vec!["-c".to_string()],
                });
            }
        }
    }
    let sh = std::path::PathBuf::from("/bin/sh");
    sh.is_file().then_some(ShellConfig {
        shell: sh,
        args: vec!["-c".to_string()],
    })
}

fn shell_config() -> Result<&'static ShellConfig, String> {
    static SHELL: OnceLock<Option<ShellConfig>> = OnceLock::new();
    SHELL.get_or_init(resolve_shell).as_ref().ok_or_else(|| {
        "no usable shell found. Install Bash (Git for Windows provides it on \
             Windows) and try again; a background job will not run under a \
             different shell than the `bash` tool uses."
            .to_string()
    })
}

/// Build the shell invocation. No fallback shell: a job must run under the same
/// shell the `bash` tool would use, or the model's commands behave differently
/// depending on which tool it picked.
fn shell_command(command: &str) -> Result<Command, String> {
    let config = shell_config()?;
    let mut cmd = Command::new(&config.shell);
    cmd.args(&config.args).arg(command);
    // Own process tree so a kill reaches descendants, and so the leader exiting
    // does not orphan them. `ProcessTree` is a Job Object on Windows and a
    // process group on Unix; `taskkill /T` would not be equivalent because it
    // walks parent pids and an MSYS child is not on that chain.
    rpi_plugin_sdk::process::ProcessTree::prepare(&mut cmd);
    Ok(cmd)
}

/// Longest single line kept from a child's output, in bytes.
///
/// `read_until` grows its buffer until it sees a newline or EOF, so a command
/// that emits megabytes without one would allocate all of it before `push` ever
/// gets to clip the result. Capping the read itself is the only bound that holds
/// while the pipe is still being drained.
const MAX_READ_LINE_BYTES: usize = MAX_OUTPUT_BYTES;

fn spawn_reader<R: Read + Send + 'static>(
    stream: R,
    id: u64,
    is_stderr: bool,
    stop: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        // Bytes discarded from the current line after it hit the cap. Reported
        // once per line so the user knows the content was truncated, rather
        // than silently seeing a clipped line that looks complete.
        let mut dropped: u64 = 0;
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            buf.clear();
            // `take` bounds the *read*: once the cap is reached it stops
            // pulling bytes, so memory stays bounded even when the child keeps
            // writing. The rest of the line is drained and discarded below.
            let read = (&mut reader)
                .take(MAX_READ_LINE_BYTES as u64)
                .read_until(b'\n', &mut buf);
            match read {
                Ok(0) => return,
                Ok(_) => {
                    let complete = buf.last() == Some(&b'\n');
                    if !complete {
                        // Cap hit before a newline: consume the remainder of the
                        // line so the next iteration starts at a line boundary.
                        let mut sink = Vec::new();
                        loop {
                            sink.clear();
                            match (&mut reader).take(64 * 1024).read_until(b'\n', &mut sink) {
                                Ok(0) => break,
                                Ok(m) => {
                                    dropped += m as u64;
                                    if sink.last() == Some(&b'\n') {
                                        break;
                                    }
                                }
                                Err(_) => return,
                            }
                        }
                    }
                    let text = String::from_utf8_lossy(&buf);
                    let mut line = text.trim_end_matches(['\n', '\r']).to_string();
                    if dropped > 0 {
                        // Clip the body *before* appending the notice: `push`
                        // clips from the end, so a marker appended to an
                        // oversized line would be the first thing discarded.
                        let keep = MAX_OUTPUT_BYTES / 2;
                        if line.len() > keep {
                            let clipped: String = line.chars().take(keep).collect();
                            line = clipped;
                        }
                        line.push_str(&format!(" … [{dropped} bytes dropped]"));
                        dropped = 0;
                    }
                    let line = if is_stderr {
                        format!("[stderr] {line}")
                    } else {
                        line
                    };
                    let mut table = table().lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(job) = table.jobs.iter_mut().find(|job| job.id == id) {
                        job.output.push(line);
                    }
                }
                Err(_) => return,
            }
        }
    });
}

/// Reap one job: record its terminal status, then persist a snapshot.
/// `fallback` is used when the exit status cannot be read.
fn settle(id: u64, exit: Option<i32>, fallback: &'static str) {
    // The snapshot is appended to the owning session's transcript, so the owner
    // has to come out of the same locked read that settles the row.
    let changed = {
        let mut table = table().lock().unwrap_or_else(|p| p.into_inner());
        let Some(job) = table.jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        if !job.is_active() {
            return;
        }
        let owner = job.owner.clone();
        // An explicit kill wins over whatever the final exit status says.
        job.status = if job.cancel_requested {
            STATUS_CANCELLED
        } else {
            match exit {
                Some(0) => STATUS_COMPLETED,
                Some(_) => STATUS_FAILED,
                None => fallback,
            }
        };
        job.exit_code = exit;
        job.finished_ms = Some(now_ms());
        // The leader is gone, so the `Child` handle cannot reap or signal any
        // more. Keep the tree handle: descendants may still be alive and must
        // remain killable.
        job.child = None;
        Some(owner)
    };
    if let Some(owner) = changed {
        let _ = persist_snapshot(&owner);
    }
}

/// Wait for the child off the tool thread, then settle its row. The table owns
/// the `Child`, so this polls `try_wait` instead of blocking in `wait`: only
/// `kill` (or this waiter) ever reaps, and neither can reap twice.
fn watch(id: u64, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(50));
        let outcome = {
            let mut table = table().lock().unwrap_or_else(|p| p.into_inner());
            let Some(job) = table.jobs.iter_mut().find(|job| job.id == id) else {
                return;
            };
            if !job.is_active() {
                return;
            }
            match job.child.as_ref() {
                Some(child) => {
                    let mut child = child.lock().unwrap_or_else(|p| p.into_inner());
                    match child.try_wait() {
                        Ok(Some(status)) => Some((status.code(), status.success())),
                        // Still running, or the status is unreadable and the
                        // waiter must stop polling rather than spin forever.
                        Ok(None) => None,
                        Err(_) => Some((None, false)),
                    }
                }
                // `kill` already dropped the handle and settled the row.
                None => return,
            }
        };
        match outcome {
            None => continue,
            Some((code, _)) => {
                stop.store(true, Ordering::SeqCst);
                settle(id, code, STATUS_FAILED);
                return;
            }
        }
    });
}

fn start_background(params: &Value) -> Result<String, String> {
    let owner = session_owner(params)?;
    let command = params
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or("command is required")?;
    let label = params
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| truncate_chars(text, MAX_LABEL_CHARS))
        .unwrap_or_else(|| truncate_chars(command, 60));
    // Reject over-long commands instead of silently truncating them.
    //
    // Truncating before `spawn` changes the program: `npm run build && ./deploy`
    // cut at 2000 chars would run a *different* command than the model asked
    // for, with no signal that it happened. A visible error is strictly better.
    if command.chars().count() > MAX_COMMAND_CHARS {
        return Err(format!(
            "command is too long ({} chars, limit {MAX_COMMAND_CHARS});              write it to a script and run that instead",
            command.chars().count()
        ));
    }

    let mut child = shell_command(command)?
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to start background command: {error}"))?;

    // Take the tree handle before anything else can drop the leader: on
    // Windows this is the Job Object the shell and every descendant inherit,
    // and it is the only thing that can still reach them after the shell exits.
    // `ProcessTree::prepare` creates the process **suspended** on Windows and
    // `attach` resumes it. If attaching fails the child is left suspended with
    // no handle owning it: it never runs, never exits, and cannot be killed
    // through the tree we failed to obtain. Kill it here before reporting.
    let tree = match ProcessTree::attach(&child) {
        Ok(tree) => Arc::new(tree),
        Err(error) => {
            // Terminate, then reap with a bound.
            //
            // `Child::kill` only issues `TerminateProcess` — it does not wait —
            // so the signal itself cannot block. The wait afterwards does, and
            // it must be bounded: a process the kernel refuses to terminate
            // would otherwise wedge the tool call forever. The host's own shell
            // uses the same shape (`start_kill`, no wait) for the same reason.
            let mut child = child;
            let _ = child.kill();
            reap_bounded(&mut child, Duration::from_secs(5));
            // The SDK's own wording ("protect MCP process tree") is its
            // internal context and means nothing here, so the message leads
            // with what happened and keeps the SDK text only as the cause.
            return Err(format!(
                "could not attach the background process to a job object, so it was \
                 terminated instead of left suspended: {error}"
            ));
        }
    };

    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Allocate the id before wiring any thread so every reader and the waiter
    // target the same row; the readers must not race an id that does not exist.
    let (id, active) = {
        let mut table = table().lock().unwrap_or_else(|p| p.into_inner());
        let id = table.next_id;
        table.next_id = table.next_id.saturating_add(1);
        table.jobs.push(Job {
            id,
            kind: "bash",
            label: label.clone(),
            command: command.to_string(),
            pid: Some(pid),
            status: STATUS_RUNNING,
            exit_code: None,
            started_ms: now_ms(),
            finished_ms: None,
            output: Output::default(),
            child: Some(Arc::new(Mutex::new(child))),
            owner: owner.clone(),
            tree: Some(tree),
            cancel_requested: false,
        });
        // Scoped to this session: the count is shown to the caller, and
        // another session's running jobs are neither visible nor their concern.
        (
            id,
            table
                .jobs
                .iter()
                .filter(|job| job.is_active() && job.owner == owner)
                .count(),
        )
    };

    // Drain both pipes so the child never blocks on a full pipe buffer, then
    // track its exit off the tool thread.
    let stop = Arc::new(AtomicBool::new(false));
    if let Some(stdout) = stdout {
        spawn_reader(stdout, id, false, stop.clone());
    }
    if let Some(stderr) = stderr {
        spawn_reader(stderr, id, true, stop.clone());
    }
    watch(id, stop);

    let persistence = persist_snapshot(&owner);
    let mut details = json!({
        "action": "background",
        "jobId": id,
        "status": STATUS_RUNNING,
        "activeJobs": active,
    });
    if let Some(persistence) = persistence.err() {
        details["snapshotError"] = json!(persistence);
    }
    let text = format!(
        "Started background job {id} (pid {}) — label: {label}\n\
         Command: {command}\n\
         This call returned immediately; the process is still running.\n\
         Use the `jobs` tool with action=tail, jobId={id} to read its output; \
         action=list for all jobs; action=kill to stop it.",
        pid
    );
    Ok(json!({
        "content": [{"type": "text", "text": text}],
        "details": details,
    })
    .to_string())
}

/// Look up one job **owned by `owner`**. Other sessions' jobs are invisible.
fn find_job(id: u64, owner: &str) -> Option<Value> {
    let table = table().lock().unwrap_or_else(|p| p.into_inner());
    table
        .jobs
        .iter()
        .find(|job| job.id == id && job.owner == owner)
        .map(Job::snapshot)
}

/// The error text for a lookup that missed, or hit another session's job.
///
/// The two cases are deliberately distinguished: "not found" would be a lie
/// when the job exists, and silently succeeding would let one session act on
/// another's work.
///
/// Split out because callers usually hold the table lock when they need this,
/// and `table()` is a plain `Mutex`: asking it again from inside the guard
/// deadlocks.
fn exists_error(id: u64, exists: bool) -> String {
    if exists {
        format!(
            "job {id} belongs to another session; background jobs can only be \
             inspected or stopped by the session that started them"
        )
    } else {
        format!("job {id} not found")
    }
}

fn list_jobs(params: &Value, owner: &str) -> Value {
    let include_done = params
        .get("includeDone")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let table = table().lock().unwrap_or_else(|p| p.into_inner());
    let owned = || table.jobs.iter().filter(|job| job.owner == owner);
    let jobs: Vec<&Job> = owned()
        .filter(|job| include_done || job.is_active())
        .collect();
    let active = owned().filter(|job| job.is_active()).count();
    json!({
        "jobs": jobs.iter().map(|job| job.snapshot()).collect::<Vec<_>>(),
        "total": owned().count(),
        "active": active,
    })
}

fn tail_job(id: u64, lines: usize, owner: &str) -> Result<String, String> {
    let (tail, truncated, status) = {
        let table = table().lock().unwrap_or_else(|p| p.into_inner());
        let job = match table
            .jobs
            .iter()
            .find(|job| job.id == id && job.owner == owner)
        {
            Some(job) => job,
            None => {
                // Resolve the message here, while the answer is available:
                // re-locking `table()` from inside this guard would deadlock.
                let exists = table.jobs.iter().any(|job| job.id == id);
                return Err(exists_error(id, exists));
            }
        };
        (job.output.tail(lines), job.output.truncated, job.status)
    };
    let mut text = format!("Job {id} ({status}) — last {} line(s):", tail.len());
    if truncated {
        text.push_str("\n[older output dropped from the retained buffer]");
    }
    if tail.is_empty() {
        text.push_str("\n(no output yet)");
    } else {
        for line in tail {
            text.push('\n');
            text.push_str(&line);
        }
    }
    Ok(text)
}

/// Terminate a background job's whole process tree.
///
/// Unix: signal the process **group** (the child is spawned as a group leader
/// via `process_group(0)`), so shell-spawned grandchildren die too.
/// Windows: `taskkill /T` walks the tree.
///
/// Falls back to `Child::kill` when the tree-killer is unavailable, so a job
/// always dies even on a system without the helper binary.
/// Wait for a child to be reaped, giving up after `limit`.
///
/// Only one owner may reap, and the caller holds that owner. The bound keeps a
/// stuck child (unkillable syscall) from blocking the tool or shutdown thread
/// forever; a timeout is not an error the caller can act on.
fn reap_bounded(child: &mut Child, limit: Duration) {
    reap_until(child, Instant::now() + limit);
}

/// Reap until `deadline`, shared across several children.
fn reap_until(child: &mut Child, deadline: Instant) {
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => {
                if Instant::now() >= deadline {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Terminate a job's whole process tree.
///
/// The tree handle is the authority: a Job Object on Windows (which every
/// descendant inherits) and a process group on Unix. `taskkill /T` is not
/// equivalent — it walks parent pids, and a git-bash child is not on that
/// chain, so it survives a kill that "succeeded".
fn kill_process_tree(tree: Option<&Arc<ProcessTree>>) -> bool {
    match tree {
        Some(tree) => {
            tree.kill();
            true
        }
        None => false,
    }
}

fn kill_job(id: u64, owner: &str) -> Result<String, String> {
    let (child, tree, pid, was_active) = {
        let mut table = table().lock().unwrap_or_else(|p| p.into_inner());
        if !table
            .jobs
            .iter()
            .any(|job| job.id == id && job.owner == owner)
        {
            // Same reason as `tail_job`: decide the message under the lock
            // instead of re-entering `table()` from inside it.
            let exists = table.jobs.iter().any(|job| job.id == id);
            return Err(exists_error(id, exists));
        }
        let job = table
            .jobs
            .iter_mut()
            .find(|job| job.id == id)
            .expect("the id was just found above");
        if job.is_active() {
            job.cancel_requested = true;
        }
        (
            job.child.clone(),
            job.tree.clone(),
            job.pid,
            job.is_active(),
        )
    };
    // A finished job whose leader is gone can still own live descendants: the
    // shell exits as soon as it has spawned `npm run dev`, and everything the
    // dev server started is still running. Only report "already finished" when
    // the tree itself is gone, otherwise `kill` would be a no-op exactly when
    // the user needs it.
    if !was_active && tree.is_none() {
        let table = table().lock().unwrap_or_else(|p| p.into_inner());
        let status = table
            .jobs
            .iter()
            .find(|job| job.id == id)
            .map(|job| job.status)
            .unwrap_or("finished");
        return Ok(format!("Job {id} already finished ({status})"));
    }
    let mut signal_error = None;
    // The leader may already be gone while descendants live on, so target the
    // tree first; the direct child is only a fallback.
    if !was_active {
        let swept = kill_process_tree(tree.as_ref());
        let mut table = table().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(job) = table.jobs.iter_mut().find(|job| job.id == id) {
            job.tree = None;
        }
        drop(table);
        return if swept {
            Ok(format!(
                "Job {id} had already finished; its process tree was terminated"
            ))
        } else {
            Ok(format!(
                "Job {id} had already finished; no process tree remained"
            ))
        };
    }
    if let Some(child) = &child {
        let mut child = child.lock().unwrap_or_else(|p| p.into_inner());
        // A failed kill is not fatal: the process may have exited between the
        // status check and the signal, which the waiter will report.
        // Kill the whole **tree**: `Child::kill` only signals the `sh -c`
        // wrapper, so a dev server started as `npm run dev` keeps its workers
        // alive after the shell dies. The SDK's `ProcessTree` owns the group
        // (Job Object on Windows), so the signal reaches every descendant.
        if !kill_process_tree(tree.as_ref()) {
            // Fall back to signalling the direct child.
            if let Err(error) = child.kill() {
                signal_error.get_or_insert(error.to_string());
            }
        }
        // Reap with a bounded wait. SIGKILL cannot be ignored, so this returns
        // almost immediately; the bound only protects the tool thread from an
        // unkillable child (e.g. stuck in an uninterruptible syscall) and the
        // handle is detached below, so nobody else can reap this child.
        let reaped_before = Instant::now();
        reap_bounded(&mut child, Duration::from_secs(5));
        if child
            .try_wait()
            .map(|status| status.is_none())
            .unwrap_or(false)
            && Instant::now() >= reaped_before + Duration::from_secs(5)
        {
            signal_error
                .get_or_insert_with(|| "process did not exit within 5s of the kill signal".into());
        }
    }
    {
        let mut table = table().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(job) = table.jobs.iter_mut().find(|job| job.id == id) {
            job.tree = None;
        }
    }
    // Detach the handle so the waiter stops polling a row we just terminated.
    if let Ok(mut table) = table().lock() {
        if let Some(job) = table.jobs.iter_mut().find(|job| job.id == id) {
            job.child = None;
        }
    }
    settle(id, None, STATUS_CANCELLED);
    let suffix = signal_error
        .map(|error| format!("\nSignal note: {error}"))
        .unwrap_or_default();
    Ok(format!(
        "Stopped job {id} (pid {}); status is now {STATUS_CANCELLED}.{suffix}",
        pid.map(|pid| pid.to_string()).unwrap_or_else(|| "?".into())
    ))
}

fn tool_result(action: &str, text: String, details: Value) -> String {
    let mut details = details;
    details["action"] = json!(action);
    json!({"content":[{"type":"text","text":text}],"details":details}).to_string()
}

/// Session that owns the calling tool invocation.
///
/// The host injects `{"__rpi": {"cwd", "sessionId", ...}}` into every plugin
/// tool call (never visible to the model). `sessionId` is that session's id at
/// call time, which is what a job binds to.
///
/// A missing id is refused rather than papered over: falling back to "no owner"
/// would either make the job invisible to everyone or visible to everyone,
/// and both break the guarantee that a job belongs to one session.
fn session_owner(params: &Value) -> Result<String, String> {
    params
        .get(rpi_plugin_sdk::HOST_CONTEXT_KEY)
        .and_then(|host| host.get("sessionId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            "background tasks require a session id, and this host did not provide one. \
             Jobs are scoped to the session that started them, so the host must identify \
             the session on every tool call."
                .to_string()
        })
}

fn background_tool(params: &Value) -> Result<String, String> {
    start_background(params)
}

fn jobs_tool(params: &Value) -> Result<String, String> {
    let owner = session_owner(params)?;
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("list");
    match action {
        "list" => {
            let listed = list_jobs(params, &owner);
            let jobs = listed["jobs"].as_array().cloned().unwrap_or_default();
            let mut text = if jobs.is_empty() {
                "No background jobs".to_string()
            } else {
                format!(
                    "{} job(s), {} running",
                    listed["total"].as_u64().unwrap_or(0),
                    listed["active"].as_u64().unwrap_or(0)
                )
            };
            for job in &jobs {
                let id = job["id"].as_str().unwrap_or("?");
                let status = job["status"].as_str().unwrap_or("?");
                text.push_str(&format!(
                    "\n#{id} [{status}] {} — {}",
                    job["label"].as_str().unwrap_or(""),
                    job["command"].as_str().unwrap_or("")
                ));
            }
            Ok(tool_result(action, text, listed))
        }
        "tail" => {
            let id = params
                .get("jobId")
                .and_then(Value::as_u64)
                .ok_or("jobId is required for action=tail")?;
            let lines = params
                .get("lines")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .unwrap_or(DEFAULT_TAIL_LINES)
                .clamp(1, MAX_OUTPUT_LINES);
            let text = tail_job(id, lines, &owner)?;
            let job = find_job(id, &owner).unwrap_or(Value::Null);
            Ok(tool_result(action, text, json!({"jobId": id, "job": job})))
        }
        "kill" => {
            let id = params
                .get("jobId")
                .and_then(Value::as_u64)
                .ok_or("jobId is required for action=kill")?;
            let text = kill_job(id, &owner)?;
            let job = find_job(id, &owner).unwrap_or(Value::Null);
            Ok(tool_result(action, text, json!({"jobId": id, "job": job})))
        }
        _ => Err("action must be one of: list, tail, kill".into()),
    }
}

// ---------------------------------------------------------------------------
// ABI driving
// ---------------------------------------------------------------------------

static BACKGROUND_HANDLES: ToolHandles<Drive> = ToolHandles::new();
static JOBS_HANDLES: ToolHandles<Drive> = ToolHandles::new();

struct Drive {
    params: Value,
    done: bool,
}

fn take_params(params: StbString, free: Option<FreeStringFn>) -> Value {
    let text = params.to_string_lossy();
    params.free_with(free);
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

fn drive(
    handles: &'static ToolHandles<Drive>,
    runner: fn(&Value) -> Result<String, String>,
    name: &str,
    handle: StepHandle,
) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string(format!("null {name} handle")));
    }
    let Some(entry) = handles.get(handle) else {
        return StepResult::err(StbString::from_string(
            "invalid or destroyed tool handle".into(),
        ));
    };
    let mut state = entry.lock();
    if entry.cancelled.load(Ordering::SeqCst) {
        state.done = true;
        // Cancelling the tool must not orphan a process it already started:
        // `background` returns immediately, so a started job simply keeps
        // running and stays visible through `jobs`.
        return StepResult::err(StbString::from_string(format!("{name} cancelled")));
    }
    if state.done {
        return StepResult::err(StbString::from_string(format!(
            "{name} polled after completion"
        )));
    }
    state.done = true;
    match runner(&state.params) {
        Ok(value) => StepResult::done(StbString::from_string(value)),
        Err(error) => StepResult::err(StbString::from_string(error)),
    }
}

extern "C" fn background_execute(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let parsed = take_params(params, free);
    BACKGROUND_HANDLES.insert(Drive {
        params: parsed,
        done: false,
    })
}

extern "C" fn background_poll(
    handle: StepHandle,
    _: Option<ToolPartialCb>,
    _: *mut c_void,
) -> StepResult {
    drive(
        &BACKGROUND_HANDLES,
        background_tool,
        BACKGROUND_TOOL,
        handle,
    )
}

extern "C" fn background_cancel(handle: StepHandle) {
    BACKGROUND_HANDLES.cancel(handle);
}

extern "C" fn background_destroy(handle: StepHandle) {
    drop(BACKGROUND_HANDLES.remove(handle));
}

extern "C" fn jobs_execute(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let parsed = take_params(params, free);
    JOBS_HANDLES.insert(Drive {
        params: parsed,
        done: false,
    })
}

extern "C" fn jobs_poll(
    handle: StepHandle,
    _: Option<ToolPartialCb>,
    _: *mut c_void,
) -> StepResult {
    drive(&JOBS_HANDLES, jobs_tool, JOBS_TOOL, handle)
}

extern "C" fn jobs_cancel(handle: StepHandle) {
    JOBS_HANDLES.cancel(handle);
}

extern "C" fn jobs_destroy(handle: StepHandle) {
    drop(JOBS_HANDLES.remove(handle));
}

/// Which session the host is serving right now, for the shutdown sweep.
///
/// `SessionShutdown` carries no payload — the host puts it in the "no payload"
/// group (`rpi-extensions/src/translate.rs`) — so this is the only identity
/// channel available at that point. `__rpi.sessionId` is not, because it exists
/// only inside a tool call and shutdown is an event.
///
/// Two consequences worth knowing:
///
/// * `RPI_SESSION_ID` is process-global and writable by any code in this process
///   (`std::env::set_var`), unlike the per-call snapshot. A misbehaving plugin
///   could therefore make this read the wrong id.
/// * The damage is bounded: shutdown only runs as the process exits, so a bad
///   read at worst skips the sweep (the OS reclaims the children anyway) or
///   marks the wrong session's rows interrupted in a process that is going away.
///
/// The clean fix is a `SessionShutdown` payload carrying the id; until the host
/// provides one this stays the least-bad option. Do not add a cached
/// "last session seen" fallback: the env var is rewritten on switch, so a cache
/// would go stale exactly when this matters, while the env stays current.
fn current_session() -> Option<String> {
    std::env::var(rpi_plugin_sdk::SESSION_ID_ENV)
        .ok()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

/// Mark still-running jobs as interrupted and persist once. Called on session
/// shutdown so a supervisor never sees a session that looks permanently busy.
///
/// Only the **closing** session's jobs are touched. A host can serve several
/// sessions from one process (`switch_session`, `/import`, fork all swap the
/// active session in place without a process exit), and killing another
/// session's dev server because this one closed is exactly the cross-session
/// interference job ownership exists to prevent. When the closing session is
/// unknown — no `RPI_SESSION_ID`, e.g. a host that predates it — every job is
/// treated as belonging to it, matching the old behaviour for single-session
/// hosts.
fn shutdown_jobs() -> i32 {
    let closing = current_session();
    // Take the children out **and terminate them**.
    //
    // Dropping the `Child` handle only closes our end of the pipes: the process
    // keeps running, orphaned. A session that exits with a dev server still
    // holding a port is the visible symptom.
    let (owners, children) = {
        let mut table = table().lock().unwrap_or_else(|p| p.into_inner());
        let mut owners: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut children = Vec::new();
        let mine = |job: &Job| match &closing {
            Some(id) => &job.owner == id,
            None => true,
        };
        for job in table
            .jobs
            .iter_mut()
            .filter(|job| job.is_active() && mine(job))
        {
            job.status = STATUS_INTERRUPTED;
            job.finished_ms = Some(now_ms());
            // The tree handle is kept (not cleared): the leader may exit before
            // its descendants do, and the sweep below runs after this lock is
            // gone. It is the only thing that reaches those descendants.
            if let Some(child) = job.child.take() {
                children.push((child, job.tree.clone()));
            }
            owners.insert(job.owner.clone());
        }
        (owners, children)
    };
    // Terminate, then **reap**. Order matters: `child.take()` above dropped the
    // watcher's handle, so nothing else will ever call `try_wait` on these. On
    // Unix that leaves the exited process as a zombie until the host exits.
    // One shared deadline for the whole sweep, not five seconds per job.
    //
    // The host gives `SessionShutdown` a bounded budget (15s today); a per-job
    // five-second reap would blow it at the fourth stuck child and get the rest
    // skipped. Sharing the budget bounds shutdown as a whole.
    let deadline = Instant::now() + Duration::from_secs(10);
    for (child, tree) in children {
        if !kill_process_tree(tree.as_ref()) {
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
            }
        }
        if let Ok(mut child) = child.lock() {
            reap_until(&mut child, deadline);
        }
    }
    {
        // Every active job of this session was interrupted above, so its tree
        // is gone now. Other sessions' rows keep their tree handle: they are still
        // running and must stay killable.
        let mut table = table().lock().unwrap_or_else(|p| p.into_inner());
        let mine = |job: &Job| match &closing {
            Some(id) => &job.owner == id,
            None => true,
        };
        for job in table.jobs.iter_mut().filter(|job| mine(job)) {
            job.tree = None;
        }
    }
    // One record per affected session: each session's transcript describes
    // only the jobs it owned.
    for owner in owners {
        let _ = persist_snapshot(&owner);
    }
    0
}

rpi_plugin_sdk::event_handler! {
fn handle_event(event, _user_data) {
    if event.tag() == EventTag::SessionShutdown {
        return shutdown_jobs();
    }
    0
}
}

use rpi_plugin_sdk::callbacks::free_string;

rpi_plugin_sdk::export_plugin!(|api| {
    let _ = RUNTIME.set(*api);
    if let Some(register_event) = api.register_event_handler {
        if register_event(
            EventTag::SessionShutdown,
            handle_event as EventHandlerFn,
            std::ptr::null_mut(),
        ) != 0
        {
            return 1;
        }
    }
    let Some(register) = api.register_tool else {
        return 1;
    };
    let schema = Box::new(StableToolSchema {
        name: StbString::from_string(BACKGROUND_TOOL.into()),
        description: StbString::from_string(
            "Start a shell command in the background and return immediately with a job id. \
             Use this for long-running work (dev servers, watchers, big builds, test suites) \
             instead of `bash`, which blocks and can hit its timeout. The command runs under \
             the same shell the `bash` tool uses, using Bash/POSIX syntax, even on Windows: \
             PowerShell cmdlets (Get-ChildItem, Select-Object, Select-String) are not valid \
             here; use ls, find, rg, and POSIX pipelines instead. Read output with the \
             `jobs` tool (action=tail), list everything with action=list, stop it with action=kill."
                .into(),
        ),
        parameters: StbString::from_string(
            r#"{"type":"object","properties":{"command":{"type":"string","minLength":1,"description":"Shell command to run in the background."},"label":{"type":"string","description":"Short human-readable name shown in job listings."}},"required":["command"]}"#
                .into(),
        ),
    });
    let rc = register(
        &*schema,
        background_execute,
        background_poll,
        background_cancel,
        background_destroy,
        free_string,
    );
    drop(schema);
    if rc != 0 {
        return rc;
    }
    let jobs_schema = Box::new(StableToolSchema {
        name: StbString::from_string(JOBS_TOOL.into()),
        description: StbString::from_string(
            "Inspect background jobs started with the `background` tool. Actions: list (all jobs \
             and their status), tail (recent output of one job), kill (stop a running job)."
                .into(),
        ),
        parameters: StbString::from_string(
            r#"{"type":"object","properties":{"action":{"type":"string","enum":["list","tail","kill"],"description":"list = all jobs; tail = recent output; kill = stop the job."},"jobId":{"type":"integer","minimum":0,"description":"Job id returned by the `background` tool."},"lines":{"type":"integer","minimum":1,"description":"For action=tail: how many trailing output lines to return."},"includeDone":{"type":"boolean","description":"For action=list: include finished jobs (default true)."}},"required":["action"]}"#
                .into(),
        ),
    });
    let jobs_rc = register(
        &*jobs_schema,
        jobs_execute,
        jobs_poll,
        jobs_cancel,
        jobs_destroy,
        free_string,
    );
    drop(jobs_schema);
    jobs_rc
});

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    // The process table and the snapshot gate are process-wide statics, so every
    // test that spawns a job or appends a snapshot runs in this one serial test.
    static HOST: OnceLock<Mutex<MockHost>> = OnceLock::new();

    #[derive(Default)]
    struct MockHost {
        appends: Vec<Value>,
        append_fails: bool,
    }

    fn host() -> &'static Mutex<MockHost> {
        HOST.get_or_init(|| Mutex::new(MockHost::default()))
    }

    rpi_plugin_sdk::string_handler! {
    fn host_runtime(action, args, out, _user_data) {
        let args: Value = serde_json::from_str(args).unwrap();
        assert_eq!(RuntimeActionId::try_from(action).unwrap(), RuntimeActionId::AppendEntry);
        assert_eq!(args["customType"], SNAPSHOT_ENTRY);
        let mut host = host().lock().unwrap();
        if host.append_fails {
            out.write("mock append failure".into());
            return 1;
        }
        host.appends.push(args["data"].clone());
        out.write(json!({"entryId": "snapshot"}).to_string());
        0
    }
    }

    fn install_host() {
        let _ = RUNTIME.set(PluginApi {
            abi_version: rpi_plugin_sdk::RPI_PLUGIN_ABI_VERSION_UNIFIED,
            struct_size: std::mem::size_of::<PluginApi>() as u32,
            runtime_action: host_runtime,
            free_string,
            user_data: std::ptr::null_mut(),
            register_tool: None,
            register_command: None,
            register_shortcut: None,
            register_flag: None,
            register_provider: None,
            register_message_renderer: None,
            register_markdown_transformer: None,
            register_entry_renderer: None,
            register_event_handler: None,
            register_resources_discover: None,
            dispatch_event: None,
            declare: None,
            register_before_agent_start: None,
            register_oauth_provider: None,
        });
    }

    /// Serializes tests that touch the process-global job table.
    ///
    /// `table()` is shared by every test in this binary, so a test that calls
    /// `reset()` erases jobs another test is still asserting on. Cargo runs
    /// tests in parallel by default; taking this guard makes the suite behave
    /// like `--test-threads=1` for the table, without requiring the flag.
    static TABLE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn table_guard() -> std::sync::MutexGuard<'static, ()> {
        TABLE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The session id every test tool call belongs to.
    const TEST_SESSION: &str = "01a19d08-fdfb-7640-b070-fe252b4b77ac";

    /// Mimic the host: inject `__rpi` the way the real host does, so tests
    /// exercise the same code path that resolves a job's owner.
    fn with_session(mut params: Value) -> Value {
        params[rpi_plugin_sdk::HOST_CONTEXT_KEY] = json!({
            "cwd": "/tmp",
            "sessionId": TEST_SESSION,
        });
        params
    }

    /// Same call, but stamped with a different session.
    fn with_other_session(mut params: Value) -> Value {
        params[rpi_plugin_sdk::HOST_CONTEXT_KEY] = json!({
            "cwd": "/tmp",
            "sessionId": "other-session",
        });
        params
    }

    fn reset() {
        *table().lock().unwrap() = Table::default();
        *host().lock().unwrap() = MockHost::default();
    }

    fn wait_for_status(id: u64, want: &str) -> Value {
        // Bounded wait on the watcher thread; no unbounded sleeps.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let job = find_job(id, TEST_SESSION).unwrap();
            if job["status"] == want {
                return job;
            }
            assert!(
                Instant::now() < deadline,
                "job {id} never reached {want}: {job}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn output_buffer_is_bounded_by_lines_and_reports_truncation() {
        let mut output = Output::default();
        assert!(!output.truncated);
        output.push("one".into());
        assert_eq!(output.tail(10), vec!["one".to_string()]);
        for n in 0..MAX_OUTPUT_LINES + 10 {
            output.push(format!("line {n}"));
        }
        assert_eq!(output.lines.len(), MAX_OUTPUT_LINES);
        assert!(output.truncated);
        assert_eq!(
            output.tail(1),
            vec![format!("line {}", MAX_OUTPUT_LINES + 9)]
        );
        // Requesting more than retained never panics or duplicates.
        assert_eq!(output.tail(usize::MAX).len(), MAX_OUTPUT_LINES);
        assert!(output.bytes <= MAX_OUTPUT_BYTES);
    }

    #[test]
    fn oversized_line_is_clipped_rather_than_retained_whole() {
        let mut output = Output::default();
        output.push("x".repeat(MAX_OUTPUT_BYTES * 2));
        assert!(output.truncated);
        assert!(!output.lines.is_empty());
        assert!(output.bytes <= MAX_OUTPUT_BYTES);
    }

    /// A newline-free stream must not grow the reader's buffer without bound.
    ///
    /// `Output::push` clips what it stores, but it never sees the line until
    /// `read_until` has already accumulated all of it. This drives the real
    /// reader over a stream with no newline at all and asserts the stored line
    /// stayed bounded — the case a `push`-only test cannot reach.
    #[test]
    fn newline_free_stream_is_read_in_bounded_memory() {
        let _guard = table_guard();
        const EMITTED: usize = MAX_READ_LINE_BYTES * 4;
        struct Flood(usize);
        impl Read for Flood {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0 == 0 {
                    return Ok(0);
                }
                let n = buf.len().min(self.0);
                for byte in &mut buf[..n] {
                    *byte = b'x';
                }
                self.0 -= n;
                Ok(n)
            }
        }

        let id = 9_000_001;
        {
            let mut table = table().lock().unwrap_or_else(|p| p.into_inner());
            table.jobs.push(Job {
                id,
                kind: "bash",
                label: "flood".into(),
                command: "flood".into(),
                pid: None,
                status: STATUS_RUNNING,
                exit_code: None,
                started_ms: 0,
                finished_ms: None,
                output: Output::default(),
                child: None,
                owner: TEST_SESSION.to_string(),
                tree: None,
                cancel_requested: false,
            });
        }

        let stop = Arc::new(AtomicBool::new(false));
        spawn_reader(Flood(EMITTED), id, false, Arc::clone(&stop));

        // Wait for the reader to finish draining, then stop it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let done = {
                let table = table().lock().unwrap_or_else(|p| p.into_inner());
                table
                    .jobs
                    .iter()
                    .find(|job| job.id == id)
                    .is_some_and(|job| !job.output.lines.is_empty())
            };
            if done || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        stop.store(true, Ordering::SeqCst);

        let mut table = table().lock().unwrap_or_else(|p| p.into_inner());
        // The table is process-global and other tests run concurrently, so scope
        // every assertion to this job rather than to the table.
        let job = table.jobs.iter().find(|job| job.id == id).unwrap();
        assert!(
            !job.output.lines.is_empty(),
            "the reader must record the truncated line rather than dropping it"
        );
        for line in &job.output.lines {
            assert!(
                line.len() <= MAX_READ_LINE_BYTES + 64,
                "a stored line must stay bounded, got {} bytes",
                line.len()
            );
        }
        assert!(
            job.output.lines.iter().any(|l| l.contains("bytes dropped")),
            "truncation must be reported; stored lines: {} (first: {:?})",
            job.output.lines.len(),
            job.output.lines.first().map(|l| &l[..l.len().min(80)])
        );
        table.jobs.retain(|job| job.id != id);
    }

    /// An over-long command is rejected, never silently truncated.
    ///
    /// Truncating before `spawn` would run a *different* command than the model
    /// asked for (`a && b` cut mid-way runs only `a`), with no signal.
    #[test]
    fn over_long_commands_are_rejected_not_truncated() {
        let _guard = table_guard();
        let long = "x".repeat(MAX_COMMAND_CHARS + 1);
        let error = start_background(&with_session(json!({ "command": long })))
            .expect_err("an over-long command must be rejected");
        assert!(
            error.contains("too long"),
            "the error must say the command is too long: {error}"
        );
        // The bound is inclusive: a command exactly at the limit is **not**
        // rejected for its length. Assert on the check itself rather than
        // spawning (the process table is a shared static and another test
        // asserts on `active` counts).
        let at_limit = "x".repeat(MAX_COMMAND_CHARS);
        assert_eq!(at_limit.chars().count(), MAX_COMMAND_CHARS);
        assert!(
            at_limit.chars().count() <= MAX_COMMAND_CHARS,
            "an at-limit command must pass the length gate"
        );
    }

    fn case_process_tree_kill_reaches_grandchildren() {
        // The command records the pid of a grandchild it spawns, so the test
        // can assert on *that* process rather than counting sleepers: counting
        // is not an identity assertion, and it races both startup and any
        // unrelated process on the machine.
        let pid_file = std::env::temp_dir().join(format!(
            "rpi-bg-pid-{}-{}",
            std::process::id(),
            next_unique_suffix()
        ));
        let _ = std::fs::remove_file(&pid_file);
        let command = record_pid_command(&pid_file);
        install_host();
        let payload: Value = serde_json::from_str(
            &background_tool(&with_session(json!({"command": command, "label": "tree"}))).unwrap(),
        )
        .unwrap();
        let id = payload["details"]["jobId"].as_u64().unwrap();

        let pid = wait_for_pid_file(&pid_file);
        assert!(
            pid_is_running(pid),
            "the grandchild must be running before the kill"
        );

        let _ = jobs_tool(&with_session(json!({"action": "kill", "jobId": id}))).unwrap();

        // The tree kill is asynchronous on Windows (the job object is closed
        // when the handle drops), so give it a moment to take effect.
        let deadline = Instant::now() + Duration::from_secs(10);
        while pid_is_running(pid) {
            assert!(
                Instant::now() < deadline,
                "kill must terminate the grandchild, but pid {pid} is still running"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_file(&pid_file);
    }

    /// A descendant that outlives the shell must remain killable.
    ///
    /// The shell exits as soon as it has spawned the background child, so the
    /// job settles with `child = None` while the grandchild is still running.
    /// Before the tree handle was kept, `jobs kill` answered "already finished" and the
    /// process leaked — the case a kill-while-active test cannot reach.
    #[test]
    fn settled_job_can_still_terminate_its_surviving_descendants() {
        let _guard = table_guard();
        let before = count_sleepers();
        install_host();
        let payload: Value = serde_json::from_str(
            &background_tool(&with_session(
                json!({"command": "sleep 60 &", "label": "orphan"}),
            ))
            .unwrap(),
        )
        .unwrap();
        let id = payload["details"]["jobId"].as_u64().unwrap();

        // The shell exits immediately; its background child keeps running.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut settled = false;
        while std::time::Instant::now() < deadline {
            let status = {
                let table = table().lock().unwrap_or_else(|p| p.into_inner());
                table
                    .jobs
                    .iter()
                    .find(|job| job.id == id)
                    .map(|job| job.status)
            };
            if status.is_some_and(|status| status != STATUS_RUNNING) {
                settled = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            settled,
            "the shell must settle while the child keeps running"
        );
        assert!(
            count_sleepers() > before,
            "the descendant must still be running after settlement"
        );

        // The row is finished, but the tree is not: kill must still reach it.
        let reply = jobs_tool(&with_session(json!({"action": "kill", "jobId": id}))).unwrap();
        assert!(
            reply.contains("process tree was terminated")
                || reply.contains("no process tree remained"),
            "a settled job with a live tree must not report a plain no-op: {reply}"
        );
        std::thread::sleep(std::time::Duration::from_millis(600));
        assert_eq!(
            count_sleepers(),
            before,
            "kill on a settled job must reap the surviving descendants"
        );
    }

    /// The resolved shell must be bash, and it must accept a POSIX command.
    ///
    /// Picking `cmd /C` on Windows meant `ls -la` worked through the host's
    /// `bash` tool and failed here. Asserting on the shell name — not just on a
    /// command succeeding — is what keeps the two tools from drifting apart
    /// again.
    #[test]
    fn the_resolved_shell_is_bash() {
        let config = shell_config().expect("a bash must be resolvable on a dev machine");
        let name = config
            .shell
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert_eq!(
            name, "bash",
            "background must run under bash like the `bash` tool does, got {:?}",
            config.shell
        );
        assert_eq!(config.args, vec!["-c".to_string()]);
    }

    /// A POSIX command must work through `background` itself, end to end.
    ///
    /// `[[ ]]` is a bashism: it distinguishes bash from a POSIX-only `sh`, so
    /// this fails if the resolver ever falls back to something narrower.
    #[test]
    fn posix_bash_syntax_runs_through_the_resolved_shell() {
        let _guard = table_guard();
        install_host();
        reset();
        let value: Value = serde_json::from_str(
            &background_tool(&with_session(json!({
                "command": "if [[ -n bash ]]; then echo is-bash; fi",
                "label": "bashism"
            })))
            .unwrap(),
        )
        .unwrap();
        let id = value["details"]["jobId"].as_u64().unwrap();
        let job = wait_for_status(id, STATUS_COMPLETED);
        assert_eq!(job["exitCode"], 0, "the bashism must run: {job}");
        let tail = tail_job(id, 10, TEST_SESSION).unwrap();
        assert!(
            tail.contains("is-bash"),
            "bash syntax must work through background, got: {tail}"
        );
    }

    /// A command that writes to both stdout and stderr.
    ///
    /// One fixture for every platform, because there is one shell: `background`
    /// resolves bash the way the host's `bash` tool does, so POSIX syntax is
    /// what runs on Windows too. A `cmd /C`-specific fixture would have been
    /// evidence that the plugin had picked the wrong shell.
    fn stdout_and_stderr_command() -> &'static str {
        "echo hello; echo oops >&2"
    }

    /// A command that exits with a non-zero status.
    fn failing_command() -> &'static str {
        "exit 3"
    }

    /// Whether a pid still names a live process.
    ///
    /// The SDK exposes this for Windows only, and the plugin needs it on every
    /// platform to assert on a specific descendant.
    #[cfg(windows)]
    fn pid_is_running(pid: u32) -> bool {
        rpi_plugin_sdk::process::process_is_running(pid).unwrap_or(false)
    }

    #[cfg(unix)]
    fn pid_is_running(pid: u32) -> bool {
        // The process table answers directly, and a zombie is reported as
        // gone: the tree kill is what this asserts, and a reaped-but-unwaited
        // pid is not a surviving descendant.
        //
        // `ps` rather than a signal probe because the crate forbids `unsafe`
        // and `libc` is not a dependency of this package.
        let out = std::process::Command::new("ps")
            .args(["-o", "state=", "-p", &pid.to_string()])
            .output();
        match out {
            Ok(out) => {
                let state = String::from_utf8_lossy(&out.stdout);
                let state = state.trim();
                !state.is_empty() && !state.starts_with('Z')
            }
            Err(_) => false,
        }
    }

    /// A suffix that makes a temp file name unique within this process.
    fn next_unique_suffix() -> u64 {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// A command that starts a long-lived grandchild and records its pid.
    ///
    /// `&` backgrounds it under `sh`, and under `cmd` it starts a second
    /// process instead — the pid written is the one the shell reports, which is
    /// the process the tree kill has to reach.
    #[cfg(unix)]
    fn record_pid_command(pid_file: &std::path::Path) -> String {
        format!("sleep 60 & echo $! > {}", pid_file.display())
    }

    #[cfg(windows)]
    fn record_pid_command(pid_file: &std::path::Path) -> String {
        format!(
            "start /B cmd /C \"ping -n 61 127.0.0.1 > nul\" & for /f \"tokens=2 delims=\" \" %i in ('wmic process where \"name='ping.exe'\" get processid /value\"') do @echo %i > {}",
            pid_file.display()
        )
    }

    /// Read the pid a background command recorded, waiting for the file.
    fn wait_for_pid_file(pid_file: &std::path::Path) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(text) = std::fs::read_to_string(pid_file) {
                if let Ok(pid) = text.trim().parse::<u32>() {
                    if pid != 0 {
                        return pid;
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "the command never recorded a pid in {}",
                pid_file.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn count_sleepers() -> usize {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg("ps -A -o command | grep -c '^sleep 60' || true")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .unwrap_or(0)
    }

    #[test]
    fn an_empty_table_snapshots_an_empty_job_list() {
        assert_eq!(
            snapshot_json(&Table::default(), "any-session")["jobs"],
            json!([])
        );
    }

    /// Closing one session must not stop another session's jobs.
    ///
    /// A host can serve several sessions from one process — `switch_session`,
    /// `/import` and fork all swap the active session in place — and
    /// `SessionShutdown` is dispatched per process, not per session. Sweeping
    /// every row would kill the other session's dev server, which is the exact
    /// cross-session interference job ownership exists to prevent.
    #[test]
    fn shutdown_only_stops_the_closing_sessions_jobs() {
        let _guard = table_guard();
        install_host();
        reset();
        let mine: Value = serde_json::from_str(
            &background_tool(&with_session(
                json!({"command": "sleep 30", "label": "mine"}),
            ))
            .unwrap(),
        )
        .unwrap();
        let my_id = mine["details"]["jobId"].as_u64().unwrap();
        let theirs: Value = serde_json::from_str(
            &background_tool(&with_other_session(
                json!({"command": "sleep 30", "label": "theirs"}),
            ))
            .unwrap(),
        )
        .unwrap();
        let their_id = theirs["details"]["jobId"].as_u64().unwrap();

        // Close the *other* session while this one keeps its job.
        std::env::set_var(rpi_plugin_sdk::SESSION_ID_ENV, "other-session");
        shutdown_jobs();
        std::env::remove_var(rpi_plugin_sdk::SESSION_ID_ENV);

        let their_status = find_job(their_id, "other-session").unwrap()["status"].clone();
        assert_ne!(
            their_status, STATUS_RUNNING,
            "the closing session's job must be interrupted"
        );
        let my_status = find_job(my_id, TEST_SESSION).unwrap()["status"].clone();
        assert_eq!(
            my_status, STATUS_RUNNING,
            "another session closing must not stop this session's job"
        );

        let _ = kill_job(my_id, TEST_SESSION);
    }

    /// An unknown or wrong closing session must not strand the sweep unreachable.
    ///
    /// When `RPI_SESSION_ID` names a session with no jobs, nothing is touched —
    /// the other sessions' rows keep running. That is the safest outcome: the
    /// process is exiting, so their children die with it, and marking them
    /// interrupted would be a lie about jobs this session never owned.
    #[test]
    fn shutdown_with_an_unrelated_session_id_touches_nothing() {
        let _guard = table_guard();
        install_host();
        reset();
        let mine: Value = serde_json::from_str(
            &background_tool(&with_session(
                json!({"command": "sleep 20", "label": "mine"}),
            ))
            .unwrap(),
        )
        .unwrap();
        let my_id = mine["details"]["jobId"].as_u64().unwrap();

        std::env::set_var(rpi_plugin_sdk::SESSION_ID_ENV, "a-session-with-no-jobs");
        shutdown_jobs();
        std::env::remove_var(rpi_plugin_sdk::SESSION_ID_ENV);

        assert_eq!(
            find_job(my_id, TEST_SESSION).unwrap()["status"],
            STATUS_RUNNING,
            "an unrelated closing id must not interrupt this session's job"
        );
        let _ = kill_job(my_id, TEST_SESSION);
    }

    /// The durable snapshot must describe one session only.
    ///
    /// The tool queries filter by owner, but the snapshot is appended to the
    /// calling session's transcript. Serializing the whole table would write
    /// another session's commands and pids into this session's history — the
    /// isolation would hold on reads and leak on writes.
    #[test]
    fn snapshot_only_carries_the_owning_sessions_jobs() {
        let _guard = table_guard();
        install_host();
        reset();
        let mine: Value = serde_json::from_str(
            &background_tool(&with_session(
                json!({"command": "sleep 20", "label": "mine"}),
            ))
            .unwrap(),
        )
        .unwrap();
        let my_id = mine["details"]["jobId"].as_u64().unwrap();
        let theirs: Value = serde_json::from_str(
            &background_tool(&with_other_session(
                json!({"command": "sleep 20", "label": "theirs"}),
            ))
            .unwrap(),
        )
        .unwrap();
        let their_id = theirs["details"]["jobId"].as_u64().unwrap();

        let table = table().lock().unwrap_or_else(|p| p.into_inner());
        let for_me = snapshot_json(&table, TEST_SESSION);
        let for_them = snapshot_json(&table, "other-session");
        drop(table);

        // `id` is serialized as a string; the tool surface accepts either form.
        let ids = |snapshot: &Value| -> Vec<u64> {
            snapshot["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|job| job["id"].as_str().and_then(|id| id.parse().ok()))
                .collect()
        };
        assert_eq!(ids(&for_me), vec![my_id], "my snapshot is mine alone");
        assert_eq!(
            ids(&for_them),
            vec![their_id],
            "their snapshot is theirs alone"
        );
        let mine_text = for_me.to_string();
        assert!(
            !mine_text.contains("theirs"),
            "my snapshot must not mention another session's job: {mine_text}"
        );

        let _ = kill_job(my_id, TEST_SESSION);
        let _ = kill_job(their_id, "other-session");
    }

    /// A job belongs to the session that started it, and no other.
    ///
    /// This is the whole point of the owner field: a forked or cloned session
    /// inherits context but not background work, and must not be able to read
    /// or stop another session's job even by guessing its id.
    #[test]
    fn a_job_is_invisible_and_unstoppable_from_another_session() {
        let _guard = table_guard();
        install_host();
        reset();
        let value: Value = serde_json::from_str(
            &background_tool(&with_session(
                json!({"command": "sleep 30", "label": "mine"}),
            ))
            .unwrap(),
        )
        .unwrap();
        let id = value["details"]["jobId"].as_u64().unwrap();

        // The owning session sees it.
        let mine = list_jobs(&with_session(json!({})), TEST_SESSION);
        assert_eq!(mine["total"], 1, "the owner must see its own job: {mine}");

        // Another session sees nothing, even though the row exists.
        let theirs = list_jobs(&with_other_session(json!({})), "other-session");
        assert_eq!(
            theirs["total"], 0,
            "another session must not see this job: {theirs}"
        );
        assert!(
            tail_job(id, 5, "other-session").is_err(),
            "another session must not read the output"
        );
        let refused = kill_job(id, "other-session").expect_err("kill must be refused");
        assert!(
            refused.contains("belongs to another session"),
            "the refusal must say why: {refused}"
        );
        // Still running: the refusal did not touch the process.
        assert_eq!(
            find_job(id, TEST_SESSION).unwrap()["status"],
            STATUS_RUNNING
        );

        let _ = kill_job(id, TEST_SESSION);
    }

    #[test]
    fn unknown_actions_and_missing_ids_are_rejected() {
        let _guard = table_guard();
        assert!(jobs_tool(&with_session(json!({"action": "bogus"}))).is_err());
        assert!(jobs_tool(&with_session(json!({"action": "tail"}))).is_err());
        assert!(jobs_tool(&with_session(json!({"action": "kill"}))).is_err());
        assert!(tail_job(999, 5, TEST_SESSION).is_err());
        assert!(kill_job(999, TEST_SESSION).is_err());
        assert!(background_tool(&with_session(json!({}))).is_err());
        assert!(background_tool(&with_session(json!({"command": "   "}))).is_err());
    }

    // All tests that touch the process-wide table or the snapshot gate live in
    // this single serial test: they share statics, so parallel execution would
    // let one case's jobs and appends leak into another's assertions.
    #[test]
    fn serially_drives_jobs_snapshots_and_the_abi() {
        let _guard = table_guard();
        case_background_returns_immediately_and_reports_its_job();
        case_natural_completion_is_recorded_with_its_exit_status();
        case_a_failing_command_is_settled_as_failed();
        case_shutdown_interrupts_still_running_jobs_and_persists_once();
        case_a_failed_snapshot_append_does_not_kill_the_job();
        case_process_tree_kill_reaches_grandchildren();
        case_abi_drivers_round_trip_and_reject_bad_params();
    }

    fn case_background_returns_immediately_and_reports_its_job() {
        install_host();
        reset();
        let started = std::time::Instant::now();
        let value: Value = serde_json::from_str(
            &background_tool(&with_session(
                json!({"command": "sleep 30", "label": "probe"}),
            ))
            .unwrap(),
        )
        .unwrap();
        // The tool must not wait for the child; `sleep 30` proves it.
        assert!(started.elapsed() < Duration::from_secs(5));
        let id = value["details"]["jobId"].as_u64().unwrap();
        assert_eq!(value["details"]["status"], STATUS_RUNNING);
        let text = value["content"][0]["text"].as_str().unwrap();
        assert!(text.contains(&format!("job {id}")));
        assert!(text.contains("jobs"));
        assert!(text.contains("tail"));
        assert_eq!(value["details"]["activeJobs"], 1);

        // A snapshot was appended and carries the supervisor contract.
        let appends = host().lock().unwrap().appends.clone();
        assert!(!appends.is_empty());
        let job = &appends.last().unwrap()["jobs"][0];
        assert_eq!(job["id"], id.to_string());
        assert_eq!(job["kind"], "bash");
        assert_eq!(job["label"], "probe");
        assert_eq!(job["command"], "sleep 30");
        assert_eq!(job["status"], STATUS_RUNNING);

        // `list` reports it as active; `kill` terminates it.
        let listed: Value =
            serde_json::from_str(&jobs_tool(&with_session(json!({"action": "list"}))).unwrap())
                .unwrap();
        assert_eq!(listed["details"]["active"], 1);
        let killed: Value = serde_json::from_str(
            &jobs_tool(&with_session(json!({"action": "kill", "jobId": id}))).unwrap(),
        )
        .unwrap();
        assert_eq!(killed["details"]["job"]["status"], STATUS_CANCELLED);
        let job = wait_for_status(id, STATUS_CANCELLED);
        assert_eq!(job["status"], STATUS_CANCELLED);
        assert_eq!(
            host().lock().unwrap().appends.last().unwrap()["jobs"][0]["status"],
            STATUS_CANCELLED
        );
        // Killing again is a reported no-op, not an error.
        let again = jobs_tool(&with_session(json!({"action": "kill", "jobId": id}))).unwrap();
        assert!(again.contains("already finished"));
    }

    fn case_natural_completion_is_recorded_with_its_exit_status() {
        install_host();
        reset();
        let value: Value = serde_json::from_str(
            &background_tool(&with_session(
                json!({"command": stdout_and_stderr_command()}),
            ))
            .unwrap(),
        )
        .unwrap();
        let id = value["details"]["jobId"].as_u64().unwrap();
        let job = wait_for_status(id, STATUS_COMPLETED);
        assert_eq!(job["exitCode"], 0);

        // Capture both streams without racing the readers.
        let deadline = Instant::now() + Duration::from_secs(10);
        let tail = loop {
            let tail = tail_job(id, 50, TEST_SESSION).unwrap();
            if tail.contains("oops") {
                break tail;
            }
            assert!(Instant::now() < deadline, "output never arrived: {tail}");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(tail.contains("hello"), "{tail}");
        assert!(tail.contains("[stderr] oops"), "{tail}");
        assert!(tail.contains(STATUS_COMPLETED));
    }

    fn case_a_failing_command_is_settled_as_failed() {
        install_host();
        reset();
        let value: Value = serde_json::from_str(
            &background_tool(&with_session(json!({"command": failing_command()}))).unwrap(),
        )
        .unwrap();
        let id = value["details"]["jobId"].as_u64().unwrap();
        let job = wait_for_status(id, STATUS_FAILED);
        assert_eq!(job["exitCode"], 3);
    }

    fn case_shutdown_interrupts_still_running_jobs_and_persists_once() {
        install_host();
        reset();
        let value: Value = serde_json::from_str(
            &background_tool(&with_session(json!({"command": "sleep 30"}))).unwrap(),
        )
        .unwrap();
        let id = value["details"]["jobId"].as_u64().unwrap();
        let before = host().lock().unwrap().appends.len();
        assert_eq!(
            handle_event(
                rpi_plugin_sdk::StablePluginEvent::empty(EventTag::SessionShutdown),
                std::ptr::null_mut(),
            ),
            0
        );
        let job = find_job(id, TEST_SESSION).unwrap();
        assert_eq!(job["status"], STATUS_INTERRUPTED);
        let appends = host().lock().unwrap().appends.clone();
        assert_eq!(appends.len(), before + 1);
        assert_eq!(
            appends.last().unwrap()["jobs"][0]["status"],
            STATUS_INTERRUPTED
        );
        // A second shutdown has nothing left to interrupt, so no new snapshot.
        assert_eq!(
            handle_event(
                rpi_plugin_sdk::StablePluginEvent::empty(EventTag::SessionShutdown),
                std::ptr::null_mut(),
            ),
            0
        );
        assert_eq!(host().lock().unwrap().appends.len(), appends.len());
        let _ = kill_job(id, TEST_SESSION);
    }

    fn case_a_failed_snapshot_append_does_not_kill_the_job() {
        install_host();
        reset();
        host().lock().unwrap().append_fails = true;
        let value: Value = serde_json::from_str(
            &background_tool(&with_session(json!({"command": "sleep 30"}))).unwrap(),
        )
        .unwrap();
        let id = value["details"]["jobId"].as_u64().unwrap();
        assert_eq!(value["details"]["status"], STATUS_RUNNING);
        assert!(value["details"]["snapshotError"].is_string());
        // The process is still running and still inspectable.
        assert_eq!(
            find_job(id, TEST_SESSION).unwrap()["status"],
            STATUS_RUNNING
        );
        host().lock().unwrap().append_fails = false;
        let _ = kill_job(id, TEST_SESSION);
    }

    fn invoke(
        handles: &'static rpi_plugin_sdk::handles::ToolHandles<Drive>,
        execute: rpi_plugin_sdk::ToolExecuteFn,
        poll: rpi_plugin_sdk::ToolPollFn,
        params: Value,
    ) -> rpi_plugin_sdk::StepResultTag {
        let handle = execute(
            StbStringRef::from_str("call"),
            StbString::from_string(params.to_string()),
            Some(free_string),
        );
        let result = poll(handle, None, std::ptr::null_mut());
        drop(handles.remove(handle));
        result.tag
    }

    fn case_abi_drivers_round_trip_and_reject_bad_params() {
        use rpi_plugin_sdk::StepResultTag::{Done, Err as Failed};
        install_host();
        reset();
        assert_eq!(
            invoke(
                &JOBS_HANDLES,
                jobs_execute,
                jobs_poll,
                with_session(json!({"action": "list"}))
            ),
            Done
        );
        // Missing jobId surfaces as a failed step, not a panic.
        assert_eq!(
            invoke(
                &JOBS_HANDLES,
                jobs_execute,
                jobs_poll,
                with_session(json!({"action": "tail"}))
            ),
            Failed
        );
        assert_eq!(
            invoke(
                &BACKGROUND_HANDLES,
                background_execute,
                background_poll,
                with_session(json!({"command": "sleep 10", "label": "abi"}))
            ),
            Done
        );
        let id = table().lock().unwrap().jobs[0].id;
        assert_eq!(
            invoke(
                &JOBS_HANDLES,
                jobs_execute,
                jobs_poll,
                with_session(json!({"action": "kill", "jobId": id}))
            ),
            Done
        );
    }
}
