//! Implementation details and resource ownership for the neutral catalog.
//!
//! Blocking workers own their admission slot until exit. The shared owner retains
//! their JoinHandles; dropping a request cancels its worker, not its join/slot.
//! Drop cancels all workers, which retain the cleanup owner until they exit;
//! shutdown additionally waits for that exit. Kernel filesystem calls cannot be
//! hard-preempted. Metadata checks/open are not an atomic filesystem sandbox.
use super::*;
use std::{
    collections::HashSet,
    fs::{self, File, ReadDir},
    io::Read,
    path::{Component, Path},
    sync::Mutex,
};
use tokio::{
    sync::{oneshot, Notify},
    task::JoinHandle,
};

pub(super) struct Inner {
    root: PathBuf,
    limits: CatalogLimits,
    state: Mutex<State>,
    notify: Notify,
    stop: CatalogControl,
}
struct State {
    generations: Vec<Slot>,
    workers: Vec<JoinHandle<()>>,
    active: usize,
    serial: u64,
}
struct Slot {
    key: u64,
    busy: bool,
    touched: Instant,
    data: Option<Generation>,
    tokens: Vec<Token>,
}
#[derive(Clone)]
struct Token {
    value: String,
    position: Option<usize>,
    limit: usize,
}
struct Generation {
    filter: Option<PathBuf>,
    scan: Option<Scan>,
    summaries: Vec<SessionSummary>,
    selected: Vec<bool>,
    bytes: usize,
    ids: HashSet<String>,
    active: Duration,
}
struct Scan {
    canonical_root: PathBuf,
    stack: Vec<Frame>,
    pending: Option<Pending>,
    entries: usize,
    reads: usize,
}
struct Frame {
    directory: ReadDir,
    path: PathBuf,
    id: Option<String>,
}
struct Pending {
    file: File,
    path: PathBuf,
    id: String,
    encoded: String,
    name: String,
    header: Vec<u8>,
    modified: Option<i64>,
}
struct CancelOnDrop(Option<CatalogControl>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(control) = &self.0 {
            control.cancel();
        }
    }
}
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn new(root: PathBuf, limits: CatalogLimits) -> Result<SessionCatalog, CatalogError> {
    valid_path(&root)?;
    let d = CatalogLimits::default();
    if limits.step_entries == 0
        || limits.step_entries > d.step_entries
        || limits.step_read_bytes == 0
        || limits.step_read_bytes > d.step_read_bytes
        || limits.total_entries == 0
        || limits.total_entries > d.total_entries
        || limits.total_read_bytes == 0
        || limits.total_read_bytes > d.total_read_bytes
        || limits.summary_bytes == 0
        || limits.summary_bytes > d.summary_bytes
        || limits.step_time.is_zero()
        || limits.step_time > d.step_time
        || limits.active_time.is_zero()
        || limits.active_time > d.active_time
        || limits.idle_ttl.is_zero()
        || limits.idle_ttl > d.idle_ttl
        || limits.max_generations == 0
        || limits.max_generations > 2
        || limits.max_workers == 0
        || limits.max_workers > 2
    {
        return Err(CatalogError::InvalidInput);
    }
    Ok(SessionCatalog {
        inner: Arc::new(Inner {
            root,
            limits,
            state: Mutex::new(State {
                generations: vec![],
                workers: vec![],
                active: 0,
                serial: 0,
            }),
            notify: Notify::new(),
            stop: CatalogControl::default(),
        }),
    })
}
fn valid_path(path: &Path) -> Result<(), CatalogError> {
    let s = path.to_str().ok_or(CatalogError::InvalidInput)?;
    if !path.is_absolute()
        || s.len() > 4096
        || s.chars().any(|c| c.is_control())
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(CatalogError::InvalidInput);
    }
    #[cfg(windows)]
    {
        use std::path::Prefix;
        if !matches!(path.components().next(), Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_) | Prefix::UNC(_,_) | Prefix::VerbatimUNC(_,_)))
        {
            return Err(CatalogError::InvalidInput);
        }
        for c in path.components() {
            if let Component::Normal(s) = c {
                let s = s.to_str().ok_or(CatalogError::InvalidInput)?;
                if s.contains(['<', '>', ':', '"', '|', '?', '*']) || s.ends_with(['.', ' ']) {
                    return Err(CatalogError::InvalidInput);
                }
                let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
                if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                    || (stem.len() == 4
                        && (stem.starts_with("COM") || stem.starts_with("LPT"))
                        && matches!(stem.as_bytes()[3], b'1'..=b'9'))
                {
                    return Err(CatalogError::InvalidInput);
                }
            }
        }
    }
    Ok(())
}
fn io_error(e: std::io::Error) -> CatalogError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        CatalogError::AccessDenied
    } else {
        CatalogError::Unavailable
    }
}
fn is_link(m: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        m.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        m.file_type().is_symlink()
    }
}
fn checked_path(path: &Path, root: &Path, directory: bool) -> Result<fs::Metadata, CatalogError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| CatalogError::AccessDenied)?;
    let mut ancestor = root.to_path_buf();
    for component in relative.components() {
        ancestor.push(component);
        if is_link(&fs::symlink_metadata(&ancestor).map_err(io_error)?) {
            return Err(CatalogError::AccessDenied);
        }
    }
    let m = fs::symlink_metadata(path).map_err(io_error)?;
    if is_link(&m) || (if directory { !m.is_dir() } else { !m.is_file() }) {
        return Err(CatalogError::AccessDenied);
    }
    let canonical = fs::canonicalize(path).map_err(io_error)?;
    if !canonical.starts_with(root) {
        return Err(CatalogError::AccessDenied);
    }
    Ok(m)
}
fn canonical_filter(path: Option<&Path>) -> Result<Option<PathBuf>, CatalogError> {
    path.map(|p| {
        valid_path(p)?;
        if !fs::metadata(p)
            .map_err(|_| CatalogError::InvalidInput)?
            .is_dir()
        {
            return Err(CatalogError::InvalidInput);
        }
        fs::canonicalize(p).map_err(io_error)
    })
    .transpose()
}

pub(super) async fn page(
    inner: &Arc<Inner>,
    query: CatalogQuery,
    control: CatalogControl,
) -> Result<CatalogPage, CatalogError> {
    control.check()?;
    inner.stop.check()?;
    if query.limit == 0 || query.limit > 100 {
        return Err(CatalogError::InvalidInput);
    }
    if let Some(p) = query.cwd.as_deref() {
        valid_path(p)?;
    }
    if query
        .cursor
        .as_ref()
        .is_some_and(|s| s.is_empty() || s.len() > 256 || !s.is_ascii())
    {
        return Err(CatalogError::InvalidCursor);
    }
    let (tx, rx) = oneshot::channel();
    {
        let mut state = lock(&inner.state);
        inner.stop.check()?;
        // Handles are retained until is_finished reports actual blocking worker exit.
        state.workers.retain(|h| !h.is_finished());
        state
            .generations
            .retain(|s| s.busy || s.touched.elapsed() < inner.limits.idle_ttl);
        let key = if let Some(cursor) = &query.cursor {
            let slot = state
                .generations
                .iter_mut()
                .find(|s| s.tokens.iter().any(|t| &t.value == cursor))
                .ok_or(CatalogError::InvalidCursor)?;
            if slot.busy {
                return Err(CatalogError::Conflict);
            }
            slot.key
        } else {
            if state.workers.len() >= inner.limits.max_workers {
                return Err(CatalogError::Conflict);
            }
            if state.generations.len() >= inner.limits.max_generations {
                let index = state
                    .generations
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| !s.busy)
                    .min_by_key(|(_, s)| s.touched)
                    .map(|(i, _)| i)
                    .ok_or(CatalogError::Conflict)?;
                state.generations.remove(index);
            }
            state.serial = state
                .serial
                .checked_add(1)
                .ok_or(CatalogError::LimitExceeded)?;
            let key = state.serial;
            state.generations.push(Slot {
                key,
                busy: false,
                touched: Instant::now(),
                data: None,
                tokens: vec![],
            });
            key
        };
        if state.workers.len() >= inner.limits.max_workers {
            return Err(CatalogError::Conflict);
        }
        let slot = state.generations.iter_mut().find(|s| s.key == key).unwrap();
        slot.busy = true;
        let data = slot.data.take();
        let tokens = slot.tokens.clone();
        state.active += 1;
        let owner = inner.clone();
        let worker_control = control.clone();
        let handle = tokio::task::spawn_blocking(move || {
            let mut tokens = tokens;
            let mut data = data;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                work(&owner, &query, &worker_control, &mut data, &mut tokens)
            }))
            .unwrap_or(Err(CatalogError::Unavailable));
            let result = result.and_then(|p| {
                worker_control.check()?;
                owner.stop.check()?;
                Ok(p)
            });
            let preserve = result.is_ok()
                || (data.is_some()
                    && matches!(
                        result,
                        Err(CatalogError::InvalidCursor | CatalogError::InvalidInput)
                    ));
            let mut state = lock(&owner.state);
            if preserve && !owner.stop.cancelled.load(Ordering::Acquire) {
                if let Some(slot) = state.generations.iter_mut().find(|s| s.key == key) {
                    slot.data = data;
                    slot.tokens = tokens;
                    slot.busy = false;
                    if result.is_ok() {
                        slot.touched = Instant::now();
                    }
                }
            } else {
                state.generations.retain(|s| s.key != key);
            }
            // Send while still holding admission, but never block on the receiver.
            let _ = tx.send(result);
            state.active -= 1;
            owner.notify.notify_waiters();
        });
        state.workers.push(handle);
    }
    let mut guard = CancelOnDrop(Some(control));
    let result = rx.await.map_err(|_| CatalogError::Unavailable)?;
    guard.0.take();
    result
}
pub(super) fn cancel(inner: &Inner) {
    inner.stop.cancel();
}
pub(super) async fn shutdown(inner: &Arc<Inner>) {
    cancel(inner);
    loop {
        let notified = inner.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let active = {
            let mut s = lock(&inner.state);
            if s.workers.iter().all(JoinHandle::is_finished) {
                s.generations.clear();
                s.workers.clear();
                return;
            }
            s.active
        };
        // Do not take unfinished handles out of the retained owner: cancelling
        // this shutdown future must not make a later shutdown miss worker exit.
        if active == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        } else {
            notified.await;
        }
    }
}
fn check(
    inner: &Inner,
    control: &CatalogControl,
    active: Duration,
    start: Instant,
) -> Result<(), CatalogError> {
    control.check()?;
    inner.stop.check()?;
    if active.saturating_add(start.elapsed()) >= inner.limits.active_time {
        // This is a generation-wide resource budget, distinct from the
        // caller's per-request cancellation/deadline control.
        return Err(CatalogError::LimitExceeded);
    }
    Ok(())
}
fn token(position: Option<usize>, limit: usize) -> Token {
    Token {
        value: uuid::Uuid::new_v4().to_string(),
        position,
        limit,
    }
}
fn work(
    inner: &Inner,
    query: &CatalogQuery,
    control: &CatalogControl,
    data: &mut Option<Generation>,
    tokens: &mut Vec<Token>,
) -> Result<CatalogPage, CatalogError> {
    let start = Instant::now();
    check(inner, control, Duration::ZERO, start)?;
    let filter = canonical_filter(query.cwd.as_deref())?;
    check(inner, control, Duration::ZERO, start)?;
    let mut position = 0;
    if let Some(cursor) = &query.cursor {
        let t = tokens
            .iter()
            .find(|t| &t.value == cursor)
            .ok_or(CatalogError::InvalidCursor)?;
        if t.limit != query.limit || data.as_ref().map(|g| &g.filter) != Some(&filter) {
            return Err(CatalogError::InvalidCursor);
        }
        position = t.position.unwrap_or(0);
    }
    if data.is_none() {
        let scan = match fs::canonicalize(&inner.root) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(io_error(e)),
            Ok(root) => {
                if !fs::metadata(&root).map_err(io_error)?.is_dir() {
                    return Err(CatalogError::Unavailable);
                }
                Some(Scan {
                    stack: vec![Frame {
                        directory: fs::read_dir(&root).map_err(io_error)?,
                        path: root.clone(),
                        id: None,
                    }],
                    canonical_root: root,
                    pending: None,
                    entries: 0,
                    reads: 0,
                })
            }
        };
        *data = Some(Generation {
            filter,
            scan,
            summaries: vec![],
            selected: vec![],
            bytes: 0,
            ids: HashSet::new(),
            active: Duration::ZERO,
        });
    }
    let g = data.as_mut().unwrap();
    check(inner, control, g.active, start)?;
    if g.scan.is_some() {
        scan_step(inner, control, g, start)?;
        if let Some(cursor) = &query.cursor {
            tokens.retain(|t| &t.value != cursor);
        }
        if g.scan.is_some() {
            check(inner, control, g.active, start)?;
            g.active = g.active.saturating_add(start.elapsed());
            let t = token(None, query.limit);
            let next = t.value.clone();
            tokens.push(t);
            return Ok(CatalogPage {
                sessions: vec![],
                next_cursor: Some(next),
            });
        }
        // Selection was evaluated during discovery; pagination is frozen thereafter.
        let mut selected = g.selected.iter();
        g.summaries.retain(|_| *selected.next().unwrap());
        g.selected.clear();
        g.ids.clear();
        g.summaries.sort_by(|a, b| {
            b.created_at_ms
                .cmp(&a.created_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
    }
    let end = position.saturating_add(query.limit).min(g.summaries.len());
    let next_cursor = if end < g.summaries.len() {
        if let Some(t) = tokens
            .iter()
            .find(|t| t.position == Some(end) && t.limit == query.limit)
        {
            Some(t.value.clone())
        } else {
            let t = token(Some(end), query.limit);
            let next = t.value.clone();
            tokens.push(t);
            Some(next)
        }
    } else {
        None
    };
    check(inner, control, g.active, start)?;
    g.active = g.active.saturating_add(start.elapsed());
    Ok(CatalogPage {
        sessions: g.summaries[position..end].to_vec(),
        next_cursor,
    })
}

fn scan_step(
    inner: &Inner,
    control: &CatalogControl,
    g: &mut Generation,
    start: Instant,
) -> Result<(), CatalogError> {
    let scan = g.scan.as_mut().unwrap();
    let mut entries = 0;
    let mut reads = 0;
    loop {
        check(inner, control, g.active, start)?;
        if entries >= inner.limits.step_entries
            || reads >= inner.limits.step_read_bytes
            || start.elapsed() >= inner.limits.step_time
        {
            return Ok(());
        }
        if let Some(p) = scan.pending.as_mut() {
            // Limit actual reads, including read-ahead, before issuing each read.
            if scan.reads >= inner.limits.total_read_bytes {
                return Err(CatalogError::LimitExceeded);
            }
            let available = (inner.limits.step_read_bytes - reads)
                .min(inner.limits.total_read_bytes - scan.reads)
                .min(65536 - p.header.len())
                .min(1024);
            if available == 0 {
                return Err(CatalogError::LimitExceeded);
            }
            let mut buf = [0u8; 1024];
            let n = p.file.read(&mut buf[..available]).map_err(io_error)?;
            reads += n;
            scan.reads += n;
            if n == 0 {
                return Err(CatalogError::Unavailable);
            }
            if let Some(end) = buf[..n].iter().position(|b| *b == b'\n') {
                p.header.extend_from_slice(&buf[..=end]);
                check(inner, control, g.active, start)?;
                let (summary, selected) = parse_header(p, g.filter.as_deref())?;
                // Recheck the path after reading. This does not eliminate TOCTOU.
                checked_path(&p.path, &scan.canonical_root, false)?;
                if !g.ids.insert(summary.id.clone()) {
                    return Err(CatalogError::Conflict);
                }
                g.bytes = g
                    .bytes
                    .checked_add(
                        summary.cwd.as_os_str().len().saturating_mul(4) + summary.id.len() + 256,
                    )
                    .ok_or(CatalogError::LimitExceeded)?;
                if g.bytes > inner.limits.summary_bytes {
                    return Err(CatalogError::LimitExceeded);
                }
                g.summaries.push(summary);
                g.selected.push(selected);
                scan.pending = None;
            } else {
                p.header.extend_from_slice(&buf[..n]);
                if p.header.len() == 65536 {
                    return Err(CatalogError::LimitExceeded);
                }
            }
            continue;
        }
        let Some(frame) = scan.stack.last_mut() else {
            g.scan = None;
            return Ok(());
        };
        let Some(entry) = frame.directory.next() else {
            scan.stack.pop();
            continue;
        };
        let entry = entry.map_err(io_error)?;
        entries += 1;
        scan.entries += 1;
        if scan.entries > inner.limits.total_entries {
            return Err(CatalogError::LimitExceeded);
        }
        let depth = scan.stack.len();
        let frame = scan.stack.last().unwrap();
        let name = entry.file_name();
        let Some(name) = candidate_name(
            &name,
            if depth == 3 {
                frame.id.as_deref()
            } else {
                None
            },
        )?
        else {
            continue;
        };
        let path = entry.path();
        match depth {
            1 => {
                if !canonical_uuid(name) {
                    continue;
                }
                checked_path(&path, &scan.canonical_root, true)?;
                scan.stack.push(Frame {
                    directory: fs::read_dir(&path).map_err(io_error)?,
                    path,
                    id: Some(name.to_string()),
                });
            }
            2 => {
                if !name.starts_with("--") || !name.ends_with("--") {
                    continue;
                }
                checked_path(&path, &scan.canonical_root, true)?;
                scan.stack.push(Frame {
                    directory: fs::read_dir(&path).map_err(io_error)?,
                    path,
                    id: frame.id.clone(),
                });
            }
            3 => {
                let id = frame.id.as_ref().unwrap();
                if !name.ends_with(&format!("_{id}.jsonl")) {
                    continue;
                }
                let m = checked_path(&path, &scan.canonical_root, false)?;
                let file = File::open(&path).map_err(io_error)?;
                if !file.metadata().map_err(io_error)?.is_file() {
                    return Err(CatalogError::AccessDenied);
                }
                scan.pending = Some(Pending {
                    file,
                    path,
                    id: id.clone(),
                    encoded: frame
                        .path
                        .file_name()
                        .unwrap()
                        .to_str()
                        .ok_or(CatalogError::Unavailable)?
                        .to_owned(),
                    name: name.to_owned(),
                    header: vec![],
                    modified: m.modified().ok().and_then(system_millis),
                });
            }
            _ => unreachable!(),
        }
    }
}
fn candidate_name<'a>(
    name: &'a std::ffi::OsStr,
    leaf_id: Option<&str>,
) -> Result<Option<&'a str>, CatalogError> {
    if let Some(text) = name.to_str() {
        return Ok(Some(text));
    }
    // Recognize a raw-byte same-ID suffix even when its timestamp is not UTF-8.
    // This pure check is testable on filesystems that cannot create such names.
    if leaf_id.is_some_and(|id| {
        name.as_encoded_bytes()
            .ends_with(format!("_{id}.jsonl").as_bytes())
    }) {
        return Err(CatalogError::Unavailable);
    }
    Ok(None)
}
fn canonical_uuid(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok_and(|u| u.hyphenated().to_string() == s)
}
fn system_millis(t: std::time::SystemTime) -> Option<i64> {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).ok(),
        Err(e) => i64::try_from(e.duration().as_millis()).ok()?.checked_neg(),
    }
}
fn encoded_cwd(cwd: &str) -> String {
    let stripped = cwd
        .strip_prefix('/')
        .or_else(|| cwd.strip_prefix('\\'))
        .unwrap_or(cwd);
    format!("--{}--", stripped.replace(['/', '\\', ':'], "-"))
}
fn filename(ms: i64, id: &str) -> Option<String> {
    // Four-digit Gregorian UTC year only (0000 through 9999).
    if !(-62_167_219_200_000..=253_402_300_799_999).contains(&ms) {
        return None;
    }
    let seconds = ms.div_euclid(1000);
    let z = seconds.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let time = seconds.rem_euclid(86400);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}-{:02}-{:02}-{:03}Z_{id}.jsonl",
        time / 3600,
        (time / 60) % 60,
        time % 60,
        ms.rem_euclid(1000)
    ))
}
fn parse_header(
    p: &Pending,
    filter: Option<&Path>,
) -> Result<(SessionSummary, bool), CatalogError> {
    #[derive(serde::Deserialize)]
    struct Header {
        kind: String,
        version: u32,
        id: String,
        #[serde(rename = "createdAt")]
        created_at: i64,
        cwd: String,
    }
    // Typed fields reject duplicate identity/metadata keys rather than choosing
    // whichever duplicate happened to appear last. Unknown native fields ignored.
    let v: Header = serde_json::from_slice(&p.header).map_err(|_| CatalogError::Unavailable)?;
    if v.kind != "header" || v.version != 4 || v.id != p.id {
        return Err(CatalogError::Unavailable);
    }
    let ms = v.created_at;
    if filename(ms, &p.id).as_deref() != Some(&p.name) {
        return Err(CatalogError::Unavailable);
    }
    let cwd = v.cwd.as_str();
    let path = PathBuf::from(cwd);
    valid_path(&path).map_err(|_| CatalogError::Unavailable)?;
    if encoded_cwd(cwd) != p.encoded {
        return Err(CatalogError::Unavailable);
    }
    let canonical = match fs::metadata(&path) {
        Ok(m) if m.is_dir() => Some(fs::canonicalize(&path).map_err(io_error)?),
        Ok(_) => return Err(CatalogError::Unavailable),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_error(e)),
    };
    let selected = filter.is_none() || canonical.as_deref() == filter;
    Ok((
        SessionSummary {
            id: p.id.clone(),
            cwd: path,
            created_at_ms: ms,
            modified_at_ms: p.modified,
        },
        selected,
    ))
}

#[cfg(test)]
#[path = "session_catalog_tests.rs"]
mod tests;
