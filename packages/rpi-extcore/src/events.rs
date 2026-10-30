//! Replay and live subscription ordering, independent of SSE or ACP.
use crate::host::BufferedEvent;
use futures_util::{
    stream::{self, Stream},
    StreamExt,
};
use tokio::sync::broadcast;

pub enum StreamItem {
    Event(BufferedEvent),
    ResyncRequired { dropped: u64 },
}

pub fn replay_stream(
    replay: Vec<BufferedEvent>,
    receiver: broadcast::Receiver<BufferedEvent>,
    after: Option<u64>,
) -> impl Stream<Item = StreamItem> {
    let cursor = replay
        .last()
        .map(|event| event.seq)
        .unwrap_or_default()
        .max(after.unwrap_or_default());
    stream::iter(replay.into_iter().map(StreamItem::Event)).chain(stream::unfold(
        (receiver, cursor),
        |(mut receiver, cursor)| async move {
            loop {
                match receiver.recv().await {
                    Ok(event) if event.seq <= cursor => continue,
                    Ok(event) => {
                        let cursor = event.seq;
                        return Some((StreamItem::Event(event), (receiver, cursor)));
                    }
                    Err(broadcast::error::RecvError::Lagged(dropped)) => {
                        return Some((StreamItem::ResyncRequired { dropped }, (receiver, cursor)))
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    ))
}

/// A snapshot of the replay boundary, acquired after subscribing to live events.
pub struct Subscription {
    pub ready: serde_json::Value,
    pub replay: Vec<BufferedEvent>,
    pub receiver: broadcast::Receiver<BufferedEvent>,
    pub gap: bool,
    pub after: Option<u64>,
}
impl Subscription {
    pub async fn new(session: &crate::host::HostSession, after: Option<u64>) -> Self {
        let receiver = session.subscribe();
        let (retained, latest) = session.replay_boundary().await;
        let gap =
            crate::service::replay_gap(after, retained.first().map(|event| event.seq), latest);
        let replay = retained
            .into_iter()
            .filter(|event| after.map_or(true, |after| event.seq > after))
            .collect();
        Self {
            ready: session.ready(),
            replay,
            receiver,
            gap,
            after,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(seq: u64) -> BufferedEvent {
        BufferedEvent {
            seq,
            value: serde_json::json!({"type":"message"}),
        }
    }

    #[tokio::test]
    async fn replay_live_overlap_is_delivered_once_in_sequence() {
        let (sender, receiver) = broadcast::channel(16);
        for seq in 2..=5 {
            sender.send(event(seq)).unwrap();
        }
        drop(sender);
        let stream = replay_stream(vec![event(2), event(3), event(4)], receiver, Some(1));
        tokio::pin!(stream);
        for expected in 2..=5 {
            match stream.next().await.unwrap() {
                StreamItem::Event(event) => assert_eq!(event.seq, expected),
                StreamItem::ResyncRequired { .. } => panic!("unexpected lag"),
            }
        }
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn empty_replay_preserves_after_cursor_for_live_deduplication() {
        let (sender, receiver) = broadcast::channel(16);
        for seq in 1..=4 {
            sender.send(event(seq)).unwrap();
        }
        drop(sender);
        let stream = replay_stream(vec![], receiver, Some(3));
        tokio::pin!(stream);
        assert!(matches!(stream.next().await, Some(StreamItem::Event(event)) if event.seq == 4));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn lag_after_replay_is_explicit_and_overlap_is_still_deduplicated() {
        let (sender, receiver) = broadcast::channel(2);
        for seq in 1..=4 {
            sender.send(event(seq)).unwrap();
        }
        drop(sender);
        let stream = replay_stream(vec![event(1), event(2), event(3)], receiver, None);
        tokio::pin!(stream);
        for expected in 1..=3 {
            assert!(
                matches!(stream.next().await, Some(StreamItem::Event(event)) if event.seq == expected)
            );
        }
        assert!(matches!(
            stream.next().await,
            Some(StreamItem::ResyncRequired { dropped: 2 })
        ));
        assert!(matches!(stream.next().await, Some(StreamItem::Event(event)) if event.seq == 4));
        assert!(stream.next().await.is_none());
    }
}
