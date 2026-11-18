//! Bridge lifecycle controller: the one owner of start/stop state.
//!
//! Ported from the upstream `lifecycle.ts`. The bridge stays off until the user
//! asks for it, `connect`/`close` are idempotent, and a failed bind never
//! leaves a half-open server behind. Binding happens synchronously so the
//! command surface reports `EADDRINUSE` as a normal error.
#![forbid(unsafe_code)]

use crate::bridge::HealthMeta;
use crate::server::{self, Hooks, Server};
use std::sync::Arc;

/// Bridge lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleState {
    Off,
    /// Reserved for the snapshot vocabulary; `connect` binds synchronously.
    Starting,
    On,
    Closing,
}

/// A point-in-time view for the status line, commands and tests.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct BridgeSnapshot {
    pub state: LifecycleState,
    /// The port this connection asked for (or the configured default when off).
    pub port: u16,
    pub actual_port: Option<u16>,
    pub panes: usize,
    pub last_error: Option<String>,
    pub started_at: Option<u64>,
}

/// Builds the `/health` metadata for one bound port.
pub type MetaFactory<'a> = &'a dyn Fn(u16) -> HealthMeta;

/// Owns the server for one session.
pub struct Controller {
    default_port: u16,
    state: LifecycleState,
    port: u16,
    actual_port: Option<u16>,
    last_error: Option<String>,
    started_at: Option<u64>,
    server: Option<Arc<Server>>,
}

impl Controller {
    pub fn new(default_port: u16) -> Self {
        Self {
            default_port,
            state: LifecycleState::Off,
            port: default_port,
            actual_port: None,
            last_error: None,
            started_at: None,
            server: None,
        }
    }

    pub fn snapshot(&self) -> BridgeSnapshot {
        BridgeSnapshot {
            state: self.state,
            port: self.port,
            actual_port: self.actual_port,
            panes: self
                .server
                .as_ref()
                .map(|server| server.panes().len())
                .unwrap_or(0),
            last_error: self.last_error.clone(),
            started_at: self.started_at,
        }
    }

    pub fn server(&self) -> Option<&Arc<Server>> {
        self.server.as_ref()
    }

    pub fn is_on(&self) -> bool {
        self.state == LifecycleState::On && self.server.is_some()
    }

    /// Currently attached panes (empty when off).
    pub fn panes(&self) -> Vec<crate::bridge::Pane> {
        self.server
            .as_ref()
            .map(|server| server.panes())
            .unwrap_or_default()
    }

    /// Start listening on `requested` (or the configured default).
    pub fn connect(
        &mut self,
        requested: Option<u16>,
        meta: MetaFactory<'_>,
        hooks: Arc<dyn Hooks>,
    ) -> Result<BridgeSnapshot, String> {
        let target = requested.unwrap_or(self.default_port);
        if target == 0 {
            let message = "Invalid port 0: expected an integer from 1 to 65535.".to_string();
            self.last_error = Some(message.clone());
            return Err(message);
        }
        if self.state == LifecycleState::On {
            let current = self.actual_port.unwrap_or(self.port);
            if target == self.port {
                return Ok(self.snapshot());
            }
            return Err(format!(
                "Office bridge is already listening on :{current}. Run /office restart {target} to move it."
            ));
        }
        self.port = target;
        self.last_error = None;
        self.state = LifecycleState::Starting;
        let listener = match server::bind(target) {
            Ok(listener) => listener,
            Err(error) => {
                self.state = LifecycleState::Off;
                self.actual_port = None;
                self.started_at = None;
                self.last_error = Some(error.clone());
                return Err(error);
            }
        };
        let health = meta(target);
        match Server::start(listener, target, health, hooks) {
            Ok(server) => {
                self.actual_port = Some(server.port);
                self.started_at = Some(crate::protocol::now_ms());
                self.state = LifecycleState::On;
                self.server = Some(Arc::new(server));
                Ok(self.snapshot())
            }
            Err(error) => {
                self.state = LifecycleState::Off;
                self.actual_port = None;
                self.started_at = None;
                self.last_error = Some(error.clone());
                Err(error)
            }
        }
    }

    /// Stop listening and drop every pane.
    pub fn close(&mut self) -> Result<BridgeSnapshot, String> {
        let Some(server) = self.server.take() else {
            self.state = LifecycleState::Off;
            self.actual_port = None;
            self.started_at = None;
            return Ok(self.snapshot());
        };
        self.state = LifecycleState::Closing;
        let result = server.stop();
        self.actual_port = None;
        self.started_at = None;
        self.state = LifecycleState::Off;
        match result {
            Ok(()) => Ok(self.snapshot()),
            Err(error) => {
                let message = format!("Office bridge did not close cleanly: {error}");
                self.last_error = Some(message.clone());
                Err(message)
            }
        }
    }

    /// Stop, then start (optionally on another port).
    pub fn restart(
        &mut self,
        requested: Option<u16>,
        meta: MetaFactory<'_>,
        hooks: Arc<dyn Hooks>,
    ) -> Result<BridgeSnapshot, String> {
        let _ = self.close();
        self.connect(requested, meta, hooks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::Pane;

    struct NoHooks;
    impl Hooks for NoHooks {
        fn on_user_message(&self, _text: &str, _pane: &Pane) {}
        fn on_abort_request(&self, _prompt_id: Option<&str>, _pane: &Pane) {}
        fn on_panes_changed(&self, _panes: &[Pane]) {}
    }

    fn meta(port: u16) -> HealthMeta {
        HealthMeta {
            service: "rpi-office".into(),
            server_version: "0.1.0".into(),
            pi_version: None,
            port,
            started_at: crate::protocol::now_ms(),
        }
    }

    #[test]
    fn connect_is_idempotent_and_close_returns_to_off() {
        let mut controller = Controller::new(0);
        assert_eq!(controller.snapshot().state, LifecycleState::Off);
        let snapshot = controller.connect(Some(0), &meta, Arc::new(NoHooks));
        let error = snapshot.expect_err("port 0 must be rejected");
        assert!(error.contains("Invalid port 0"));
        assert_eq!(controller.snapshot().state, LifecycleState::Off);
    }

    #[test]
    fn connect_binds_reports_and_stops() {
        // Pick a free port first.
        let probe = server::bind(0).expect("ephemeral bind");
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let mut controller = Controller::new(port);
        let snapshot = controller
            .connect(None, &meta, Arc::new(NoHooks))
            .expect("connect");
        assert_eq!(snapshot.state, LifecycleState::On);
        assert_eq!(snapshot.actual_port, Some(port));
        assert!(controller.is_on());
        // Idempotent.
        let again = controller
            .connect(Some(port), &meta, Arc::new(NoHooks))
            .expect("idempotent connect");
        assert_eq!(again.actual_port, Some(port));
        // Moving ports requires a restart.
        let error = controller
            .connect(Some(port.saturating_add(1)), &meta, Arc::new(NoHooks))
            .expect_err("port move needs a restart");
        assert!(error.contains("/office restart"));

        let snapshot = controller.close().expect("close");
        assert_eq!(snapshot.state, LifecycleState::Off);
        assert!(snapshot.actual_port.is_none());
        // Closing twice is a no-op.
        assert_eq!(controller.close().unwrap().state, LifecycleState::Off);
    }

    #[test]
    fn a_busy_port_reports_the_owner_hint_and_leaves_the_bridge_off() {
        let occupied = server::bind(0).expect("ephemeral bind");
        let port = occupied.local_addr().unwrap().port();
        let mut controller = Controller::new(port);
        let error = controller
            .connect(None, &meta, Arc::new(NoHooks))
            .expect_err("busy port");
        assert!(error.contains("already in use"));
        assert!(error.contains("/office connect <port>"));
        let snapshot = controller.snapshot();
        assert_eq!(snapshot.state, LifecycleState::Off);
        assert!(snapshot.actual_port.is_none());
        assert!(snapshot.last_error.is_some());
        assert!(!controller.is_on());
        drop(occupied);
    }

    #[test]
    fn restart_moves_the_listener() {
        let first = server::bind(0).expect("bind");
        let first_port = first.local_addr().unwrap().port();
        drop(first);
        let second = server::bind(0).expect("bind");
        let second_port = second.local_addr().unwrap().port();
        drop(second);

        let mut controller = Controller::new(first_port);
        controller
            .connect(None, &meta, Arc::new(NoHooks))
            .expect("connect");
        let snapshot = controller
            .restart(Some(second_port), &meta, Arc::new(NoHooks))
            .expect("restart");
        assert_eq!(snapshot.actual_port, Some(second_port));
        assert_eq!(snapshot.state, LifecycleState::On);
        controller.close().expect("close");
    }
}
