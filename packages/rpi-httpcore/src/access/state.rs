use super::{AccessError, AuthLease, Generation, PrincipalId, SharingState};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{Mutex, Notify},
    time,
};
use uuid::Uuid;

struct LeaseRecord {
    principal: PrincipalId,
    generation: Generation,
    expires_at: Instant,
}

struct Inner {
    sharing: Mutex<SharingState>,
    leases: Mutex<HashMap<String, LeaseRecord>>,
    invalidated: Notify,
    max_leases: usize,
}

/// Shared authorization state for HTTP, SSE and WebSocket transports.
#[derive(Clone)]
pub struct AccessState {
    inner: Arc<Inner>,
}

impl AccessState {
    pub fn new(enabled: bool, max_leases: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                sharing: Mutex::new(SharingState {
                    enabled,
                    generation: 0,
                }),
                leases: Mutex::new(HashMap::new()),
                invalidated: Notify::new(),
                max_leases,
            }),
        }
    }

    pub async fn sharing(&self) -> SharingState {
        *self.inner.sharing.lock().await
    }

    /// Enable sharing without changing the current generation.
    pub async fn enable_sharing(&self) -> SharingState {
        let mut sharing = self.inner.sharing.lock().await;
        sharing.enabled = true;
        *sharing
    }

    /// Disable sharing, advance the generation and revoke every lease.
    pub async fn revoke_all(&self) -> SharingState {
        let mut sharing = self.inner.sharing.lock().await;
        sharing.enabled = false;
        sharing.generation = sharing.generation.wrapping_add(1);
        let state = *sharing;
        self.inner.leases.lock().await.clear();
        drop(sharing);
        self.inner.invalidated.notify_waiters();
        state
    }

    /// Revoke all leases belonging to one principal without changing the
    /// sharing generation.
    pub async fn revoke_principal(&self, principal: &PrincipalId) -> usize {
        let mut leases = self.inner.leases.lock().await;
        let before = leases.len();
        leases.retain(|_, lease| &lease.principal != principal);
        let revoked = before - leases.len();
        drop(leases);
        if revoked > 0 {
            self.inner.invalidated.notify_waiters();
        }
        revoked
    }

    /// Issue a lease for the current sharing generation.
    pub async fn issue(
        &self,
        principal: PrincipalId,
        ttl: Duration,
    ) -> Result<AuthLease, AccessError> {
        let sharing = self.inner.sharing.lock().await;
        if !sharing.enabled {
            return Err(AccessError::SharingDisabled);
        }
        let now = Instant::now();
        let mut leases = self.inner.leases.lock().await;
        leases.retain(|_, lease| lease.expires_at > now);
        if leases.len() >= self.inner.max_leases {
            return Err(AccessError::CapacityExceeded);
        }
        let credential = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let expires_at = now + ttl;
        leases.insert(
            credential.clone(),
            LeaseRecord {
                principal: principal.clone(),
                generation: sharing.generation,
                expires_at,
            },
        );
        Ok(AuthLease {
            credential,
            principal,
            generation: sharing.generation,
            expires_at,
        })
    }

    pub async fn validate(&self, lease: &AuthLease) -> Result<(), AccessError> {
        let sharing = self.inner.sharing.lock().await;
        if !sharing.enabled {
            return Err(AccessError::SharingDisabled);
        }
        if sharing.generation != lease.generation {
            return Err(AccessError::GenerationMismatch);
        }
        if lease.expires_at <= Instant::now() {
            return Err(AccessError::Expired);
        }
        let leases = self.inner.leases.lock().await;
        let Some(record) = leases.get(&lease.credential) else {
            return Err(AccessError::Revoked);
        };
        if record.generation != lease.generation || record.principal != lease.principal {
            return Err(AccessError::Revoked);
        }
        if record.expires_at <= Instant::now() {
            return Err(AccessError::Expired);
        }
        Ok(())
    }

    /// Authenticate a bearer credential and reconstruct its lease context.
    pub async fn authenticate(&self, credential: &str) -> Result<AuthLease, AccessError> {
        self.validate_credential(credential).await
    }

    /// Validate a bearer credential and reconstruct its lease context.
    pub async fn validate_credential(&self, credential: &str) -> Result<AuthLease, AccessError> {
        let sharing = self.inner.sharing.lock().await;
        if !sharing.enabled {
            return Err(AccessError::SharingDisabled);
        }
        let now = Instant::now();
        let mut leases = self.inner.leases.lock().await;
        leases.retain(|_, lease| lease.expires_at > now);
        let Some(record) = leases.get(credential) else {
            return Err(AccessError::Revoked);
        };
        if record.generation != sharing.generation {
            return Err(AccessError::GenerationMismatch);
        }
        Ok(AuthLease {
            credential: credential.to_owned(),
            principal: record.principal.clone(),
            generation: record.generation,
            expires_at: record.expires_at,
        })
    }

    /// Re-check authorization immediately before an owner commits an action.
    pub async fn authorize_before_commit(&self, lease: &AuthLease) -> Result<(), AccessError> {
        self.validate(lease).await
    }

    /// Check whether a lease is still valid while an operation or connection
    /// is active. This is intentionally a point-in-time check; callers must
    /// check again before committing or sending a result.
    pub async fn while_authorized(&self, lease: &AuthLease) -> bool {
        self.validate(lease).await.is_ok()
    }

    pub async fn revoke(&self, lease: &AuthLease) -> bool {
        let removed = self
            .inner
            .leases
            .lock()
            .await
            .remove(&lease.credential)
            .is_some();
        if removed {
            self.inner.invalidated.notify_waiters();
        }
        removed
    }

    pub async fn revoke_credential(&self, credential: &str) -> bool {
        let removed = self.inner.leases.lock().await.remove(credential).is_some();
        if removed {
            self.inner.invalidated.notify_waiters();
        }
        removed
    }

    /// Wait until this lease is revoked, expires or its generation is closed.
    pub async fn wait_invalidated(&self, lease: &AuthLease) {
        loop {
            let notified = self.inner.invalidated.notified();
            if self.validate(lease).await.is_err() {
                return;
            }
            tokio::select! {
                _ = notified => continue,
                _ = time::sleep_until(time::Instant::from_std(lease.expires_at)) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn closing_and_reopening_sharing_invalidates_old_generation() {
        let access = AccessState::new(true, 8);
        let principal = PrincipalId::new("http-client").unwrap();
        let lease = access
            .issue(principal, Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(lease.generation(), 0);
        assert!(access.validate(&lease).await.is_ok());

        let closed = access.revoke_all().await;
        assert!(!closed.enabled);
        assert_eq!(closed.generation, 1);
        assert_eq!(
            access.validate(&lease).await,
            Err(AccessError::SharingDisabled)
        );

        access.enable_sharing().await;
        assert_eq!(
            access.validate(&lease).await,
            Err(AccessError::GenerationMismatch)
        );
        let replacement = access
            .issue(
                PrincipalId::new("http-client").unwrap(),
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        assert_eq!(replacement.generation(), 1);
    }

    #[tokio::test]
    async fn revoke_wakes_waiting_connections() {
        let access = AccessState::new(true, 8);
        let lease = access
            .issue(PrincipalId::new("sse").unwrap(), Duration::from_secs(60))
            .await
            .unwrap();
        let waiter = {
            let access = access.clone();
            let lease = lease.clone();
            tokio::spawn(async move { access.wait_invalidated(&lease).await })
        };
        tokio::task::yield_now().await;
        assert!(access.revoke(&lease).await);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn expired_leases_are_rejected_and_capacity_is_reclaimed() {
        let access = AccessState::new(true, 1);
        let _expired = access
            .issue(
                PrincipalId::new("client").unwrap(),
                Duration::from_millis(1),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(access
            .issue(
                PrincipalId::new("replacement").unwrap(),
                Duration::from_secs(1),
            )
            .await
            .is_ok());
    }
}
