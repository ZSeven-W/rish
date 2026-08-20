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
    /// Linux kernel command line. An empty value lets the engine fall back
    /// to its pinned guest default (e.g. the docker diagnostic cmdline).
    #[serde(default)]
    pub command_line: String,
}

impl VmConfig {
    pub fn validate(&self) -> Result<(), VmError> {
        let minimum_memory_mib = match (self.architecture.as_str(), self.acceleration) {
            ("aarch64", _) => 256,
            ("x86_64", VmAcceleration::Interpreter) => 128,
            ("x86_64", _) => {
                return Err(VmError::InvalidConfig(
                    "x86_64 guests currently require software interpreter acceleration".to_owned(),
                ));
            }
            _ => {
                return Err(VmError::InvalidConfig(
                    "the mobile backend currently supports aarch64 or interpreted x86_64"
                        .to_owned(),
                ));
            }
        };
        if self.vcpus == 0 {
            return Err(VmError::InvalidConfig(
                "at least one virtual CPU is required".to_owned(),
            ));
        }
        if self.memory_mib < minimum_memory_mib {
            return Err(VmError::InvalidConfig(format!(
                "at least {minimum_memory_mib} MiB of guest memory is required"
            )));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config(architecture: &str, acceleration: VmAcceleration, memory_mib: u32) -> VmConfig {
        VmConfig {
            architecture: architecture.to_owned(),
            vcpus: 1,
            memory_mib,
            kernel_path: "/kernel".to_owned(),
            initrd_path: None,
            root_disk_path: "/root.img".to_owned(),
            acceleration,
            devices: vec![VmDevice::Console],
            command_line: String::new(),
        }
    }

    #[test]
    fn interpreted_x86_64_accepts_bounded_mobile_memory() {
        config("x86_64", VmAcceleration::Interpreter, 128)
            .validate()
            .unwrap();
    }

    #[test]
    fn x86_64_rejects_hardware_acceleration_labels() {
        let error = config("x86_64", VmAcceleration::Kvm, 256)
            .validate()
            .unwrap_err();
        assert!(matches!(error, VmError::InvalidConfig(_)));
    }
}
