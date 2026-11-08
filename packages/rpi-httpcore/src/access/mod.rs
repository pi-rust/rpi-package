//! Transport-neutral authorization leases and sharing revocation state.
//!
//! This module deliberately knows nothing about Axum, native services or Work.
//! It provides the common lifecycle contract that HTTP, SSE and WebSocket
//! adapters can use to bind a connection to one authorization generation.

mod admission;
mod pairing;
mod state;
mod types;

pub use admission::{Admission, AdmissionError, AdmissionPermit};
pub use pairing::{PairingError, PairingRedemption, PairingStore, PairingTicket};
pub use state::AccessState;
pub use types::{AccessError, AuthLease, Generation, PrincipalId, SharingState};
