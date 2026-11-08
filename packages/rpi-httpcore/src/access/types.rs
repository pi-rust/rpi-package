use std::{fmt, time::Instant};

/// Monotonically increasing sharing generation.
pub type Generation = u64;

/// An opaque, non-secret identity associated with an authorization lease.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PrincipalId(String);

impl PrincipalId {
    /// Create a principal identifier. Empty identifiers are not valid.
    pub fn new(value: impl Into<String>) -> Result<Self, AccessError> {
        let value = value.into();
        if value.is_empty() {
            return Err(AccessError::InvalidPrincipal);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A revocable authorization lease.
///
/// The bearer credential is stored privately and is not included in `Debug` or
/// serialization. The explicit accessor is only for placing it in a cookie or
/// connection context; callers must never log it.
#[derive(Clone)]
pub struct AuthLease {
    pub(crate) credential: String,
    pub(crate) principal: PrincipalId,
    pub(crate) generation: Generation,
    pub(crate) expires_at: Instant,
}

impl fmt::Debug for AuthLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthLease")
            .field("principal", &self.principal)
            .field("generation", &self.generation)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl AuthLease {
    /// Return the opaque bearer value for a cookie or connection context.
    /// Callers must not log it, put it in a URL, or echo it in errors.
    pub fn credential(&self) -> &str {
        &self.credential
    }

    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    pub fn generation(&self) -> Generation {
        self.generation
    }

    pub fn expires_at(&self) -> Instant {
        self.expires_at
    }

    pub fn is_expired(&self) -> bool {
        self.expires_at <= Instant::now()
    }
}

/// Current sharing status visible to admission and connection code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SharingState {
    pub enabled: bool,
    pub generation: Generation,
}

/// Why a lease or principal could not be authorized.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessError {
    InvalidPrincipal,
    SharingDisabled,
    Expired,
    Revoked,
    GenerationMismatch,
    CapacityExceeded,
}
