use std::collections::BTreeSet;

use rish_core::Capability;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GuestCapabilityState {
    Available,
    Missing,
    Restricted,
    Unavailable,
    UnsupportedVersion(u16),
    Unmapped,
}

#[derive(Debug, Error)]
pub enum VmError {
    #[error("invalid VM configuration: {0}")]
    InvalidConfig(String),

    #[error("invalid guest kernel contract: {0}")]
    InvalidContract(String),

    #[error("VM engine is unavailable: {0}")]
    Unavailable(String),

    #[error("VM acceleration {0:?} was not reported by the engine probe")]
    UnsupportedAcceleration(crate::VmAcceleration),

    #[error("VM boot failed: {0}")]
    Boot(String),

    #[error("guest protocol verification failed: {0}")]
    Protocol(String),

    #[error("guest rejected the bootstrap handshake: {0}")]
    HandshakeRejected(String),

    #[error("invalid guest kernel evidence: {0}")]
    InvalidKernelEvidence(String),

    #[error("kernel evidence belongs to session {actual}, expected {expected}")]
    KernelEvidenceSessionMismatch { expected: String, actual: String },

    #[error("kernel evidence is for release {actual}, expected running release {expected}")]
    KernelReleaseMismatch { expected: String, actual: String },

    #[error("guest kernel contract is missing required symbols: {missing:?}")]
    KernelContractUnsatisfied { missing: BTreeSet<String> },

    #[error("required guest capability {capability} is not available: {state:?}")]
    RequiredGuestCapability {
        capability: Capability,
        state: GuestCapabilityState,
    },

    #[error("guest operation failed: {0}")]
    Guest(String),
}
