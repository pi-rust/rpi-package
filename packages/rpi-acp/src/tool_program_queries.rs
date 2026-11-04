//! ACP-only envelopes for neutral, read-only child queries on existing sessions.
use super::{Session, Writer};
use rpi_extcore::{
    reply::{ReplyKind, ServiceError, ServiceResult},
    service::{
        tool_program_queries::{ChildBodyQuery, ChildPageQuery, ChildPhaseQuery},
        tool_program_snapshot::ChildSnapshotQuery,
    },
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

pub(super) const METHODS: [&str; 4] = [
    "session/tool_program_children",
    "session/tool_program_phases",
    "session/tool_program_body",
    "session/tool_program_snapshot",
];

pub(super) fn capability() -> Value {
    json!({"version":1,"optional":true,"recovery":"readOnly","methods":METHODS})
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request<Q> {
    session_id: String,
    query: Q,
    // Omission or an object only; null/scalars/arrays are not metadata objects.
    #[serde(default, rename = "_meta")]
    _meta: Map<String, Value>,
}

enum Query {
    Children(ChildPageQuery),
    Phases(ChildPhaseQuery),
    Body(ChildBodyQuery),
    Snapshot(ChildSnapshotQuery),
}

fn request<Q: DeserializeOwned>(params: Value) -> Result<Request<Q>, ()> {
    let request: Request<Q> = serde_json::from_value(params).map_err(|_| ())?;
    if request.session_id.trim().is_empty()
        || request.session_id.len() > 512
        || request.session_id.chars().any(char::is_control)
    {
        return Err(());
    }
    Ok(request)
}

fn parse(method: &str, params: Value) -> Result<(String, Query), ()> {
    macro_rules! parse {
        ($variant:ident) => {{
            let request = request(params)?;
            Ok((request.session_id, Query::$variant(request.query)))
        }};
    }
    match method {
        "session/tool_program_children" => parse!(Children),
        "session/tool_program_phases" => parse!(Phases),
        "session/tool_program_body" => parse!(Body),
        "session/tool_program_snapshot" => parse!(Snapshot),
        _ => Err(()),
    }
}

/// Never derive caller fault from native diagnostic strings.
fn map_reply_kind(kind: ReplyKind) -> (i64, &'static str) {
    let code = match kind {
        ReplyKind::InvalidInput | ReplyKind::NotFound => -32602,
        _ => -32001,
    };
    let name = match kind {
        ReplyKind::Success => "success",
        ReplyKind::Created => "created",
        ReplyKind::InvalidInput => "invalid_input",
        ReplyKind::NotFound => "not_found",
        ReplyKind::AccessDenied => "access_denied",
        ReplyKind::LimitExceeded => "limit_exceeded",
        ReplyKind::UnsupportedContent => "unsupported_content",
        ReplyKind::Internal => "internal",
        ReplyKind::HostUnavailable => "host_unavailable",
        ReplyKind::Timeout => "timeout",
        ReplyKind::Unsupported => "unsupported",
        ReplyKind::Conflict => "conflict",
        ReplyKind::Empty => "empty",
        ReplyKind::Unavailable => "unavailable",
    };
    (code, name)
}

// Preserve known neutral-core diagnostics/codes, never arbitrary native output
// or adapter debug strings. New core diagnostics get a safe bounded fallback.
fn error_value((kind, value): ServiceError) -> Value {
    let (code, name) = map_reply_kind(kind);
    let message = value["error"]
        .as_str()
        .filter(|message| {
            matches!(
                *message,
                "invalid bounded native child query"
                    | "native host returned an invalid bounded child page"
                    | "native host does not advertise the exact read-only child query profile"
                    | "native child query failed"
                    | "native child query rejected"
                    | "native child source identity unavailable"
                    | "native child host unavailable"
                    | "session not found"
                    | "child snapshot changed or exceeded its replay budget; reload"
                    | "native child metadata is inconsistent"
                    | "native child snapshot timed out"
                    | "invalid bounded fresh child snapshot query"
                    | "native host does not advertise child snapshot queries and events"
            )
        })
        .unwrap_or("read-only child query failed");
    let data_code = match value["code"].as_str() {
        Some(code @ ("tool_program_queries_unsupported" | "child_snapshot_resync_required")) => {
            code
        }
        _ => name,
    };
    json!({"code":code,"message":message,"data":{"code":data_code}})
}

async fn reply<T: Serialize>(writer: &Writer, id: &Value, result: ServiceResult<T>) {
    let result = result.and_then(|value| {
        serde_json::to_value(value).map_err(|_| (ReplyKind::Internal, Value::Null))
    });
    match result {
        Ok(value) => writer.respond(id, value).await,
        Err(error) => {
            // Private typed-error path; legacy Writer::respond_error is unchanged.
            let _ = writer
                .write(&json!({"jsonrpc":"2.0","id":id,"error":error_value(error)}))
                .await;
        }
    }
}

/// ACP transport tasks own their core future through the final response write.
/// A finishing write still occupies its slot and request ID. Aborting and joining
/// drops native correlation before cancellation acknowledgement; FrameWrite keeps
/// an interrupted output frame terminal for the whole connection.
#[derive(Default)]
pub(super) struct QueryTasks {
    tasks: HashMap<String, QueryTask>,
}

struct QueryTask {
    session_id: String,
    handle: tokio::task::JoinHandle<()>,
}

impl QueryTasks {
    pub(super) fn contains(&self, key: &str) -> bool {
        self.tasks.contains_key(key)
    }

    pub(super) async fn reap(&mut self) {
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

    pub(super) async fn start(
        &mut self,
        writer: Arc<Writer>,
        sessions: &Mutex<HashMap<String, Arc<Session>>>,
        id: Value,
        method: &str,
        params: Value,
    ) {
        // Deserialize both outer and nested DTO shapes and capture an attached
        // session BEFORE spawning. Semantic budgets remain neutral-core owned.
        let (session_id, query) = match parse(method, params) {
            Ok(request) => request,
            Err(()) => {
                writer
                    .respond_error(&id, -32602, "invalid child query params")
                    .await;
                return;
            }
        };
        let session = sessions.lock().await.get(&session_id).cloned();
        let Some(session) = session else {
            writer.respond_error(&id, -32602, "unknown sessionId").await;
            return;
        };
        if self.tasks.len() >= 64 {
            writer
                .respond_error(&id, -32001, "child query task limit exceeded")
                .await;
            return;
        }
        let key = super::request_key(&id);
        let handle = tokio::spawn(async move {
            // No native abort, run lease, replay, or new child for these reads.
            match query {
                Query::Children(q) => {
                    reply(&writer, &id, session.tool_program_children(q).await).await
                }
                Query::Phases(q) => reply(&writer, &id, session.tool_program_phases(q).await).await,
                Query::Body(q) => reply(&writer, &id, session.tool_program_body(q).await).await,
                Query::Snapshot(q) => {
                    reply(&writer, &id, session.tool_program_snapshot(q).await).await
                }
            }
        });
        self.tasks.insert(key, QueryTask { session_id, handle });
    }

    pub(super) async fn cancel(&mut self, key: &str) -> bool {
        if let Some(task) = self.tasks.remove(key) {
            task.handle.abort();
            let _ = task.handle.await;
            true
        } else {
            false
        }
    }

    pub(super) async fn cancel_session(&mut self, session_id: &str) {
        let keys: Vec<_> = self
            .tasks
            .iter()
            .filter(|(_, task)| task.session_id == session_id)
            .map(|(key, _)| key.clone())
            .collect();
        // Signal every task before joining, including writes waiting on output.
        for key in &keys {
            self.tasks[key].handle.abort();
        }
        for key in keys {
            self.cancel(&key).await;
        }
    }

    pub(super) async fn shutdown(&mut self) {
        for task in self.tasks.values() {
            task.handle.abort();
        }
        for (_, task) in self.tasks.drain() {
            let _ = task.handle.await;
        }
    }
}

impl Drop for QueryTasks {
    fn drop(&mut self) {
        // Safety net if the connection future itself is dropped. Normal EOF and
        // host shutdown explicitly join every task through shutdown().
        for task in self.tasks.values() {
            task.handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn finishing_response_retains_slot_and_id_until_joined_or_reaped() {
        use super::super::protocol_configuration_tests::ObservedPending;
        use tokio::io::{AsyncBufReadExt, BufReader};
        let (out, mut peer) = tokio::io::duplex(8);
        let pending = Arc::new(tokio::sync::Notify::new());
        let slow = Arc::new(Writer::with_output(ObservedPending {
            output: out,
            pending: pending.clone(),
        }));
        let mut tasks = QueryTasks::default();
        let handle = tokio::spawn(async move {
            slow.respond(&json!(1), json!({"large":"finishing response"}))
                .await;
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), pending.notified())
            .await
            .unwrap();
        tasks.tasks.insert(
            super::super::request_key(&json!(1)),
            QueryTask {
                session_id: "session".into(),
                handle,
            },
        );
        for id in 2..=64 {
            tasks.tasks.insert(
                id.to_string(),
                QueryTask {
                    session_id: "session".into(),
                    handle: tokio::spawn(std::future::pending()),
                },
            );
        }
        tasks.reap().await;
        assert_eq!(tasks.tasks.len(), 64);
        assert!(tasks.contains(&super::super::request_key(&json!(1))));
        let (native, _native_peer) = tokio::io::duplex(1024);
        let (host, _events) = rpi_extcore::host::HostSession::test_connection_with_ready(
            native,
            64,
            json!({"sessionId":"session"}),
        );
        let session = Session::attach_for_test(host).await.unwrap();
        let sessions = Mutex::new(HashMap::from([("session".into(), session.clone())]));
        let (out, fast_peer) = tokio::io::duplex(4096);
        let fast = Arc::new(Writer::with_output(out));
        let params = json!({"sessionId":"session","query":{"direction":"newestFirst","limit":1,"maxBytes":32768}});
        tasks
            .start(fast, &sessions, json!(65), METHODS[0], params)
            .await;
        let line = BufReader::new(fast_peer)
            .lines()
            .next_line()
            .await
            .unwrap()
            .unwrap();
        let frame: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(frame["error"]["code"], -32001);
        assert_eq!(tasks.tasks.len(), 64);
        // Complete the observed partial write, then reap its handle and ID.
        use tokio::io::AsyncReadExt;
        let mut bytes = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            peer.read_to_end(&mut bytes),
        )
        .await
        .unwrap()
        .unwrap();
        // Output EOF means the writer task released its writer; join completion
        // may still be waiting for one executor turn.
        tokio::task::yield_now().await;
        tasks.reap().await;
        assert_eq!(tasks.tasks.len(), 63);
        assert!(!tasks.contains(&super::super::request_key(&json!(1))));
        tasks.shutdown().await;
        assert!(tasks.tasks.is_empty());
        session.kill().await;
    }

    #[test]
    fn core_error_categories_and_untrusted_values_are_bounded_and_sanitized() {
        for kind in [ReplyKind::InvalidInput, ReplyKind::NotFound] {
            assert_eq!(map_reply_kind(kind).0, -32602);
        }
        for kind in [
            ReplyKind::Unsupported,
            ReplyKind::Conflict,
            ReplyKind::Timeout,
            ReplyKind::HostUnavailable,
            ReplyKind::AccessDenied,
            ReplyKind::LimitExceeded,
            ReplyKind::UnsupportedContent,
            ReplyKind::Internal,
            ReplyKind::Unavailable,
            ReplyKind::Empty,
            ReplyKind::Success,
            ReplyKind::Created,
        ] {
            let value = error_value((
                kind,
                json!({"error":"SECRET C:/private/debug\n",
                "code":"SECRET", "debug":{"path":"PRIVATE"}}),
            ));
            assert_eq!(value["code"], -32001);
            assert_eq!(value["message"], "read-only child query failed");
            assert!(value["data"]["code"].as_str().unwrap().len() <= 32);
            assert!(!value.to_string().contains("SECRET"));
            assert!(value.to_string().len() < 200);
        }
        assert_eq!(
            error_value((
                ReplyKind::Conflict,
                json!({"code":"child_snapshot_resync_required",
            "error":"child snapshot changed or exceeded its replay budget; reload"})
            ))["data"]["code"],
            "child_snapshot_resync_required"
        );
    }
}
