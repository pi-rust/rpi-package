use super::{ConnectionCloseReason, ConnectionError, ConnectionPolicy};
use crate::access::{AccessState, AuthLease};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{mpsc, Mutex, Notify, OwnedSemaphorePermit, Semaphore};

struct Inner {
    access: AccessState,
    policy: ConnectionPolicy,
    slots: Arc<Semaphore>,
    accepting: AtomicBool,
    gate: Mutex<()>,
    shutdown: Notify,
}

/// Shared lifecycle state for a set of protocol connections.
#[derive(Clone)]
pub struct ConnectionRegistry {
    inner: Arc<Inner>,
}

/// A connection slot bound to one authorization Lease generation.
pub struct ConnectionPermit {
    inner: Arc<Inner>,
    lease: AuthLease,
    _slot: OwnedSemaphorePermit,
}

impl ConnectionRegistry {
    pub fn new(access: AccessState, policy: ConnectionPolicy) -> Self {
        Self {
            inner: Arc::new(Inner {
                access,
                slots: Arc::new(Semaphore::new(policy.max_connections)),
                policy,
                accepting: AtomicBool::new(true),
                gate: Mutex::new(()),
                shutdown: Notify::new(),
            }),
        }
    }

    pub fn policy(&self) -> ConnectionPolicy {
        self.inner.policy
    }

    pub fn is_accepting(&self) -> bool {
        self.inner.accepting.load(Ordering::Acquire)
    }

    /// Admit one connection after checking its Lease and generation.
    pub async fn accept(&self, lease: &AuthLease) -> Result<ConnectionPermit, ConnectionError> {
        let _gate = self.inner.gate.lock().await;
        if !self.inner.accepting.load(Ordering::Acquire) {
            return Err(ConnectionError::ShuttingDown);
        }
        self.inner.access.validate(lease).await?;
        let slot = self
            .inner
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ConnectionError::AtCapacity)?;
        Ok(ConnectionPermit {
            inner: self.inner.clone(),
            lease: lease.clone(),
            _slot: slot,
        })
    }

    /// Reject new connections and notify admitted connections without waiting
    /// for them to drop.
    pub async fn begin_drain(&self) {
        let _gate = self.inner.gate.lock().await;
        self.inner.accepting.store(false, Ordering::Release);
        self.inner.shutdown.notify_waiters();
    }

    /// Wait until all admitted connections have released their slots.
    pub async fn wait_drained(&self) {
        if let Ok(all) = self
            .inner
            .slots
            .clone()
            .acquire_many_owned(self.inner.policy.max_connections as u32)
            .await
        {
            drop(all);
        }
    }

    /// Reject new connections and wait for all admitted connections to drop.
    pub async fn drain(&self) {
        self.begin_drain().await;
        self.wait_drained().await;
    }

    pub fn validate_message_size(&self, bytes: usize) -> Result<(), ConnectionError> {
        if bytes > self.inner.policy.max_message_bytes {
            Err(ConnectionError::MessageTooLarge)
        } else {
            Ok(())
        }
    }

    /// Create a bounded outbound queue for one connection.
    pub fn channel<T>(&self) -> (mpsc::Sender<T>, mpsc::Receiver<T>) {
        mpsc::channel(self.inner.policy.send_buffer)
    }
}

impl ConnectionPermit {
    pub fn lease(&self) -> &AuthLease {
        &self.lease
    }

    pub async fn authorized(&self) -> bool {
        self.inner.access.while_authorized(&self.lease).await
    }

    /// Wait until the Lease is invalidated or the registry starts shutdown.
    pub async fn wait_closed(&self) {
        let _ = self.wait_closed_reason().await;
    }

    pub async fn wait_closed_reason(&self) -> ConnectionCloseReason {
        let invalidated = self.inner.access.wait_invalidated(&self.lease);
        let mut shutdown = Box::pin(self.inner.shutdown.notified());
        // Register before checking the sticky accepting flag. This closes the
        // notify_waiters race where drain can run between a state check and
        // polling the notification future.
        shutdown.as_mut().enable();
        if !self.inner.accepting.load(Ordering::Acquire) {
            return ConnectionCloseReason::Shutdown;
        }
        tokio::pin!(invalidated);
        tokio::select! {
            _ = &mut invalidated => ConnectionCloseReason::LeaseInvalidated,
            _ = &mut shutdown => ConnectionCloseReason::Shutdown,
        }
    }

    pub fn validate_message_size(&self, bytes: usize) -> Result<(), ConnectionError> {
        if bytes > self.inner.policy.max_message_bytes {
            Err(ConnectionError::MessageTooLarge)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    fn policy() -> ConnectionPolicy {
        ConnectionPolicy::new(1, 64, 2).unwrap()
    }

    #[tokio::test]
    async fn revoked_lease_closes_a_connection() {
        let access = AccessState::new(true, 4);
        let lease = access
            .issue(
                crate::access::PrincipalId::new("client").unwrap(),
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        let registry = ConnectionRegistry::new(access.clone(), policy());
        let permit = registry.accept(&lease).await.unwrap();
        assert!(permit.authorized().await);
        access.revoke(&lease).await;
        timeout(Duration::from_secs(1), permit.wait_closed())
            .await
            .unwrap();
        assert!(!permit.authorized().await);
    }

    #[tokio::test]
    async fn drain_rejects_new_connections_and_waits_for_existing_ones() {
        let access = AccessState::new(true, 4);
        let lease = access
            .issue(
                crate::access::PrincipalId::new("client").unwrap(),
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        let registry = ConnectionRegistry::new(access, policy());
        let permit = registry.accept(&lease).await.unwrap();
        let draining = {
            let registry = registry.clone();
            tokio::spawn(async move { registry.drain().await })
        };
        tokio::task::yield_now().await;
        assert!(!registry.is_accepting());
        assert!(matches!(
            registry.accept(&lease).await,
            Err(ConnectionError::ShuttingDown)
        ));
        assert!(!draining.is_finished());
        drop(permit);
        timeout(Duration::from_secs(1), draining)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn wait_closed_observes_shutdown_that_started_before_wait() {
        let access = AccessState::new(true, 4);
        let lease = access
            .issue(
                crate::access::PrincipalId::new("client").unwrap(),
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        let registry = ConnectionRegistry::new(access, policy());
        let permit = registry.accept(&lease).await.unwrap();
        registry.begin_drain().await;
        assert_eq!(
            timeout(Duration::from_secs(1), permit.wait_closed_reason())
                .await
                .unwrap(),
            ConnectionCloseReason::Shutdown
        );
    }

    #[tokio::test]
    async fn connection_policy_bounds_messages_and_outbound_queue() {
        let access = AccessState::new(true, 4);
        let lease = access
            .issue(
                crate::access::PrincipalId::new("client").unwrap(),
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        let registry = ConnectionRegistry::new(access, policy());
        let permit = registry.accept(&lease).await.unwrap();
        assert!(permit.validate_message_size(64).is_ok());
        assert_eq!(
            permit.validate_message_size(65),
            Err(ConnectionError::MessageTooLarge)
        );
        let (sender, mut receiver) = registry.channel();
        sender.send(1u8).await.unwrap();
        sender.send(2u8).await.unwrap();
        assert!(sender.try_send(3u8).is_err());
        assert_eq!(receiver.recv().await, Some(1));
    }
}
