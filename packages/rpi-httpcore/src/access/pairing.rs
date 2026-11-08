use super::{AccessState, Generation};
use std::{
    collections::HashMap,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingError {
    SharingDisabled,
    AtCapacity,
    Expired,
    Revoked,
    GenerationMismatch,
}

struct TicketRecord {
    generation: Generation,
    expires_at: Instant,
}

struct Inner {
    tickets: Mutex<HashMap<String, TicketRecord>>,
    access: AccessState,
    max_tickets: usize,
}

/// A short-lived, single-use credential that can be presented to a host for
/// an application-specific permission decision.
#[derive(Clone)]
pub struct PairingTicket {
    credential: String,
    generation: Generation,
    expires_at: Instant,
}

impl fmt::Debug for PairingTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingTicket")
            .field("generation", &self.generation)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl PairingTicket {
    pub fn generation(&self) -> Generation {
        self.generation
    }

    pub fn expires_at(&self) -> Instant {
        self.expires_at
    }

    /// Return the opaque value for a QR/pairing presentation. Callers must not
    /// log it, put it in a URL, or echo it in an error response.
    pub fn credential(&self) -> &str {
        &self.credential
    }
}

/// The result of an atomic ticket redemption. Permission mapping belongs to
/// the host; redeeming a ticket grants no default capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PairingRedemption {
    pub generation: Generation,
}

/// Bounded pairing-ticket storage bound to an AccessState generation.
#[derive(Clone)]
pub struct PairingStore {
    inner: Arc<Inner>,
}

impl PairingStore {
    pub fn new(access: AccessState, max_tickets: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                tickets: Mutex::new(HashMap::new()),
                access,
                max_tickets,
            }),
        }
    }

    pub async fn issue(&self, ttl: Duration) -> Result<PairingTicket, PairingError> {
        let sharing = self.inner.access.sharing().await;
        if !sharing.enabled {
            return Err(PairingError::SharingDisabled);
        }
        let now = Instant::now();
        let mut tickets = self.inner.tickets.lock().await;
        let current = self.inner.access.sharing().await;
        if !current.enabled || current.generation != sharing.generation {
            return Err(if !current.enabled {
                PairingError::SharingDisabled
            } else {
                PairingError::GenerationMismatch
            });
        }
        tickets.retain(|_, ticket| ticket.expires_at > now);
        if tickets.len() >= self.inner.max_tickets {
            return Err(PairingError::AtCapacity);
        }
        let credential = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let expires_at = now + ttl;
        tickets.insert(
            credential.clone(),
            TicketRecord {
                generation: sharing.generation,
                expires_at,
            },
        );
        Ok(PairingTicket {
            credential,
            generation: sharing.generation,
            expires_at,
        })
    }

    /// Consume a ticket exactly once. The returned value contains no default
    /// authorization; the host must map it to its own permission set.
    pub async fn redeem(&self, credential: &str) -> Result<PairingRedemption, PairingError> {
        let sharing = self.inner.access.sharing().await;
        if !sharing.enabled {
            return Err(PairingError::SharingDisabled);
        }
        let mut tickets = self.inner.tickets.lock().await;
        let Some(ticket) = tickets.remove(credential) else {
            return Err(PairingError::Revoked);
        };
        if ticket.expires_at <= Instant::now() {
            return Err(PairingError::Expired);
        }
        let current = self.inner.access.sharing().await;
        if !current.enabled {
            return Err(PairingError::SharingDisabled);
        }
        if ticket.generation != current.generation {
            return Err(PairingError::GenerationMismatch);
        }
        Ok(PairingRedemption {
            generation: ticket.generation,
        })
    }

    pub async fn clear(&self) {
        self.inner.tickets.lock().await.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn concurrent_redemption_succeeds_once() {
        let access = AccessState::new(true, 8);
        let store = PairingStore::new(access, 8);
        let ticket = store.issue(Duration::from_secs(60)).await.unwrap();
        let credential = Arc::<str>::from(ticket.credential());
        let results = futures_util::future::join_all((0..16).map(|_| {
            let store = store.clone();
            let credential = credential.clone();
            async move { store.redeem(&credential).await.is_ok() }
        }))
        .await;
        assert_eq!(results.into_iter().filter(|redeemed| *redeemed).count(), 1);
    }

    #[tokio::test]
    async fn old_generation_tickets_cannot_be_redeemed_after_reopen() {
        let access = AccessState::new(true, 8);
        let store = PairingStore::new(access.clone(), 8);
        let ticket = store.issue(Duration::from_secs(60)).await.unwrap();
        access.revoke_all().await;
        access.enable_sharing().await;
        assert_eq!(
            store.redeem(ticket.credential()).await,
            Err(PairingError::GenerationMismatch)
        );
    }
}
