use rish_core::{Capability, CapabilityProfile, Platform, PrivilegeMode, SupportLevel};

#[must_use]
pub fn portable_offload_profile(platform: Platform, privilege: PrivilegeMode) -> CapabilityProfile {
    CapabilityProfile::new(platform, privilege, "portable-offload")
        .with(Capability::CommandOffload, SupportLevel::Bridged)
        .with(Capability::OciImages, SupportLevel::Emulated)
        .with(Capability::VirtualFilesystem, SupportLevel::Emulated)
        .with(Capability::ProcessNamespace, SupportLevel::Emulated)
        .with(Capability::UserNamespace, SupportLevel::Emulated)
        .with(Capability::MountNamespace, SupportLevel::Emulated)
        .with(Capability::NetworkNamespace, SupportLevel::Emulated)
        .with(Capability::UtsNamespace, SupportLevel::Emulated)
        .with(Capability::IpcNamespace, SupportLevel::Emulated)
        .with(Capability::CgroupsV2, SupportLevel::Emulated)
        .with(Capability::DeviceNodes, SupportLevel::Bridged)
        .with(Capability::Systemd, SupportLevel::Emulated)
        .with(Capability::PortForwarding, SupportLevel::Bridged)
        .with(Capability::LinuxElf, SupportLevel::Unavailable)
        .with(Capability::PrivilegedContainers, SupportLevel::Unavailable)
        .with(Capability::KernelModules, SupportLevel::Unavailable)
        .with(Capability::NestedContainers, SupportLevel::Unavailable)
        .with(Capability::RawSockets, SupportLevel::Unavailable)
        .with(Capability::TunTap, SupportLevel::Unavailable)
        .with(Capability::FullVirtualMachine, SupportLevel::Planned)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct KernelProbe {
    pub linux_elf: bool,
    pub process_namespace: bool,
    pub user_namespace: bool,
    pub mount_namespace: bool,
    pub network_namespace: bool,
    pub uts_namespace: bool,
    pub ipc_namespace: bool,
    pub cgroups_v2: bool,
    pub privileged_containers: bool,
    pub kernel_modules: bool,
    pub device_nodes: bool,
    pub systemd: bool,
    pub nested_containers: bool,
    pub raw_sockets: bool,
    pub tun_tap: bool,
}

#[must_use]
pub fn native_linux_profile(
    platform: Platform,
    privilege: PrivilegeMode,
    probe: &KernelProbe,
) -> CapabilityProfile {
    let mut profile = CapabilityProfile::new(platform, privilege, "native-linux")
        .with(Capability::CommandOffload, SupportLevel::Bridged)
        .with(Capability::OciImages, SupportLevel::Native)
        .with(Capability::VirtualFilesystem, SupportLevel::Native)
        .with(Capability::PortForwarding, SupportLevel::Native);

    for (capability, available) in [
        (Capability::LinuxElf, probe.linux_elf),
        (Capability::ProcessNamespace, probe.process_namespace),
        (Capability::UserNamespace, probe.user_namespace),
        (Capability::MountNamespace, probe.mount_namespace),
        (Capability::NetworkNamespace, probe.network_namespace),
        (Capability::UtsNamespace, probe.uts_namespace),
        (Capability::IpcNamespace, probe.ipc_namespace),
        (Capability::CgroupsV2, probe.cgroups_v2),
        (
            Capability::PrivilegedContainers,
            probe.privileged_containers,
        ),
        (Capability::KernelModules, probe.kernel_modules),
        (Capability::DeviceNodes, probe.device_nodes),
        (Capability::Systemd, probe.systemd),
        (Capability::NestedContainers, probe.nested_containers),
        (Capability::RawSockets, probe.raw_sockets),
        (Capability::TunTap, probe.tun_tap),
    ] {
        profile = profile.with(
            capability,
            if available {
                SupportLevel::Native
            } else {
                SupportLevel::Unavailable
            },
        );
    }

    profile.with(Capability::FullVirtualMachine, SupportLevel::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_profile_never_claims_privileged_container_support() {
        let profile = portable_offload_profile(Platform::Ios, PrivilegeMode::AppSandbox);

        assert_eq!(
            profile.level(Capability::PrivilegedContainers),
            SupportLevel::Unavailable
        );
        assert_eq!(
            profile.level(Capability::ProcessNamespace),
            SupportLevel::Emulated
        );
    }

    #[test]
    fn native_profile_only_exposes_probed_kernel_features() {
        let profile = native_linux_profile(
            Platform::Android,
            PrivilegeMode::Root,
            &KernelProbe {
                linux_elf: true,
                cgroups_v2: true,
                ..KernelProbe::default()
            },
        );

        assert_eq!(profile.level(Capability::LinuxElf), SupportLevel::Native);
        assert_eq!(profile.level(Capability::CgroupsV2), SupportLevel::Native);
        assert_eq!(
            profile.level(Capability::NetworkNamespace),
            SupportLevel::Unavailable
        );
    }
}
