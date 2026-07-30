use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};

use rish_core::{
    Capability, CapabilityProfile, CapabilityRequirement, MissingCapability, Platform,
    PrivilegeMode, SupportLevel,
};

#[must_use]
pub fn portable_offload_profile(platform: Platform, privilege: PrivilegeMode) -> CapabilityProfile {
    CapabilityProfile::new(platform, privilege, "portable-offload")
        .with(Capability::CommandOffload, SupportLevel::Bridged)
        // Optional product handlers are unavailable until a live dispatcher
        // binds and attests them. The generic planner must not promise Docker,
        // device, or forwarding operations from a static platform name.
        .with(Capability::OciImages, SupportLevel::Unavailable)
        .with(Capability::VirtualFilesystem, SupportLevel::Emulated)
        .with(Capability::ProcessNamespace, SupportLevel::Emulated)
        .with(Capability::UserNamespace, SupportLevel::Emulated)
        .with(Capability::MountNamespace, SupportLevel::Emulated)
        .with(Capability::NetworkNamespace, SupportLevel::Emulated)
        .with(Capability::UtsNamespace, SupportLevel::Emulated)
        .with(Capability::IpcNamespace, SupportLevel::Emulated)
        .with(Capability::CgroupNamespace, SupportLevel::Emulated)
        .with(Capability::TimeNamespace, SupportLevel::Emulated)
        .with(Capability::CgroupsV2, SupportLevel::Emulated)
        .with(Capability::DeviceNodes, SupportLevel::Unavailable)
        .with(Capability::Systemd, SupportLevel::Emulated)
        .with(Capability::PortForwarding, SupportLevel::Unavailable)
        .with(Capability::LinuxElf, SupportLevel::Unavailable)
        .with(Capability::PrivilegedContainers, SupportLevel::Unavailable)
        .with(Capability::KernelModules, SupportLevel::Unavailable)
        .with(Capability::NestedContainers, SupportLevel::Unavailable)
        .with(Capability::RawSockets, SupportLevel::Unavailable)
        .with(Capability::TunTap, SupportLevel::Unavailable)
        .with(Capability::FullVirtualMachine, SupportLevel::Planned)
}

/// Evidence-gated native-Linux capability token.
///
/// The fields are private, this type is not deserializable, and production
/// code can obtain one only through [`probe_native_linux`].
#[derive(Clone, Debug)]
pub struct VerifiedNativeProfile {
    profile: CapabilityProfile,
}

impl VerifiedNativeProfile {
    #[must_use]
    pub fn capabilities(&self) -> &CapabilityProfile {
        &self.profile
    }

    #[must_use]
    pub fn level(&self, capability: Capability) -> SupportLevel {
        self.profile.level(capability)
    }

    pub fn require(
        &self,
        requirement: &CapabilityRequirement,
    ) -> Result<SupportLevel, MissingCapability> {
        self.profile.require(requirement)
    }

    #[cfg(test)]
    pub(crate) fn for_test(profile: CapabilityProfile) -> Self {
        assert_eq!(profile.backend, "native-linux");
        Self { profile }
    }
}

#[derive(Debug)]
pub enum NativeProbeError {
    UnsupportedHost,
    EvidenceIo {
        path: &'static str,
        source: io::Error,
    },
    InvalidEvidence(&'static str),
    InsufficientAndroidPrivilege,
}

impl fmt::Display for NativeProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedHost => {
                formatter.write_str("native Linux probing is unavailable on this target")
            }
            Self::EvidenceIo { path, source } => {
                write!(
                    formatter,
                    "cannot read native Linux evidence from {path}: {source}"
                )
            }
            Self::InvalidEvidence(message) => {
                write!(formatter, "invalid native Linux evidence: {message}")
            }
            Self::InsufficientAndroidPrivilege => formatter
                .write_str("Android native Linux requires uid 0 with effective CAP_SYS_ADMIN"),
        }
    }
}

impl std::error::Error for NativeProbeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::EvidenceIo { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct NativeProbeEvidence {
    linux_elf: bool,
    process_namespace: bool,
    user_namespace: bool,
    mount_namespace: bool,
    network_namespace: bool,
    uts_namespace: bool,
    ipc_namespace: bool,
    cgroup_namespace: bool,
    time_namespace: bool,
    cgroups_v2: bool,
    privileged_containers: bool,
    kernel_modules: bool,
    device_nodes: bool,
    systemd: bool,
    nested_containers: bool,
    raw_sockets: bool,
    tun_tap: bool,
}

const CAP_SYS_ADMIN: u8 = 21;

/// Probes the current process and mints a non-forgeable native-Linux token.
///
/// No caller-provided platform, privilege, profile, or boolean probe result is
/// accepted. Optional features fail closed when their evidence is absent.
pub fn probe_native_linux() -> Result<VerifiedNativeProfile, NativeProbeError> {
    let platform = detected_platform()?;
    verify_linux_ostype()?;
    verify_current_executable_is_elf()?;

    let status = read_required("/proc/self/status")?;
    let (effective_uid, effective_capabilities) = parse_process_status(&status)?;
    let host_root = effective_uid == 0 && initial_root_mapping();
    if platform == Platform::Android
        && (!host_root || !has_capability(effective_capabilities, CAP_SYS_ADMIN))
    {
        return Err(NativeProbeError::InsufficientAndroidPrivilege);
    }

    let evidence = conservative_evidence(platform);
    let privilege = if host_root {
        PrivilegeMode::Root
    } else if effective_capabilities != 0 {
        PrivilegeMode::Elevated
    } else {
        PrivilegeMode::AppSandbox
    };
    Ok(VerifiedNativeProfile {
        profile: profile_from_evidence(platform, privilege, &evidence),
    })
}

fn detected_platform() -> Result<Platform, NativeProbeError> {
    if cfg!(target_os = "android") {
        return Ok(Platform::Android);
    }
    if cfg!(all(target_os = "linux", not(target_env = "ohos"))) {
        return Ok(Platform::Linux);
    }
    Err(NativeProbeError::UnsupportedHost)
}

fn verify_linux_ostype() -> Result<(), NativeProbeError> {
    if read_required("/proc/sys/kernel/ostype")?.trim() == "Linux" {
        Ok(())
    } else {
        Err(NativeProbeError::InvalidEvidence(
            "kernel ostype is not Linux",
        ))
    }
}

fn verify_current_executable_is_elf() -> Result<(), NativeProbeError> {
    let mut header = [0_u8; 64];
    File::open("/proc/self/exe")
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|source| NativeProbeError::EvidenceIo {
            path: "/proc/self/exe",
            source,
        })?;
    let expected_class = if cfg!(target_pointer_width = "64") {
        2
    } else {
        1
    };
    let expected_data = if cfg!(target_endian = "little") { 1 } else { 2 };
    let machine = match std::env::consts::ARCH {
        "x86" => 3,
        "x86_64" => 62,
        "arm" => 40,
        "aarch64" => 183,
        "riscv64" => 243,
        _ => {
            return Err(NativeProbeError::InvalidEvidence(
                "target architecture has no supported ELF machine",
            ));
        }
    };
    let read_u16 = |offset: usize| {
        let bytes = [header[offset], header[offset + 1]];
        if expected_data == 1 {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        }
    };
    let read_u32 = |offset: usize| {
        let bytes = [
            header[offset],
            header[offset + 1],
            header[offset + 2],
            header[offset + 3],
        ];
        if expected_data == 1 {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        }
    };
    let elf_type = read_u16(16);
    let elf_machine = read_u16(18);
    let expected_header_size = if expected_class == 2 { 64 } else { 52 };
    let header_size = read_u16(if expected_class == 2 { 52 } else { 40 });
    if header[..4] != *b"\x7fELF"
        || header[4] != expected_class
        || header[5] != expected_data
        || header[6] != 1
        || !matches!(elf_type, 2 | 3)
        || elf_machine != machine
        || read_u32(20) != 1
        || header_size != expected_header_size
    {
        return Err(NativeProbeError::InvalidEvidence(
            "/proc/self/exe is not a supported ELF executable",
        ));
    }
    Ok(())
}

fn parse_process_status(status: &str) -> Result<(u32, u64), NativeProbeError> {
    let uid_lines = status
        .lines()
        .filter(|line| line.starts_with("Uid:"))
        .collect::<Vec<_>>();
    let capability_lines = status
        .lines()
        .filter(|line| line.starts_with("CapEff:"))
        .collect::<Vec<_>>();
    if uid_lines.len() != 1 || capability_lines.len() != 1 {
        return Err(NativeProbeError::InvalidEvidence(
            "/proc/self/status must contain one Uid and one CapEff line",
        ));
    }
    let uid_fields = uid_lines[0].split_whitespace().collect::<Vec<_>>();
    if uid_fields.len() != 5 {
        return Err(NativeProbeError::InvalidEvidence(
            "Uid must contain real, effective, saved, and filesystem ids",
        ));
    }
    let uid = uid_fields[2]
        .parse::<u32>()
        .map_err(|_| NativeProbeError::InvalidEvidence("effective uid is not a u32"))?;
    let capability_fields = capability_lines[0].split_whitespace().collect::<Vec<_>>();
    if capability_fields.len() != 2
        || capability_fields[1].is_empty()
        || capability_fields[1].len() > 16
    {
        return Err(NativeProbeError::InvalidEvidence(
            "CapEff must be one hexadecimal u64",
        ));
    }
    let capabilities = u64::from_str_radix(capability_fields[1], 16)
        .map_err(|_| NativeProbeError::InvalidEvidence("CapEff is not hexadecimal"))?;
    Ok((uid, capabilities))
}

fn conservative_evidence(platform: Platform) -> NativeProbeEvidence {
    // Presence checks and CapEff bits are not active proof under seccomp, LSM,
    // Android SELinux, device cgroups, or cgroup delegation. Until isolated
    // helper probes exist, every privileged capability remains unavailable.
    let _ = platform;
    // Reading the already-running executable proves its format, but not that
    // seccomp/LSM policy permits a new execve. Until an isolated child probe
    // and a live OEM executor are bound, LinuxElf stays unavailable.
    NativeProbeEvidence::default()
}

fn profile_from_evidence(
    platform: Platform,
    privilege: PrivilegeMode,
    evidence: &NativeProbeEvidence,
) -> CapabilityProfile {
    let mut profile = CapabilityProfile::new(platform, privilege, "native-linux")
        .with(Capability::CommandOffload, SupportLevel::Unavailable)
        .with(Capability::OciImages, SupportLevel::Unavailable)
        .with(Capability::VirtualFilesystem, SupportLevel::Native)
        .with(
            Capability::LinuxElf,
            if evidence.linux_elf {
                SupportLevel::Native
            } else {
                SupportLevel::Unavailable
            },
        )
        .with(Capability::PortForwarding, SupportLevel::Unavailable);

    for (capability, available) in [
        (Capability::ProcessNamespace, evidence.process_namespace),
        (Capability::UserNamespace, evidence.user_namespace),
        (Capability::MountNamespace, evidence.mount_namespace),
        (Capability::NetworkNamespace, evidence.network_namespace),
        (Capability::UtsNamespace, evidence.uts_namespace),
        (Capability::IpcNamespace, evidence.ipc_namespace),
        (Capability::CgroupNamespace, evidence.cgroup_namespace),
        (Capability::TimeNamespace, evidence.time_namespace),
        (Capability::CgroupsV2, evidence.cgroups_v2),
        (
            Capability::PrivilegedContainers,
            evidence.privileged_containers,
        ),
        (Capability::KernelModules, evidence.kernel_modules),
        (Capability::DeviceNodes, evidence.device_nodes),
        (Capability::Systemd, evidence.systemd),
        (Capability::NestedContainers, evidence.nested_containers),
        (Capability::RawSockets, evidence.raw_sockets),
        (Capability::TunTap, evidence.tun_tap),
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

fn read_required(path: &'static str) -> Result<String, NativeProbeError> {
    const MAX_EVIDENCE_BYTES: u64 = 64 * 1024;
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(MAX_EVIDENCE_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|source| NativeProbeError::EvidenceIo { path, source })?;
    if bytes.len() as u64 > MAX_EVIDENCE_BYTES {
        return Err(NativeProbeError::InvalidEvidence(
            "proc evidence exceeds size limit",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| NativeProbeError::InvalidEvidence("proc evidence is not UTF-8"))
}

fn has_capability(mask: u64, capability: u8) -> bool {
    mask & (1_u64 << capability) != 0
}

fn initial_root_mapping() -> bool {
    let Ok(uid_map) = fs::read_to_string("/proc/self/uid_map") else {
        return false;
    };
    let mut lines = uid_map.lines();
    let Some(first) = lines.next() else {
        return false;
    };
    if lines.next().is_some() {
        return false;
    }
    let fields = first.split_whitespace().collect::<Vec<_>>();
    fields.len() == 3
        && fields[0] == "0"
        && fields[1] == "0"
        && fields[2].parse::<u64>().is_ok_and(|length| length > 0)
        && same_user_namespace_as_pid_one()
}

#[cfg(target_family = "unix")]
fn same_user_namespace_as_pid_one() -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(current) = fs::metadata("/proc/self/ns/user") else {
        return false;
    };
    let Ok(pid_one) = fs::metadata("/proc/1/ns/user") else {
        return false;
    };
    current.dev() == pid_one.dev() && current.ino() == pid_one.ino()
}

#[cfg(not(target_family = "unix"))]
fn same_user_namespace_as_pid_one() -> bool {
    false
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
        for optional_handler in [
            Capability::OciImages,
            Capability::DeviceNodes,
            Capability::PortForwarding,
        ] {
            assert_eq!(profile.level(optional_handler), SupportLevel::Unavailable);
        }
    }

    #[test]
    fn process_status_requires_effective_uid_and_capabilities() {
        assert_eq!(
            parse_process_status(
                "Name:\ttest\nUid:\t1000\t2000\t3000\t4000\nCapEff:\t0000000000200000\n"
            )
            .unwrap(),
            (2000, 1 << CAP_SYS_ADMIN)
        );
        assert!(parse_process_status("Uid:\t0\t0\t0\t0\n").is_err());
        assert!(parse_process_status("CapEff:\t0\n").is_err());
        assert!(parse_process_status("Uid:\t0\t0\t0\t0\nUid:\t0\t0\t0\t0\nCapEff:\t0\n").is_err());
        assert!(parse_process_status("Uid:\t0\t0\t0\nCapEff:\t0\n").is_err());
        assert!(parse_process_status("Uid:\t0\t0\t0\t0\nCapEff:\t10000000000000000\n").is_err());
    }

    #[test]
    fn evidence_only_exposes_verified_kernel_features() {
        let evidence = NativeProbeEvidence {
            linux_elf: true,
            cgroups_v2: true,
            ..NativeProbeEvidence::default()
        };
        let profile = profile_from_evidence(Platform::Linux, PrivilegeMode::Root, &evidence);

        assert_eq!(profile.level(Capability::LinuxElf), SupportLevel::Native);
        assert_eq!(profile.level(Capability::CgroupsV2), SupportLevel::Native);
        assert_eq!(
            profile.level(Capability::NetworkNamespace),
            SupportLevel::Unavailable
        );
    }

    #[test]
    fn passive_host_facts_do_not_promote_privileged_capabilities() {
        let evidence = conservative_evidence(Platform::Linux);
        let profile = profile_from_evidence(Platform::Linux, PrivilegeMode::Root, &evidence);

        assert_eq!(
            profile.level(Capability::LinuxElf),
            SupportLevel::Unavailable
        );
        for capability in [
            Capability::ProcessNamespace,
            Capability::MountNamespace,
            Capability::NetworkNamespace,
            Capability::CgroupsV2,
            Capability::PrivilegedContainers,
            Capability::KernelModules,
            Capability::DeviceNodes,
            Capability::Systemd,
            Capability::NestedContainers,
            Capability::RawSockets,
            Capability::TunTap,
            Capability::PortForwarding,
        ] {
            assert_eq!(profile.level(capability), SupportLevel::Unavailable);
        }
    }

    #[test]
    fn android_elf_does_not_claim_generic_linux_abi() {
        let evidence = conservative_evidence(Platform::Android);
        let profile = profile_from_evidence(Platform::Android, PrivilegeMode::Root, &evidence);

        assert_eq!(
            profile.level(Capability::LinuxElf),
            SupportLevel::Unavailable
        );
    }

    #[test]
    fn unsupported_targets_cannot_probe_native_linux() {
        if cfg!(target_os = "ios") || cfg!(target_os = "macos") || cfg!(target_env = "ohos") {
            assert!(matches!(
                probe_native_linux(),
                Err(NativeProbeError::UnsupportedHost)
            ));
        }
    }
}
