use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use rish_core::{Capability as CoreCapability, SupportLevel};
use rish_guest_protocol::{
    Capability as GuestCapability, CapabilityStatus, ErrorCode, GuestCapabilities, GuestLimits,
    HandshakeOutcome, HelloAck, Message, PeerInfo, ProtocolVersion, RemoteError, capability_name,
};

use super::*;
use crate::KernelEvidenceSource;

#[derive(Clone)]
enum AckMode {
    Accepted {
        features: Vec<GuestCapability>,
        response_version: ProtocolVersion,
    },
    Rejected,
}

#[derive(Clone)]
struct TestChannel {
    ack: AckMode,
    enabled_kconfig: BTreeSet<String>,
    evidence_session: String,
    evidence_kernel_release: String,
    limits: GuestLimits,
    requested_capabilities: Arc<Mutex<Vec<String>>>,
}

impl GuestChannel for TestChannel {
    fn bootstrap(&self, hello: &Envelope) -> Result<Envelope, VmError> {
        let Message::Hello(hello) = &hello.message else {
            return Err(VmError::Guest("expected Hello".to_owned()));
        };
        *self.requested_capabilities.lock().unwrap() = hello.requested_capabilities.clone();
        let outcome = match &self.ack {
            AckMode::Accepted { features, .. } => HandshakeOutcome::Accepted {
                selected_version: CURRENT_PROTOCOL_VERSION,
                session_id: "verified-session".to_owned(),
                peer: guest_peer(),
                capabilities: Box::new(GuestCapabilities {
                    kernel_release: "6.12-rish".to_owned(),
                    architecture: "aarch64".to_owned(),
                    init_system: "systemd".to_owned(),
                    cgroup_version: Some(2),
                    container_runtimes: vec!["youki".to_owned()],
                    features: features.clone(),
                }),
                limits: self.limits.clone(),
            },
            AckMode::Rejected => HandshakeOutcome::Rejected {
                error: RemoteError::new(ErrorCode::PermissionDenied, "guest policy denied boot"),
            },
        };
        let version = match self.ack {
            AckMode::Accepted {
                response_version, ..
            } => response_version,
            AckMode::Rejected => CURRENT_PROTOCOL_VERSION,
        };
        Ok(Envelope::with_version(
            version,
            Message::HelloAck(HelloAck {
                request_id: hello.request_id.clone(),
                outcome,
            }),
        ))
    }

    fn kernel_config(&self, _session: &GuestSession) -> Result<GuestKernelEvidence, VmError> {
        GuestKernelEvidence::new(
            self.evidence_session.clone(),
            KernelEvidenceSource::BuildManifest {
                kernel_release: self.evidence_kernel_release.clone(),
                sha256: "a".repeat(64),
            },
            self.enabled_kconfig.clone(),
        )
    }

    fn execute(&self, _command: &GuestCommand) -> Result<HostReply, VmError> {
        Err(VmError::Guest("not used by verification tests".to_owned()))
    }
}

struct TestEngine {
    probe: VmProbe,
    channel: TestChannel,
    boot_error: Option<String>,
    boot_called: Arc<AtomicBool>,
}

impl VmEngine for TestEngine {
    fn probe(&self) -> VmProbe {
        self.probe.clone()
    }

    fn boot(&self, _config: &VmConfig) -> Result<Box<dyn GuestChannel>, VmError> {
        self.boot_called.store(true, Ordering::SeqCst);
        if let Some(message) = &self.boot_error {
            return Err(VmError::Boot(message.clone()));
        }
        Ok(Box::new(self.channel.clone()))
    }
}

fn config() -> VmConfig {
    VmConfig {
        architecture: "aarch64".to_owned(),
        vcpus: 2,
        memory_mib: 1024,
        kernel_path: "/app/kernel".to_owned(),
        initrd_path: None,
        root_disk_path: "/app/root.img".to_owned(),
        acceleration: VmAcceleration::Interpreter,
        devices: vec![crate::VmDevice::Console],
    }
}

fn guest_peer() -> PeerInfo {
    PeerInfo {
        name: "rish-guest".to_owned(),
        version: "0.1.0".to_owned(),
        platform: "linux".to_owned(),
        architecture: "aarch64".to_owned(),
    }
}

fn guest_limits() -> GuestLimits {
    GuestLimits {
        max_frame_size: rish_guest_protocol::DEFAULT_MAX_FRAME_SIZE as u32,
        max_concurrent_exec: 4,
        max_port_forwards: 8,
        max_stream_chunk_size: 32 * 1024,
    }
}

fn feature(name: &str, status: CapabilityStatus) -> GuestCapability {
    GuestCapability {
        name: name.to_owned(),
        version: 1,
        status,
        attributes: BTreeMap::new(),
        reason: None,
    }
}

fn engine(
    probe: VmProbe,
    features: Vec<GuestCapability>,
    enabled_kconfig: impl IntoIterator<Item = &'static str>,
) -> (TestEngine, Arc<AtomicBool>) {
    let boot_called = Arc::new(AtomicBool::new(false));
    (
        TestEngine {
            probe,
            channel: TestChannel {
                ack: AckMode::Accepted {
                    features,
                    response_version: CURRENT_PROTOCOL_VERSION,
                },
                enabled_kconfig: enabled_kconfig.into_iter().map(str::to_owned).collect(),
                evidence_session: "verified-session".to_owned(),
                evidence_kernel_release: "6.12-rish".to_owned(),
                limits: guest_limits(),
                requested_capabilities: Arc::new(Mutex::new(Vec::new())),
            },
            boot_error: None,
            boot_called: Arc::clone(&boot_called),
        },
        boot_called,
    )
}

fn available_probe() -> VmProbe {
    VmProbe::available([VmAcceleration::Interpreter])
}

fn minimal_contract(capabilities: impl IntoIterator<Item = CoreCapability>) -> GuestKernelContract {
    GuestKernelContract::new(["CONFIG_NAMESPACES"], capabilities)
}

fn linux_elf_contract() -> GuestKernelContract {
    minimal_contract([CoreCapability::LinuxElf])
}

#[test]
fn unavailable_probe_never_attempts_boot() {
    let (engine, boot_called) = engine(
        VmProbe::unavailable("hypervisor entitlement missing"),
        Vec::new(),
        ["CONFIG_NAMESPACES"],
    );

    let error = VmCandidate::new(rish_core::Platform::Ios, config(), linux_elf_contract())
        .boot(&engine)
        .unwrap_err();

    assert!(matches!(error, VmError::Unavailable(_)));
    assert!(!boot_called.load(Ordering::SeqCst));
}

#[test]
fn invalid_config_never_reaches_the_engine() {
    let (engine, boot_called) = engine(available_probe(), Vec::new(), ["CONFIG_NAMESPACES"]);
    let mut invalid = config();
    invalid.vcpus = 0;

    let error = VmCandidate::new(rish_core::Platform::Ios, invalid, linux_elf_contract())
        .boot(&engine)
        .unwrap_err();

    assert!(matches!(error, VmError::InvalidConfig(_)));
    assert!(!boot_called.load(Ordering::SeqCst));
}

#[test]
fn boot_failure_never_yields_a_profile() {
    let (mut engine, boot_called) = engine(available_probe(), Vec::new(), ["CONFIG_NAMESPACES"]);
    engine.boot_error = Some("kernel image rejected".to_owned());

    let error = VmCandidate::new(rish_core::Platform::Android, config(), linux_elf_contract())
        .boot(&engine)
        .unwrap_err();

    assert!(matches!(error, VmError::Boot(_)));
    assert!(boot_called.load(Ordering::SeqCst));
}

#[test]
fn missing_kconfig_fails_closed_after_boot() {
    let (engine, _) = engine(available_probe(), Vec::new(), []);

    let error = VmCandidate::new(rish_core::Platform::Harmony, config(), linux_elf_contract())
        .boot(&engine)
        .unwrap_err();

    assert!(matches!(
        error,
        VmError::KernelContractUnsatisfied { ref missing }
            if missing.contains("CONFIG_NAMESPACES")
    ));
}

#[test]
fn rejected_handshake_fails_closed() {
    let (mut engine, _) = engine(available_probe(), Vec::new(), ["CONFIG_NAMESPACES"]);
    engine.channel.ack = AckMode::Rejected;

    let error = VmCandidate::new(rish_core::Platform::Ios, config(), linux_elf_contract())
        .boot(&engine)
        .unwrap_err();

    assert!(matches!(error, VmError::HandshakeRejected(_)));
}

#[test]
fn accepted_handshake_with_unaccepted_version_fails_closed() {
    let (mut engine, _) = engine(available_probe(), Vec::new(), ["CONFIG_NAMESPACES"]);
    engine.channel.ack = AckMode::Accepted {
        features: Vec::new(),
        response_version: ProtocolVersion::new(99, 0),
    };

    let error = VmCandidate::new(rish_core::Platform::Ios, config(), linux_elf_contract())
        .boot(&engine)
        .unwrap_err();

    assert!(matches!(error, VmError::Protocol(_)));
}

#[test]
fn missing_required_guest_capability_fails_closed() {
    let (engine, _) = engine(available_probe(), Vec::new(), ["CONFIG_NAMESPACES"]);

    let error = VmCandidate::new(
        rish_core::Platform::Android,
        config(),
        minimal_contract([CoreCapability::Systemd]),
    )
    .boot(&engine)
    .unwrap_err();

    assert!(matches!(
        error,
        VmError::RequiredGuestCapability {
            capability: CoreCapability::Systemd,
            state: crate::GuestCapabilityState::Missing
        }
    ));
}

#[test]
fn restricted_required_guest_capability_fails_closed() {
    let (engine, _) = engine(
        available_probe(),
        vec![feature(
            capability_name::SYSTEMD,
            CapabilityStatus::Restricted,
        )],
        ["CONFIG_NAMESPACES"],
    );

    let error = VmCandidate::new(
        rish_core::Platform::Android,
        config(),
        minimal_contract([CoreCapability::Systemd]),
    )
    .boot(&engine)
    .unwrap_err();

    assert!(matches!(
        error,
        VmError::RequiredGuestCapability {
            capability: CoreCapability::Systemd,
            state: crate::GuestCapabilityState::Restricted
        }
    ));
}

#[test]
fn successful_profile_maps_only_explicitly_available_features() {
    let (engine, _) = engine(
        available_probe(),
        vec![
            feature(capability_name::EXEC, CapabilityStatus::Available),
            feature(capability_name::OCI, CapabilityStatus::Restricted),
            feature(capability_name::SYSTEMD, CapabilityStatus::Unavailable),
            feature(capability_name::MODULES, CapabilityStatus::Available),
            feature("vendor.future_feature", CapabilityStatus::Available),
        ],
        ["CONFIG_NAMESPACES"],
    );

    let vm = VmCandidate::new(
        rish_core::Platform::Ios,
        config(),
        minimal_contract([CoreCapability::LinuxElf]),
    )
    .boot(&engine)
    .unwrap();
    let profile = vm.profile();

    assert_eq!(
        profile.level(CoreCapability::FullVirtualMachine),
        SupportLevel::Virtualized
    );
    assert_eq!(
        profile.level(CoreCapability::CommandOffload),
        SupportLevel::Unavailable
    );
    assert_eq!(
        profile.level(CoreCapability::LinuxElf),
        SupportLevel::Virtualized
    );
    assert_eq!(
        profile.level(CoreCapability::OciImages),
        SupportLevel::Unavailable
    );
    assert_eq!(
        profile.level(CoreCapability::Systemd),
        SupportLevel::Unavailable
    );
    assert_eq!(
        profile.level(CoreCapability::KernelModules),
        SupportLevel::Unavailable
    );
    assert_eq!(profile.capabilities().levels().len(), 2);
    assert_eq!(
        *engine.channel.requested_capabilities.lock().unwrap(),
        vec![capability_name::EXEC.to_owned()]
    );
}

#[test]
fn kernel_evidence_must_match_the_accepted_session() {
    let (mut engine, _) = engine(available_probe(), Vec::new(), ["CONFIG_NAMESPACES"]);
    engine.channel.evidence_session = "other-session".to_owned();

    let error = VmCandidate::new(rish_core::Platform::Ios, config(), linux_elf_contract())
        .boot(&engine)
        .unwrap_err();

    assert!(matches!(
        error,
        VmError::KernelEvidenceSessionMismatch { .. }
    ));
}

#[test]
fn kernel_evidence_must_match_the_running_release() {
    let (mut engine, _) = engine(
        available_probe(),
        vec![feature(capability_name::EXEC, CapabilityStatus::Available)],
        ["CONFIG_NAMESPACES"],
    );
    engine.channel.evidence_kernel_release = "other-kernel".to_owned();

    let error = VmCandidate::new(rish_core::Platform::Ios, config(), linux_elf_contract())
        .boot(&engine)
        .unwrap_err();

    assert!(matches!(error, VmError::KernelReleaseMismatch { .. }));
}

#[test]
fn available_exec_without_capacity_fails_closed() {
    let (mut engine, _) = engine(
        available_probe(),
        vec![feature(capability_name::EXEC, CapabilityStatus::Available)],
        ["CONFIG_NAMESPACES"],
    );
    engine.channel.limits.max_concurrent_exec = 0;

    let error = VmCandidate::new(rish_core::Platform::Ios, config(), linux_elf_contract())
        .boot(&engine)
        .unwrap_err();

    assert!(matches!(error, VmError::Protocol(_)));
}
