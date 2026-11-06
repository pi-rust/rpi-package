//! Authenticated JSON-text WS adapter. Owner work stays on the existing `LocalSet`.
use crate::{
    ServerState,
    executor::{self, RealtimeAction, RealtimeReply},
    security::AuthLease,
};
use axum::{
    Json,
    extract::{
        Extension, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use rpi_node_api::api::realtime::{
    self as protocol, Capabilities, ClientFrame, Discovery, Hello, MAX_MESSAGE_BYTES, OperationRef,
    RealtimeError, RealtimeErrorCode as Code, ServerFrame, VERSION, WireEvent,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch},
    task::{AbortHandle, JoinSet},
    time::{Instant, MissedTickBehavior},
};

const MAX_CONNECTIONS: u32 = 16;
const MAX_JOBS: usize = 16;
const MAX_SUBSCRIPTIONS: usize = 16;
// Includes heartbeat request IDs. Rotate rather than grow tombstones forever.
const MAX_IDS: usize = 4096;
const OUTBOX_ITEMS: usize = 256;
const OUTBOX_BYTES: usize = 4 * 1024 * 1024;
const NODE_OUTBOX_BYTES: usize = 32 * 1024 * 1024;
const HEARTBEAT: Duration = Duration::from_secs(15);
const IDLE_TIMEOUT: Duration = Duration::from_secs(45);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
type CloseReason = (u16, &'static str);

pub(crate) struct RealtimeState {
    connections: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}
impl RealtimeState {
    pub(crate) fn new() -> Self {
        Self {
            connections: Arc::new(Semaphore::new(MAX_CONNECTIONS as usize)),
            bytes: Arc::new(Semaphore::new(NODE_OUTBOX_BYTES)),
        }
    }
    pub(crate) async fn wait_closed(&self) -> Result<(), &'static str> {
        let _all = self
            .connections
            .clone()
            .acquire_many_owned(MAX_CONNECTIONS)
            .await
            .map_err(|_| "WS limiter closed")?;
        Ok(())
    }
}

pub(crate) async fn discovery(
    State(state): State<Arc<ServerState>>,
    Extension(_auth): Extension<AuthLease>,
) -> Response {
    let discovery = Discovery::new(
        state.node_id.clone(),
        state.workspace.remote_operations().owner_epoch().to_owned(),
    );
    if discovery.validate().is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(discovery).into_response()
}

pub(crate) async fn upgrade(
    State(state): State<Arc<ServerState>>,
    Extension(auth): Extension<AuthLease>,
    headers: HeaderMap,
    uri: Uri,
    ws: WebSocketUpgrade,
) -> Response {
    // No query tickets, cookies or alternate authentication channel. The ordinary
    // Host/Origin/Cookie middleware must succeed before reaching this handler.
    if uri.query().is_some() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let supported = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.split(',').any(|s| s.trim() == protocol::SUBPROTOCOL));
    if !supported {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Ok(permit) = state.realtime.connections.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let Ok(connection_id) = crate::random_secret() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let hello = Hello {
        version: VERSION,
        node_id: state.node_id.clone(),
        owner_epoch: state.workspace.remote_operations().owner_epoch().to_owned(),
        connection_id,
        capabilities: Capabilities::default(),
    };
    if hello.validate().is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    ws.protocols([protocol::SUBPROTOCOL])
        .max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_MESSAGE_BYTES + 64 * 1024)
        .on_upgrade(move |socket| connection(socket, state, auth, hello, permit))
}

struct Queued {
    message: Message,
    _local: OwnedSemaphorePermit,
    _node: OwnedSemaphorePermit,
}
#[derive(Clone)]
struct Outbox {
    tx: mpsc::Sender<Queued>,
    bytes: Arc<Semaphore>,
    node: Arc<Semaphore>,
    close: watch::Sender<Option<CloseReason>>,
}
impl Outbox {
    fn close(&self, reason: CloseReason) {
        self.close.send_if_modified(|current| {
            if current.is_some() {
                false
            } else {
                *current = Some(reason);
                true
            }
        });
    }
    fn frame(&self, frame: &ServerFrame) -> bool {
        let Ok(text) = frame.encode_text() else {
            self.close((1013, "resync required"));
            return false;
        };
        self.message(Message::Text(text.into()))
    }
    fn message(&self, message: Message) -> bool {
        if self.close.borrow().is_some() {
            return false;
        }
        let len = match &message {
            Message::Text(text) => text.len(),
            Message::Binary(bytes) | Message::Ping(bytes) | Message::Pong(bytes) => bytes.len(),
            Message::Close(frame) => frame.as_ref().map_or(0, |f| f.reason.len() + 2),
        }
        .max(1);
        let Ok(len) = u32::try_from(len) else {
            self.close((1013, "resync required"));
            return false;
        };
        let local = self.bytes.clone().try_acquire_many_owned(len);
        let node = self.node.clone().try_acquire_many_owned(len);
        let (Ok(local), Ok(node)) = (local, node) else {
            self.close((1013, "resync required"));
            return false;
        };
        if self
            .tx
            .try_send(Queued {
                message,
                _local: local,
                _node: node,
            })
            .is_err()
        {
            self.close((1013, "resync required"));
            return false;
        }
        true
    }
    fn error(&self, request_id: String, error: RealtimeError) {
        self.frame(&ServerFrame::Error {
            version: VERSION,
            request_id: Some(request_id),
            error,
        });
    }
}
struct AbortOnDrop(AbortHandle);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// No connection task owns an owner future. Aborting these jobs merely drops a
// reply receiver; the granted executor work and receipt outlive the socket.
enum ReplyContext {
    Reserve,
    Query,
    Status(OperationRef),
    Subscribe {
        session_id: String,
        subscription_id: String,
    },
}
struct Completed {
    request_id: String,
    context: ReplyContext,
    result: Result<RealtimeReply, RealtimeError>,
}
fn request_id(frame: &ClientFrame) -> &str {
    match frame {
        ClientFrame::Reserve { request_id, .. }
        | ClientFrame::Execute { request_id, .. }
        | ClientFrame::Status { request_id, .. }
        | ClientFrame::Query { request_id, .. }
        | ClientFrame::Subscribe { request_id, .. }
        | ClientFrame::Unsubscribe { request_id, .. }
        | ClientFrame::Ping { request_id, .. } => request_id,
    }
}

#[allow(clippy::too_many_lines)] // Keep connection admission, cancellation and cleanup together.
async fn connection(
    socket: WebSocket,
    state: Arc<ServerState>,
    auth: AuthLease,
    hello: Hello,
    _connection: OwnedSemaphorePermit,
) {
    let (sink, mut input) = socket.split();
    let (tx, rx) = mpsc::channel(OUTBOX_ITEMS);
    let (close, mut closed) = watch::channel(None);
    let out = Outbox {
        tx,
        bytes: Arc::new(Semaphore::new(OUTBOX_BYTES)),
        node: state.realtime.bytes.clone(),
        close,
    };
    let mut writer = tokio::spawn(write_loop(
        sink,
        rx,
        out.close.subscribe(),
        state.clone(),
        auth.clone(),
    ));
    let _writer_guard = AbortOnDrop(writer.abort_handle());
    let mut writer_finished = false;
    let mut jobs = JoinSet::<Completed>::new();
    let mut sources = JoinSet::<String>::new();
    let mut subscriptions = HashMap::<String, Option<AbortHandle>>::new();
    let mut requests_seen = HashSet::new();
    let mut subscriptions_seen = HashSet::new();
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_seen = Instant::now();
    let mut window = Instant::now();
    let mut window_messages = 0usize;
    out.frame(&ServerFrame::Hello {
        version: VERSION,
        hello,
    });
    let reason = loop {
        tokio::select! {
            biased;
            () = auth.invalidated(&state.sharing, &state.shutdown) => break (1008, "authorization ended"),
            _ = closed.changed() => break (*closed.borrow()).unwrap_or((1001, "connection closed")),
            _ = &mut writer => { writer_finished = true; break (1001, "writer stopped"); },
            Some(done) = jobs.join_next(), if !jobs.is_empty() => {
                if let Ok(done) = done {
                    complete(done, &out, &mut subscriptions, &mut sources);
                } else { break (1011, "request task failed"); }
            },
            Some(done) = sources.join_next(), if !sources.is_empty() => {
                match done {
                    Ok(id) => { subscriptions.remove(&id); },
                    Err(error) if error.is_cancelled() => {},
                    Err(_) => break (1011, "event source failed; resync required"),
                }
            },
            _ = heartbeat.tick() => {
                if last_seen.elapsed() >= IDLE_TIMEOUT { break (1001, "heartbeat timeout"); }
                out.message(Message::Ping(Vec::new().into()));
            },
            message = input.next() => {
                let Some(Ok(message)) = message else { break (1001, "connection closed"); };
                last_seen = Instant::now();
                // Includes protocol pings/pongs: every incoming frame has a bounded rate.
                if window.elapsed() >= Duration::from_secs(10) { window = Instant::now(); window_messages = 0; }
                window_messages += 1;
                if window_messages > 120 { break (1008, "request rate exceeded"); }
                let text = match message {
                    Message::Text(text) => text,
                    Message::Ping(bytes) => { out.message(Message::Pong(bytes)); continue; },
                    Message::Pong(_) => continue,
                    Message::Close(_) => break (1000, "closed"),
                    Message::Binary(_) => break (1003, "JSON text required"),
                };
                let frame = match ClientFrame::decode_text(text.as_str()) {
                    Ok(frame) => frame,
                    Err(error) => break (if error.code == Code::LimitExceeded {1009} else {1008}, "invalid realtime message"),
                };
                if requests_seen.len() >= MAX_IDS { break (1012, "rotate connection; resync required"); }
                if !requests_seen.insert(request_id(&frame).to_owned()) { break (1008, "request ID reused"); }
                match frame {
                    ClientFrame::Ping {request_id, ..} => { out.frame(&ServerFrame::Pong {version: VERSION, request_id}); },
                    ClientFrame::Unsubscribe {request_id, subscription_id, ..} => {
                        if let Some(Some(handle)) = subscriptions.remove(&subscription_id) { handle.abort(); }
                        out.frame(&ServerFrame::Unsubscribed {version: VERSION, request_id, subscription_id});
                    },
                    frame => {
                        let id = request_id(&frame).to_owned();
                        if jobs.len() >= MAX_JOBS { out.error(id, executor::error(Code::LimitExceeded)); continue; }
                        let (action, context) = match frame {
                            ClientFrame::Reserve {session_id, client_instance_id, kind, ..} => (RealtimeAction::Reserve {session_id, client_instance_id, kind}, ReplyContext::Reserve),
                            ClientFrame::Execute {operation, payload, ..} => (RealtimeAction::Execute {operation: operation.clone(), payload}, ReplyContext::Status(operation)),
                            ClientFrame::Status {operation, ..} => (RealtimeAction::Status {operation: operation.clone()}, ReplyContext::Status(operation)),
                            ClientFrame::Query {session_id, payload, ..} => (RealtimeAction::Query {session_id, payload}, ReplyContext::Query),
                            ClientFrame::Subscribe {session_id, subscription_id, after_seq, ..} => {
                                if subscriptions_seen.contains(&subscription_id) { out.error(id, executor::error(Code::Conflict)); continue; }
                                if subscriptions.len() >= MAX_SUBSCRIPTIONS { out.error(id, executor::error(Code::LimitExceeded)); continue; }
                                subscriptions_seen.insert(subscription_id.clone());
                                subscriptions.insert(subscription_id.clone(), None);
                                (RealtimeAction::Subscribe {session_id: session_id.clone(), after: after_seq.unwrap_or(0)}, ReplyContext::Subscribe {session_id, subscription_id})
                            },
                            _ => unreachable!("control frames handled above"),
                        };
                        let state = state.clone();
                        let auth = auth.clone();
                        jobs.spawn(async move {
                            let result = executor::request(state, auth, action).await;
                            Completed { request_id: id, context, result }
                        });
                    }
                }
            }
        }
    };
    out.close(reason);
    jobs.abort_all();
    sources.abort_all();
    while jobs.join_next().await.is_some() {}
    while sources.join_next().await.is_some() {}
    if !writer_finished
        && tokio::time::timeout(Duration::from_millis(250), &mut writer)
            .await
            .is_err()
    {
        writer.abort();
        let _ = writer.await;
    }
    // The connection permit drops only after all network futures and buffered
    // byte permits have been released. Owner execution is deliberately separate.
}

fn complete(
    done: Completed,
    out: &Outbox,
    subscriptions: &mut HashMap<String, Option<AbortHandle>>,
    sources: &mut JoinSet<String>,
) {
    let Completed {
        request_id,
        context,
        result,
    } = done;
    if let ReplyContext::Subscribe {
        subscription_id, ..
    } = &context
    {
        if !matches!(&result, Ok(RealtimeReply::Subscribed(_))) {
            subscriptions.remove(subscription_id);
        }
    }
    let reply = match result {
        Ok(reply) => reply,
        Err(error) => {
            out.error(request_id, error);
            return;
        }
    };
    match (context, reply) {
        (ReplyContext::Query, RealtimeReply::Result(value)) => {
            out.frame(&ServerFrame::Result {
                version: VERSION,
                request_id,
                value,
            });
        }
        (ReplyContext::Reserve, RealtimeReply::Reserved(operation)) => {
            out.frame(&ServerFrame::Reserved {
                version: VERSION,
                request_id,
                operation,
            });
        }
        (ReplyContext::Status(operation), RealtimeReply::Status(status)) => {
            out.frame(&ServerFrame::Status {
                version: VERSION,
                request_id,
                operation,
                status,
            });
        }
        (
            ReplyContext::Subscribe {
                session_id,
                subscription_id,
            },
            RealtimeReply::Subscribed(mut stream),
        ) => {
            // Unsubscribe may have overtaken the asynchronous subscribe result.
            if !subscriptions.contains_key(&subscription_id) {
                out.error(request_id, executor::error(Code::Conflict));
                return;
            }
            if !out.frame(&ServerFrame::Subscribed {
                version: VERSION,
                request_id,
                session_id: session_id.clone(),
                subscription_id: subscription_id.clone(),
            }) {
                return;
            }
            let out = out.clone();
            let key = subscription_id.clone();
            let map_key = subscription_id.clone();
            let handle = sources.spawn(async move {
                while let Some(item) = stream.next().await {
                    let Ok(event) = item else {
                        resync(&out, &session_id, &subscription_id);
                        return key;
                    };
                    if !out.frame(&ServerFrame::Events {
                        version: VERSION,
                        session_id: session_id.clone(),
                        subscription_id: subscription_id.clone(),
                        events: vec![event],
                    }) {
                        return key;
                    }
                }
                resync(&out, &session_id, &subscription_id);
                key
            });
            subscriptions.insert(map_key, Some(handle));
        }
        _ => out.error(request_id, executor::error(Code::Internal)),
    }
}

fn resync(out: &Outbox, session_id: &str, subscription_id: &str) {
    out.frame(&ServerFrame::Events {
        version: VERSION,
        session_id: session_id.into(),
        subscription_id: subscription_id.into(),
        events: vec![WireEvent {
            id: None,
            seq: None,
            event_type: "resync_required".into(),
            data: serde_json::json!({"type":"resync_required"}),
        }],
    });
}

async fn write_loop(
    mut sink: futures_util::stream::SplitSink<WebSocket, Message>,
    mut rx: mpsc::Receiver<Queued>,
    mut close: watch::Receiver<Option<CloseReason>>,
    state: Arc<ServerState>,
    auth: AuthLease,
) {
    let reason = loop {
        if let Some(reason) = *close.borrow() {
            break reason;
        }
        tokio::select! {
            biased;
            () = auth.invalidated(&state.sharing, &state.shutdown) => break (1008, "authorization ended"),
            _ = close.changed() => break (*close.borrow()).unwrap_or((1001, "closed")),
            item = rx.recv() => {
                let Some(item) = item else { break (1001, "closed"); };
                if !auth.is_valid(&state.sharing) { break (1008, "authorization ended"); }
                // Keep remaining item fields (byte permits) alive during socket I/O.
                tokio::select! {
                    biased;
                    () = auth.invalidated(&state.sharing, &state.shutdown) => break (1008, "authorization ended"),
                    _ = close.changed() => break (*close.borrow()).unwrap_or((1001, "closed")),
                    sent = tokio::time::timeout(WRITE_TIMEOUT, sink.send(item.message)) => {
                        if !matches!(sent, Ok(Ok(()))) { break (1013, "resync required"); }
                    }
                }
            }
        }
    };
    // Congested peers may not receive the close frame. EOF is equally a lost
    // connection requiring cursor/snapshot reconciliation, never an empty delta.
    let _ = tokio::time::timeout(
        Duration::from_millis(100),
        sink.send(Message::Close(Some(CloseFrame {
            code: reason.0,
            reason: reason.1.into(),
        }))),
    )
    .await;
}

#[cfg(test)]
mod tests;
