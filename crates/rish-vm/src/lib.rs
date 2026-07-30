use std::collections::BTreeSet;

use rish_core::{
    Capability, CapabilityProfile, GuestCommand, HostReply, Platform, PrivilegeMode, SupportLevel,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GuestKernelContract {
    pub required_kconfig: BTreeSet<String>,
    pub required_capabilities: BTreeSet<Capability>,
}

impl GuestKernelContract {
    #[must_use]
    pub fn container_host() -> Self {
        let required_kconfig = [
            "CONFIG_NAMESPACES",
            "CONFIG_PID_NS",
            "CONFIG_USER_NS",
            "CONFIG_UTS_NS",
            "CONFIG_IPC_NS",
            "CONFIG_NET_NS",
            "CONFIG_CGROUPS",
            "CONFIG_CGROUP_BPF",
            "CONFIG_CGROUP_CPUACCT",
            "CONFIG_CGROUP_DEVICE",
            "CONFIG_CGROUP_FREEZER",
            "CONFIG_CGROUP_PIDS",
            "CONFIG_MEMCG",
            "CONFIG_BLK_CGROUP",
            "CONFIG_CPUSETS",
            "CONFIG_SECCOMP",
            "CONFIG_SECCOMP_FILTER",
            "CONFIG_OVERLAY_FS",
            "CONFIG_MODULES",
            "CONFIG_DEVTMPFS",
            "CONFIG_DEVTMPFS_MOUNT",
            "CONFIG_TUN",
            "CONFIG_VETH",
            "CONFIG_BRIDGE",
            "CONFIG_NETFILTER",
            "CONFIG_NF_TABLES",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();

        let required_capabilities = [
            Capability::LinuxElf,
            Capability::ProcessNamespace,
            Capability::UserNamespace,
            Capability::MountNamespace,
            Capability::NetworkNamespace,
            Capability::UtsNamespace,
            Capability::IpcNamespace,
            Capability::CgroupsV2,
            Capability::PrivilegedContainers,
            Capability::KernelModules,
            Capability::DeviceNodes,
            Capability::Systemd,
            Capability::NestedContainers,
            Capability::PortForwarding,
            Capability::RawSockets,
            Capability::TunTap,
        ]
        .into_iter()
        .collect();

        Self {
            required_kconfig,
            required_capabilities,
        }
    }

    #[must_use]
    pub fn validate_kconfig(&self, enabled: &BTreeSet<String>) -> KernelContractReport {
        KernelContractReport {
            missing: self.required_kconfig.difference(enabled).cloned().collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct KernelContractReport {
    pub missing: BTreeSet<String>,
}

impl KernelContractReport {
    #[must_use]
    pub fn is_satisfied(&self) -> bool {
        self.missing.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmProbe {
    pub available: bool,
    pub acceleration: VmAcceleration,
    pub reason: Option<String>,
}

pub trait GuestChannel: Send + Sync {
    fn execute(&self, command: &GuestCommand) -> Result<HostReply, VmError>;
}

pub trait VmEngine: Send + Sync {
    fn probe(&self) -> VmProbe;
    fn boot(&self, config: &VmConfig) -> Result<Box<dyn GuestChannel>, VmError>;
}

#[must_use]
pub fn full_vm_profile(platform: Platform) -> CapabilityProfile {
    let mut profile = CapabilityProfile::new(platform, PrivilegeMode::VmGuest, "full-vm")
        .with(Capability::CommandOffload, SupportLevel::Bridged)
        .with(Capability::OciImages, SupportLevel::Virtualized)
        .with(Capability::VirtualFilesystem, SupportLevel::Virtualized)
        .with(Capability::FullVirtualMachine, SupportLevel::Virtualized);

    for capability in GuestKernelContract::container_host().required_capabilities {
        profile = profile.with(capability, SupportLevel::Virtualized);
    }
    profile
}

#[derive(Debug, Error)]
pub enum VmError {
    #[error("invalid VM configuration: {0}")]
    InvalidConfig(String),

    #[error("VM engine is unavailable: {0}")]
    Unavailable(String),

    #[error("guest operation failed: {0}")]
    Guest(String),
}

#[cfg(test)]
mod tests {
    use rish_core::CapabilityRequirement;

    use super::*;

    #[test]
    fn full_vm_satisfies_real_kernel_container_requirements() {
        let profile = full_vm_profile(Platform::Ios);

        for capability in [
            Capability::CgroupsV2,
            Capability::KernelModules,
            Capability::NestedContainers,
            Capability::NetworkNamespace,
        ] {
            profile
                .require(&CapabilityRequirement::kernel(capability))
                .unwrap();
        }
    }

    #[test]
    fn guest_contract_reports_missing_kernel_configuration() {
        let enabled = BTreeSet::from(["CONFIG_NAMESPACES".to_owned()]);
        let report = GuestKernelContract::container_host().validate_kconfig(&enabled);

        assert!(!report.is_satisfied());
        assert!(report.missing.contains("CONFIG_CGROUPS"));
        assert!(report.missing.contains("CONFIG_NET_NS"));
    }
}
