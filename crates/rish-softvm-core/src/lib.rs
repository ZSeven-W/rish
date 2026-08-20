//! Pure-Rust no-JIT x86_64 full-system interpreter.
//!
//! This crate is the CPU, memory, and chipset core for the rish software VM.
//! It is a bytecode-free interpreter: every instruction is decoded with
//! iced-x86 and executed by Rust code, with deterministic instruction
//! counting and no executable-memory translation. It runs the real pinned
//! Alpine Linux guest inside iOS and Android apps.

pub mod arch;
pub mod boot;
pub mod cpu;
pub mod devices;
mod error;
pub mod memory;
mod ops;

pub use boot::bzimage;
pub use cpu::{Cpu, register_index};
pub use error::CpuError;
pub use memory::Memory;
