use super::{middleware, ConnectionConfigError, ConnectionPolicy, ConnectionRegistry};
use crate::access::{AccessState, Admission, AdmissionError, PairingStore};
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
    pairing: Option<PairingStore>,
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
            pairing: None,
        })
    }

    /// Attach an optional pairing store so shutdown automatically stops new
    /// ticket issuance and clears outstanding tickets.
    pub fn with_pairing_store(mut self, pairing: PairingStore) -> Self {
        self.pairing = Some(pairing);
        self
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
        if let Some(pairing) = &self.pairing {
            pairing.shutdown().await;
        }
        // Flip both accepting gates before waiting. Otherwise a long-lived
        // connection could keep the first drain phase open while new owner
        // operations still enter through the second gate.
        let ((), ()) = tokio::join!(self.admission.begin_drain(), self.connections.begin_drain());
        let ((), ()) = tokio::join!(
            self.admission.wait_drained(),
            self.connections.wait_drained()
        );
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
        let access = AccessState::new(true, 4);
        let lease = access
            .issue(
                crate::access::PrincipalId::new("owner").unwrap(),
                std::time::Duration::from_secs(60),
            )
            .await
            .unwrap();
        let pairing = PairingStore::new(access.clone(), 8);
        let ticket = pairing
            .issue(std::time::Duration::from_secs(60))
            .await
            .unwrap();
        let state = TransportState::new(access, 1, ConnectionPolicy::new(1, 64, 1).unwrap())
            .unwrap()
            .with_pairing_store(pairing.clone());
        let operation = state.admission().try_admit().await.unwrap();
        let connection = state.connections().accept(&lease).await.unwrap();
        let called = Arc::new(AtomicBool::new(false));
        let callback_called = called.clone();
        let draining = {
            let state = state.clone();
            tokio::spawn(async move {
                state
                    .drain_with(|| async move {
                        callback_called.store(true, Ordering::Release);
                    })
                    .await;
            })
        };
        tokio::task::yield_now().await;
        assert!(!state.admission().is_accepting());
        assert!(!state.connections().is_accepting());
        assert!(matches!(
            state.admission().try_admit().await,
            Err(AdmissionError::ShuttingDown)
        ));
        assert!(matches!(
            state.connections().accept(&lease).await,
            Err(crate::transport::ConnectionError::ShuttingDown)
        ));
        assert!(!draining.is_finished());
        assert!(matches!(
            pairing.issue(std::time::Duration::from_secs(60)).await,
            Err(crate::access::PairingError::ShuttingDown)
        ));
        drop(connection);
        drop(operation);
        tokio::time::timeout(std::time::Duration::from_secs(1), draining)
            .await
            .unwrap()
            .unwrap();
        assert!(called.load(Ordering::Acquire));
        assert!(matches!(
            pairing.redeem(ticket.credential()).await,
            Err(crate::access::PairingError::ShuttingDown)
        ));
    }
}
