//! Short-lived explicit bearer delegations. Raw tickets are never retained.
use crate::{sharing::Access, SharingControl};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;

const TTL: Duration = Duration::from_secs(120);
const CAPACITY: usize = 64;
const REDEMPTIONS_PER_MINUTE: usize = 10;

/// A single-use bearer secret, not a password. Never log or persist this value.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QrAuthorization {
    pub ticket: String,
    /// Unix epoch seconds; enforcement uses a monotonic clock.
    pub expires_at: u64,
    pub node_id: String,
}
struct Record {
    digest: [u8; 32],
    until: Instant,
    generation: u64,
}
#[derive(Default)]
struct Records {
    tickets: Vec<Record>,
    attempts: Vec<Instant>,
}
pub struct Pairing {
    node_id: String,
    records: Mutex<Records>,
}
impl Pairing {
    pub fn new(node_id: String) -> Self {
        Self {
            node_id,
            records: Mutex::new(Records::default()),
        }
    }
    pub fn issue(
        &self,
        sharing: &SharingControl,
        access: Access,
    ) -> Result<QrAuthorization, String> {
        let ticket = crate::random_secret()?;
        let now = Instant::now();
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "system clock unavailable")?
            .as_secs()
            + TTL.as_secs();
        let mut records = self.records.lock().map_err(|_| "QR state unavailable")?;
        sharing
            .while_current(access, || {
                records
                    .tickets
                    .retain(|r| r.until > now && r.generation == access.generation);
                if records.tickets.len() >= CAPACITY {
                    return Err("QR ticket capacity reached".into());
                }
                records.tickets.push(Record {
                    digest: Sha256::digest(ticket.as_bytes()).into(),
                    until: now + TTL,
                    generation: access.generation,
                });
                Ok(QrAuthorization {
                    ticket,
                    expires_at,
                    node_id: self.node_id.clone(),
                })
            })
            .unwrap_or_else(|| Err("sharing disabled or changed".into()))
    }
    /// Caller holds the session store lock. Commit and consume occur in the
    /// same sharing epoch, without awaits. Lock order: sessions -> records -> policy.
    pub fn redeem<T>(
        &self,
        sharing: &SharingControl,
        access: Access,
        ticket: &str,
        commit: impl FnOnce() -> Result<T, axum::http::StatusCode>,
    ) -> Result<T, axum::http::StatusCode> {
        use axum::http::StatusCode;
        let mut records = self
            .records
            .lock()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        sharing
            .while_current(access, || {
                let now = Instant::now();
                records
                    .attempts
                    .retain(|t| now.duration_since(*t) < Duration::from_secs(60));
                if records.attempts.len() >= REDEMPTIONS_PER_MINUTE {
                    return Err(StatusCode::TOO_MANY_REQUESTS);
                }
                records.attempts.push(now);
                records
                    .tickets
                    .retain(|r| r.until > now && r.generation == access.generation);
                if ticket.len() != 64
                    || !ticket
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(StatusCode::UNAUTHORIZED);
                }
                let digest: [u8; 32] = Sha256::digest(ticket.as_bytes()).into();
                // Scan all bounded records; compare fixed-length digests, not bearer strings.
                let mut found = None;
                for (index, record) in records.tickets.iter().enumerate() {
                    if bool::from(record.digest.ct_eq(&digest)) {
                        found = Some(index);
                    }
                }
                let index = found.ok_or(StatusCode::UNAUTHORIZED)?;
                // A failed session-capacity commit leaves the ticket usable.
                let result = commit()?;
                records.tickets.swap_remove(index);
                Ok(result)
            })
            .unwrap_or(Err(StatusCode::FORBIDDEN))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    };

    #[test]
    fn expiry_capacity_hash_storage_and_rate_window() {
        let pairing = Pairing::new("test-node".into());
        let policy = SharingControl::new(true);
        let access = policy.snapshot();
        let ticket = pairing.issue(&policy, access).unwrap();
        {
            let records = pairing.records.lock().unwrap();
            assert_eq!(records.tickets.len(), 1);
            let hash: [u8; 32] = Sha256::digest(ticket.ticket.as_bytes()).into();
            assert_eq!(records.tickets[0].digest, hash);
            let remaining = records.tickets[0].until.duration_since(Instant::now());
            assert!(remaining <= TTL && remaining > TTL - Duration::from_secs(1));
        }
        for _ in 1..CAPACITY {
            pairing.issue(&policy, access).unwrap();
        }
        assert!(pairing.issue(&policy, access).is_err());
        pairing.records.lock().unwrap().tickets[0].until = Instant::now();
        assert_eq!(
            pairing.redeem(&policy, access, &ticket.ticket, || panic!(
                "expired ticket committed"
            )),
            Err::<(), _>(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(pairing.records.lock().unwrap().tickets.len(), CAPACITY - 1);
        let fresh = pairing.issue(&policy, access).unwrap();
        for _ in 1..REDEMPTIONS_PER_MINUTE {
            assert_eq!(
                pairing.redeem(&policy, access, "bad", || Ok(())),
                Err(StatusCode::UNAUTHORIZED)
            );
        }
        assert_eq!(
            pairing.redeem(&policy, access, &fresh.ticket, || Ok(())),
            Err(StatusCode::TOO_MANY_REQUESTS)
        );
        {
            let mut records = pairing.records.lock().unwrap();
            assert_eq!(records.attempts.len(), REDEMPTIONS_PER_MINUTE);
            for time in &mut records.attempts {
                *time = Instant::now() - Duration::from_secs(60);
            }
        }
        assert_eq!(
            pairing.redeem(&policy, access, &fresh.ticket, || Ok(())),
            Ok(())
        );
        assert_eq!(
            pairing.redeem(&policy, access, &fresh.ticket, || Ok(())),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn failed_session_commit_does_not_consume_ticket() {
        let pairing = Pairing::new("test-node".into());
        let policy = SharingControl::new(true);
        let access = policy.snapshot();
        let ticket = pairing.issue(&policy, access).unwrap();
        assert_eq!(
            pairing.redeem(&policy, access, &ticket.ticket, || Err::<(), _>(
                StatusCode::CONFLICT
            )),
            Err(StatusCode::CONFLICT)
        );
        assert_eq!(
            pairing.redeem(&policy, access, &ticket.ticket, || Ok(())),
            Ok(())
        );
    }

    #[test]
    fn stale_issue_and_redeem_cannot_prune_new_epoch_records() {
        let pairing = Pairing::new("test-node".into());
        let policy = SharingControl::new(true);
        let old = policy.snapshot();
        let old_ticket = pairing.issue(&policy, old).unwrap();
        policy.set_enabled(false);
        assert!(pairing.issue(&policy, policy.snapshot()).is_err());
        policy.set_enabled(true);
        let current = policy.snapshot();
        let fresh = pairing.issue(&policy, current).unwrap();
        assert!(pairing.issue(&policy, old).is_err());
        assert_eq!(
            pairing.redeem(&policy, old, &old_ticket.ticket, || panic!("stale commit")),
            Err::<(), _>(StatusCode::FORBIDDEN)
        );
        assert_eq!(pairing.records.lock().unwrap().tickets.len(), 1);
        assert_eq!(
            pairing.redeem(&policy, current, &old_ticket.ticket, || Ok(())),
            Err(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(
            pairing.redeem(&policy, current, &fresh.ticket, || Ok(())),
            Ok(())
        );
    }

    #[test]
    fn concurrent_redeemers_commit_exactly_once() {
        let pairing = Arc::new(Pairing::new("test-node".into()));
        let policy = SharingControl::new(true);
        let access = policy.snapshot();
        let ticket = pairing.issue(&policy, access).unwrap();
        let barrier = Arc::new(Barrier::new(8));
        let committed = Arc::new(AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let (pairing, policy, barrier, committed, ticket) = (
                    pairing.clone(),
                    policy.clone(),
                    barrier.clone(),
                    committed.clone(),
                    &ticket.ticket,
                );
                scope.spawn(move || {
                    barrier.wait();
                    let _ = pairing.redeem(&policy, access, ticket, || {
                        committed.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    });
                });
            }
        });
        assert_eq!(committed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn issue_and_redeem_racing_consent_never_revive_old_epoch() {
        for _ in 0..64 {
            let pairing = Pairing::new("race-node".into());
            let policy = SharingControl::new(true);
            let old = policy.snapshot();
            let ticket = pairing.issue(&policy, old).unwrap();
            let barrier = Barrier::new(3);
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    barrier.wait();
                    policy.set_enabled(false);
                    policy.set_enabled(true);
                });
                scope.spawn(|| {
                    barrier.wait();
                    // May commit only if it wins the epoch lock before revocation.
                    let _ = pairing.redeem(&policy, old, &ticket.ticket, || Ok(()));
                });
                barrier.wait();
                let _ = pairing.issue(&policy, old);
            });
            let current = policy.snapshot();
            assert!(pairing.issue(&policy, old).is_err());
            assert_eq!(
                pairing.redeem(&policy, current, &ticket.ticket, || panic!(
                    "revived ticket"
                )),
                Err::<(), _>(StatusCode::UNAUTHORIZED)
            );
            let fresh = pairing.issue(&policy, current).unwrap();
            assert_eq!(
                pairing.redeem(&policy, current, &fresh.ticket, || Ok(())),
                Ok(())
            );
        }
    }
}
