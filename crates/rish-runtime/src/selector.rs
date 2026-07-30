use rish_core::{CapabilityProfile, CapabilityRequirement};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendClass {
    PortableOffload,
    FullVirtualMachine,
    NativeLinux,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BackendCandidate {
    pub class: BackendClass,
    pub profile: CapabilityProfile,
    pub startup_cost: u32,
}

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

impl std::fmt::Display for BackendSelectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
mod tests {
    use rish_core::{Capability, Platform, PrivilegeMode};

    use super::*;
    use crate::portable_offload_profile;

    fn vm_candidate() -> BackendCandidate {
        BackendCandidate {
            class: BackendClass::FullVirtualMachine,
            profile: CapabilityProfile::new(Platform::Ios, PrivilegeMode::VmGuest, "test-vm").with(
                Capability::NestedContainers,
                rish_core::SupportLevel::Virtualized,
            ),
            startup_cost: 100,
        }
    }

    #[test]
    fn real_dind_requirement_skips_semantic_offload_backend() {
        let candidates = [
            BackendCandidate {
                class: BackendClass::PortableOffload,
                profile: portable_offload_profile(Platform::Ios, PrivilegeMode::AppSandbox),
                startup_cost: 1,
            },
            vm_candidate(),
        ];
        let requirement = CapabilityRequirement::kernel(Capability::NestedContainers);

        let selected = select_backend(
            &candidates,
            &[requirement],
            SelectionPolicy::LowestStartupCost,
        )
        .unwrap();

        assert_eq!(selected.class, BackendClass::FullVirtualMachine);
    }
}
