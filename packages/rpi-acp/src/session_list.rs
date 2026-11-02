//! Thin ACP session/list mapping. Discovery, snapshots and cursors belong to core.
use crate::protocol::Writer;
use rpi_extcore::session_catalog::{
    CatalogControl, CatalogError, CatalogLimits, CatalogPage, CatalogQuery, SessionCatalog,
};
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

pub(crate) fn parse(params: &Value) -> Result<CatalogQuery, CatalogError> {
    let object = match params {
        Value::Null => None,
        Value::Object(object) => Some(object),
        _ => return Err(CatalogError::InvalidInput),
    };
    let optional = |name: &str| -> Result<Option<String>, CatalogError> {
        match object.and_then(|object| object.get(name)) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) if !text.is_empty() && !text.contains('\0') => {
                Ok(Some(text.clone()))
            }
            _ => Err(CatalogError::InvalidInput),
        }
    };
    let cwd = optional("cwd")?.map(PathBuf::from);
    if cwd.as_ref().is_some_and(|cwd| !cwd.is_absolute()) {
        return Err(CatalogError::InvalidInput);
    }
    let cursor = optional("cursor")?;
    if cursor.as_ref().is_some_and(|cursor| {
        cursor.len() > 256 || !cursor.bytes().all(|byte| byte.is_ascii_graphic())
    }) {
        return Err(CatalogError::InvalidCursor);
    }
    // Wire pagination is deliberately fixed; client metadata cannot raise it.
    Ok(CatalogQuery {
        cwd,
        cursor,
        limit: 50,
    })
}

/// Proleptic Gregorian UTC conversion, using Euclidean division for pre-epoch
/// milliseconds. Restrict to four-digit years before any arithmetic; extreme
/// filesystem timestamps must never overflow or escape RFC3339's wire shape.
fn utc_date(ms: i64) -> Option<String> {
    if !(-62_167_219_200_000..=253_402_300_799_999).contains(&ms) {
        return None;
    }
    let seconds = ms.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        time / 3600,
        time / 60 % 60,
        time % 60,
        ms.rem_euclid(1000)
    ))
}

pub(crate) fn format_page(page: CatalogPage) -> Result<Value, CatalogError> {
    let mut sessions = Vec::with_capacity(page.sessions.len());
    for session in page.sessions {
        let cwd = session.cwd.to_str().ok_or(CatalogError::Unavailable)?;
        let mut value = json!({"sessionId":session.id,"cwd":cwd});
        if let Some(date) = session.modified_at_ms.and_then(utc_date) {
            value["updatedAt"] = json!(date);
        }
        // No invented title, activity time, createdAt or metadata sidecar.
        sessions.push(value);
    }
    let mut result = json!({"sessions":sessions});
    if let Some(cursor) = page.next_cursor {
        result["nextCursor"] = json!(cursor);
    }
    Ok(result)
}

pub(crate) fn wire_error(error: &CatalogError) -> (i64, &'static str) {
    match error {
        CatalogError::InvalidInput => (-32602, "invalid session/list parameters"),
        CatalogError::InvalidCursor => (-32602, "invalid session/list cursor"),
        CatalogError::Cancelled => (-32800, "session/list cancelled"),
        CatalogError::DeadlineExceeded => (-32800, "session/list deadline exceeded"),
        CatalogError::LimitExceeded => (-32001, "session/list limit exceeded"),
        CatalogError::AccessDenied => (-32001, "session/list access denied"),
        CatalogError::Conflict => (-32001, "session/list busy"),
        CatalogError::Unavailable => (-32001, "session/list unavailable"),
    }
}

struct ListTask {
    control: CatalogControl,
    handle: tokio::task::JoinHandle<()>,
}

/// Connection-owned resources only. Construction captures configuration without
/// touching disk; the first accepted page is the first catalog filesystem I/O.
pub(crate) struct ListTasks {
    catalog: Result<Arc<SessionCatalog>, CatalogError>,
    tasks: HashMap<String, ListTask>,
    closing: tokio::sync::watch::Sender<bool>,
}
impl ListTasks {
    pub(crate) fn new(root: Result<PathBuf, String>) -> Self {
        Self {
            catalog: root
                .map_err(|_| CatalogError::Unavailable)
                .and_then(|root| SessionCatalog::new(root, CatalogLimits::default()))
                .map(Arc::new),
            tasks: HashMap::new(),
            closing: tokio::sync::watch::channel(false).0,
        }
    }

    pub(crate) async fn reap(&mut self) {
        let keys: Vec<_> = self
            .tasks
            .iter()
            .filter(|(_, task)| task.handle.is_finished())
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            if let Some(task) = self.tasks.remove(&key) {
                let _ = task.handle.await;
            }
        }
    }

    pub(crate) fn contains(&self, key: &str) -> bool {
        self.tasks.contains_key(key)
    }

    pub(crate) fn cancel(&self, key: &str) -> bool {
        if let Some(task) = self.tasks.get(key) {
            task.control.cancel();
            true
        } else {
            false
        }
    }

    pub(crate) async fn start(
        &mut self,
        id: Value,
        key: String,
        params: &Value,
        writer: Arc<Writer>,
    ) {
        let query = match parse(params) {
            Ok(query) => query,
            Err(error) => {
                let (code, message) = wire_error(&error);
                writer.respond_error(&id, code, message).await;
                return;
            }
        };
        if self.tasks.len() >= 2 {
            let (code, message) = wire_error(&CatalogError::Conflict);
            writer.respond_error(&id, code, message).await;
            return;
        }
        let catalog = match &self.catalog {
            Ok(catalog) => Arc::clone(catalog),
            Err(error) => {
                let (code, message) = wire_error(error);
                writer.respond_error(&id, code, message).await;
                return;
            }
        };
        let control = CatalogControl::with_timeout(std::time::Duration::from_secs(30));
        let task_control = control.clone();
        let mut closing = self.closing.subscribe();
        let handle = tokio::spawn(async move {
            let result = async {
                let page = catalog.page(query, task_control.clone()).await?;
                task_control.check()?;
                let result = format_page(page)?;
                task_control.check()?;
                Ok::<_, CatalogError>(result)
            }
            .await;
            // Only terminal connection closure may interrupt the completion write.
            // Prefer an immediately writable best-effort response even on EOF.
            // Dropping an in-progress write can truncate a frame: this connection
            // is closing and must never resume. Live request cancellation still
            // writes exactly one completion through the normal Writer path.
            tokio::select! {
                biased;
                _ = async {
                    match result {
                        Ok(result) => writer.respond(&id, result).await,
                        Err(error) => {
                            let (code, message) = wire_error(&error);
                            writer.respond_error(&id, code, message).await;
                        }
                    }
                } => {},
                _ = closing.changed() => {},
            }
        });
        self.tasks.insert(key, ListTask { control, handle });
    }

    fn close(&self) {
        for task in self.tasks.values() {
            task.control.cancel();
        }
        self.closing.send_replace(true);
    }

    pub(crate) async fn shutdown(&mut self) {
        self.close();
        for (_, task) in self.tasks.drain() {
            let _ = task.handle.await;
        }
        if let Ok(catalog) = &self.catalog {
            catalog.shutdown().await;
        }
    }
}

impl Drop for ListTasks {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("session_list_tests.rs");
    include!("session_list_fixture_tests.rs");
}
