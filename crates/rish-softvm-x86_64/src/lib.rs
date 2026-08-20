//! Fail-closed Rust adapter for a no-JIT x86-64 full-system interpreter.
//!
//! The production provider is a pinned UTM QEMU 10.0.2 TCTI build exposed
//! through the versioned C ABI in [`abi`]. This crate does not embed QEMU, does
//! not silently fall back to TCG JIT or a hypervisor, and does not claim that
//! Linux is available when the provider or guest control channel is absent.

pub mod abi;

mod artifacts;
mod config;
mod engine;
mod error;
mod machine;
mod provider;
mod pure_rust;
mod serial;
mod transport;
mod worker;

pub use artifacts::{ArtifactFile, KernelFormat, ValidatedArtifacts};
pub use config::{EngineLimits, TctiSourceLock};
pub use engine::{DEFAULT_GUEST_COMMAND_LINE, X86_64SoftwareEngine};
pub use error::SoftVmError;
pub use machine::{BootSnapshot, RunReport, X86_64Machine};
pub use provider::{
    MachineProvider, MachineState, ProviderBuildInfo, ProviderKind, ProviderMachine,
    ProviderRequest, ProviderRun, ProviderSnapshot, TctiProvider,
};
pub use pure_rust::{
    PURE_RUST_MAX_MEMORY_MIB, PURE_RUST_MIN_MEMORY_MIB, PURE_RUST_TARGET, PureRustProvider,
};
pub use serial::{ControlChannel, ProviderIo};
pub use transport::SerialGuestTransport;

/// Result type used by the AMD64 software VM adapter.
pub type Result<T> = std::result::Result<T, SoftVmError>;
