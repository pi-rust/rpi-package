//! Desktop-owned access policy, independent from listener and agent lifetimes.
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Access {
    pub enabled: bool,
    pub generation: u64,
}

/// Only the host receives this handle; no HTTP policy mutation endpoint exists.
#[derive(Clone)]
pub struct SharingControl(watch::Sender<Access>);
impl SharingControl {
    pub fn new(enabled: bool) -> Self {
        Self(
            watch::channel(Access {
                enabled,
                generation: 0,
            })
            .0,
        )
    }
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.snapshot().enabled
    }
    /// Changes the access epoch, revoking old logins and streams, not agent work.
    pub fn set_enabled(&self, enabled: bool) {
        self.0.send_if_modified(|access| {
            if access.enabled == enabled {
                return false;
            }
            access.enabled = enabled;
            access.generation = access.generation.wrapping_add(1);
            true
        });
    }
    pub fn snapshot(&self) -> Access {
        *self.0.borrow()
    }
    pub fn subscribe(&self) -> watch::Receiver<Access> {
        self.0.subscribe()
    }
    pub fn permits(&self, access: Access) -> bool {
        access.enabled && self.snapshot() == access
    }
    /// A short synchronous commit, preventing a stale login from pruning a
    /// newer epoch's tokens. The closure must never await or change policy.
    pub fn while_current<T>(&self, access: Access, commit: impl FnOnce() -> T) -> Option<T> {
        let current = self.0.borrow();
        if access.enabled && *current == access {
            Some(commit())
        } else {
            None
        }
    }
}
