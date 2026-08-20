use std::time::Duration;

use crate::SoftVmError;

pub const GUEST_ARCHITECTURE: &str = "x86_64";
pub const QEMU_VERSION: &str = "10.0.2";
pub const QEMU_UTM_TAG: &str = "v10.0.2-utm";
pub const QEMU_UTM_COMMIT: &str = "37ba092d59aff24900dfd0d5e01d4ed68441ba07";
pub const QEMU_SOURCE_ARCHIVE: &str = "qemu-10.0.2-utm.tar.xz";
pub const QEMU_SOURCE_ARCHIVE_BYTES: u64 = 136_751_908;
pub const QEMU_SOURCE_ARCHIVE_SHA256: &str =
    "f1d7357547a71ae3339a115d5c8f2b72e3b0089531d67c2aca43326d320ac6ca";
pub const QEMU_TARGET_LIST: &str = "x86_64-softmmu";
pub const TCTI_HOST_ARCHITECTURE: &str = "aarch64";

/// Immutable source inputs for the reviewed TCTI provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TctiSourceLock {
    pub version: &'static str,
    pub tag: &'static str,
    pub commit: &'static str,
    pub archive: &'static str,
    pub archive_bytes: u64,
    pub archive_sha256: &'static str,
    pub target_list: &'static str,
    pub host_architecture: &'static str,
}

impl Default for TctiSourceLock {
    fn default() -> Self {
        Self {
            version: QEMU_VERSION,
            tag: QEMU_UTM_TAG,
            commit: QEMU_UTM_COMMIT,
            archive: QEMU_SOURCE_ARCHIVE,
            archive_bytes: QEMU_SOURCE_ARCHIVE_BYTES,
            archive_sha256: QEMU_SOURCE_ARCHIVE_SHA256,
            target_list: QEMU_TARGET_LIST,
            host_architecture: TCTI_HOST_ARCHITECTURE,
        }
    }
}

/// Host-side resource and latency limits for one software VM.
#[derive(Clone, Debug)]
pub struct EngineLimits {
    pub max_kernel_bytes: u64,
    pub max_initrd_bytes: u64,
    pub max_root_disk_bytes: u64,
    pub max_console_bytes: usize,
    pub max_control_bytes: usize,
    pub max_units_per_run: u64,
    pub max_units_per_request: u64,
    pub provider_quantum_units: u64,
    pub startup_timeout: Duration,
}

impl Default for EngineLimits {
    fn default() -> Self {
        Self {
            max_kernel_bytes: 128 * 1024 * 1024,
            max_initrd_bytes: 512 * 1024 * 1024,
            max_root_disk_bytes: 16 * 1024 * 1024 * 1024,
            max_console_bytes: 1024 * 1024,
            max_control_bytes: 16 * 1024 * 1024,
            max_units_per_run: 10_000_000,
            max_units_per_request: 2_000_000_000,
            provider_quantum_units: 50_000,
            startup_timeout: Duration::from_secs(20),
        }
    }
}

impl EngineLimits {
    pub(crate) fn validate(&self) -> Result<(), SoftVmError> {
        if self.max_kernel_bytes == 0
            || self.max_initrd_bytes == 0
            || self.max_root_disk_bytes == 0
            || self.max_console_bytes == 0
            || self.max_control_bytes == 0
            || self.max_units_per_run == 0
            || self.max_units_per_request == 0
            || self.provider_quantum_units == 0
            || self.startup_timeout.is_zero()
        {
            return Err(SoftVmError::InvalidConfig(
                "all x86_64 TCTI engine limits must be non-zero".to_owned(),
            ));
        }
        if self.provider_quantum_units > self.max_units_per_run {
            return Err(SoftVmError::InvalidConfig(
                "provider quantum cannot exceed the per-run unit limit".to_owned(),
            ));
        }
        if self.max_units_per_request < self.max_units_per_run {
            return Err(SoftVmError::InvalidConfig(
                "request unit budget cannot be smaller than one full run".to_owned(),
            ));
        }
        Ok(())
    }
}
