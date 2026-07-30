//! Evidence-gated Linux virtual-machine backend.
//!
//! A VM capability profile is deliberately unavailable before an engine probe,
//! successful boot, versioned guest handshake, and kernel-contract validation.
//! The only public path to a runnable profile is [`VmCandidate::boot`].

mod boot;
mod config;
mod contract;
mod error;
mod mapping;

pub use boot::{
    BootedVm, GuestChannel, GuestSession, VerifiedVmProfile, VmCandidate, VmEngine, VmProbe,
};
pub use config::{VmAcceleration, VmConfig, VmDevice, VmNetworkMode};
pub use contract::{
    GuestKernelContract, GuestKernelEvidence, KernelContractReport, KernelEvidenceSource,
};
pub use error::{GuestCapabilityState, VmError};
