use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

/// Admission errors are deliberately transport-neutral. HTTP adapters may map
/// `AtCapacity` to 429 and `ShuttingDown` to their shutdown status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    AtCapacity,
    ShuttingDown,
}

struct Inner {
    accepting: AtomicBool,
    gate: Mutex<()>,
    permits: Arc<Semaphore>,
    limit: u32,
}

/// Bounds owner operations and provides a shutdown drain barrier.
#[derive(Clone)]
pub struct Admission {
    inner: Arc<Inner>,
}

/// A permit held for the complete owner operation. Dropping it releases the
/// operation slot, including on cancellation or handler failure.
pub struct AdmissionPermit {
    _permit: OwnedSemaphorePermit,
}

impl Admission {
    pub fn new(limit: u32) -> Result<Self, AdmissionError> {
        if limit == 0 {
            return Err(AdmissionError::AtCapacity);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                accepting: AtomicBool::new(true),
                gate: Mutex::new(()),
                permits: Arc::new(Semaphore::new(limit as usize)),
                limit,
            }),
        })
    }

    pub fn is_accepting(&self) -> bool {
        self.inner.accepting.load(Ordering::Acquire)
    }

    /// Acquire an operation slot without queueing indefinitely.
    pub async fn try_admit(&self) -> Result<AdmissionPermit, AdmissionError> {
        let _gate = self.inner.gate.lock().await;
        if !self.inner.accepting.load(Ordering::Acquire) {
            return Err(AdmissionError::ShuttingDown);
        }
        self.inner
            .permits
            .clone()
            .try_acquire_owned()
            .map(|permit| AdmissionPermit { _permit: permit })
            .map_err(|_| AdmissionError::AtCapacity)
    }

    /// Stop new owner operations and wait until every admitted operation has
    /// released its permit. This does not cancel or own the underlying owner.
    pub async fn drain(&self) {
        let _gate = self.inner.gate.lock().await;
        self.inner.accepting.store(false, Ordering::Release);
        drop(_gate);

        let permits = self
            .inner
            .permits
            .clone()
            .acquire_many_owned(self.inner.limit);
        if let Ok(all) = permits.await {
            drop(all);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn admission_rejects_at_capacity_and_drains_active_work() {
        let admission = Admission::new(1).unwrap();
        let first = admission.try_admit().await.unwrap();
        assert!(matches!(
            admission.try_admit().await,
            Err(AdmissionError::AtCapacity)
        ));

        let draining = {
            let admission = admission.clone();
            tokio::spawn(async move { admission.drain().await })
        };
        tokio::task::yield_now().await;
        assert!(!admission.is_accepting());
        assert!(matches!(
            admission.try_admit().await,
            Err(AdmissionError::ShuttingDown)
        ));
        assert!(!draining.is_finished());

        drop(first);
        timeout(Duration::from_secs(1), draining)
            .await
            .unwrap()
            .unwrap();
    }
}
