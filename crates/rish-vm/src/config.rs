use rish_core::Platform;
use serde::{Deserialize, Serialize};

use crate::VmError;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmAcceleration {
    Interpreter,
    AndroidVirtualizationFramework,
    Kvm,
    OemHypervisor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmNetworkMode {
    Disabled,
    UserNat,
    Tap,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum VmDevice {
    Console,
    Random,
    Block {
        path: String,
        read_only: bool,
    },
    Network {
        mode: VmNetworkMode,
    },
    Vsock {
        guest_cid: u32,
    },
    SharedFilesystem {
        host_path: String,
        tag: String,
        read_only: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmConfig {
    pub architecture: String,
    pub vcpus: u8,
    pub memory_mib: u32,
    pub kernel_path: String,
    pub initrd_path: Option<String>,
    pub root_disk_path: String,
    pub acceleration: VmAcceleration,
    pub devices: Vec<VmDevice>,
}

impl VmConfig {
    pub fn validate(&self) -> Result<(), VmError> {
        if self.architecture != "aarch64" {
            return Err(VmError::InvalidConfig(
                "the mobile backend currently requires aarch64".to_owned(),
            ));
        }
        if self.vcpus == 0 {
            return Err(VmError::InvalidConfig(
                "at least one virtual CPU is required".to_owned(),
            ));
        }
        if self.memory_mib < 256 {
            return Err(VmError::InvalidConfig(
                "at least 256 MiB of guest memory is required".to_owned(),
            ));
        }
        if self.kernel_path.is_empty() || self.root_disk_path.is_empty() {
            return Err(VmError::InvalidConfig(
                "kernel and root disk paths are required".to_owned(),
            ));
        }
        if !self
            .devices
            .iter()
            .any(|device| matches!(device, VmDevice::Console))
        {
            return Err(VmError::InvalidConfig(
                "a console device is required for recovery".to_owned(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn platform_name(platform: Platform) -> &'static str {
    match platform {
        Platform::Ios => "ios",
        Platform::Android => "android",
        Platform::Harmony => "harmony",
        Platform::Linux => "linux",
    }
}
