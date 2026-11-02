//! Deterministic qualification of result delivery versus actual blocking exit.
use super::*;
use std::{future::Future, pin::Pin, task::Context};

const WATCHDOG: Duration = Duration::from_secs(10);

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "session-catalog-completion-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(root.join("sessions")).unwrap();
        // Unrelated entries suffice to force bounded scan continuations without
        // coupling this lifecycle fixture to native header parsing or ordering.
        for name in ["one", "two", "three"] {
            fs::write(root.join("sessions").join(name), b"ignored").unwrap();
        }
        Self { root }
    }
    fn catalog(&self, max_workers: usize) -> SessionCatalog {
        new(
            self.root.join("sessions"),
            CatalogLimits {
                step_entries: 1,
                step_read_bytes: 7,
                max_workers,
                ..CatalogLimits::default()
            },
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn query(cursor: Option<String>, limit: usize) -> CatalogQuery {
    CatalogQuery {
        cwd: None,
        cursor,
        limit,
    }
}

// Always release on assertion panic, including a watchdog failure. finish is
// idempotent and deliberately tolerates an already-exited/disconnected worker.
struct Gate {
    reached: Option<oneshot::Receiver<()>>,
    release: Option<std::sync::mpsc::Sender<()>>,
}
impl Gate {
    fn install(catalog: &SessionCatalog) -> Self {
        let (reached_tx, reached_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut hook = lock(&catalog.inner.tail_hook);
        assert!(hook.is_none(), "previous one-shot hook was not admitted");
        *hook = Some(TailHook {
            reached: reached_tx,
            release: release_rx,
        });
        Self {
            reached: Some(reached_rx),
            release: Some(release_tx),
        }
    }
    async fn reached(&mut self) {
        tokio::time::timeout(WATCHDOG, self.reached.take().unwrap())
            .await
            .expect("worker tail watchdog")
            .expect("worker did not reach tail");
    }
    fn finish(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        self.finish();
    }
}
fn pending<F: Future>(mut future: Pin<&mut F>) {
    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    assert!(future.as_mut().poll(&mut cx).is_pending());
}
async fn finish<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(WATCHDOG, future)
        .await
        .expect("completion watchdog")
}
fn assert_tail(catalog: &SessionCatalog, workers: usize) {
    let state = lock(&catalog.inner.state);
    assert_eq!(state.active, 0, "result bookkeeping already finished");
    assert_eq!(state.workers.len(), workers);
    assert!(state.workers.iter().all(|w| !w.is_finished()));
    assert!(state.generations.iter().all(|slot| !slot.busy));
}
async fn conflict(catalog: &SessionCatalog) {
    assert_eq!(
        finish(catalog.page(query(None, 1), CatalogControl::default()))
            .await
            .unwrap_err(),
        CatalogError::Conflict
    );
}

#[tokio::test]
async fn session_catalog_completion_one_worker_gates_public_result_and_continuation() {
    let fixture = Fixture::new();
    let catalog = fixture.catalog(1);
    let mut gate = Gate::install(&catalog);
    let control = CatalogControl::default();
    let mut page = Box::pin(catalog.page(query(None, 1), control.clone()));
    pending(page.as_mut());
    gate.reached().await;
    assert_tail(&catalog, 1);
    let worker = Arc::clone(&lock(&catalog.inner.state).workers[0]);
    // The result has been sent and state unlocked. Only actual exit is missing.
    pending(page.as_mut());
    assert_eq!(control.check(), Ok(()));
    conflict(&catalog).await;
    assert_tail(&catalog, 1);
    gate.finish();
    let first = finish(page).await.unwrap();
    assert!(worker.is_finished());
    assert_eq!(control.check(), Ok(()));
    assert!(first.sessions.is_empty());
    let cursor = first.next_cursor.expect("bounded continuation");
    // No retry, sleep or scheduler yield between sequential public calls.
    finish(catalog.page(query(Some(cursor), 1), CatalogControl::default()))
        .await
        .unwrap();
    finish(catalog.shutdown()).await;
}

#[tokio::test]
async fn session_catalog_completion_default_two_waits_only_for_own_worker() {
    let fixture = Fixture::new();
    let catalog = fixture.catalog(CatalogLimits::default().max_workers);
    let mut first_gate = Gate::install(&catalog);
    let mut first = Box::pin(catalog.page(query(None, 1), CatalogControl::default()));
    pending(first.as_mut());
    first_gate.reached().await;
    let first_worker = Arc::clone(&lock(&catalog.inner.state).workers[0]);
    let mut second_gate = Gate::install(&catalog);
    let mut second = Box::pin(catalog.page(query(None, 1), CatalogControl::default()));
    pending(second.as_mut());
    second_gate.reached().await;
    let second_worker = Arc::clone(&lock(&catalog.inner.state).workers[1]);
    assert_tail(&catalog, 2);
    pending(first.as_mut());
    pending(second.as_mut());
    conflict(&catalog).await;
    assert_tail(&catalog, 2);

    first_gate.finish();
    let first_page = finish(first).await.unwrap();
    assert!(first_worker.is_finished());
    assert!(!second_worker.is_finished());
    // Completing the first request cannot require the independent second exit;
    // its reclaimed slot admits a continuation while worker two remains gated.
    finish(catalog.page(
        query(Some(first_page.next_cursor.unwrap()), 1),
        CatalogControl::default(),
    ))
    .await
    .unwrap();
    assert!(!second_worker.is_finished());
    pending(second.as_mut());
    second_gate.finish();
    finish(second).await.unwrap();
    assert!(second_worker.is_finished());
    finish(catalog.shutdown()).await;
}

#[tokio::test]
async fn session_catalog_completion_worker_error_waits_for_exit_preserves_cursor() {
    let fixture = Fixture::new();
    let catalog = fixture.catalog(1);
    let first = finish(catalog.page(query(None, 1), CatalogControl::default()))
        .await
        .unwrap();
    let cursor = first.next_cursor.unwrap();
    let mut gate = Gate::install(&catalog);
    // Admitted cursor validation fails inside the worker, not before admission.
    let mut page =
        Box::pin(catalog.page(query(Some(cursor.clone()), 2), CatalogControl::default()));
    pending(page.as_mut());
    gate.reached().await;
    assert_tail(&catalog, 1);
    pending(page.as_mut());
    conflict(&catalog).await;
    gate.finish();
    assert_eq!(finish(page).await.unwrap_err(), CatalogError::InvalidCursor);
    finish(catalog.page(query(Some(cursor), 1), CatalogControl::default()))
        .await
        .unwrap();
    finish(catalog.shutdown()).await;
}

#[tokio::test]
async fn session_catalog_completion_drop_in_handle_wait_retains_shutdown_retry() {
    let fixture = Fixture::new();
    let catalog = fixture.catalog(1);
    let mut gate = Gate::install(&catalog);
    let control = CatalogControl::default();
    let mut page = Box::pin(catalog.page(query(None, 1), control.clone()));
    pending(page.as_mut());
    gate.reached().await;
    pending(page.as_mut()); // Receive the result and enter the handle wait.
    assert_tail(&catalog, 1);
    drop(page);
    assert_eq!(control.check(), Err(CatalogError::Cancelled));
    assert_tail(&catalog, 1);
    conflict(&catalog).await;
    let worker = Arc::clone(&lock(&catalog.inner.state).workers[0]);
    let mut shutdown = Box::pin(catalog.shutdown());
    pending(shutdown.as_mut());
    drop(shutdown);
    assert_tail(&catalog, 1);
    assert!(Arc::ptr_eq(&worker, &lock(&catalog.inner.state).workers[0]));
    assert_eq!(catalog.inner.stop.check(), Err(CatalogError::Cancelled));
    gate.finish();
    finish(catalog.shutdown()).await;
    assert!(worker.is_finished());
    let state = lock(&catalog.inner.state);
    assert_eq!(state.active, 0);
    assert!(state.workers.is_empty());
    assert!(state.generations.is_empty());
}

#[tokio::test]
async fn session_catalog_completion_catalog_drop_owner_release_and_isolation() {
    let fixture = Fixture::new();
    let catalog = fixture.catalog(1);
    let other = fixture.catalog(1);
    let weak = Arc::downgrade(&catalog.inner);
    let mut gate = Gate::install(&catalog);
    let control = CatalogControl::default();
    let mut page = Box::pin(catalog.page(query(None, 1), control.clone()));
    pending(page.as_mut());
    gate.reached().await;
    pending(page.as_mut());
    let worker = Arc::clone(&lock(&catalog.inner.state).workers[0]);
    drop(page);
    drop(catalog);
    assert_eq!(control.check(), Err(CatalogError::Cancelled));
    // No catalog/page owner remains: the blocking closure retains Inner.
    assert_eq!(weak.strong_count(), 1);
    assert_eq!(
        weak.upgrade().unwrap().stop.check(),
        Err(CatalogError::Cancelled)
    );
    assert!(!worker.is_finished());
    // Hooks, cancellation and capacity are per catalog, even at the same root.
    finish(other.page(query(None, 1), CatalogControl::default()))
        .await
        .unwrap();
    assert!(!worker.is_finished());
    // Temporarily acquire the cleanup owner to use the existing exit observer;
    // release it before checking weak ownership, keeping only the saved wrapper.
    let cleanup = weak.upgrade().unwrap();
    gate.finish();
    finish(shutdown(&cleanup)).await;
    assert!(worker.is_finished());
    drop(cleanup);
    assert!(weak.upgrade().is_none(), "worker ownership must not cycle");
    finish(other.shutdown()).await;
}

#[tokio::test]
async fn session_catalog_completion_handle_wait_futures_remain_send() {
    fn send<T: Send>(_: &T) {}
    let fixture = Fixture::new();
    let catalog = fixture.catalog(1);
    let mut gate = Gate::install(&catalog);
    let mut page = Box::pin(catalog.page(query(None, 1), CatalogControl::default()));
    send(&page);
    pending(page.as_mut());
    gate.reached().await;
    pending(page.as_mut());
    send(&page);
    let shutdown_future = catalog.shutdown();
    send(&shutdown_future);
    drop(shutdown_future); // Never polled: do not change cancellation semantics.
    gate.finish();
    finish(page).await.unwrap();
    finish(catalog.shutdown()).await;
}
