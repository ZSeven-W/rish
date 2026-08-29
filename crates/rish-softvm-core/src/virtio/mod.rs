//! virtio-mmio block device emulation.
//!
//! # Transport choice: virtio-mmio, not virtio-pci
//!
//! The pure-Rust interpreter emulates the block device behind the virtio-mmio
//! transport instead of virtio-pci. Rationale:
//!
//! - virtio-mmio is a flat 512-byte MMIO register file plus one interrupt
//!   line. It needs no PCI config space, no BAR mapping, no capability-list
//!   parsing, and no MSI/MSI-X; the split virtqueue lives entirely in
//!   ordinary guest RAM.
//! - The pinned virt kernel has the transport built in
//!   (`CONFIG_VIRTIO_MMIO=y`) and supports command-line device discovery
//!   (`CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y`): the provider appends
//!   `virtio_mmio.device=1K@0xfebf0000:10` to the kernel command line and
//!   the guest probes the register file at boot. virtio-pci would instead
//!   need PCI bus enumeration (config space at 0xCF8/0xCFC plus either the
//!   ACPI MCFG or legacy BARs) for the guest to find the device at all.
//! - The device's IRQ rides the existing I/O APIC pin 10 (identity-mapped
//!   from the ISA IRQ in the published MADT), so no new interrupt
//!   infrastructure is needed.
//!
//! The block *driver* itself (`virtio_blk`) is a kernel module in the
//! pinned guest; the container initramfs bakes it in and insmods it, see
//! `guest/x86_64/build-container-initramfs.sh`.
//!
//! # Implemented scope
//!
//! - Modern (v2) virtio-mmio register file: magic, version, IDs, feature
//!   negotiation, queue configuration, status, notify, interrupt ack, and
//!   the block config space (capacity, size/seg limits, block size).
//! - One split virtqueue with fail-closed bounds checking: descriptor,
//!   available, and used rings are walked entirely inside guest RAM with
//!   checked arithmetic; a malformed chain latches a device fault instead of
//!   panicking or reading past the end of RAM.
//! - virtio-blk IN (read) and OUT (write) requests with per-request
//!   sector-range validation (requests past the backend capacity complete
//!   with `VIRTIO_BLK_S_IOERR`), plus GET_ID (device serial).
//!
//! # Explicitly not implemented
//!
//! - No FLUSH (write barriers), no DISCARD, no WRITE_ZEROES: the matching
//!   feature bits are not offered, so a conforming driver never sends them;
//!   if one does anyway the request completes with `VIRTIO_BLK_S_UNSUPP`.
//! - No multi-queue (VIRTIO_BLK_F_MQ), no geometry/topology reporting
//!   (VIRTIO_BLK_F_GEOMETRY / VIRTIO_BLK_F_TOPOLOGY), no size_max, no
//!   VIRTIO_F_ACCESS_PLATFORM, no packed virtqueues, no virtio-iommu.
//! - A single queue of at most 128 entries; device-reset semantics are
//!   register-level only (there is no live-migration state).
//! - A latched fault (guest accessed memory outside its RAM) is sticky until
//!   the machine restarts; the device then stops processing kicks rather
//!   than guessing.

mod block;
mod queue;

pub mod backend;

pub use backend::{BlockBackend, FileBlockBackend};
pub use block::VirtioMmioBlk;
pub use queue::GuestMemory;

/// MMIO register window base. Clear of guest RAM (the pure-Rust provider
/// caps memory at 1024 MiB), the local APIC (0xFEE0_0000), and the I/O APIC
/// (0xFEC0_0000); matches QEMU's virtio-mmio placement on pc machines.
pub const VIRTIO_MMIO_BASE: u64 = 0xFEBF_0000;

/// Byte size of the register file: spec registers plus the block config
/// space (0x000..=0x1FF).
pub const VIRTIO_MMIO_REGISTER_BYTES: u64 = 0x200;

/// Total MMIO window accepted by the memory dispatch. The command line
/// advertises a 1 KiB region; reads past the register file return zero.
pub const VIRTIO_MMIO_WINDOW_BYTES: u64 = 0x400;

/// IRQ line the device raises: I/O APIC pin 10, matching the command-line
/// fragment below.
pub const VIRTIO_IRQ: u8 = 10;

/// Command-line fragment the pure-Rust provider appends when it attaches the
/// block device. Format: size (KiB) @ base address : irq.
pub const VIRTIO_CMDLINE_FRAGMENT: &str = "virtio_mmio.device=1K@0xfebf0000:10";

/// Error surface for the virtio device. Everything fails closed: a malformed
/// queue or an out-of-RAM access stops the device instead of touching memory
/// it does not own.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VirtioError {
    /// A guest-physical access escaped guest RAM.
    OutOfBounds { address: u64, bytes: u64 },
    /// The queue rings or a request chain are malformed in a way that cannot
    /// be serviced safely.
    BadQueue(&'static str),
    /// The host block backend rejected an operation (I/O error or an invalid
    /// backend size).
    Backend(String),
}

impl std::fmt::Display for VirtioError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfBounds { address, bytes } => {
                write!(
                    formatter,
                    "access of {bytes} bytes at {address:#x} is outside guest RAM"
                )
            }
            Self::BadQueue(message) => formatter.write_str(message),
            Self::Backend(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for VirtioError {}
