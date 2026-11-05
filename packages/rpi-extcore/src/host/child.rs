//! Native child-tree ownership independent of RPC reader cancellation. Only
//! this retained blocking reaper owns the contained process tree. EOF, stop ACK
//! and an aborted reader are not OS exit evidence.
use super::*;
#[cfg(test)]
#[path = "child_tests.rs"]
mod tests;

pub(super) struct NativeChild {
    stop: tokio::sync::watch::Sender<bool>,
    exit: tokio::sync::watch::Receiver<Option<bool>>,
}
impl NativeChild {
    pub(super) fn start(mut tree: rpi_mcp_core::ProcessTree) -> Self {
        let (stop, stopping) = tokio::sync::watch::channel(false);
        let (exited, exit) = tokio::sync::watch::channel(None);
        // A started blocking task cannot be aborted with the reader. It owns
        // cleanup until exit even when every request/host future is dropped.
        tokio::task::spawn_blocking(move || {
            loop {
                if *stopping.borrow() || stopping.has_changed().is_err() {
                    break;
                }
                match tree.child_mut().try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
            tree.stop(); // includes owned group/job cleanup and leader wait
            let confirmed = tree
                .child_mut()
                .try_wait()
                .is_ok_and(|status| status.is_some());
            exited.send_replace(Some(confirmed));
        });
        Self { stop, exit }
    }
    pub(super) fn stop_signal(&self) -> tokio::sync::watch::Sender<bool> {
        self.stop.clone()
    }
    pub(super) fn exit_signal(&self) -> tokio::sync::watch::Receiver<Option<bool>> {
        self.exit.clone()
    }
    pub(super) async fn stop_checked(&self) -> Result<(), String> {
        self.stop.send_replace(true);
        Self::wait_checked(self.exit.clone()).await
    }
    pub(super) async fn wait_checked(
        mut exit: tokio::sync::watch::Receiver<Option<bool>>,
    ) -> Result<(), String> {
        tokio::time::timeout(Duration::from_secs(10), async {
            if exit.borrow().is_none() {
                let _ = exit.wait_for(Option::is_some).await;
            }
            if *exit.borrow() == Some(true) {
                Ok(())
            } else {
                Err("owned native child OS exit not confirmed".into())
            }
        })
        .await
        .map_err(|_| "owned native child OS exit deadline exceeded".to_string())?
    }
}
impl Drop for NativeChild {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}
