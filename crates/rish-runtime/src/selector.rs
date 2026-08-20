use std::fmt;

use rish_core::{
    Capability, CapabilityProfile, CapabilityRequirement, PrivilegeMode, SupportLevel,
};
use rish_vm::BootedVm;
use serde::{Deserialize, Serialize};

use crate::profile::VerifiedNativeProfile;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
enum VerifiedGuestArchitecture {
    #[serde(rename = "arm64")]
    Arm64,
    #[serde(rename = "amd64")]
    Amd64,
}

impl VerifiedGuestArchitecture {
    const fn oci_name(self) -> &'static str {
        match self {
            Self::Arm64 => "arm64",
            Self::Amd64 => "amd64",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendClass {
    PortableOffload,
    FullVirtualMachine,
    NativeLinux,
}

/// A capability profile bound to a backend class through controlled constructors.
///
/// Native-Linux and full-VM candidates require evidence-gated tokens.
/// Candidates are serializable for diagnostics but deliberately cannot be
/// deserialized into trusted runtime state.
///
/// A serialized diagnostic cannot be promoted back into a trusted candidate:
///
/// ```compile_fail
/// use rish_runtime::BackendCandidate;
///
/// let json = r#"{
///   "class":"full_virtual_machine",
///   "profile":{},
///   "startup_cost":0,
///   "verified_guest_architecture":"amd64"
/// }"#;
/// let _: BackendCandidate = serde_json::from_str(json).unwrap();
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BackendCandidate {
    class: BackendClass,
    profile: CapabilityProfile,
    startup_cost: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    verified_guest_architecture: Option<VerifiedGuestArchitecture>,
}

impl BackendCandidate {
    pub fn portable_offload(
        profile: CapabilityProfile,
        startup_cost: u32,
    ) -> Result<Self, BackendCandidateError> {
        validate_portable_profile(&profile)?;
        Ok(Self {
            class: BackendClass::PortableOffload,
            profile,
            startup_cost,
            verified_guest_architecture: None,
        })
    }

    /// A caller-created `CapabilityProfile` is intentionally not accepted.
    ///
    /// ```compile_fail
    /// use rish_core::{CapabilityProfile, Platform, PrivilegeMode};
    /// use rish_runtime::BackendCandidate;
    ///
    /// let forged = CapabilityProfile::new(
    ///     Platform::Linux,
    ///     PrivilegeMode::Root,
    ///     "native-linux",
    /// );
    /// let _ = BackendCandidate::native_linux(&forged, 0);
    /// ```
    pub fn native_linux(
        profile: &VerifiedNativeProfile,
        startup_cost: u32,
    ) -> Result<Self, BackendCandidateError> {
        if profile.level(Capability::LinuxElf) != SupportLevel::Native {
            return Err(BackendCandidateError::new(
                "native-Linux candidate requires actively verified Linux ELF execution",
            ));
        }
        Ok(Self {
            class: BackendClass::NativeLinux,
            profile: profile.capabilities().clone(),
            startup_cost,
            verified_guest_architecture: None,
        })
    }

    pub fn full_virtual_machine(
        vm: &BootedVm,
        startup_cost: u32,
    ) -> Result<Self, BackendCandidateError> {
        let profile = vm.profile().capabilities();
        if profile.level(Capability::FullVirtualMachine) != SupportLevel::Virtualized {
            return Err(BackendCandidateError::new(
                "full-VM candidate requires a verified virtual-machine token",
            ));
        }
        if profile.level(Capability::LinuxElf) != SupportLevel::Virtualized
            && profile.level(Capability::CommandOffload) != SupportLevel::Virtualized
        {
            return Err(BackendCandidateError::new(
                "full-VM candidate requires a verified guest execution capability",
            ));
        }
        let session = vm.session();
        if session.peer().platform != "linux"
            || session.peer().architecture != session.capabilities().architecture
        {
            return Err(BackendCandidateError::new(
                "full-VM candidate requires a consistent verified Linux guest identity",
            ));
        }
        let verified_guest_architecture = canonical_oci_architecture(&session.peer().architecture)
            .ok_or_else(|| {
                BackendCandidateError::new(format!(
                    "full-VM candidate has unsupported verified guest architecture {}",
                    session.peer().architecture
                ))
            })?;
        Ok(Self {
            class: BackendClass::FullVirtualMachine,
            profile: profile.clone(),
            startup_cost,
            verified_guest_architecture: Some(verified_guest_architecture),
        })
    }

    #[must_use]
    pub fn class(&self) -> BackendClass {
        self.class
    }

    #[must_use]
    pub fn profile(&self) -> &CapabilityProfile {
        &self.profile
    }

    #[must_use]
    pub fn startup_cost(&self) -> u32 {
        self.startup_cost
    }

    /// Returns the OCI architecture authenticated by the live VM bootstrap.
    ///
    /// This is present only for [`BackendClass::FullVirtualMachine`]. The
    /// `BootedVm` handshake has already matched both guest identity fields to
    /// the configured VM architecture. Native architecture spellings are
    /// normalized to OCI (`aarch64` → `arm64`, `x86_64` → `amd64`).
    #[must_use]
    pub fn verified_guest_architecture(&self) -> Option<&str> {
        self.verified_guest_architecture
            .map(VerifiedGuestArchitecture::oci_name)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendCandidateError {
    message: String,
}

impl BackendCandidateError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for BackendCandidateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BackendCandidateError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionPolicy {
    LowestStartupCost,
    PreferNativeLinux,
    PreferIsolation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendSelectionError {
    pub failures: Vec<String>,
}

impl fmt::Display for BackendSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "no backend satisfies the request: {}",
            self.failures.join("; ")
        )
    }
}

impl std::error::Error for BackendSelectionError {}

pub fn select_backend<'a>(
    candidates: &'a [BackendCandidate],
    requirements: &[CapabilityRequirement],
    policy: SelectionPolicy,
) -> Result<&'a BackendCandidate, BackendSelectionError> {
    let mut viable = Vec::new();
    let mut failures = Vec::new();

    for candidate in candidates {
        match candidate.profile.require_all(requirements) {
            Ok(()) => viable.push(candidate),
            Err(error) => failures.push(format!("{:?}: {error}", candidate.class)),
        }
    }

    viable
        .into_iter()
        .min_by_key(|candidate| score(candidate, policy))
        .ok_or(BackendSelectionError { failures })
}

fn validate_portable_profile(profile: &CapabilityProfile) -> Result<(), BackendCandidateError> {
    if profile.backend != "portable-offload" {
        return Err(BackendCandidateError::new(
            "portable candidate requires a portable-offload profile",
        ));
    }
    if !matches!(
        profile.privilege,
        PrivilegeMode::AppSandbox | PrivilegeMode::Elevated
    ) {
        return Err(BackendCandidateError::new(
            "portable candidate requires app-sandbox or elevated privilege",
        ));
    }
    if profile
        .levels()
        .values()
        .any(|level| level.provides_kernel_semantics())
    {
        return Err(BackendCandidateError::new(
            "portable candidate cannot claim native or virtualized kernel semantics",
        ));
    }
    for capability in [
        Capability::OciImages,
        Capability::DeviceNodes,
        Capability::PortForwarding,
    ] {
        if profile.level(capability) != SupportLevel::Unavailable {
            return Err(BackendCandidateError::new(format!(
                "portable {capability} support requires a live dispatcher-bound evidence token"
            )));
        }
    }
    if matches!(
        profile.level(Capability::FullVirtualMachine),
        SupportLevel::Native
            | SupportLevel::Virtualized
            | SupportLevel::Bridged
            | SupportLevel::Emulated
    ) {
        return Err(BackendCandidateError::new(
            "portable candidate cannot claim a runnable full VM",
        ));
    }
    Ok(())
}

fn canonical_oci_architecture(guest_architecture: &str) -> Option<VerifiedGuestArchitecture> {
    match guest_architecture {
        "aarch64" | "arm64" => Some(VerifiedGuestArchitecture::Arm64),
        "x86_64" | "amd64" => Some(VerifiedGuestArchitecture::Amd64),
        _ => None,
    }
}

fn score(candidate: &BackendCandidate, policy: SelectionPolicy) -> (u8, u32) {
    let class_score = match policy {
        SelectionPolicy::LowestStartupCost => 0,
        SelectionPolicy::PreferNativeLinux => match candidate.class {
            BackendClass::NativeLinux => 0,
            BackendClass::FullVirtualMachine => 1,
            BackendClass::PortableOffload => 2,
        },
        SelectionPolicy::PreferIsolation => match candidate.class {
            BackendClass::FullVirtualMachine => 0,
            BackendClass::PortableOffload => 1,
            BackendClass::NativeLinux => 2,
        },
    };
    (class_score, candidate.startup_cost)
}

#[cfg(test)]
#[path = "selector_tests.rs"]
mod tests;
