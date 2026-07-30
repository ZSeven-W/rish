use std::collections::{BTreeMap, BTreeSet};

use rish_core::{
    Capability as CoreCapability, CapabilityProfile, Platform, PrivilegeMode, SupportLevel,
};
use rish_guest_protocol::{Capability as GuestCapability, CapabilityStatus, capability_name};

use crate::{GuestCapabilityState, GuestKernelContract, VmError, contract::is_kconfig_symbol};

const SUPPORTED_GUEST_CAPABILITY_VERSION: u16 = 1;

pub(crate) fn validate_contract(contract: &GuestKernelContract) -> Result<(), VmError> {
    if contract.required_kconfig().is_empty() {
        return Err(VmError::InvalidContract(
            "at least one concrete CONFIG_* symbol is required".to_owned(),
        ));
    }
    if contract
        .required_kconfig()
        .iter()
        .any(|symbol| !is_kconfig_symbol(symbol))
    {
        return Err(VmError::InvalidContract(
            "kernel requirements must be canonical CONFIG_* symbols".to_owned(),
        ));
    }
    if contract.required_capabilities().is_empty() {
        return Err(VmError::InvalidContract(
            "at least one guest capability must be explicitly required".to_owned(),
        ));
    }
    for capability in contract.required_capabilities() {
        if wire_name(*capability).is_none() {
            return Err(VmError::InvalidContract(format!(
                "{capability} has no guest protocol mapping"
            )));
        }
    }
    Ok(())
}

pub(crate) fn requested_capability_names(
    contract: &GuestKernelContract,
) -> Result<Vec<String>, VmError> {
    validate_contract(contract)?;
    Ok(contract
        .required_capabilities()
        .iter()
        .filter_map(|capability| wire_name(*capability))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

pub(crate) fn verified_profile(
    platform: Platform,
    contract: &GuestKernelContract,
    features: &[GuestCapability],
) -> Result<CapabilityProfile, VmError> {
    let features = indexed_features(features)?;
    let mut profile = CapabilityProfile::new(platform, PrivilegeMode::VmGuest, "verified-full-vm")
        .with(
            CoreCapability::FullVirtualMachine,
            SupportLevel::Virtualized,
        );

    for capability in contract.required_capabilities() {
        let state = capability_state(*capability, &features);
        if state != GuestCapabilityState::Available {
            return Err(VmError::RequiredGuestCapability {
                capability: *capability,
                state,
            });
        }
        profile = profile.with(*capability, SupportLevel::Virtualized);
    }

    Ok(profile)
}

fn indexed_features(
    features: &[GuestCapability],
) -> Result<BTreeMap<&str, &GuestCapability>, VmError> {
    let mut indexed = BTreeMap::new();
    for feature in features {
        if feature.name.is_empty() {
            return Err(VmError::Protocol(
                "guest capability name cannot be empty".to_owned(),
            ));
        }
        if indexed.insert(feature.name.as_str(), feature).is_some() {
            return Err(VmError::Protocol(format!(
                "guest returned duplicate capability {}",
                feature.name
            )));
        }
    }
    Ok(indexed)
}

fn capability_state(
    capability: CoreCapability,
    features: &BTreeMap<&str, &GuestCapability>,
) -> GuestCapabilityState {
    let Some(name) = wire_name(capability) else {
        return GuestCapabilityState::Unmapped;
    };
    let Some(feature) = features.get(name).copied() else {
        return GuestCapabilityState::Missing;
    };
    if feature.version != SUPPORTED_GUEST_CAPABILITY_VERSION {
        return GuestCapabilityState::UnsupportedVersion(feature.version);
    }
    match feature.status {
        CapabilityStatus::Available => GuestCapabilityState::Available,
        CapabilityStatus::Restricted => GuestCapabilityState::Restricted,
        CapabilityStatus::Unavailable => GuestCapabilityState::Unavailable,
    }
}

fn wire_name(capability: CoreCapability) -> Option<&'static str> {
    use CoreCapability as C;

    Some(match capability {
        C::CommandOffload | C::LinuxElf => capability_name::EXEC,
        C::OciImages => capability_name::OCI,
        C::VirtualFilesystem => capability_name::VIRTUAL_FILESYSTEM,
        C::ProcessNamespace
        | C::UserNamespace
        | C::MountNamespace
        | C::UtsNamespace
        | C::IpcNamespace
        | C::CgroupNamespace
        | C::TimeNamespace => capability_name::NAMESPACES,
        C::NetworkNamespace => capability_name::NETWORK_NAMESPACES,
        C::CgroupsV2 => capability_name::CGROUPS_V2,
        C::PrivilegedContainers => capability_name::PRIVILEGED_CONTAINERS,
        C::KernelModules => capability_name::MODULES,
        C::DeviceNodes => capability_name::DEVICES,
        C::Systemd => capability_name::SYSTEMD,
        C::NestedContainers => capability_name::NESTED_CONTAINERS,
        C::PortForwarding => capability_name::PORT_FORWARDING,
        C::RawSockets => capability_name::RAW_SOCKETS,
        C::TunTap => capability_name::TUN_TAP,
        C::FullVirtualMachine => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_core_capability_except_the_vm_token_has_a_wire_name() {
        for capability in [
            CoreCapability::CommandOffload,
            CoreCapability::OciImages,
            CoreCapability::LinuxElf,
            CoreCapability::VirtualFilesystem,
            CoreCapability::ProcessNamespace,
            CoreCapability::UserNamespace,
            CoreCapability::MountNamespace,
            CoreCapability::NetworkNamespace,
            CoreCapability::UtsNamespace,
            CoreCapability::IpcNamespace,
            CoreCapability::CgroupNamespace,
            CoreCapability::TimeNamespace,
            CoreCapability::CgroupsV2,
            CoreCapability::PrivilegedContainers,
            CoreCapability::KernelModules,
            CoreCapability::DeviceNodes,
            CoreCapability::Systemd,
            CoreCapability::NestedContainers,
            CoreCapability::PortForwarding,
            CoreCapability::RawSockets,
            CoreCapability::TunTap,
        ] {
            assert!(wire_name(capability).is_some(), "missing {capability}");
        }
        assert!(wire_name(CoreCapability::FullVirtualMachine).is_none());
    }

    #[test]
    fn requested_names_are_deduplicated_and_contract_scoped() {
        let contract = GuestKernelContract::new(
            ["CONFIG_BINFMT_ELF"],
            [CoreCapability::CommandOffload, CoreCapability::LinuxElf],
        );
        assert_eq!(
            requested_capability_names(&contract).unwrap(),
            vec![capability_name::EXEC.to_owned()]
        );
    }
}
