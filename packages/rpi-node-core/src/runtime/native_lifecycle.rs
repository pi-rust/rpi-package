//! Native-only cancellation and process ownership helpers.

use std::sync::{Arc, Mutex, MutexGuard};
use tokio::process::Child;

/// Only short, non-I/O bookkeeping sections use this lock. Drop cleanup must
/// publish terminal state synchronously, before another request can be polled.
pub(crate) struct State<T>(Mutex<T>);

impl<T> State<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(Mutex::new(value))
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, T> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(crate) struct CleanupGuard(Option<Box<dyn FnOnce() + Send + Sync>>);

impl CleanupGuard {
    pub(crate) fn new(cleanup: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self(Some(Box::new(cleanup)))
    }

    pub(crate) fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        if let Some(cleanup) = self.0.take() {
            cleanup();
        }
    }
}

/// Initiate termination even outside a runtime when the child lock is free;
/// inside a runtime also reap it. The child's `kill_on_drop` is the final fallback.
pub(crate) fn terminate_child(child: &Arc<tokio::sync::Mutex<Child>>) {
    if let Ok(mut child) = child.try_lock() {
        let _ = child.start_kill();
    }
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        let child = Arc::clone(child);
        drop(runtime.spawn(async move {
            let mut child = child.lock().await;
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill().await;
            }
        }));
    }
}

#[cfg(test)]
pub(crate) fn test_child() -> Child {
    tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "runtime::native_lifecycle::tests::child_fixture",
            "--ignored",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "internal child fixture: run only as a subprocess"]
    fn child_fixture() {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }

    #[tokio::test]
    async fn termination_kills_and_reaps_child_even_when_lock_is_contended() {
        for contended in [false, true] {
            let child = Arc::new(tokio::sync::Mutex::new(test_child()));
            let held = if contended {
                Some(child.lock().await)
            } else {
                None
            };
            terminate_child(&child);
            drop(held);
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if child.lock().await.try_wait().unwrap().is_some() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("cleanup must terminate and reap the OS child");
        }
    }
}
