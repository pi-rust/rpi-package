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
/// Verifier verdicts recorded by an embedding host. Rows and field sizes are
/// bounded so a goal cannot grow its persisted state without limit.
pub const MAX_VERIFICATIONS: usize = 50;
pub const MAX_VERIFICATION_CHARS: usize = 2_000;
/// `goal_wait` deadlines below this are clamped up: a shorter wait is a poll.
pub const MIN_WAIT_MS: i64 = 10_000;
pub const MAX_WAIT_MS: i64 = i32::MAX as i64;

/// Terminal and stopped outcomes are distinct so the status line, the safety
/// epoch, and the continuation policy can each key off one value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
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

/// One completion-verification verdict recorded by an embedding host through
/// `/goal _verify-result`. Purely a record: the seven statuses are untouched,
/// so a rejection is expressed by re-queueing [`Goal::pending_next_action`],
/// not by inventing a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VerdictOutcome {
    Pass,
    NotSatisfied,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalVerification {
    pub outcome: VerdictOutcome,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub next_action: Option<String>,
    #[serde(default)]
    pub at_ms: i64,
}

impl GoalVerification {
    pub fn outcome_str(&self) -> &'static str {
        match self.outcome {
            VerdictOutcome::Pass => "pass",
            VerdictOutcome::NotSatisfied => "not satisfied",
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
    /// Verifier verdicts recorded by the host, oldest first, bounded at
    /// [`MAX_VERIFICATIONS`].
    #[serde(default)]
    pub verifications: Vec<GoalVerification>,
    /// Verifier-directed next action awaiting dispatch by the continuation
    /// worker. Cleared when its run is dispatched; a verdict that arrives while
    /// the goal is stopped keeps it queued for `/goal resume`.
    #[serde(default)]
    pub pending_next_action: Option<String>,
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
            verifications: Vec::new(),
            pending_next_action: None,
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

    /// Append a verdict, evicting the oldest row past [`MAX_VERIFICATIONS`].
    pub fn push_verification(&mut self, verdict: GoalVerification) {
        if self.verifications.len() >= MAX_VERIFICATIONS {
            self.verifications.remove(0);
        }
        self.verifications.push(verdict);
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
    fn verdict_rows_are_bounded_and_legacy_entries_default_to_empty() {
        let mut goal = Goal::new("t".into(), String::new(), 0);
        for i in 0..(MAX_VERIFICATIONS + 5) {
            goal.push_verification(GoalVerification {
                outcome: if i % 2 == 0 {
                    VerdictOutcome::Pass
                } else {
                    VerdictOutcome::NotSatisfied
                },
                reason: None,
                next_action: None,
                at_ms: i as i64,
            });
        }
        assert_eq!(goal.verifications.len(), MAX_VERIFICATIONS);
        assert_eq!(goal.verifications[0].at_ms, 5, "oldest rows are evicted");
        assert_eq!(
            goal.verifications[MAX_VERIFICATIONS - 1].at_ms,
            (MAX_VERIFICATIONS + 4) as i64
        );

        let legacy: Goal =
            serde_json::from_value(json!({"title": "t", "status": "active"})).unwrap();
        assert!(legacy.verifications.is_empty());
        assert!(legacy.pending_next_action.is_none());
    }

    #[test]
    fn verdict_outcomes_round_trip_through_the_host_json_shape() {
        let verdict: GoalVerification = serde_json::from_value(json!({
            "outcome": "notSatisfied",
            "reason": "tests not run",
            "nextAction": "run the tests",
            "atMs": 12
        }))
        .unwrap();
        assert_eq!(verdict.outcome, VerdictOutcome::NotSatisfied);
        assert_eq!(verdict.next_action.as_deref(), Some("run the tests"));
        let serialized = serde_json::to_value(&verdict).unwrap();
        assert_eq!(serialized["outcome"], json!("notSatisfied"));
        assert_eq!(serialized["nextAction"], json!("run the tests"));
    }
}
