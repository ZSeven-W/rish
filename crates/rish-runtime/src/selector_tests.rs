use std::collections::BTreeMap;

use rish_core::{
    Capability, CapabilityProfile, GuestCommand, HostReply, Platform, PrivilegeMode, SupportLevel,
};
use rish_guest_protocol::{
    CURRENT_PROTOCOL_VERSION, Capability as GuestCapability, CapabilityStatus, Envelope,
    GuestCapabilities, GuestLimits, HandshakeOutcome, HelloAck, Message, PeerInfo, capability_name,
};
use rish_vm::{
    GuestChannel, GuestKernelContract, GuestKernelEvidence, GuestSession, KernelEvidenceSource,
    VmAcceleration, VmCandidate, VmConfig, VmDevice, VmEngine, VmError, VmProbe,
};

use super::*;
use crate::portable_offload_profile;

struct TestEngine;

struct TestChannel;

impl VmEngine for TestEngine {
    fn probe(&self) -> VmProbe {
        VmProbe::available([VmAcceleration::Interpreter])
    }

    fn boot(&self, _config: &VmConfig) -> Result<Box<dyn GuestChannel>, VmError> {
        Ok(Box::new(TestChannel))
    }
}

impl GuestChannel for TestChannel {
    fn bootstrap(&self, hello: &Envelope) -> Result<Envelope, VmError> {
        let Message::Hello(hello) = &hello.message else {
            return Err(VmError::Guest("expected Hello".to_owned()));
        };
        Ok(Envelope::with_version(
            CURRENT_PROTOCOL_VERSION,
            Message::HelloAck(HelloAck {
                request_id: hello.request_id.clone(),
                outcome: HandshakeOutcome::Accepted {
                    selected_version: CURRENT_PROTOCOL_VERSION,
                    session_id: "selector-test".to_owned(),
                    peer: PeerInfo {
                        name: "guest".to_owned(),
                        version: "0.1.0".to_owned(),
                        platform: "linux".to_owned(),
                        architecture: "aarch64".to_owned(),
                    },
                    capabilities: Box::new(GuestCapabilities {
                        kernel_release: "6.12".to_owned(),
                        architecture: "aarch64".to_owned(),
                        init_system: "systemd".to_owned(),
                        cgroup_version: Some(2),
                        container_runtimes: vec!["youki".to_owned()],
                        features: vec![GuestCapability {
                            name: capability_name::NESTED_CONTAINERS.to_owned(),
                            version: 1,
                            status: CapabilityStatus::Available,
                            attributes: BTreeMap::new(),
                            reason: None,
                        }],
                    }),
                    limits: GuestLimits {
                        max_frame_size: rish_guest_protocol::DEFAULT_MAX_FRAME_SIZE as u32,
                        max_concurrent_exec: 1,
                        max_port_forwards: 0,
                        max_stream_chunk_size: 32 * 1024,
                    },
                },
            }),
        ))
    }

    fn kernel_config(&self, _session: &GuestSession) -> Result<GuestKernelEvidence, VmError> {
        GuestKernelEvidence::new(
            "selector-test",
            KernelEvidenceSource::BuildManifest {
                kernel_release: "6.12".to_owned(),
                sha256: "a".repeat(64),
            },
            ["CONFIG_NAMESPACES"],
        )
    }

    fn execute(&self, _command: &GuestCommand) -> Result<HostReply, VmError> {
        Err(VmError::Guest("unused".to_owned()))
    }
}

fn verified_vm_candidate() -> BackendCandidate {
    let vm = VmCandidate::new(
        Platform::Ios,
        VmConfig {
            architecture: "aarch64".to_owned(),
            vcpus: 1,
            memory_mib: 512,
            kernel_path: "/app/kernel".to_owned(),
            initrd_path: None,
            root_disk_path: "/app/root.img".to_owned(),
            acceleration: VmAcceleration::Interpreter,
            devices: vec![VmDevice::Console],
        },
        GuestKernelContract::new(["CONFIG_NAMESPACES"], [Capability::NestedContainers]),
    )
    .boot(&TestEngine)
    .unwrap();
    BackendCandidate::full_virtual_machine(vm.profile(), 100)
}

#[test]
fn real_dind_requirement_skips_semantic_offload_backend() {
    let candidates = [
        BackendCandidate::portable_offload(
            portable_offload_profile(Platform::Ios, PrivilegeMode::AppSandbox),
            1,
        )
        .unwrap(),
        verified_vm_candidate(),
    ];
    let requirement = CapabilityRequirement::kernel(Capability::NestedContainers);

    let selected = select_backend(
        &candidates,
        &[requirement],
        SelectionPolicy::LowestStartupCost,
    )
    .unwrap();

    assert_eq!(selected.class(), BackendClass::FullVirtualMachine);
}

#[test]
fn portable_constructor_rejects_forged_vm_capability() {
    let profile =
        CapabilityProfile::new(Platform::Ios, PrivilegeMode::AppSandbox, "portable-offload")
            .with(Capability::FullVirtualMachine, SupportLevel::Virtualized);

    assert!(BackendCandidate::portable_offload(profile, 1).is_err());
}

#[test]
fn native_constructor_rejects_vm_guest_and_virtualized_levels() {
    let profile = CapabilityProfile::new(Platform::Android, PrivilegeMode::VmGuest, "native-linux")
        .with(Capability::LinuxElf, SupportLevel::Virtualized);

    assert!(BackendCandidate::native_linux(profile, 1).is_err());
}

#[test]
fn backend_classes_reject_wrong_privilege_boundaries() {
    let portable =
        CapabilityProfile::new(Platform::Android, PrivilegeMode::Root, "portable-offload");
    let native =
        CapabilityProfile::new(Platform::Android, PrivilegeMode::AppSandbox, "native-linux");

    assert!(BackendCandidate::portable_offload(portable, 1).is_err());
    assert!(BackendCandidate::native_linux(native, 1).is_err());
}
