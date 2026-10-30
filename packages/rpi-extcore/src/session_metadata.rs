//! Scoped, supplemental session organization metadata. Native transcripts stay authoritative.
use crate::history::{SavedSessionListError, SavedSessionMetadata};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionMetadata {
    pub title: Option<String>,
    pub archived: bool,
}

/// One writer per scope, as for Registry. Files are separate from native histories.
#[derive(Clone, Debug)]
pub struct MetadataStore { directory: PathBuf }
impl MetadataStore {
    pub fn new(root: &Path) -> Result<Self, String> {
        if !root.is_absolute() { return Err("metadata scope must be absolute".into()); }
        Ok(Self { directory: root.join(".session-metadata") })
    }
    fn path(&self, id: &str) -> Result<PathBuf, String> {
        if uuid::Uuid::parse_str(id).map(|v| v.to_string()).as_deref() != Ok(id) {
            return Err("session identity must be a canonical UUID".into());
        }
        Ok(self.directory.join(format!("{id}.json")))
    }
    pub fn read(&self, id: &str) -> Result<SessionMetadata, String> {
        use std::io::Read;
        const MAX_BYTES: u64 = 16 * 1024;
        match std::fs::File::open(self.path(id)?) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
                if bytes.len() as u64 > MAX_BYTES { return Err("session metadata exceeds 16 KiB".into()); }
                serde_json::from_slice(&bytes).map_err(|e| format!("invalid session metadata: {e}"))
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SessionMetadata::default()),
            Err(e) => Err(e.to_string()),
        }
    }
    /// Serialize read/modify/persist for this scope and canonical session ID.
    /// Like Registry, independent service processes must not share a writer scope.
    pub fn update(&self, id: &str, change: impl FnOnce(&mut SessionMetadata)) -> Result<SessionMetadata, String> {
        use std::sync::{Arc, Mutex, OnceLock, Weak};
        type Locks = std::collections::HashMap<PathBuf, Weak<Mutex<()>>>;
        static LOCKS: OnceLock<Mutex<Locks>> = OnceLock::new();
        let key = self.path(id)?;
        let lock = {
            let mut locks = LOCKS.get_or_init(Default::default).lock().map_err(|e| e.to_string())?;
            locks.retain(|_, lock| lock.strong_count() > 0);
            let lock = locks.get(&key).and_then(Weak::upgrade).unwrap_or_else(|| Arc::new(Mutex::new(())));
            locks.insert(key, Arc::downgrade(&lock));
            lock
        };
        let _guard = lock.lock().map_err(|e| e.to_string())?;
        let mut metadata = self.read(id)?;
        change(&mut metadata);
        self.write(id, &metadata)?;
        Ok(metadata)
    }
    pub fn write(&self, id: &str, metadata: &SessionMetadata) -> Result<(), String> {
        if serde_json::to_vec(metadata).map_err(|e| e.to_string())?.len() > 16 * 1024 {
            return Err("session metadata exceeds 16 KiB".into());
        }
        let path = self.path(id)?;
        std::fs::create_dir_all(&self.directory).map_err(|e| e.to_string())?;
        let temp = self.directory.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp).map_err(|e| e.to_string())?;
            file.write_all(&serde_json::to_vec(metadata).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            drop(file);
            std::fs::rename(&temp, path).map_err(|e| e.to_string())
        })();
        let _ = std::fs::remove_file(temp);
        result
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ArchiveFilter { #[default] Exclude, Include, Only }
impl ArchiveFilter {
    fn accepts(self, archived: bool) -> bool {
        match self { Self::Exclude => !archived, Self::Include => true, Self::Only => archived }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor { version: u8, cwd: Option<String>, filter: ArchiveFilter, created_at: i64, id: String }

pub struct MetadataPage {
    pub sessions: Vec<(SavedSessionMetadata, SessionMetadata)>,
    pub next_cursor: Option<String>,
}

/// Apply organization filters before selecting a page. Retains at most one native
/// page plus limit+1 output records. Cursor binds filters and immutable native order.
pub fn list_sessions(root: &Path, cwd: Option<&Path>, cursor: Option<&str>, limit: usize,
    filter: ArchiveFilter, cancelled: &AtomicBool) -> Result<MetadataPage, SavedSessionListError> {
    use SavedSessionListError::{InvalidCursor, Io};
    let store = MetadataStore::new(root).map_err(Io)?;
    let cwd_key = cwd.map(|p| std::fs::canonicalize(p).map(|p| p.to_string_lossy().into_owned())).transpose().map_err(|e| Io(e.to_string()))?;
    let after = cursor.map(|text| {
        if text.len() > 8192 { return Err(InvalidCursor); }
        let c: Cursor = serde_json::from_str(text).map_err(|_| InvalidCursor)?;
        if c.version != 1 || c.cwd != cwd_key || c.filter != filter || uuid::Uuid::parse_str(&c.id).is_err() { return Err(InvalidCursor); }
        Ok(c)
    }).transpose()?;
    let limit = limit.clamp(1, 100);
    let mut sessions = Vec::with_capacity(limit + 1);
    let mut native_cursor = None;
    'scan: loop {
        let page = crate::history::list_saved_sessions_cancelable(root, cwd, native_cursor.as_deref(), 100, cancelled)?;
        for mut session in page.sessions {
            if after.as_ref().is_some_and(|c| session.created_at > c.created_at || (session.created_at == c.created_at && session.id <= c.id)) { continue; }
            let metadata = store.read(&session.id).map_err(Io)?;
            if !filter.accepts(metadata.archived) { continue; }
            if let Some(title) = &metadata.title { session.title = Some(title.clone()); }
            sessions.push((session, metadata));
            if sessions.len() > limit { break 'scan; }
        }
        native_cursor = page.next_cursor;
        if native_cursor.is_none() { break; }
    }
    let more = sessions.len() > limit;
    sessions.truncate(limit);
    let next_cursor = if more {
        let last = &sessions.last().unwrap().0;
        Some(serde_json::to_string(&Cursor { version: 1, cwd: cwd_key, filter, created_at: last.created_at, id: last.id.clone() }).map_err(|e| Io(e.to_string()))?)
    } else { None };
    Ok(MetadataPage { sessions, next_cursor })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scoped_restart_and_replacement() {
        let root = std::env::temp_dir().join(format!("metadata-{}", uuid::Uuid::new_v4()));
        let id = uuid::Uuid::new_v4().to_string();
        let store = MetadataStore::new(&root).unwrap();
        assert!(store.read("../escape").is_err());
        let mut metadata = SessionMetadata { title: Some("User title".into()), archived: true };
        store.write(&id, &metadata).unwrap();
        assert_eq!(MetadataStore::new(&root).unwrap().read(&id).unwrap(), metadata);
        metadata.archived = false;
        store.write(&id, &metadata).unwrap();
        assert_eq!(store.read(&id).unwrap(), metadata);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn bounded_reads_and_concurrent_updates() {
        let root = std::env::temp_dir().join(format!("metadata-bounds-{}", uuid::Uuid::new_v4()));
        let id = uuid::Uuid::new_v4().to_string();
        let store = MetadataStore::new(&root).unwrap();
        store.write(&id, &SessionMetadata::default()).unwrap();
        std::fs::write(store.path(&id).unwrap(), vec![b' '; 16385]).unwrap();
        assert!(store.read(&id).unwrap_err().contains("16 KiB"));
        store.write(&id, &SessionMetadata::default()).unwrap();
        for _ in 0..50 {
            store.write(&id, &SessionMetadata::default()).unwrap();
            std::thread::scope(|scope| {
                scope.spawn(|| store.update(&id, |m| m.title = Some("chosen".into())).unwrap());
                scope.spawn(|| store.update(&id, |m| m.archived = true).unwrap());
            });
            assert_eq!(store.read(&id).unwrap(), SessionMetadata { title: Some("chosen".into()), archived: true });
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn filtering_precedes_selection_and_cursor_binds_filter() {
        let root = std::env::temp_dir().join(format!("metadata-list-{}", uuid::Uuid::new_v4()));
        let store = MetadataStore::new(&root).unwrap();
        let mut ids = Vec::new();
        for n in 0..5 {
            let id = uuid::Uuid::new_v4().to_string();
            let dir = root.join(&id).join("cwd");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{n}_{id}.jsonl")), format!("{}\n", serde_json::json!({"kind":"header","version":4,"id":id,"createdAt":n,"cwd":root}))).unwrap();
            store.write(&id, &SessionMetadata { title: Some(format!("override {n}")), archived: n % 2 == 0 }).unwrap();
            ids.push(id);
        }
        let flag = AtomicBool::new(false);
        let first = list_sessions(&root, None, None, 1, ArchiveFilter::Exclude, &flag).unwrap();
        assert_eq!(first.sessions[0].0.id, ids[3]);
        assert_eq!(first.sessions[0].0.title.as_deref(), Some("override 3"));
        let next = first.next_cursor.unwrap();
        let second = list_sessions(&root, None, Some(&next), 1, ArchiveFilter::Exclude, &flag).unwrap();
        assert_eq!(second.sessions[0].0.id, ids[1]);
        assert!(second.next_cursor.is_none());
        assert!(list_sessions(&root, None, Some(&next), 1, ArchiveFilter::Only, &flag).is_err());
        assert_eq!(list_sessions(&root, None, None, 50, ArchiveFilter::Only, &flag).unwrap().sessions.len(), 3);
        std::fs::remove_dir_all(root).unwrap();
    }
}
