use std::fmt;

use rish_core::{
    Capability, CapabilityProfile, CapabilityRequirement, Platform, PrivilegeMode, SupportLevel,
};
use rish_vm::VerifiedVmProfile;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendClass {
    PortableOffload,
    FullVirtualMachine,
    NativeLinux,
}

/// A capability profile bound to a backend class through controlled constructors.
///
/// In particular, a full-VM candidate can only be created from the evidence-gated
/// [`VerifiedVmProfile`] token. Candidates are serializable for diagnostics but
/// deliberately cannot be deserialized into trusted runtime state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BackendCandidate {
    class: BackendClass,
    profile: CapabilityProfile,
    startup_cost: u32,
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
        })
    }

    pub fn native_linux(
        profile: CapabilityProfile,
        startup_cost: u32,
    ) -> Result<Self, BackendCandidateError> {
        validate_native_profile(&profile)?;
        Ok(Self {
            class: BackendClass::NativeLinux,
            profile,
            startup_cost,
        })
    }

    #[must_use]
    pub fn full_virtual_machine(profile: &VerifiedVmProfile, startup_cost: u32) -> Self {
        Self {
            class: BackendClass::FullVirtualMachine,
            profile: profile.capabilities().clone(),
            startup_cost,
        }
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
    if profile.level(Capability::FullVirtualMachine).is_runnable() {
        return Err(BackendCandidateError::new(
            "portable candidate cannot claim a runnable full VM",
        ));
    }
    Ok(())
}

fn validate_native_profile(profile: &CapabilityProfile) -> Result<(), BackendCandidateError> {
    if profile.backend != "native-linux" {
        return Err(BackendCandidateError::new(
            "native candidate requires a native-linux profile",
        ));
    }
    if !matches!(
        profile.privilege,
        PrivilegeMode::Elevated | PrivilegeMode::Root
    ) || profile.platform == Platform::Ios
    {
        return Err(BackendCandidateError::new(
            "native Linux candidate requires elevated/root privilege on a non-iOS host",
        ));
    }
    if profile
        .levels()
        .values()
        .any(|level| *level == SupportLevel::Virtualized)
    {
        return Err(BackendCandidateError::new(
            "native candidate cannot claim virtualized capabilities",
        ));
    }
    if profile.level(Capability::FullVirtualMachine).is_runnable() {
        return Err(BackendCandidateError::new(
            "native candidate cannot claim a runnable full VM",
        ));
    }
    Ok(())
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
