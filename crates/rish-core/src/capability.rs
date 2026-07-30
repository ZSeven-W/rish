use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{Platform, PrivilegeMode};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    CommandOffload,
    OciImages,
    LinuxElf,
    VirtualFilesystem,
    ProcessNamespace,
    UserNamespace,
    MountNamespace,
    NetworkNamespace,
    UtsNamespace,
    IpcNamespace,
    CgroupNamespace,
    TimeNamespace,
    CgroupsV2,
    PrivilegedContainers,
    KernelModules,
    DeviceNodes,
    Systemd,
    NestedContainers,
    PortForwarding,
    RawSockets,
    TunTap,
    FullVirtualMachine,
}

impl fmt::Display for Capability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let encoded = serde_json::to_value(self).map_err(|_| fmt::Error)?;
        formatter.write_str(encoded.as_str().ok_or(fmt::Error)?)
    }
}

impl FromStr for Capability {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_json::from_value(serde_json::Value::String(value.to_owned()))
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportLevel {
    Native,
    Virtualized,
    Bridged,
    Emulated,
    Planned,
    Unavailable,
}

impl SupportLevel {
    #[must_use]
    pub const fn is_runnable(self) -> bool {
        matches!(
            self,
            Self::Native | Self::Virtualized | Self::Bridged | Self::Emulated
        )
    }

    #[must_use]
    pub const fn provides_kernel_semantics(self) -> bool {
        matches!(self, Self::Native | Self::Virtualized)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapabilityRequirement {
    pub capability: Capability,
    #[serde(default)]
    pub kernel_semantics_required: bool,
}

impl CapabilityRequirement {
    #[must_use]
    pub const fn any(capability: Capability) -> Self {
        Self {
            capability,
            kernel_semantics_required: false,
        }
    }

    #[must_use]
    pub const fn kernel(capability: Capability) -> Self {
        Self {
            capability,
            kernel_semantics_required: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapabilityProfile {
    pub platform: Platform,
    pub privilege: PrivilegeMode,
    pub backend: String,
    levels: BTreeMap<Capability, SupportLevel>,
}

impl CapabilityProfile {
    #[must_use]
    pub fn new(platform: Platform, privilege: PrivilegeMode, backend: impl Into<String>) -> Self {
        Self {
            platform,
            privilege,
            backend: backend.into(),
            levels: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with(mut self, capability: Capability, level: SupportLevel) -> Self {
        self.levels.insert(capability, level);
        self
    }

    #[must_use]
    pub fn level(&self, capability: Capability) -> SupportLevel {
        self.levels
            .get(&capability)
            .copied()
            .unwrap_or(SupportLevel::Unavailable)
    }

    pub fn require(
        &self,
        requirement: &CapabilityRequirement,
    ) -> Result<SupportLevel, MissingCapability> {
        let actual = self.level(requirement.capability);
        let satisfied = if requirement.kernel_semantics_required {
            actual.provides_kernel_semantics()
        } else {
            actual.is_runnable()
        };

        if satisfied {
            Ok(actual)
        } else {
            Err(MissingCapability {
                capability: requirement.capability,
                actual,
                kernel_semantics_required: requirement.kernel_semantics_required,
            })
        }
    }

    pub fn require_all(
        &self,
        requirements: &[CapabilityRequirement],
    ) -> Result<(), MissingCapability> {
        for requirement in requirements {
            self.require(requirement)?;
        }
        Ok(())
    }

    #[must_use]
    pub fn levels(&self) -> &BTreeMap<Capability, SupportLevel> {
        &self.levels
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MissingCapability {
    pub capability: Capability,
    pub actual: SupportLevel,
    pub kernel_semantics_required: bool,
}

impl fmt::Display for MissingCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.kernel_semantics_required {
            write!(
                formatter,
                "{} requires real kernel semantics, actual level is {:?}",
                self.capability, self.actual
            )
        } else {
            write!(
                formatter,
                "{} is not runnable, actual level is {:?}",
                self.capability, self.actual
            )
        }
    }
}

impl std::error::Error for MissingCapability {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planned_capability_does_not_pass_runtime_negotiation() {
        let profile =
            CapabilityProfile::new(Platform::Ios, PrivilegeMode::AppSandbox, "portable-offload")
                .with(Capability::CgroupsV2, SupportLevel::Planned);

        assert!(
            profile
                .require(&CapabilityRequirement::any(Capability::CgroupsV2))
                .is_err()
        );
    }

    #[test]
    fn native_requirement_rejects_semantic_emulation() {
        let profile =
            CapabilityProfile::new(Platform::Ios, PrivilegeMode::AppSandbox, "portable-offload")
                .with(Capability::Systemd, SupportLevel::Emulated);

        assert!(
            profile
                .require(&CapabilityRequirement::kernel(Capability::Systemd))
                .is_err()
        );
    }

    #[test]
    fn virtual_machine_satisfies_kernel_semantics() {
        let profile = CapabilityProfile::new(Platform::Ios, PrivilegeMode::VmGuest, "full-vm")
            .with(Capability::Systemd, SupportLevel::Virtualized);

        assert!(
            profile
                .require(&CapabilityRequirement::kernel(Capability::Systemd))
                .is_ok()
        );
    }
}
