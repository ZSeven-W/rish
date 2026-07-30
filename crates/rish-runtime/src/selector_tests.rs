use std::collections::BTreeMap;

use rish_core::{
    Capability, CapabilityProfile, GuestCommand, HostReply, Platform, PrivilegeMode, SupportLevel,
};
use rish_guest_protocol::{
    CURRENT_PROTOCOL_VERSION, Capability as GuestCapability, CapabilityStatus, Envelope,
    GuestCapabilities, GuestLimits, HandshakeOutcome, HelloAck, Message, PeerInfo, capability_name,
};
use rish_vm::{
    BootedVm, GuestChannel, GuestKernelContract, GuestKernelEvidence, GuestSession,
    KernelEvidenceSource, VmAcceleration, VmCandidate, VmConfig, VmDevice, VmEngine, VmError,
    VmProbe,
};

use super::*;
use crate::{CommandPlan, OffloadRegistry, Planner, VerifiedVmRuntime, portable_offload_profile};

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
                        features: vec![
                            GuestCapability {
                                name: capability_name::NESTED_CONTAINERS.to_owned(),
                                version: 1,
                                status: CapabilityStatus::Available,
                                attributes: BTreeMap::new(),
                                reason: None,
                            },
                            GuestCapability {
                                name: capability_name::EXEC.to_owned(),
                                version: 1,
                                status: CapabilityStatus::Available,
                                attributes: BTreeMap::new(),
                                reason: None,
                            },
                        ],
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
            ["CONFIG_NAMESPACES", "CONFIG_BINFMT_ELF"],
        )
    }

    fn execute(&self, command: &GuestCommand) -> Result<HostReply, VmError> {
        Ok(HostReply {
            exit_code: 0,
            stdout: command.program.as_bytes().to_vec(),
            stderr: Vec::new(),
            payload: serde_json::Value::Null,
        })
    }
}

fn booted_vm() -> BootedVm {
    VmCandidate::new(
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
        GuestKernelContract::new(
            ["CONFIG_NAMESPACES", "CONFIG_BINFMT_ELF"],
            [Capability::NestedContainers, Capability::LinuxElf],
        ),
    )
    .boot(&TestEngine)
    .unwrap()
}

fn booted_vm_without_exec_contract() -> BootedVm {
    VmCandidate::new(
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
    .unwrap()
}

fn verified_vm_candidate() -> BackendCandidate {
    let vm = booted_vm();
    BackendCandidate::full_virtual_machine(&vm, 100).unwrap()
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
fn verified_full_vm_candidate_routes_commands_to_vm_exec() {
    let planner = Planner::new(
        verified_vm_candidate(),
        OffloadRegistry::portable_defaults(),
    );
    let command = GuestCommand::new("grep", ["--perl-regexp".to_owned(), "x".to_owned()]);

    assert!(matches!(
        planner.plan(&command).unwrap(),
        CommandPlan::HostCall { call } if call.operation == "vm.exec"
    ));
    assert_eq!(planner.backend_class(), BackendClass::FullVirtualMachine);
}

#[test]
fn path_elf_names_do_not_gain_registry_semantics_from_their_basename() {
    let planner = Planner::new(
        verified_vm_candidate(),
        OffloadRegistry::portable_defaults(),
    );
    let command = GuestCommand::new("/tmp/docker", ["ps".to_owned()]);

    let CommandPlan::HostCall { call } = planner.plan(&command).unwrap() else {
        panic!("full VM should plan path-bearing Guest ELF through vm.exec");
    };
    assert_eq!(call.operation, "vm.exec");
    assert!(
        !call
            .requirements
            .iter()
            .any(|requirement| requirement.capability == Capability::OciImages)
    );
}

#[test]
fn verified_vm_runtime_executes_through_the_live_booted_vm() {
    let vm = booted_vm();
    let runtime = VerifiedVmRuntime::new(&vm, OffloadRegistry::portable_defaults()).unwrap();
    let command = GuestCommand::new("grep", ["needle".to_owned()]);

    let outcome = runtime.execute(&command).unwrap();
    assert_eq!(outcome.stdout, b"grep");
    assert_eq!(outcome.path, rish_core::ExecutionPath::VirtualMachine);
}

#[test]
fn vm_without_a_verified_exec_capability_is_not_a_runtime_candidate() {
    let vm = booted_vm_without_exec_contract();

    assert!(BackendCandidate::full_virtual_machine(&vm, 1).is_err());
    assert!(VerifiedVmRuntime::new(&vm, OffloadRegistry::portable_defaults()).is_err());
}

#[test]
fn portable_constructor_rejects_forged_vm_capability() {
    let profile =
        CapabilityProfile::new(Platform::Ios, PrivilegeMode::AppSandbox, "portable-offload")
            .with(Capability::FullVirtualMachine, SupportLevel::Virtualized);

    assert!(BackendCandidate::portable_offload(profile, 1).is_err());
}

#[test]
fn portable_constructor_rejects_unbound_optional_handlers() {
    for capability in [
        Capability::OciImages,
        Capability::DeviceNodes,
        Capability::PortForwarding,
    ] {
        let profile = portable_offload_profile(Platform::Ios, PrivilegeMode::AppSandbox)
            .with(capability, SupportLevel::Bridged);
        assert!(BackendCandidate::portable_offload(profile, 1).is_err());
    }
}

#[test]
fn native_candidate_is_created_from_verified_token() {
    let profile = CapabilityProfile::new(Platform::Linux, PrivilegeMode::Root, "native-linux")
        .with(Capability::LinuxElf, SupportLevel::Native)
        .with(Capability::FullVirtualMachine, SupportLevel::Unavailable);
    let verified = crate::VerifiedNativeProfile::for_test(profile);
    let candidate = BackendCandidate::native_linux(&verified, 1).unwrap();

    assert_eq!(candidate.class(), BackendClass::NativeLinux);
    assert_eq!(
        candidate.profile().level(Capability::LinuxElf),
        SupportLevel::Native
    );
}

#[test]
fn native_candidate_requires_active_linux_elf_evidence() {
    let profile = CapabilityProfile::new(Platform::Linux, PrivilegeMode::Root, "native-linux")
        .with(Capability::LinuxElf, SupportLevel::Unavailable)
        .with(Capability::VirtualFilesystem, SupportLevel::Native);
    let verified = crate::VerifiedNativeProfile::for_test(profile);

    assert!(BackendCandidate::native_linux(&verified, 1).is_err());
}

#[test]
fn backend_classes_reject_wrong_privilege_boundaries() {
    let portable =
        CapabilityProfile::new(Platform::Android, PrivilegeMode::Root, "portable-offload");

    assert!(BackendCandidate::portable_offload(portable, 1).is_err());
}
