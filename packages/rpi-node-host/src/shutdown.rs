//! Bounded synchronous cleanup without releasing ownership on an unconfirmed stop.
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

/// `cleanup` must release resources in dependency order on success. On error,
/// panic or worker startup failure the entire remaining value is retained until
/// process exit. A timeout leaves the worker running and owning the value.
pub(crate) fn run_retaining<T: Send + 'static>(
    value: T,
    timeout: Duration,
    cleanup: impl FnOnce(&mut T) -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    let value = Arc::new(Mutex::new(Some(value)));
    let owned = value.clone();
    let (done, completed) = mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("workspace-shutdown".into())
        .spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| {
                let mut held = owned
                    .lock()
                    .map_err(|_| "Workspace shutdown state poisoned")?;
                cleanup(held.as_mut().ok_or("Workspace shutdown state missing")?)?;
                // Drop before reporting success, not merely when the worker exits.
                drop(held.take());
                Ok(())
            }))
            .unwrap_or_else(|_| {
                Err("Workspace shutdown panicked; data lock retained until exit".into())
            });
            if result.is_err() {
                // The outer Arc may disappear as soon as the caller gets the error.
                // Keep the lock AND the HTTP executor; neither may drop on failure.
                std::mem::forget(owned);
            }
            let _ = done.send(result);
        });
    if spawned.is_err() {
        std::mem::forget(value);
        return Err("Unable to start workspace shutdown; data lock retained until exit".into());
    }
    completed.recv_timeout(timeout).map_err(|_| {
        "Workspace shutdown exceeded its wait limit; data lock retained until cleanup or exit"
            .to_owned()
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Resource(Arc<AtomicBool>);
    impl Drop for Resource {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn confirmed_cleanup_releases_before_success() {
        let dropped = Arc::new(AtomicBool::new(false));
        run_retaining(
            Resource(dropped.clone()),
            Duration::from_secs(2),
            |_| Ok(()),
        )
        .unwrap();
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn executor_or_drain_failure_retains_ownership() {
        for error in ["executor unavailable", "HTTP drain timeout"] {
            let dropped = Arc::new(AtomicBool::new(false));
            assert!(
                run_retaining(
                    Resource(dropped.clone()),
                    Duration::from_secs(2),
                    move |_| { Err(error.into()) }
                )
                .is_err()
            );
            assert!(!dropped.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn panic_retains_ownership_instead_of_unwinding_the_guard() {
        let dropped = Arc::new(AtomicBool::new(false));
        assert!(
            run_retaining(Resource(dropped.clone()), Duration::from_secs(2), |_| {
                panic!("simulated cleanup failure")
            })
            .is_err()
        );
        assert!(!dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn timeout_keeps_lock_until_worker_finishes() {
        struct Tracked(Resource, mpsc::SyncSender<()>);
        impl Drop for Tracked {
            fn drop(&mut self) {
                self.0.0.store(true, Ordering::SeqCst);
                let _ = self.1.send(());
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let (release, wait) = mpsc::sync_channel(1);
        let (finished, observe) = mpsc::sync_channel(1);
        assert!(
            run_retaining(
                Tracked(Resource(dropped.clone()), finished),
                Duration::from_millis(10),
                move |_| {
                    wait.recv().unwrap();
                    Ok(())
                },
            )
            .is_err()
        );
        assert!(!dropped.load(Ordering::SeqCst));
        release.send(()).unwrap();
        observe.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(dropped.load(Ordering::SeqCst));
    }
}
