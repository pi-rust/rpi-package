use super::*;
use rpi_node_core::workspace::{WorkspaceError, WorkspaceEventStream};
use std::{
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll},
};

fn outbox(node: Arc<Semaphore>) -> (Outbox, mpsc::Receiver<Queued>) {
    let (tx, rx) = mpsc::channel(OUTBOX_ITEMS);
    let (close, _) = watch::channel(None);
    (
        Outbox {
            tx,
            bytes: Arc::new(Semaphore::new(OUTBOX_BYTES)),
            node,
            close,
        },
        rx,
    )
}

#[test]
fn entry_budget_closes_without_waiting_for_consumer() {
    let node = Arc::new(Semaphore::new(NODE_OUTBOX_BYTES));
    let (out, rx) = outbox(node.clone());
    for _ in 0..OUTBOX_ITEMS {
        assert!(out.message(Message::Ping(Vec::new().into())));
    }
    assert!(!out.message(Message::Ping(Vec::new().into())));
    assert_eq!(*out.close.borrow(), Some((1013, "resync required")));
    assert_eq!(out.bytes.available_permits(), OUTBOX_BYTES - OUTBOX_ITEMS);
    assert_eq!(node.available_permits(), NODE_OUTBOX_BYTES - OUTBOX_ITEMS);
    drop(rx);
    assert_eq!(out.bytes.available_permits(), OUTBOX_BYTES);
    assert_eq!(node.available_permits(), NODE_OUTBOX_BYTES);
}

#[test]
fn byte_budget_includes_queued_and_inflight_frames() {
    let node = Arc::new(Semaphore::new(NODE_OUTBOX_BYTES));
    let (out, mut rx) = outbox(node.clone());
    assert!(out.message(Message::Text("x".repeat(MAX_MESSAGE_BYTES).into())));
    {
        let item = rx.try_recv().unwrap();
        // Moving the payload into an I/O future must not release the byte charge.
        let _payload = item.message;
        assert_eq!(
            out.bytes.available_permits(),
            OUTBOX_BYTES - MAX_MESSAGE_BYTES
        );
        assert!(out.message(Message::Text("x".repeat(MAX_MESSAGE_BYTES).into())));
        assert!(!out.message(Message::Ping(Vec::new().into())));
        // The partially moved item retains both permits until this scope ends.
    }
    drop(rx);
    assert_eq!(node.available_permits(), NODE_OUTBOX_BYTES);
    assert_eq!(out.bytes.available_permits(), OUTBOX_BYTES);
}

#[test]
fn node_byte_budget_is_shared_and_failed_acquisition_rolls_back_local_charge() {
    let node = Arc::new(Semaphore::new(8));
    let (first, first_rx) = outbox(node.clone());
    let (second, second_rx) = outbox(node.clone());
    assert!(first.message(Message::Text("123456".into())));
    assert!(!second.message(Message::Text("789".into())));
    assert_eq!(second.bytes.available_permits(), OUTBOX_BYTES);
    assert_eq!(node.available_permits(), 2);
    drop(first_rx);
    drop(second_rx);
    assert_eq!(node.available_permits(), 8);
}

#[tokio::test]
async fn natural_source_end_sends_resync_after_ack_not_fake_settlement() {
    let (out, mut rx) = outbox(Arc::new(Semaphore::new(NODE_OUTBOX_BYTES)));
    let mut subscriptions = HashMap::from([("sub".into(), None)]);
    let mut sources = JoinSet::new();
    let stream: WorkspaceEventStream = Box::pin(futures_util::stream::empty());
    complete(
        Completed {
            request_id: "subscribe".into(),
            context: ReplyContext::Subscribe {
                session_id: "session".into(),
                subscription_id: "sub".into(),
            },
            result: Ok(RealtimeReply::Subscribed(stream)),
        },
        &out,
        &mut subscriptions,
        &mut sources,
    );
    let Message::Text(ack) = rx.recv().await.unwrap().message else {
        panic!("text required")
    };
    assert!(matches!(
        ServerFrame::decode_text(&ack).unwrap(),
        ServerFrame::Subscribed { .. }
    ));
    assert_eq!(sources.join_next().await.unwrap().unwrap(), "sub");
    let Message::Text(event) = rx.recv().await.unwrap().message else {
        panic!("text required")
    };
    let ServerFrame::Events { events, .. } = ServerFrame::decode_text(&event).unwrap() else {
        panic!("resync required")
    };
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "resync_required");
    assert_eq!(events[0].seq, None);
    assert_eq!(events[0].id, None);
}

struct PendingSource(Arc<AtomicUsize>);
impl futures_util::Stream for PendingSource {
    type Item = Result<WireEvent, WorkspaceError>;
    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
impl Drop for PendingSource {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn unsubscribe_abort_drops_source_and_late_subscribe_cannot_install_it() {
    let (out, mut rx) = outbox(Arc::new(Semaphore::new(NODE_OUTBOX_BYTES)));
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut subscriptions = HashMap::from([("sub".into(), None)]);
    let mut sources = JoinSet::new();
    let make = || Completed {
        request_id: "subscribe".into(),
        context: ReplyContext::Subscribe {
            session_id: "session".into(),
            subscription_id: "sub".into(),
        },
        result: Ok(RealtimeReply::Subscribed(Box::pin(PendingSource(
            dropped.clone(),
        )))),
    };
    complete(make(), &out, &mut subscriptions, &mut sources);
    let handle = subscriptions.remove("sub").unwrap().unwrap();
    handle.abort();
    assert!(
        sources
            .join_next()
            .await
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    let _ack = rx.recv().await.unwrap();
    complete(make(), &out, &mut subscriptions, &mut sources);
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
    assert!(sources.is_empty());
    let Message::Text(text) = rx.recv().await.unwrap().message else {
        panic!("text required")
    };
    assert!(
        matches!(ServerFrame::decode_text(&text).unwrap(), ServerFrame::Error {error, ..} if error.code == Code::Conflict)
    );
}

#[tokio::test]
async fn connection_drain_waits_for_last_permit() {
    let state = RealtimeState::new();
    let permit = state.connections.clone().acquire_owned().await.unwrap();
    let waiting = state.wait_closed();
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut waiting)
            .await
            .is_err()
    );
    drop(permit);
    tokio::time::timeout(Duration::from_millis(100), &mut waiting)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        state.connections.available_permits(),
        MAX_CONNECTIONS as usize
    );
}
