//! Session-owned client text I/O. No filesystem, MCP, or adapter transport code.
//! Handlers must not retain HostSession. Use a transport writer/Weak owner, and
//! cancel/remove its pending client request on future drop (RAII).
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    future::Future,
    io::{self, Write},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

pub const MAX_PENDING: usize = 64;
pub const MAX_TEXT_BYTES: usize = 262_144;
pub const MAX_PAYLOAD_BYTES: usize = 786_432;
pub const MAX_ERROR_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileIOCapabilities {
    pub read_text: bool,
    pub write_text: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileIOReadRange {
    Complete,
    Lines { offset_1based: u64, limit: u64 },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileIOOperation {
    ReadText {
        absolute_path: String,
        range: FileIOReadRange,
        max_bytes: usize,
    },
    WriteText {
        absolute_path: String,
        content: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileIORequest {
    pub request_id: String,
    pub operation: FileIOOperation,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileIOResponse {
    Text { content: String },
    Written,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileIOErrorCode {
    Unsupported,
    Denied,
    NotFound,
    InvalidResponse,
    TooLarge,
    Timeout,
    Cancelled,
    Disconnected,
    InvalidPath,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIOError {
    pub code: FileIOErrorCode,
    pub message: String,
}
impl FileIOError {
    pub fn new(code: FileIOErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Sticky neutral cancellation signal; no dependency on an adapter or MCP ABI.
#[derive(Clone, Debug)]
pub struct FileIOCancellation(watch::Sender<bool>);
impl Default for FileIOCancellation {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}
impl FileIOCancellation {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    pub async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        let _ = receiver.wait_for(|v| *v).await;
    }
}
/// Object-safe async handler. The returned future is dropped on cancellation or
/// the 50-second core deadline. Transport pending-request guards must be RAII.
pub trait FileIOHandler: Send + Sync + 'static {
    fn request(
        &self,
        request: FileIORequest,
        cancel: FileIOCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<FileIOResponse, FileIOError>> + Send + '_>>;
}

pub(crate) fn supports_profile(value: &Value) -> bool {
    [
        ("version", 1),
        ("maxPending", MAX_PENDING as u64),
        ("maxTextBytes", MAX_TEXT_BYTES as u64),
        ("maxPayloadBytes", MAX_PAYLOAD_BYTES as u64),
        ("timeoutMs", 60_000),
    ]
    .iter()
    .all(|(key, n)| value[*key].as_u64() == Some(*n))
}

pub(crate) fn fits_payload(value: &Value) -> bool {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|n| *n <= MAX_PAYLOAD_BYTES)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "file I/O payload limit")
                })?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(0), value).is_ok()
}
fn absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    !path.contains('\0')
        && !path.is_empty()
        && (path.starts_with('/')
            || (bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'/' | b'\\'))
            || (path.starts_with("\\\\")
                && path[2..].split('\\').filter(|s| !s.is_empty()).count() >= 2))
}
fn parse_request(value: &Value, capabilities: FileIOCapabilities) -> Result<FileIORequest, ()> {
    let object = value.as_object().ok_or(())?;
    let id = value["requestId"].as_str().ok_or(())?;
    let uuid = id
        .strip_prefix("native:")
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .ok_or(())?;
    if id != format!("native:{uuid}") || !fits_payload(value) {
        return Err(());
    }
    let path = value["path"]
        .as_str()
        .filter(|path| absolute(path))
        .ok_or(())?;
    let operation = match value["operation"].as_str() {
        Some("read_text_file") if capabilities.read_text => {
            let max_bytes = value["maxBytes"]
                .as_u64()
                .filter(|n| *n <= MAX_TEXT_BYTES as u64)
                .ok_or(())? as usize;
            let range = match (value.get("line"), value.get("limit")) {
                (None, None) if object.len() == 5 => FileIOReadRange::Complete,
                (Some(line), Some(limit)) if object.len() == 7 => FileIOReadRange::Lines {
                    offset_1based: line.as_u64().filter(|n| *n > 0).ok_or(())?,
                    limit: limit.as_u64().filter(|n| *n > 0).ok_or(())?,
                },
                _ => return Err(()),
            };
            FileIOOperation::ReadText {
                absolute_path: path.into(),
                range,
                max_bytes,
            }
        }
        Some("write_text_file") if capabilities.write_text && object.len() == 5 => {
            let content = value["content"]
                .as_str()
                .filter(|s| s.len() <= MAX_TEXT_BYTES)
                .ok_or(())?;
            FileIOOperation::WriteText {
                absolute_path: path.into(),
                content: content.into(),
            }
        }
        _ => return Err(()),
    };
    Ok(FileIORequest {
        request_id: id.into(),
        operation,
    })
}
fn frame(request: &FileIORequest, result: Result<FileIOResponse, FileIOError>) -> Value {
    let mut frame = json!({"type":"file_io_response","requestId":request.request_id});
    let result = match (result, &request.operation) {
        (Ok(FileIOResponse::Text { content }), FileIOOperation::ReadText { max_bytes, .. })
            if content.len() <= *max_bytes =>
        {
            Ok(json!({"content":content}))
        }
        (Ok(FileIOResponse::Written), FileIOOperation::WriteText { .. }) => Ok(json!({})),
        (Err(error), _) if error.message.len() <= MAX_ERROR_BYTES => Err(error),
        _ => Err(FileIOError::new(
            FileIOErrorCode::InvalidResponse,
            "invalid client file I/O response",
        )),
    };
    match result {
        Ok(result) => frame["result"] = result,
        Err(error) => frame["error"] = json!(error),
    }
    if !fits_payload(&frame) {
        frame.as_object_mut().unwrap().remove("result");
        frame["error"] = json!(FileIOError::new(
            FileIOErrorCode::TooLarge,
            "client file I/O exceeds payload limit"
        ));
    }
    frame
}

struct Call {
    cancel: FileIOCancellation,
    exited: bool,
    closed: bool,
}
struct State {
    stopped: bool,
    calls: HashMap<String, Call>,
}
struct Invocation {
    request: FileIORequest,
    cancel: FileIOCancellation,
    deadline: tokio::time::Instant,
}
/// Strong session owner, weak native routing. Supervisor retains workers but
/// never this owner or HostSession. Admission persists through worker exit AND
/// native closure, including replies resolved but not yet consumed by native.
pub(crate) struct FileIOService {
    capabilities: FileIOCapabilities,
    handler: Arc<dyn FileIOHandler>,
    state: Arc<Mutex<State>>,
    stop: FileIOCancellation,
    requests: mpsc::Sender<Invocation>,
    join: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}
impl FileIOService {
    pub fn start(
        capabilities: FileIOCapabilities,
        handler: Arc<dyn FileIOHandler>,
        commands: crate::host::FileIOCommands,
    ) -> Arc<Self> {
        let state = Arc::new(Mutex::new(State {
            stopped: false,
            calls: HashMap::new(),
        }));
        let stop = FileIOCancellation::default();
        let (requests, mut receive) = mpsc::channel::<Invocation>(MAX_PENDING);
        let task_state = state.clone();
        let task_stop = stop.clone();
        let task_handler = handler.clone();
        let join = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = task_stop.cancelled() => break,
                    Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                        match result {
                            Ok(id) => {
                                let mut state = task_state.lock().expect("file I/O state");
                                if let Some(call) = state.calls.get_mut(&id) {
                                    call.exited = true;
                                    if call.closed { state.calls.remove(&id); }
                                }
                            }
                            Err(_) => { commands.fail_closed(); break; }
                        }
                    },
                    invocation = receive.recv(), if tasks.len() < MAX_PENDING => {
                        let Some(invocation) = invocation else { break };
                        let handler = task_handler.clone();
                        let commands = commands.clone();
                        tasks.spawn(async move {
                            let Invocation { request, cancel, deadline } = invocation;
                            let id = request.request_id.clone();
                            let _publication = CancelOnDrop(cancel.clone());
                            async {
                                let handler_cancel = FileIOCancellation::default();
                                let result = {
                                    let _handler_cancel = CancelOnDrop(handler_cancel.clone());
                                    tokio::select! {
                                        biased;
                                        _ = cancel.cancelled() => return,
                                        result = tokio::time::timeout_at(deadline, handler.request(request.clone(), handler_cancel)) => match result {
                                            Ok(result) => result,
                                            Err(_) => Err(FileIOError::new(FileIOErrorCode::Timeout, "client file I/O timed out; a dispatched write may have committed")),
                                        },
                                    }
                                };
                                commands.respond(frame(&request, result), cancel.clone()).await;
                            }.await;
                            id
                        });
                    }
                }
            }
            receive.close();
            {
                let mut state = task_state.lock().expect("file I/O state");
                state.stopped = true;
                for call in state.calls.values() {
                    call.cancel.cancel();
                }
            }
            while let Some(invocation) = receive.recv().await {
                invocation.cancel.cancel();
            }
            while tasks.join_next().await.is_some() {}
            task_state.lock().expect("file I/O state").calls.clear();
        });
        Arc::new(Self {
            capabilities,
            handler,
            state,
            stop,
            requests,
            join: tokio::sync::Mutex::new(Some(join)),
        })
    }
    pub fn matches(
        &self,
        capabilities: FileIOCapabilities,
        handler: &Arc<dyn FileIOHandler>,
    ) -> bool {
        self.capabilities == capabilities && Arc::ptr_eq(&self.handler, handler)
    }
    pub fn route(&self, value: &Value) -> Result<(), ()> {
        match value["type"].as_str() {
            Some("file_io_request") => {
                let request = parse_request(value, self.capabilities)?;
                let mut state = self.state.lock().expect("file I/O state");
                if state.stopped
                    || state.calls.len() >= MAX_PENDING
                    || state.calls.contains_key(&request.request_id)
                {
                    return Err(());
                }
                let cancel = FileIOCancellation::default();
                state.calls.insert(
                    request.request_id.clone(),
                    Call {
                        cancel: cancel.clone(),
                        exited: false,
                        closed: false,
                    },
                );
                self.requests
                    .try_send(Invocation {
                        request,
                        cancel,
                        deadline: tokio::time::Instant::now() + Duration::from_secs(50),
                    })
                    .map_err(|_| ())?;
            }
            Some("file_io_request_closed") => {
                let id = value["requestId"].as_str().ok_or(())?;
                let mut state = self.state.lock().expect("file I/O state");
                if let Some(call) = state.calls.get_mut(id) {
                    call.closed = true;
                    call.cancel.cancel();
                    if call.exited {
                        state.calls.remove(id);
                    }
                }
            }
            Some("agent_settled") => self.cancel_calls(),
            _ => {}
        }
        Ok(())
    }
    pub fn cancel_calls(&self) {
        for call in self.state.lock().expect("file I/O state").calls.values() {
            call.cancel.cancel();
        }
    }
    pub fn close(&self) {
        self.state.lock().expect("file I/O state").stopped = true;
        self.cancel_calls();
        self.stop.cancel();
    }
    pub async fn join(&self) {
        let mut join = self.join.lock().await;
        if let Some(task) = join.as_mut() {
            let _ = task.await;
        }
        join.take();
    }
}
impl Drop for FileIOService {
    fn drop(&mut self) {
        self.close();
    }
}
struct CancelOnDrop(FileIOCancellation);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
#[cfg(test)]
#[path = "file_io_tests.rs"]
mod tests;
