use crate::access::{AccessState, AuthLease};
use futures_util::{stream, Stream, StreamExt};
use std::{future::Future, pin::Pin};

/// Guard a protocol-owned event stream with a revocable authorization Lease.
///
/// The transport layer owns only lifecycle and authorization checks. Event
/// payloads, replay cursors and resync semantics remain owned by the caller.
pub fn authorized_stream<S, T, E>(
    events: S,
    access: AccessState,
    lease: AuthLease,
) -> impl Stream<Item = Result<T, E>> + Send
where
    S: Stream<Item = Result<T, E>> + Send + 'static,
    T: Send + 'static,
    E: Send + 'static,
{
    let invalidation_access = access.clone();
    let invalidation_lease = lease.clone();
    let invalidation: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
        invalidation_access
            .wait_invalidated(&invalidation_lease)
            .await;
    });
    stream::unfold(
        (Box::pin(events), invalidation),
        move |(mut events, mut invalidation)| {
            let access = access.clone();
            let lease = lease.clone();
            async move {
                tokio::select! {
                    biased;
                    _ = &mut invalidation => None,
                    item = events.next() => {
                        let item = item?;
                        if !access.while_authorized(&lease).await {
                            return None;
                        }
                        Some((item, (events, invalidation)))
                    }
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use std::time::Duration;

    #[tokio::test]
    async fn revoked_lease_stops_a_protocol_owned_stream() {
        let access = AccessState::new(true, 4);
        let lease = access
            .issue(
                crate::access::PrincipalId::new("stream").unwrap(),
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        let mut guarded = Box::pin(authorized_stream::<_, u8, ()>(
            futures_util::stream::pending(),
            access.clone(),
            lease.clone(),
        ));
        access.revoke(&lease).await;
        assert!(tokio::time::timeout(Duration::from_secs(1), guarded.next())
            .await
            .unwrap()
            .is_none());
    }
}
