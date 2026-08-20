//! Stable C ABI consumed by the pinned QEMU TCTI provider.
//!
//! All structs are size/version tagged. Paths and callback buffers are borrowed
//! only for the duration documented in `include/rish_tcti_provider.h`.

use std::ffi::c_void;

pub const ABI_VERSION_V1: u32 = 1;
pub const STATUS_OK: i32 = 0;

pub const STRING_16: usize = 16;
pub const STRING_32: usize = 32;
pub const STRING_41: usize = 41;
pub const STRING_65: usize = 65;

pub const FEATURE_TCTI: u64 = 1 << 0;
pub const FEATURE_FULL_SYSTEM: u64 = 1 << 1;
pub const FEATURE_X86_64: u64 = 1 << 2;
pub const FEATURE_BOUNDED_RUN: u64 = 1 << 3;
pub const FEATURE_CANCEL_POLL: u64 = 1 << 4;
pub const FEATURE_SERIAL_16550: u64 = 1 << 5;
pub const FEATURE_VIRTIO_BLOCK: u64 = 1 << 6;
pub const FEATURE_INITRD: u64 = 1 << 7;
pub const FEATURE_USER_NETWORK: u64 = 1 << 8;

pub const FEATURE_JIT: u64 = 1 << 48;
pub const FEATURE_HVF: u64 = 1 << 49;
pub const FEATURE_KVM: u64 = 1 << 50;
pub const FEATURE_PRIVATE_API: u64 = 1 << 51;
pub const FEATURE_EXECUTABLE_MEMORY: u64 = 1 << 52;

pub const REQUIRED_FEATURES: u64 = FEATURE_TCTI
    | FEATURE_FULL_SYSTEM
    | FEATURE_X86_64
    | FEATURE_BOUNDED_RUN
    | FEATURE_CANCEL_POLL
    | FEATURE_SERIAL_16550
    | FEATURE_VIRTIO_BLOCK;

pub const FORBIDDEN_FEATURES: u64 =
    FEATURE_JIT | FEATURE_HVF | FEATURE_KVM | FEATURE_PRIVATE_API | FEATURE_EXECUTABLE_MEMORY;

pub const NETWORK_DISABLED: u32 = 0;
pub const NETWORK_USER_NAT: u32 = 1;

pub const MACHINE_RUNNING: u32 = 0;
pub const MACHINE_HALTED: u32 = 1;
pub const MACHINE_STOPPED: u32 = 2;
pub const MACHINE_FAULTED: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RishTctiSliceV1 {
    pub data: *const u8,
    pub len: usize,
}

impl RishTctiSliceV1 {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            data: std::ptr::null(),
            len: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RishTctiBuildInfoV1 {
    pub struct_size: usize,
    pub abi_version: u32,
    pub qemu_version: [u8; STRING_16],
    pub source_revision: [u8; STRING_41],
    pub build_id: [u8; STRING_65],
    pub guest_architecture: [u8; STRING_16],
    pub host_architecture: [u8; STRING_16],
    pub target_list: [u8; STRING_32],
    pub compiled_features: u64,
    pub min_memory_mib: u32,
    pub max_memory_mib: u32,
    pub max_vcpus: u32,
}

impl Default for RishTctiBuildInfoV1 {
    fn default() -> Self {
        Self {
            struct_size: std::mem::size_of::<Self>(),
            abi_version: ABI_VERSION_V1,
            qemu_version: [0; STRING_16],
            source_revision: [0; STRING_41],
            build_id: [0; STRING_65],
            guest_architecture: [0; STRING_16],
            host_architecture: [0; STRING_16],
            target_list: [0; STRING_32],
            compiled_features: 0,
            min_memory_mib: 0,
            max_memory_mib: 0,
            max_vcpus: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RishTctiConfigV1 {
    pub struct_size: usize,
    pub abi_version: u32,
    pub memory_mib: u32,
    pub vcpus: u32,
    pub kernel_path: RishTctiSliceV1,
    pub initrd_path: RishTctiSliceV1,
    pub root_disk_path: RishTctiSliceV1,
    pub network_mode: u32,
}

pub type SerialWriteFn =
    unsafe extern "C" fn(context: *mut c_void, data: *const u8, len: usize) -> usize;
pub type SerialReadFn =
    unsafe extern "C" fn(context: *mut c_void, data: *mut u8, capacity: usize) -> usize;
pub type ShouldCancelFn = unsafe extern "C" fn(context: *mut c_void) -> u8;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RishTctiHostCallbacksV1 {
    pub struct_size: usize,
    pub abi_version: u32,
    pub context: *mut c_void,
    pub serial_write: Option<SerialWriteFn>,
    pub serial_read: Option<SerialReadFn>,
    pub should_cancel: Option<ShouldCancelFn>,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RishTctiSnapshotV1 {
    pub struct_size: usize,
    pub abi_version: u32,
    pub state: u32,
    pub pc_valid: u8,
    pub reserved: [u8; 7],
    pub pc: u64,
    pub total_units: u64,
}

impl Default for RishTctiSnapshotV1 {
    fn default() -> Self {
        Self {
            struct_size: std::mem::size_of::<Self>(),
            abi_version: ABI_VERSION_V1,
            state: MACHINE_STOPPED,
            pc_valid: 0,
            reserved: [0; 7],
            pc: 0,
            total_units: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RishTctiRunResultV1 {
    pub struct_size: usize,
    pub abi_version: u32,
    pub executed_units: u64,
    pub snapshot: RishTctiSnapshotV1,
}

impl Default for RishTctiRunResultV1 {
    fn default() -> Self {
        Self {
            struct_size: std::mem::size_of::<Self>(),
            abi_version: ABI_VERSION_V1,
            executed_units: 0,
            snapshot: RishTctiSnapshotV1::default(),
        }
    }
}

pub type BuildInfoFn = unsafe extern "C" fn(output: *mut RishTctiBuildInfoV1) -> i32;
pub type CreateFn = unsafe extern "C" fn(
    config: *const RishTctiConfigV1,
    callbacks: *const RishTctiHostCallbacksV1,
    output: *mut *mut c_void,
) -> i32;
pub type SnapshotFn =
    unsafe extern "C" fn(handle: *mut c_void, output: *mut RishTctiSnapshotV1) -> i32;
pub type RunQuantumFn = unsafe extern "C" fn(
    handle: *mut c_void,
    max_units: u64,
    output: *mut RishTctiRunResultV1,
) -> i32;
pub type RequestStopFn = unsafe extern "C" fn(handle: *mut c_void) -> i32;
pub type DestroyFn = unsafe extern "C" fn(handle: *mut c_void);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RishTctiApiV1 {
    pub struct_size: usize,
    pub abi_version: u32,
    pub build_info: Option<BuildInfoFn>,
    pub create: Option<CreateFn>,
    pub snapshot: Option<SnapshotFn>,
    pub run_quantum: Option<RunQuantumFn>,
    pub request_stop: Option<RequestStopFn>,
    pub destroy: Option<DestroyFn>,
}
