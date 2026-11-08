use super::{middleware, ConnectionConfigError, ConnectionPolicy, ConnectionRegistry};
use crate::access::{AccessState, Admission, AdmissionError};
use axum::{middleware::from_fn_with_state, Router};
use std::future::Future;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportConfigError {
    ZeroOperationLimit,
    Connection(ConnectionConfigError),
}

/// Shared lifecycle state that can be composed with any owner router.
#[derive(Clone)]
pub struct TransportState {
    pub(crate) admission: Admission,
    pub(crate) connections: ConnectionRegistry,
}

impl TransportState {
    pub fn new(
        access: AccessState,
        operation_limit: u32,
        connection_policy: ConnectionPolicy,
    ) -> Result<Self, TransportConfigError> {
        let admission = Admission::new(operation_limit)
            .map_err(|_| TransportConfigError::ZeroOperationLimit)?;
        Ok(Self {
            admission,
            connections: ConnectionRegistry::new(access, connection_policy),
        })
    }

    pub fn admission(&self) -> &Admission {
        &self.admission
    }

    pub fn connections(&self) -> &ConnectionRegistry {
        &self.connections
    }

    /// Stop new owner operations and connections, then wait for both to drain.
    pub async fn drain(&self) {
        self.drain_with(|| async {}).await;
    }

    /// Drain transport resources and then await an owner-provided cleanup
    /// future. The callback is deliberately opaque to keep native child and
    /// Work-owner lifecycle outside this crate.
    pub async fn drain_with<F, Fut>(&self, owner_drain: F)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ()>,
    {
        self.connections.drain().await;
        self.admission.drain().await;
        owner_drain().await;
    }
}

/// Apply lifecycle admission to an arbitrary owner router.
///
/// Authentication, Host/Origin checks and protocol-specific routes remain the
/// responsibility of the caller. The returned router has no Work or rpi
/// Service state requirement, so it can be merged with any owner adapter.
pub fn lifecycle_router(routes: Router, state: TransportState) -> Router {
    routes.layer(from_fn_with_state(state, middleware::admit_owner))
}

pub(super) fn admission_response(error: AdmissionError) -> axum::response::Response {
    use axum::{response::IntoResponse, Json};
    use serde_json::json;
    let (status, message) = match error {
        AdmissionError::AtCapacity => (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "owner admission capacity exceeded",
        ),
        AdmissionError::ShuttingDown => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "service is shutting down",
        ),
    };
    (status, Json(json!({"error": message}))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn drain_with_runs_owner_cleanup_after_transport_drain() {
        let state = TransportState::new(
            AccessState::new(false, 4),
            1,
            ConnectionPolicy::new(1, 64, 1).unwrap(),
        )
        .unwrap();
        let called = Arc::new(AtomicBool::new(false));
        let callback_called = called.clone();
        state
            .drain_with(|| async move {
                callback_called.store(true, Ordering::Release);
            })
            .await;
        assert!(called.load(Ordering::Acquire));
        assert!(!state.admission().is_accepting());
        assert!(!state.connections().is_accepting());
    }
}
