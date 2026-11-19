//! Goal state v2: session-branch storage, v1 migration, and budget helpers.
//!
//! The goal lives as one `goal-state` custom entry in the current session
//! branch. v1 (0.1/0.2) entries are the same wrapper shape with a smaller
//! `Goal`, so every new field is `#[serde(default)]` and an old entry still
//! loads. The legacy project file is no longer a source of truth.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::runtime;
use rpi_plugin_sdk::RuntimeActionId;

pub const GOAL_STATE_ENTRY: &str = "goal-state";
/// Custom-message type stored in the generic `AgentMessage` payload.
pub const GOAL_CUSTOM_TYPE: &str = "goal";
/// Legacy project-scoped store written by rpi-goal <= 0.2.5. It is inert; only
/// `/goal clear` still deletes it.
pub const LEGACY_GOAL_FILE: &str = ".rpi/goals.json";

pub const MAX_TITLE_CHARS: usize = 4_000;
pub const MAX_NOTES_CHARS: usize = 20_000;
pub const MAX_SUMMARY_CHARS: usize = 4_000;
pub const MAX_REASON_CHARS: usize = 1_000;
pub const MAX_EVIDENCE_CHARS: usize = 4_000;
/// `goal_wait` deadlines below this are clamped up: a shorter wait is a poll.
pub const MIN_WAIT_MS: i64 = 10_000;
pub const MAX_WAIT_MS: i64 = i32::MAX as i64;

/// Persisted completion-verification snapshots are bounded so a goal that runs
/// for weeks cannot grow its state entry without limit. Oldest rows are dropped
/// first; the newest verdict and the newest iteration snapshot always survive.
pub const MAX_ITERATION_ROWS: usize = 50;
pub const MAX_VERIFICATIONS: usize = 50;
pub const MAX_VERIFICATION_ITEMS: usize = 100;
pub const MAX_VERIFICATION_ITEM_CHARS: usize = 4_000;
pub const MAX_VERIFICATION_REASON_CHARS: usize = 2_000;
pub const MAX_VERIFICATION_NEXT_ACTION_CHARS: usize = 2_000;
/// A host verdict is expected within this window. The worker recovers the goal
/// instead of leaving it in `verifying` forever when the host never answers.
pub const VERIFICATION_TIMEOUT_MS: i64 = 300_000;
/// Grace for an armed host to open a verification boundary after a run settled.
/// When the host misses it, the worker continues the goal with a visible notice
/// instead of stalling it.
pub const VERIFICATION_ARM_GRACE_MS: i64 = 120_000;

/// Terminal and stopped outcomes are distinct so the status line, the safety
/// epoch, and the continuation policy can each key off one value.
///
/// `verifying` is the completion-verification boundary: an armed host opened it
/// and no automatic continuation may start until the host reports a verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    Verifying,
    Paused,
    Blocked,
    Waiting,
    UsageLimited,
    BudgetLimited,
    Complete,
}

impl Default for Status {
    /// Fail safe: an unreadable status must never keep an automatic run alive.
    fn default() -> Self {
        Self::Paused
    }
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Verifying => "verifying",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::Waiting => "waiting",
            Self::UsageLimited => "usage_limited",
            Self::BudgetLimited => "budget_limited",
            Self::Complete => "complete",
        }
    }

    pub fn is_active(self) -> bool {
        self == Self::Active
    }

    /// The completion-verification boundary: a live run settled, an armed host
    /// was asked for a verdict, and no continuation may start until it arrives.
    pub fn is_verifying(self) -> bool {
        self == Self::Verifying
    }

    /// Stopped: work is preserved but nothing may continue automatically.
    pub fn is_stopped(self) -> bool {
        matches!(
            self,
            Self::Paused
                | Self::Blocked
                | Self::UsageLimited
                | Self::BudgetLimited
                | Self::Complete
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitKind {
    /// An arranged external wake source, or an explicit `goal_wait` deadline.
    #[default]
    External,
    /// A recognized provider/account usage limit stopped work.
    Usage,
    /// A provider failure that native retry did not resolve. Quiet, no deadline.
    Provider,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Waiting {
    pub reason: String,
    #[serde(default)]
    pub kind: WaitKind,
    /// Absolute epoch-ms safety wake. `None` waits for external input.
    #[serde(default)]
    pub deadline_ms: Option<i64>,
}

/// One todo item in an iteration snapshot. `status` is the host's todo state
/// (`pending` / `in_progress` / `completed`) carried verbatim, so the panel can
/// show the same word the agent saw.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalItem {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub status: String,
}

/// Per-run snapshot handed over at a verification boundary: what the run's todo
/// list looked like when it settled. Verifier input, not model context.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalIteration {
    pub iteration: u32,
    #[serde(default)]
    pub items: Vec<GoalItem>,
    pub updated_at: i64,
}

/// One completion-verifier verdict.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalVerification {
    pub iteration: u32,
    /// `pass` | `not_satisfied` | `failed` | `cancelled`.
    pub outcome: String,
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

/// Bound and normalize the todo snapshot the host hands over.
pub fn bound_items(items: Vec<GoalItem>) -> Vec<GoalItem> {
    let mut bounded: Vec<GoalItem> = items
        .into_iter()
        .take(MAX_VERIFICATION_ITEMS)
        .map(|item| GoalItem {
            content: item
                .content
                .chars()
                .take(MAX_VERIFICATION_ITEM_CHARS)
                .collect(),
            status: item.status.trim().to_string(),
        })
        .collect();
    if bounded.is_empty() {
        bounded.shrink_to_fit();
    }
    bounded
}

impl Goal {
    /// Open (or replace) the latest iteration snapshot, keeping the buffer
    /// bounded. Called from the host-facing `_verify-start` boundary.
    pub fn push_iteration(&mut self, row: GoalIteration) {
        if self.iterations.last().map(|last| last.iteration) == Some(row.iteration) {
            self.iterations.pop();
        }
        self.iterations.push(row);
        while self.iterations.len() > MAX_ITERATION_ROWS {
            self.iterations.remove(0);
        }
    }

    /// Record one verdict, keeping the buffer bounded.
    pub fn push_verification(&mut self, verdict: GoalVerification) {
        self.verifications.push(verdict);
        while self.verifications.len() > MAX_VERIFICATIONS {
            self.verifications.remove(0);
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Goal {
    /// Rotates on every start and on every stopped-state resume. Used as the
    /// stale-turn guard for the terminal tools and for stale continuations.
    #[serde(default)]
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub notes: String,
    #[serde(default, alias = "started_at")]
    pub started_at: i64,
    #[serde(default, alias = "updated_at")]
    pub updated_at: i64,
    /// Goal-owned automatic continuations dispatched so far.
    #[serde(default)]
    pub iteration: u32,
    /// Turns completed while the goal was active (the `maxTurns` budget).
    #[serde(default, alias = "turns_used")]
    pub turns_used: u32,
    #[serde(default, alias = "max_turns")]
    pub max_turns: Option<u32>,
    /// Absolute epoch-ms deadline (the `maxMinutes` budget).
    #[serde(default, alias = "deadline_ms")]
    pub deadline_ms: Option<i64>,
    #[serde(default, alias = "token_budget")]
    pub token_budget: Option<u64>,
    #[serde(default, alias = "tokens_used")]
    pub tokens_used: u64,
    /// Branch token usage captured when the goal started; usage is measured
    /// from here so a pre-existing session does not consume the budget.
    #[serde(default, alias = "baseline_tokens")]
    pub baseline_tokens: u64,
    /// Safety counter for automatic work (not the user-facing turn budget).
    #[serde(default, alias = "automatic_turns")]
    pub automatic_turns: u32,
    #[serde(default, alias = "no_progress_count")]
    pub no_progress_count: u32,
    /// Fixed-size digest of the last tool-free automatic output. The raw text is
    /// deliberately not retained.
    #[serde(default, alias = "last_output_fingerprint")]
    pub last_output_fingerprint: Option<String>,
    #[serde(default, alias = "safety_pause_cause")]
    pub safety_pause_cause: Option<String>,
    /// Accumulated milliseconds in `active` status. Waiting/stopped/offline time
    /// is excluded, so it is checkpointed rather than derived from `started_at`.
    #[serde(default, alias = "activeSeconds")]
    pub active_ms: i64,
    /// `Some(epoch_ms)` while the active clock is running.
    #[serde(default, alias = "active_since")]
    pub active_since: Option<i64>,
    #[serde(default)]
    pub waiting: Option<Waiting>,
    /// Provider responses whose usage was observed. Zero after real work means
    /// the host does not report usage to extensions, so a token budget cannot be
    /// enforced and must not look enforced.
    #[serde(default, alias = "usage_seen")]
    pub usage_seen: u32,
    #[serde(default, alias = "usage_unavailable")]
    pub usage_unavailable: bool,
    /// `completed` / `aborted` / `error` for the last finished run.
    #[serde(default, alias = "last_outcome")]
    pub last_outcome: Option<String>,
    /// When an armed host is expected to decide before the next continuation:
    /// set at `/goal start` (kickoff), on `notSatisfied`, and on resume. Cleared
    /// when a continuation is dispatched. Without an armed host this field is
    /// inert (v2 behavior dispatches at every settled boundary).
    #[serde(default, alias = "pending_dispatch")]
    pub pending_dispatch: bool,
    /// The iteration row currently open for verification, if any.
    #[serde(default, alias = "verify_iteration")]
    pub verify_iteration: Option<u32>,
    /// When `verifying` started (epoch ms); the recovery deadline is derived
    /// from it instead of a separate persisted timer.
    #[serde(default, alias = "verify_started_at")]
    pub verify_started_at: Option<i64>,
    /// Per-run todo snapshots handed over by the host at each verification
    /// boundary (bounded).
    #[serde(default)]
    pub iterations: Vec<GoalIteration>,
    /// Verdicts reported by the completion verifier (bounded).
    #[serde(default)]
    pub verifications: Vec<GoalVerification>,
}

impl Goal {
    pub fn new(title: String, notes: String, now: i64) -> Self {
        Self {
            id: new_goal_id(),
            title,
            status: Status::Active,
            notes,
            started_at: now,
            updated_at: now,
            iteration: 0,
            turns_used: 0,
            max_turns: None,
            deadline_ms: None,
            token_budget: None,
            tokens_used: 0,
            baseline_tokens: 0,
            automatic_turns: 0,
            no_progress_count: 0,
            last_output_fingerprint: None,
            safety_pause_cause: None,
            active_ms: 0,
            active_since: Some(now),
            waiting: None,
            usage_seen: 0,
            usage_unavailable: false,
            last_outcome: None,
            pending_dispatch: true,
            verify_iteration: None,
            verify_started_at: None,
            iterations: Vec::new(),
            verifications: Vec::new(),
        }
    }

    /// Accumulated active milliseconds including the currently running stretch.
    pub fn elapsed_ms(&self, now: i64) -> i64 {
        let running = self.active_since.map(|since| (now - since).max(0));
        self.active_ms.saturating_add(running.unwrap_or(0))
    }

    /// Fold the running stretch into the checkpoint and stop the clock. Called
    /// whenever the goal leaves `active` or the process is about to end.
    pub fn stop_clock(&mut self, now: i64) {
        if let Some(since) = self.active_since.take() {
            self.active_ms = self.active_ms.saturating_add((now - since).max(0));
        }
    }

    pub fn start_clock(&mut self, now: i64) {
        self.stop_clock(now);
        self.active_since = Some(now);
    }

    /// Human-readable budget line, e.g. `turns 3/20 · time 12m0s left`.
    pub fn budget_summary(&self, now: i64) -> String {
        let mut parts = Vec::new();
        match self.max_turns {
            Some(max) => parts.push(format!("turns {}/{}", self.turns_used.min(max), max)),
            None => parts.push(format!("turns {} (unlimited)", self.turns_used)),
        }
        match self.deadline_ms {
            Some(deadline) => {
                let remaining = deadline - now;
                if remaining <= 0 {
                    parts.push("time is up".to_string());
                } else {
                    parts.push(format!("{} left", format_duration_ms(remaining)));
                }
            }
            None => parts.push("time unlimited".to_string()),
        }
        match self.token_budget {
            Some(budget) => parts.push(format!(
                "tokens {}/{}",
                format_token_count(self.tokens_used),
                format_token_count(budget)
            )),
            None => parts.push(format!(
                "tokens {} (no budget)",
                format_token_count(self.tokens_used)
            )),
        }
        parts.join(" · ")
    }

    /// Static budget hint for the model context. Avoids live counters so the
    /// injected text stays fingerprint-stable across turns.
    pub fn budget_hint(&self) -> String {
        let mut parts = Vec::new();
        if let Some(max) = self.max_turns {
            parts.push(format!("{max} turns"));
        }
        if let Some(deadline) = self.deadline_ms {
            parts.push(format_duration_ms((deadline - self.started_at).max(0)));
        }
        if let Some(budget) = self.token_budget {
            parts.push(format!("{} tokens", format_token_count(budget)));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" (budget: {})", parts.join(" / "))
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Goals {
    #[serde(default)]
    pub version: u32,
    /// Write revision. The host may defer committing an entry written during a
    /// run until that turn ends, so this is what proves a queued write landed.
    #[serde(default)]
    pub revision: u64,
    /// Completion verification is armed by the host (`/goal _verify-enable`).
    /// While armed, the continuation worker does not start a goal-owned run
    /// until the host has reported a verdict for the run that just settled.
    /// Default `false`: standalone hosts keep the plain v2 continuation loop.
    #[serde(default)]
    pub verification: bool,
    #[serde(default)]
    pub active: Option<Goal>,
    #[serde(default)]
    pub archived: Vec<Goal>,
}

impl Goals {
    pub const VERSION: u32 = 2;
}

/// Serializes read-modify-write of goal state. Held only around the read and the
/// queued write: never across a host call, so a re-entrant callback cannot
/// deadlock against it.
static MUTATION: Mutex<()> = Mutex::new(());

pub fn lock() -> std::sync::MutexGuard<'static, ()> {
    MUTATION.lock().unwrap_or_else(|p| p.into_inner())
}

/// Monotonic in-process state revision. A continuation dispatched before a
/// mutation is discarded when the revision moved underneath it.
static REVISION: AtomicU64 = AtomicU64::new(0);

pub fn revision() -> u64 {
    REVISION.load(Ordering::SeqCst)
}

fn next_revision() -> u64 {
    REVISION.fetch_add(1, Ordering::SeqCst).wrapping_add(1)
}

/// The newest local intent. It stays authoritative until a branch read shows an
/// entry at least that new, because `AppendEntry` is *queued* while a run is
/// active and committed only after the current turn. Dropping the intent as soon
/// as the host accepted it would make `/goal status` report "no active goal"
/// for the rest of that turn.
fn desired() -> &'static Mutex<Option<Goals>> {
    static DESIRED: OnceLock<Mutex<Option<Goals>>> = OnceLock::new();
    DESIRED.get_or_init(|| Mutex::new(None))
}

/// Queue a state write for the next safe boundary. Writing immediately from a
/// tool would insert a custom entry between an `assistant(tool_calls)` message
/// and its `toolResult`, which strict providers reject.
pub fn queue(goals: &Goals) -> u64 {
    let mut goals = goals.clone();
    let revision = next_revision();
    goals.revision = revision;
    *desired().lock().unwrap_or_else(|p| p.into_inner()) = Some(goals);
    revision
}

/// Forget the local intent: a session switch or teardown means the queued state
/// belongs to a branch this process no longer serves.
pub fn discard_desired() {
    *desired().lock().unwrap_or_else(|p| p.into_inner()) = None;
}

fn desired_goals() -> Option<Goals> {
    desired().lock().unwrap_or_else(|p| p.into_inner()).clone()
}

fn confirm_desired(revision: u64) {
    let mut slot = desired().lock().unwrap_or_else(|p| p.into_inner());
    if slot.as_ref().map(|goals| goals.revision <= revision) == Some(true) {
        *slot = None;
    }
}

/// Write any queued state. Returns false when the host write failed, leaving the
/// value queued for a later boundary.
///
/// The take-and-write runs under [`lock`] on purpose: a take that released the
/// lock before writing could land an older snapshot after a newer mutation, which
/// is exactly the lost update this module exists to prevent. `write` performs one
/// `AppendEntry`, which does not re-enter this plugin, so no callback can observe
/// the lock held here.
pub fn flush() -> bool {
    let _guard = lock();
    let Some(goals) = desired_goals() else {
        return true;
    };
    // The intent is kept even on success: only a branch read that sees this
    // revision may drop it (see `read`).
    write(&goals).is_ok()
}

pub fn read() -> Result<Goals, String> {
    if runtime::snapshot().is_some() {
        let value = crate::call_runtime(
            RuntimeActionId::GetSessionBranch,
            json!({"customTypes": [GOAL_STATE_ENTRY]}),
        )?;
        if value.get("filtered").and_then(Value::as_bool) != Some(true) {
            return Err("GetSessionBranch must confirm filtered: true".into());
        }
        let entries = value
            .get("entries")
            .and_then(Value::as_array)
            .ok_or("GetSessionBranch returned no valid entries array")?;
        let mut committed: Option<Goals> = None;
        for entry in entries.iter().rev() {
            if entry.get("type").and_then(Value::as_str) == Some("custom")
                && entry.get("customType").and_then(Value::as_str) == Some(GOAL_STATE_ENTRY)
            {
                if let Some(data) = entry.get("data") {
                    committed = Some(serde_json::from_value(data.clone()).unwrap_or_default());
                    break;
                }
            }
        }
        // A queued write is not on the branch yet; keep serving the newest local
        // intent until the branch confirms that revision.
        if let Some(local) = desired_goals() {
            match committed.as_ref().map(|goals| goals.revision) {
                Some(revision) if revision >= local.revision => {
                    confirm_desired(revision);
                    return Ok(committed.unwrap_or_default());
                }
                _ => return Ok(local),
            }
        }
        return Ok(committed.unwrap_or_default());
    }
    // No host runtime: a direct-library caller keeps the file fallback.
    Ok(desired_goals().or_else(read_file).unwrap_or_default())
}

/// Read state, treating any host error as "no goal". Event handlers must never
/// fail a dispatch because the branch read failed.
pub fn read_lenient() -> Goals {
    read().unwrap_or_default()
}

pub fn write(goals: &Goals) -> Result<(), String> {
    let mut goals = goals.clone();
    goals.version = Goals::VERSION;
    if runtime::snapshot().is_some() {
        crate::call_runtime(
            RuntimeActionId::AppendEntry,
            json!({"customType": GOAL_STATE_ENTRY, "data": goals}),
        )?;
        return Ok(());
    }
    write_file(&goals)
}

/// Queue a state write. Callers must hold [`lock`] so the queued snapshot cannot
/// be replaced by a stale read-modify-write from another thread.
pub fn queue_locked(goals: &Goals) {
    let _ = queue(goals);
}

/// Persist immediately, making queued state visible before the next prompt.
pub fn persist(goals: &Goals) -> Result<(), String> {
    {
        let _guard = lock();
        queue(goals);
    }
    if flush() {
        Ok(())
    } else {
        Err("unable to persist the goal state".into())
    }
}

fn goals_path() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(LEGACY_GOAL_FILE)
}

fn read_file() -> Option<Goals> {
    let text = fs::read_to_string(goals_path()).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_file(goals: &Goals) -> Result<(), String> {
    let path = goals_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create goal directory: {e}"))?;
    }
    let tmp = path.with_file_name(format!("goals.json.tmp-{}", std::process::id()));
    let data = serde_json::to_string_pretty(goals).map_err(|e| format!("serialize goals: {e}"))?;
    fs::write(&tmp, data).map_err(|e| format!("write goals: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("commit goals: {e}"))?;
    Ok(())
}

/// Remove the inert pre-0.3 project file. Failures are not fatal: the file is
/// no longer read.
pub fn remove_legacy_file() {
    let _ = fs::remove_file(goals_path());
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 16-hex goal id. No dependency: splitmix64 over time, pid and a counter is
/// enough to make start/resume ids distinct within and across processes.
pub fn new_goal_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut x = now_ms() as u64
        ^ ((std::process::id() as u64) << 32)
        ^ COUNTER
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    format!("{x:016x}")
}

/// Parse a duration spec into milliseconds. Accepts bare numbers (minutes) and
/// unit suffixes `s`/`m`/`h`/`d`, combinable (`90s`, `30m`, `2h`, `1h30m`).
pub fn parse_duration_ms(spec: &str) -> Result<i64, String> {
    let spec = spec.trim().to_lowercase();
    if spec.is_empty() {
        return Err("empty duration".into());
    }
    if let Ok(minutes) = spec.parse::<i64>() {
        return Ok(minutes.saturating_mul(60_000));
    }
    let mut total: i64 = 0;
    let mut number = String::new();
    let mut saw_unit = false;
    for ch in spec.chars() {
        if ch.is_ascii_digit() {
            number.push(ch);
            continue;
        }
        let unit = match ch {
            's' => 1_000,
            'm' => 60_000,
            'h' => 3_600_000,
            'd' => 86_400_000,
            other => return Err(format!("unknown duration unit `{other}` (use s/m/h/d)")),
        };
        let value: i64 = number
            .parse()
            .map_err(|_| format!("invalid duration `{spec}`"))?;
        total = total.saturating_add(value.saturating_mul(unit));
        number.clear();
        saw_unit = true;
    }
    if !number.is_empty() {
        return Err(format!("duration `{spec}` is missing a unit (s/m/h/d)"));
    }
    if !saw_unit {
        return Err(format!("invalid duration `{spec}`"));
    }
    Ok(total)
}

/// Compact duration rendering: `2h5m`, `12m30s`, `45s`.
pub fn format_duration_ms(ms: i64) -> String {
    let secs = (ms.max(0) / 1000) as u64;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h{m}m")
    } else if m > 0 {
        format!("{m}m{s}s")
    } else {
        format!("{s}s")
    }
}

/// `999`, `18k`, `1.5m`.
pub fn format_token_count(tokens: u64) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }
    if tokens < 1_000_000 {
        let value = tokens as f64 / 1_000.0;
        return if value >= 100.0 {
            format!("{value:.0}k")
        } else {
            format!("{value:.1}k").replace(".0k", "k")
        };
    }
    let value = tokens as f64 / 1_000_000.0;
    format!("{value:.1}m").replace(".0m", "m")
}

/// Parse a token budget such as `100k` or `1.5m`.
pub fn parse_token_budget(spec: &str) -> Result<u64, String> {
    let spec = spec.trim().to_lowercase();
    if spec.is_empty() {
        return Err("empty token budget".into());
    }
    let (number, scale) = match spec.chars().last() {
        Some('k') => (&spec[..spec.len() - 1], 1_000.0),
        Some('m') => (&spec[..spec.len() - 1], 1_000_000.0),
        _ => (spec.as_str(), 1.0),
    };
    let value: f64 = number
        .parse()
        .map_err(|_| format!("invalid token budget `{spec}`"))?;
    if !value.is_finite() || value <= 0.0 {
        return Err(format!("invalid token budget `{spec}`"));
    }
    Ok((value * scale) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_entries_load_with_defaults() {
        let v1 = json!({
            "active": {
                "title": "Ship it",
                "status": "active",
                "notes": "",
                "startedAt": 10,
                "updatedAt": 20,
                "turnsUsed": 3,
                "maxTurns": 10,
                "deadlineMs": null
            },
            "archived": []
        });
        let goals: Goals = serde_json::from_value(v1).unwrap();
        let goal = goals.active.unwrap();
        assert_eq!(goal.status, Status::Active);
        assert_eq!(goal.turns_used, 3);
        assert_eq!(goal.max_turns, Some(10));
        assert_eq!(goal.id, "");
        assert_eq!(goal.iteration, 0);
        assert!(goal.waiting.is_none());
    }

    #[test]
    fn unknown_status_fails_safe_to_paused() {
        let bad = json!({"active": {"title": "t", "status": "nonsense"}});
        assert!(serde_json::from_value::<Goal>(bad).is_err());
        let missing = json!({"title": "t"});
        let goal: Goal = serde_json::from_value(missing).unwrap();
        assert_eq!(goal.status, Status::Paused);
    }

    #[test]
    fn stopped_statuses_are_never_active() {
        for status in [
            Status::Paused,
            Status::Blocked,
            Status::UsageLimited,
            Status::BudgetLimited,
            Status::Complete,
        ] {
            assert!(status.is_stopped());
            assert!(!status.is_active());
        }
        assert!(Status::Active.is_active());
        assert!(!Status::Active.is_stopped());
        assert!(!Status::Waiting.is_stopped());
    }

    #[test]
    fn elapsed_time_excludes_stopped_stretches() {
        let mut goal = Goal::new("t".into(), String::new(), 1_000);
        assert_eq!(goal.elapsed_ms(4_000), 3_000);
        goal.stop_clock(4_000);
        assert_eq!(goal.elapsed_ms(60_000), 3_000);
        goal.start_clock(60_000);
        assert_eq!(goal.elapsed_ms(62_000), 5_000);
    }

    #[test]
    fn goal_ids_are_unique_and_hex() {
        let a = new_goal_id();
        let b = new_goal_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn token_budget_parsing_accepts_suffixes() {
        assert_eq!(parse_token_budget("100k").unwrap(), 100_000);
        assert_eq!(parse_token_budget("1.5m").unwrap(), 1_500_000);
        assert_eq!(parse_token_budget("250").unwrap(), 250);
        assert!(parse_token_budget("0").is_err());
        assert!(parse_token_budget("abc").is_err());
        assert!(parse_token_budget("").is_err());
    }

    #[test]
    fn token_counts_are_compact() {
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(18_400), "18.4k");
        assert_eq!(format_token_count(100_000), "100k");
        assert_eq!(format_token_count(1_500_000), "1.5m");
    }

    #[test]
    fn duration_parsing_and_rendering_round_trip() {
        assert_eq!(parse_duration_ms("15").unwrap(), 900_000);
        assert_eq!(parse_duration_ms("1h30m").unwrap(), 5_400_000);
        assert_eq!(parse_duration_ms("90s").unwrap(), 90_000);
        assert!(parse_duration_ms("1x").is_err());
        assert_eq!(format_duration_ms(90_000), "1m30s");
        assert_eq!(format_duration_ms(5_400_000), "1h30m");
    }

    #[test]
    fn budget_summary_reports_every_configured_budget() {
        let mut goal = Goal::new("t".into(), String::new(), 0);
        goal.max_turns = Some(20);
        goal.turns_used = 3;
        goal.token_budget = Some(100_000);
        goal.tokens_used = 18_400;
        let summary = goal.budget_summary(0);
        assert!(summary.contains("turns 3/20"), "{summary}");
        assert!(summary.contains("tokens 18.4k/100k"), "{summary}");
        assert!(summary.contains("time unlimited"), "{summary}");
    }

    #[test]
    fn verification_state_round_trips_and_old_entries_default_it() {
        let mut goal = Goal::new("t".into(), String::new(), 0);
        assert!(
            goal.pending_dispatch,
            "a fresh goal must dispatch its kickoff"
        );
        goal.verify_iteration = Some(2);
        goal.verify_started_at = Some(123);
        goal.push_iteration(GoalIteration {
            iteration: 2,
            items: vec![GoalItem {
                content: "fix it".into(),
                status: "in_progress".into(),
            }],
            updated_at: 9,
        });
        goal.push_verification(GoalVerification {
            iteration: 2,
            outcome: "not_satisfied".into(),
            at: 10,
            reason: Some("missing evidence".into()),
            next_action: Some("run the suite".into()),
        });
        let encoded = serde_json::to_value(&goal).unwrap();
        assert_eq!(encoded["verifyIteration"], 2);
        assert_eq!(
            encoded["iterations"][0]["items"][0]["status"],
            "in_progress"
        );
        assert_eq!(encoded["verifications"][0]["nextAction"], "run the suite");
        let decoded: Goal = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.verifications[0].outcome, "not_satisfied");

        // A v2 entry without the new fields loads with verification inert.
        let old = json!({"title": "t", "status": "active"});
        let old_goal: Goal = serde_json::from_value(old).unwrap();
        assert!(old_goal.verify_iteration.is_none());
        assert!(old_goal.iterations.is_empty());
        assert!(old_goal.verifications.is_empty());
    }

    #[test]
    fn verification_buffers_are_bounded_oldest_first() {
        let mut goal = Goal::new("t".into(), String::new(), 0);
        for iteration in 1..=(MAX_ITERATION_ROWS as u32 + 5) {
            goal.push_iteration(GoalIteration {
                iteration,
                items: Vec::new(),
                updated_at: iteration as i64,
            });
        }
        assert_eq!(goal.iterations.len(), MAX_ITERATION_ROWS);
        assert_eq!(
            goal.iterations.first().map(|row| row.iteration),
            Some(6),
            "oldest rows are dropped first"
        );
        for iteration in 1..=(MAX_VERIFICATIONS as u32 + 3) {
            goal.push_verification(GoalVerification {
                iteration,
                outcome: "pass".into(),
                at: iteration as i64,
                reason: None,
                next_action: None,
            });
        }
        assert_eq!(goal.verifications.len(), MAX_VERIFICATIONS);
        assert_eq!(
            goal.verifications.last().map(|row| row.iteration),
            Some(MAX_VERIFICATIONS as u32 + 3),
            "newest verdict survives"
        );
    }

    #[test]
    fn reapplying_the_same_iteration_replaces_its_snapshot() {
        let mut goal = Goal::new("t".into(), String::new(), 0);
        goal.push_iteration(GoalIteration {
            iteration: 1,
            items: vec![GoalItem {
                content: "old".into(),
                status: "pending".into(),
            }],
            updated_at: 1,
        });
        goal.push_iteration(GoalIteration {
            iteration: 1,
            items: vec![GoalItem {
                content: "new".into(),
                status: "completed".into(),
            }],
            updated_at: 2,
        });
        assert_eq!(goal.iterations.len(), 1);
        assert_eq!(goal.iterations[0].items[0].content, "new");
    }

    #[test]
    fn item_bounds_trim_content_and_count() {
        let long = "x".repeat(MAX_VERIFICATION_ITEM_CHARS + 25);
        let mut items: Vec<GoalItem> = (0..MAX_VERIFICATION_ITEMS + 10)
            .map(|n| GoalItem {
                content: format!("{long}{n}"),
                status: "pending".into(),
            })
            .collect();
        items.push(GoalItem {
            content: "   ".into(),
            status: " pending ".into(),
        });
        let bounded = bound_items(items);
        assert_eq!(bounded.len(), MAX_VERIFICATION_ITEMS);
        assert!(bounded
            .iter()
            .all(|item| item.content.chars().count() <= MAX_VERIFICATION_ITEM_CHARS));
    }

    #[test]
    fn verifying_is_neither_active_nor_stopped() {
        assert!(!Status::Verifying.is_active());
        assert!(!Status::Verifying.is_stopped());
        assert!(Status::Verifying.is_verifying());
        assert_eq!(Status::Verifying.as_str(), "verifying");
    }
}
