//! Bounded, read-only native transcript discovery, independent of adapters.
//!
//! Create a [`SessionCatalog`] for one captured absolute native session root and
//! share it with `Arc` when needed. [`SessionCatalog::new`] only validates inputs;
//! discovery starts on [`SessionCatalog::page`] and never creates files or starts
//! a native host. The accepted layout is `root/UUID/encoded-cwd/timestamp_UUID.jsonl`:
//! only a bounded native v4 first-line header is parsed, never transcript bodies.
//!
//! Continue empty pages that carry a cursor: discovery may need several bounded
//! steps before returning any sessions. Once discovered, results are frozen and
//! sorted by creation time descending, then canonical UUID ascending. Cursors are
//! opaque, catalog-local and bound to the canonical cwd filter and page limit.
//! A generation can expire or be evicted; invalid cursors require a fresh query.
//!
//! Dropping a page future cancels its worker while retaining cleanup ownership
//! and admission until the worker exits. Dropping the catalog cancels all workers;
//! call [`SessionCatalog::shutdown`] to wait for exit. Filesystem operations are
//! cooperatively checked, not hard-preemptible. Discovery is an observed snapshot,
//! not an atomic filesystem transaction or a race-free hostile-filesystem sandbox.
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogError {
    InvalidInput,
    InvalidCursor,
    Cancelled,
    DeadlineExceeded,
    LimitExceeded,
    AccessDenied,
    Conflict,
    Unavailable,
}
impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid catalog input",
            Self::InvalidCursor => "invalid catalog cursor",
            Self::Cancelled => "catalog cancelled",
            Self::DeadlineExceeded => "catalog deadline exceeded",
            Self::LimitExceeded => "catalog limit exceeded",
            Self::AccessDenied => "catalog access denied",
            Self::Conflict => "catalog conflict",
            Self::Unavailable => "catalog unavailable",
        })
    }
}
impl std::error::Error for CatalogError {}
/// Cloneable request cancellation and optional monotonic deadline.
/// Clones share cancellation; a timeout is checked cooperatively by the worker.
#[derive(Clone, Default)]
pub struct CatalogControl {
    cancelled: Arc<AtomicBool>,
    deadline: Option<Instant>,
}
impl CatalogControl {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    pub fn check(&self) -> Result<(), CatalogError> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(CatalogError::Cancelled)
        } else if self.deadline.is_some_and(|d| Instant::now() >= d) {
            Err(CatalogError::DeadlineExceeded)
        } else {
            Ok(())
        }
    }
    pub fn with_timeout(timeout: Duration) -> Self {
        let now = Instant::now();
        Self {
            deadline: Some(now.checked_add(timeout).unwrap_or(now)),
            ..Self::default()
        }
    }
}
/// Per-generation budgets. Defaults are hard upper bounds; every field may be
/// reduced to a positive value. Entry/read/time step budgets bound each scan
/// continuation, while total budgets fail the entire generation closed.
#[derive(Debug, Clone)]
pub struct CatalogLimits {
    pub step_entries: usize,
    pub step_read_bytes: usize,
    pub step_time: Duration,
    pub total_entries: usize,
    pub total_read_bytes: usize,
    pub summary_bytes: usize,
    pub active_time: Duration,
    pub idle_ttl: Duration,
    pub max_generations: usize,
    pub max_workers: usize,
}
impl Default for CatalogLimits {
    fn default() -> Self {
        Self {
            step_entries: 256,
            step_read_bytes: 4 * 1024 * 1024,
            step_time: Duration::from_millis(250),
            total_entries: 10_000,
            total_read_bytes: 64 * 1024 * 1024,
            summary_bytes: 16 * 1024 * 1024,
            active_time: Duration::from_secs(30),
            idle_ttl: Duration::from_secs(60),
            max_generations: 2,
            max_workers: 2,
        }
    }
}
/// A continuation must retain its canonical cwd filter and page limit (1..=100).
/// No cursor starts a new observed-discovery generation, not an FS transaction.
pub struct CatalogQuery {
    pub cwd: Option<PathBuf>,
    pub cursor: Option<String>,
    pub limit: usize,
}
/// Header identity and recorded workspace, with no title or message content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    /// Recorded absolute cwd, including deleted workspaces; not lowercased.
    pub cwd: PathBuf,
    pub created_at_ms: i64,
    /// Observed transcript filesystem mtime, not last user/model activity.
    pub modified_at_ms: Option<i64>,
}
/// Empty sessions with a cursor can mean scan-in-progress, never completion.
/// Scan cursors are single-use; frozen page cursors are retryable/idempotent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPage {
    pub sessions: Vec<SessionSummary>,
    pub next_cursor: Option<String>,
}
/// Bounded catalog and worker owner. Public async operations are `Send`.
pub struct SessionCatalog {
    inner: Arc<implementation::Inner>,
}
impl SessionCatalog {
    /// Validate an absolute UTF-8 root and positive budgets, without disk I/O.
    /// A missing root is an empty catalog; the configured root may canonicalize.
    pub fn new(root: PathBuf, limits: CatalogLimits) -> Result<Self, CatalogError> {
        implementation::new(root, limits)
    }
    /// Advance discovery or read a frozen page of 1..=100 sessions.
    /// A cwd filter must be an existing absolute directory. Matching malformed
    /// candidates and exhausted global budgets fail closed, never truncate-success.
    /// Busy admission returns `Conflict` without queueing unbounded work.
    pub async fn page(
        &self,
        query: CatalogQuery,
        control: CatalogControl,
    ) -> Result<CatalogPage, CatalogError> {
        implementation::page(&self.inner, query, control).await
    }
    /// Permanently cancel admission/workers and wait for actual worker exit.
    /// Safe to repeat or cancel and retry; synchronous kernel I/O may delay exit.
    pub async fn shutdown(&self) {
        implementation::shutdown(&self.inner).await
    }
}
impl Drop for SessionCatalog {
    fn drop(&mut self) {
        implementation::cancel(&self.inner);
    }
}
#[path = "session_catalog_impl.rs"]
mod implementation;
