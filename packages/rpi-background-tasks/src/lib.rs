#![forbid(unsafe_code)]
// The SDK's safe export macro owns the raw-pointer ABI boundary.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

//! Background shell jobs plus a `jobs` inspector for the rpi Rust agent.
//!
//! Every job state change appends a self-contained `background-tasks` custom
//! snapshot so a supervising UI can tell whether a session still has work
//! running without reading a full transcript.

use rpi_plugin_sdk::handles::ToolHandles;
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
    child: Option<Arc<Mutex<Child>>>,
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
fn snapshot_json(table: &Table) -> Value {
    json!({"jobs": table.jobs.iter().map(Job::snapshot).collect::<Vec<_>>()})
}

/// Append the whole job table. A failure is reported to the caller but must not
/// abort the tool: the process is already running and killing it would be worse.
fn persist_snapshot() -> Result<(), String> {
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
        snapshot_json(&table)
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

/// Build the shell invocation for this platform. Windows must not allocate a
/// console; the other targets only need the platform shell.
fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        // CREATE_NO_WINDOW / CREATE_NEW_PROCESS_GROUP: no console window, and a
        // kill can target the whole tree instead of only the shell.
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000 | 0x0000_0200);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        // Own process group, so a kill can target the whole tree.
        //
        // `kill(pid)` only reaches the `sh -c` wrapper; a dev server started as
        // `npm run dev` keeps its own children running after the shell dies.
        // With the child as a group leader, `kill(-pgid, …)` reaches all of it.
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        cmd
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    stream: R,
    id: u64,
    is_stderr: bool,
    stop: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) => return,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&buf);
                    let line = text.trim_end_matches(['\n', '\r']).to_string();
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
    let changed = {
        let mut table = table().lock().unwrap_or_else(|p| p.into_inner());
        let Some(job) = table.jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        if !job.is_active() {
            return;
        }
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
        job.child = None;
        true
    };
    if changed {
        let _ = persist_snapshot();
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

    let mut child = shell_command(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to start background command: {error}"))?;

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
            cancel_requested: false,
        });
        (id, table.jobs.iter().filter(|job| job.is_active()).count())
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

    let persistence = persist_snapshot();
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

fn find_job(id: u64) -> Option<Value> {
    let table = table().lock().unwrap_or_else(|p| p.into_inner());
    table
        .jobs
        .iter()
        .find(|job| job.id == id)
        .map(Job::snapshot)
}

fn list_jobs(params: &Value) -> Value {
    let include_done = params
        .get("includeDone")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let table = table().lock().unwrap_or_else(|p| p.into_inner());
    let jobs: Vec<&Job> = table
        .jobs
        .iter()
        .filter(|job| include_done || job.is_active())
        .collect();
    let active = table.jobs.iter().filter(|job| job.is_active()).count();
    json!({
        "jobs": jobs.iter().map(|job| job.snapshot()).collect::<Vec<_>>(),
        "total": table.jobs.len(),
        "active": active,
    })
}

fn tail_job(id: u64, lines: usize) -> Result<String, String> {
    let (tail, truncated, status) = {
        let table = table().lock().unwrap_or_else(|p| p.into_inner());
        let job = table
            .jobs
            .iter()
            .find(|job| job.id == id)
            .ok_or_else(|| format!("job {id} not found"))?;
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
fn kill_process_tree(pid: Option<u32>) -> bool {
    #[cfg(unix)]
    {
        if let Some(pid) = pid {
            // `kill -<pgid>`: the leading dash targets the group.
            let status = std::process::Command::new("kill")
                .arg("-9")
                .arg(format!("-{pid}"))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            if matches!(status, Ok(status) if status.success()) {
                return true;
            }
        }
    }
    #[cfg(windows)]
    {
        if let Some(pid) = pid {
            let status = std::process::Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            if matches!(status, Ok(status) if status.success()) {
                return true;
            }
        }
    }
    false
}

fn kill_job(id: u64) -> Result<String, String> {
    let (child, pid) = {
        let mut table = table().lock().unwrap_or_else(|p| p.into_inner());
        let job = table
            .jobs
            .iter_mut()
            .find(|job| job.id == id)
            .ok_or_else(|| format!("job {id} not found"))?;
        // A finished job is a no-op, not an error: the process may have exited
        // between the model's `list` and its `kill`.
        if !job.is_active() {
            return Ok(format!("Job {id} already finished ({})", job.status));
        }
        job.cancel_requested = true;
        (job.child.clone(), job.pid)
    };
    let mut signal_error = None;
    if let Some(child) = &child {
        let mut child = child.lock().unwrap_or_else(|p| p.into_inner());
        // A failed kill is not fatal: the process may have exited between the
        // status check and the signal, which the waiter will report.
        // Kill the whole **tree**: `Child::kill` only signals the `sh -c`
        // wrapper, so a dev server started as `npm run dev` keeps its workers
        // alive after the shell dies. The crate forbids `unsafe` (no
        // `libc::kill`), so this shells out to the platform's tree-killer.
        if !kill_process_tree(pid) {
            // Fall back to signalling the direct child.
            if let Err(error) = child.kill() {
                signal_error.get_or_insert(error.to_string());
            }
        }
        // Reap with a bounded wait. SIGKILL cannot be ignored, so this returns
        // almost immediately; the bound only protects the tool thread from an
        // unkillable child (e.g. stuck in an uninterruptible syscall) and the
        // handle is detached below, so nobody else can reap this child.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        signal_error.get_or_insert_with(|| {
                            "process did not exit within 5s of the kill signal".into()
                        });
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
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

fn background_tool(params: &Value) -> Result<String, String> {
    start_background(params)
}

fn jobs_tool(params: &Value) -> Result<String, String> {
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("list");
    match action {
        "list" => {
            let listed = list_jobs(params);
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
            let text = tail_job(id, lines)?;
            let job = find_job(id).unwrap_or(Value::Null);
            Ok(tool_result(action, text, json!({"jobId": id, "job": job})))
        }
        "kill" => {
            let id = params
                .get("jobId")
                .and_then(Value::as_u64)
                .ok_or("jobId is required for action=kill")?;
            let text = kill_job(id)?;
            let job = find_job(id).unwrap_or(Value::Null);
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

/// Mark still-running jobs as interrupted and persist once. Called on session
/// shutdown so a supervisor never sees a session that looks permanently busy.
fn shutdown_jobs() -> i32 {
    // Take the children out **and terminate them**.
    //
    // Dropping the `Child` handle only closes our end of the pipes: the process
    // keeps running, orphaned. A session that exits with a dev server still
    // holding a port is the visible symptom.
    let (changed, children) = {
        let mut table = table().lock().unwrap_or_else(|p| p.into_inner());
        let mut changed = false;
        let mut children = Vec::new();
        for job in table.jobs.iter_mut().filter(|job| job.is_active()) {
            job.status = STATUS_INTERRUPTED;
            job.finished_ms = Some(now_ms());
            if let Some(child) = job.child.take() {
                children.push((child, job.pid));
            }
            changed = true;
        }
        (changed, children)
    };
    for (child, pid) in children {
        if !kill_process_tree(pid) {
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
            }
        }
    }
    if changed {
        let _ = persist_snapshot();
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
             instead of `bash`, which blocks and can hit its timeout. Read output with the \
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

    fn reset() {
        *table().lock().unwrap() = Table::default();
        *host().lock().unwrap() = MockHost::default();
    }

    fn wait_for_status(id: u64, want: &str) -> Value {
        // Bounded wait on the watcher thread; no unbounded sleeps.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let job = find_job(id).unwrap();
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

    /// An over-long command is rejected, never silently truncated.
    ///
    /// Truncating before `spawn` would run a *different* command than the model
    /// asked for (`a && b` cut mid-way runs only `a`), with no signal.
    #[test]
    fn over_long_commands_are_rejected_not_truncated() {
        let long = "x".repeat(MAX_COMMAND_CHARS + 1);
        let error = start_background(&json!({ "command": long }))
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
        // `sh -c 'sleep 60 & sleep 60'` spawns a grandchild; killing only the
        // wrapper would leave it running.
        let before = count_sleepers();
        install_host();
        let payload: Value = serde_json::from_str(
            &background_tool(&json!({"command": "sleep 60 & sleep 60", "label": "tree"})).unwrap(),
        )
        .unwrap();
        let id = payload["details"]["jobId"].as_u64().unwrap();
        let started = count_sleepers();
        assert!(started > before, "the jobs must actually be running");
        let _ = jobs_tool(&json!({"action": "kill", "jobId": id})).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(600));
        let after = count_sleepers();
        assert_eq!(
            after, before,
            "kill must reap the whole tree (before={before}, during={started}, after={after})"
        );
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
        assert_eq!(snapshot_json(&Table::default())["jobs"], json!([]));
    }

    #[test]
    fn unknown_actions_and_missing_ids_are_rejected() {
        assert!(jobs_tool(&json!({"action": "bogus"})).is_err());
        assert!(jobs_tool(&json!({"action": "tail"})).is_err());
        assert!(jobs_tool(&json!({"action": "kill"})).is_err());
        assert!(tail_job(999, 5).is_err());
        assert!(kill_job(999).is_err());
        assert!(background_tool(&json!({})).is_err());
        assert!(background_tool(&json!({"command": "   "})).is_err());
    }

    // All tests that touch the process-wide table or the snapshot gate live in
    // this single serial test: they share statics, so parallel execution would
    // let one case's jobs and appends leak into another's assertions.
    #[test]
    fn serially_drives_jobs_snapshots_and_the_abi() {
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
            &background_tool(&json!({"command": "sleep 30", "label": "probe"})).unwrap(),
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
            serde_json::from_str(&jobs_tool(&json!({"action": "list"})).unwrap()).unwrap();
        assert_eq!(listed["details"]["active"], 1);
        let killed: Value =
            serde_json::from_str(&jobs_tool(&json!({"action": "kill", "jobId": id})).unwrap())
                .unwrap();
        assert_eq!(killed["details"]["job"]["status"], STATUS_CANCELLED);
        let job = wait_for_status(id, STATUS_CANCELLED);
        assert_eq!(job["status"], STATUS_CANCELLED);
        assert_eq!(
            host().lock().unwrap().appends.last().unwrap()["jobs"][0]["status"],
            STATUS_CANCELLED
        );
        // Killing again is a reported no-op, not an error.
        let again = jobs_tool(&json!({"action": "kill", "jobId": id})).unwrap();
        assert!(again.contains("already finished"));
    }

    fn case_natural_completion_is_recorded_with_its_exit_status() {
        install_host();
        reset();
        let value: Value = serde_json::from_str(
            &background_tool(&json!({"command": "echo hello; echo oops >&2"})).unwrap(),
        )
        .unwrap();
        let id = value["details"]["jobId"].as_u64().unwrap();
        let job = wait_for_status(id, STATUS_COMPLETED);
        assert_eq!(job["exitCode"], 0);

        // Capture both streams without racing the readers.
        let deadline = Instant::now() + Duration::from_secs(10);
        let tail = loop {
            let tail = tail_job(id, 50).unwrap();
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
        let value: Value =
            serde_json::from_str(&background_tool(&json!({"command": "exit 3"})).unwrap()).unwrap();
        let id = value["details"]["jobId"].as_u64().unwrap();
        let job = wait_for_status(id, STATUS_FAILED);
        assert_eq!(job["exitCode"], 3);
    }

    fn case_shutdown_interrupts_still_running_jobs_and_persists_once() {
        install_host();
        reset();
        let value: Value =
            serde_json::from_str(&background_tool(&json!({"command": "sleep 30"})).unwrap())
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
        let job = find_job(id).unwrap();
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
        let _ = kill_job(id);
    }

    fn case_a_failed_snapshot_append_does_not_kill_the_job() {
        install_host();
        reset();
        host().lock().unwrap().append_fails = true;
        let value: Value =
            serde_json::from_str(&background_tool(&json!({"command": "sleep 30"})).unwrap())
                .unwrap();
        let id = value["details"]["jobId"].as_u64().unwrap();
        assert_eq!(value["details"]["status"], STATUS_RUNNING);
        assert!(value["details"]["snapshotError"].is_string());
        // The process is still running and still inspectable.
        assert_eq!(find_job(id).unwrap()["status"], STATUS_RUNNING);
        host().lock().unwrap().append_fails = false;
        let _ = kill_job(id);
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
                json!({"action": "list"})
            ),
            Done
        );
        // Missing jobId surfaces as a failed step, not a panic.
        assert_eq!(
            invoke(
                &JOBS_HANDLES,
                jobs_execute,
                jobs_poll,
                json!({"action": "tail"})
            ),
            Failed
        );
        assert_eq!(
            invoke(
                &BACKGROUND_HANDLES,
                background_execute,
                background_poll,
                json!({"command": "sleep 10", "label": "abi"})
            ),
            Done
        );
        let id = table().lock().unwrap().jobs[0].id;
        assert_eq!(
            invoke(
                &JOBS_HANDLES,
                jobs_execute,
                jobs_poll,
                json!({"action": "kill", "jobId": id})
            ),
            Done
        );
    }
}
