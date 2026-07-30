use std::{
    collections::{BTreeSet, HashSet},
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

use rish_core::{
    Capability, CapabilityProfile, CapabilityRequirement, GuestCommand, HostReply, Platform,
    SupportLevel,
};
use rish_guest_protocol::{
    CURRENT_PROTOCOL_VERSION, DEFAULT_MAX_FRAME_SIZE, Envelope, GuestCapabilities, GuestLimits,
    HandshakeOutcome, Hello, Message, PROTOCOL_ID, PeerInfo, ProtocolVersion, RequestId,
    SUPPORTED_PROTOCOL_VERSIONS,
};
use serde::{Deserialize, Serialize};

use crate::{
    GuestKernelContract, GuestKernelEvidence, VmAcceleration, VmConfig, VmError,
    config::platform_name, mapping,
};

static NEXT_BOOTSTRAP_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum VmProbe {
    Available {
        accelerations: BTreeSet<VmAcceleration>,
    },
    Unavailable {
        reason: String,
    },
}

impl VmProbe {
    #[must_use]
    pub fn available(accelerations: impl IntoIterator<Item = VmAcceleration>) -> Self {
        Self::Available {
            accelerations: accelerations.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }
}

pub trait GuestChannel: Send + Sync {
    /// Exchanges the bootstrap `Hello` envelope with the just-booted guest.
    fn bootstrap(&self, hello: &Envelope) -> Result<Envelope, VmError>;

    /// Reads concrete kernel configuration evidence for the negotiated session.
    fn kernel_config(&self, session: &GuestSession) -> Result<GuestKernelEvidence, VmError>;

    fn execute(&self, command: &GuestCommand) -> Result<HostReply, VmError>;
}

pub trait VmEngine: Send + Sync {
    fn probe(&self) -> VmProbe;
    fn boot(&self, config: &VmConfig) -> Result<Box<dyn GuestChannel>, VmError>;
}

#[derive(Clone, Debug)]
pub struct VmCandidate {
    platform: Platform,
    config: VmConfig,
    contract: GuestKernelContract,
}

impl VmCandidate {
    #[must_use]
    pub fn new(platform: Platform, config: VmConfig, contract: GuestKernelContract) -> Self {
        Self {
            platform,
            config,
            contract,
        }
    }

    #[must_use]
    pub fn platform(&self) -> Platform {
        self.platform
    }

    #[must_use]
    pub fn config(&self) -> &VmConfig {
        &self.config
    }

    #[must_use]
    pub fn contract(&self) -> &GuestKernelContract {
        &self.contract
    }

    /// Boots and verifies the VM. Every error path drops the unverified channel.
    pub fn boot(self, engine: &dyn VmEngine) -> Result<BootedVm, VmError> {
        self.config.validate()?;
        mapping::validate_contract(&self.contract)?;
        let accelerations = match engine.probe() {
            VmProbe::Available { accelerations } => accelerations,
            VmProbe::Unavailable { reason } => return Err(VmError::Unavailable(reason)),
        };
        if !accelerations.contains(&self.config.acceleration) {
            return Err(VmError::UnsupportedAcceleration(self.config.acceleration));
        }

        let channel = engine.boot(&self.config)?;
        let hello = bootstrap_hello(self.platform, &self.config, &self.contract)?;
        let response = channel.bootstrap(&hello)?;
        let session = verify_handshake(&hello, response, &self.config)?;
        let kernel_evidence = channel.kernel_config(&session)?;
        kernel_evidence.validate()?;

        if kernel_evidence.session_id() != session.id() {
            return Err(VmError::KernelEvidenceSessionMismatch {
                expected: session.id().to_owned(),
                actual: kernel_evidence.session_id().to_owned(),
            });
        }
        if kernel_evidence.source().kernel_release() != session.capabilities().kernel_release {
            return Err(VmError::KernelReleaseMismatch {
                expected: session.capabilities().kernel_release.clone(),
                actual: kernel_evidence.source().kernel_release().to_owned(),
            });
        }
        let report = self.contract.validate_kconfig(&kernel_evidence);
        if !report.is_satisfied() {
            return Err(VmError::KernelContractUnsatisfied {
                missing: report.missing().clone(),
            });
        }

        let profile = mapping::verified_profile(
            self.platform,
            &self.contract,
            session.capabilities().features.as_slice(),
        )?;
        Ok(BootedVm {
            profile: VerifiedVmProfile { profile },
            session,
            kernel_evidence,
            channel,
        })
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedVmProfile {
    profile: CapabilityProfile,
}

impl VerifiedVmProfile {
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
    ) -> Result<SupportLevel, rish_core::MissingCapability> {
        self.profile.require(requirement)
    }
}

pub struct BootedVm {
    profile: VerifiedVmProfile,
    session: GuestSession,
    kernel_evidence: GuestKernelEvidence,
    channel: Box<dyn GuestChannel>,
}

impl BootedVm {
    #[must_use]
    pub fn profile(&self) -> &VerifiedVmProfile {
        &self.profile
    }

    #[must_use]
    pub fn session(&self) -> &GuestSession {
        &self.session
    }

    #[must_use]
    pub fn kernel_evidence(&self) -> &GuestKernelEvidence {
        &self.kernel_evidence
    }

    pub fn execute(&self, command: &GuestCommand) -> Result<HostReply, VmError> {
        if self.profile.level(Capability::CommandOffload) != SupportLevel::Virtualized
            && self.profile.level(Capability::LinuxElf) != SupportLevel::Virtualized
        {
            return Err(VmError::RequiredGuestCapability {
                capability: Capability::LinuxElf,
                state: crate::GuestCapabilityState::Missing,
            });
        }
        self.channel.execute(command)
    }
}

impl fmt::Debug for BootedVm {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BootedVm")
            .field("profile", &self.profile)
            .field("session", &self.session)
            .field("kernel_evidence", &self.kernel_evidence)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct GuestSession {
    id: String,
    version: ProtocolVersion,
    peer: PeerInfo,
    capabilities: GuestCapabilities,
    limits: GuestLimits,
}

impl GuestSession {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn version(&self) -> ProtocolVersion {
        self.version
    }

    #[must_use]
    pub fn peer(&self) -> &PeerInfo {
        &self.peer
    }

    #[must_use]
    pub fn capabilities(&self) -> &GuestCapabilities {
        &self.capabilities
    }

    #[must_use]
    pub fn limits(&self) -> &GuestLimits {
        &self.limits
    }
}

fn bootstrap_hello(
    platform: Platform,
    config: &VmConfig,
    contract: &GuestKernelContract,
) -> Result<Envelope, VmError> {
    let sequence = NEXT_BOOTSTRAP_ID.fetch_add(1, Ordering::Relaxed);
    let request_id = RequestId::new(format!("vm-bootstrap-{sequence}"))
        .map_err(|error| VmError::Protocol(error.to_string()))?;
    let hello = Hello::host(
        request_id,
        PeerInfo {
            name: "rish-vm-host".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            platform: platform_name(platform).to_owned(),
            architecture: config.architecture.clone(),
        },
        mapping::requested_capability_names(contract)?,
        DEFAULT_MAX_FRAME_SIZE as u32,
    );
    Ok(Envelope::with_version(
        CURRENT_PROTOCOL_VERSION,
        Message::Hello(hello),
    ))
}

fn verify_handshake(
    request: &Envelope,
    response: Envelope,
    config: &VmConfig,
) -> Result<GuestSession, VmError> {
    if response.protocol != PROTOCOL_ID {
        return Err(VmError::Protocol(format!(
            "unexpected protocol discriminator {}",
            response.protocol
        )));
    }
    let request_id = match &request.message {
        Message::Hello(hello) => &hello.request_id,
        _ => return Err(VmError::Protocol("host bootstrap was not Hello".to_owned())),
    };
    let ack = match response.message {
        Message::HelloAck(ack) => ack,
        _ => {
            return Err(VmError::Protocol(
                "guest did not return HelloAck".to_owned(),
            ));
        }
    };
    if &ack.request_id != request_id {
        return Err(VmError::Protocol(
            "HelloAck request ID does not match Hello".to_owned(),
        ));
    }

    let HandshakeOutcome::Accepted {
        selected_version,
        session_id,
        peer,
        capabilities,
        limits,
    } = ack.outcome
    else {
        let HandshakeOutcome::Rejected { error } = ack.outcome else {
            unreachable!("handshake outcome is exhaustive")
        };
        return Err(VmError::HandshakeRejected(error.message));
    };

    validate_accepted_handshake(
        response.version,
        selected_version,
        &session_id,
        &peer,
        &capabilities,
        &limits,
        config,
    )?;
    Ok(GuestSession {
        id: session_id,
        version: selected_version,
        peer,
        capabilities: *capabilities,
        limits,
    })
}

#[allow(clippy::too_many_arguments)]
fn validate_accepted_handshake(
    envelope_version: ProtocolVersion,
    selected_version: ProtocolVersion,
    session_id: &str,
    peer: &PeerInfo,
    capabilities: &GuestCapabilities,
    limits: &GuestLimits,
    config: &VmConfig,
) -> Result<(), VmError> {
    if envelope_version != selected_version
        || !SUPPORTED_PROTOCOL_VERSIONS.contains(&selected_version)
    {
        return Err(VmError::Protocol(format!(
            "unaccepted guest protocol version {envelope_version}/{selected_version}"
        )));
    }
    if session_id.is_empty() {
        return Err(VmError::Protocol("guest session ID is empty".to_owned()));
    }
    if peer.platform != "linux"
        || peer.architecture != config.architecture
        || capabilities.architecture != config.architecture
    {
        return Err(VmError::Protocol(
            "guest identity does not match the configured Linux architecture".to_owned(),
        ));
    }
    let minimum = rish_guest_protocol::MIN_NEGOTIATED_FRAME_SIZE as u32;
    if limits.max_frame_size < minimum
        || limits.max_frame_size > DEFAULT_MAX_FRAME_SIZE as u32
        || limits.max_stream_chunk_size == 0
        || limits.max_stream_chunk_size > limits.max_frame_size
    {
        return Err(VmError::Protocol(
            "guest returned invalid negotiated limits".to_owned(),
        ));
    }
    let mut names = HashSet::new();
    for feature in &capabilities.features {
        if feature.name.is_empty() || !names.insert(feature.name.as_str()) {
            return Err(VmError::Protocol(
                "guest returned empty or duplicate capability names".to_owned(),
            ));
        }
        if feature.status == rish_guest_protocol::CapabilityStatus::Available
            && feature.name == rish_guest_protocol::capability_name::EXEC
            && limits.max_concurrent_exec == 0
        {
            return Err(VmError::Protocol(
                "guest advertises process.exec without execution capacity".to_owned(),
            ));
        }
        if feature.status == rish_guest_protocol::CapabilityStatus::Available
            && feature.name == rish_guest_protocol::capability_name::PORT_FORWARDING
            && limits.max_port_forwards == 0
        {
            return Err(VmError::Protocol(
                "guest advertises port forwarding without forwarding capacity".to_owned(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "boot_tests.rs"]
mod tests;
