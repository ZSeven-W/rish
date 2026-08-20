use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use rish_core::{Capability, CapabilityRequirement, GuestCommand};
use rish_runtime::{BackendCandidate, BackendClass};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const HANDLER_LABEL: &str = "io.rish.offload.handler";
pub const REQUIRES_LABEL: &str = "io.rish.requires";
pub const REQUIRES_KERNEL_LABEL: &str = "io.rish.requires-kernel";
const MAX_HANDLER_LENGTH: usize = 128;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImageConfig {
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub entrypoint: Vec<String>,
    #[serde(default)]
    pub cmd: Vec<String>,
    #[serde(default)]
    pub working_dir: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub user: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RootFilesystem {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub diff_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageConfiguration {
    pub architecture: String,
    pub os: String,
    pub config: ImageConfig,
    pub rootfs: RootFilesystem,
}

impl ImageConfiguration {
    pub fn validate(&self) -> Result<(), OciError> {
        if self.os != "linux" || self.architecture != "arm64" {
            return Err(OciError::UnsupportedPlatform {
                os: self.os.clone(),
                architecture: self.architecture.clone(),
            });
        }
        self.validate_metadata()
    }

    /// Validates metadata that may be stored for a full Linux guest.
    ///
    /// This does not assert that the current host can execute the image. A VM
    /// launch path must separately match this architecture against the
    /// authenticated guest session before dispatch.
    pub fn validate_linux_guest_metadata(&self) -> Result<(), OciError> {
        if self.os != "linux" || !matches!(self.architecture.as_str(), "arm64" | "amd64") {
            return Err(OciError::UnsupportedPlatform {
                os: self.os.clone(),
                architecture: self.architecture.clone(),
            });
        }
        self.validate_metadata()
    }

    fn validate_metadata(&self) -> Result<(), OciError> {
        if self.rootfs.kind != "layers" {
            return Err(OciError::InvalidRootFilesystem(format!(
                "rootfs type must be layers, got {}",
                self.rootfs.kind
            )));
        }
        for diff_id in &self.rootfs.diff_ids {
            validate_sha256_diff_id(diff_id)?;
        }
        if !self.config.working_dir.is_empty() && !self.config.working_dir.starts_with('/') {
            return Err(OciError::InvalidWorkingDirectory(
                self.config.working_dir.clone(),
            ));
        }
        if self
            .config
            .env
            .iter()
            .any(|entry| !valid_environment_entry(entry))
        {
            return Err(OciError::InvalidEnvironment);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OffloadContract {
    pub handler: String,
    #[serde(default)]
    pub requirements: Vec<CapabilityRequirement>,
}

impl OffloadContract {
    pub fn from_image(image: &ImageConfiguration) -> Result<Option<Self>, OciError> {
        let Some(handler) = image.config.labels.get(HANDLER_LABEL) else {
            return Ok(None);
        };
        if !valid_handler(handler) {
            return Err(OciError::InvalidContract(
                "offload handler must be a 1-128 byte ASCII identifier beginning with an \
                 alphanumeric and containing only letters, digits, '.', '_' or '-'"
                    .to_owned(),
            ));
        }

        let mut requirements = parse_requirements(image.config.labels.get(REQUIRES_LABEL), false)?;
        requirements.extend(parse_requirements(
            image.config.labels.get(REQUIRES_KERNEL_LABEL),
            true,
        )?);

        Ok(Some(Self {
            handler: handler.clone(),
            requirements,
        }))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ImageExecutionPlan {
    NativeOffload {
        handler: String,
        command: GuestCommand,
    },
    NativeLinux {
        command: GuestCommand,
    },
    VirtualMachine {
        command: GuestCommand,
    },
    Rejected {
        reason: String,
    },
}

/// Host-owned evidence that a named native image handler is actually bound.
///
/// Image labels are untrusted declarations and cannot register handlers.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OffloadHandlerRegistry {
    handlers: BTreeSet<String>,
}

impl OffloadHandlerRegistry {
    pub fn register(&mut self, handler: impl Into<String>) -> Result<(), OciError> {
        let handler = handler.into();
        if !valid_handler(&handler) {
            return Err(OciError::InvalidContract(
                "registered handler must use the canonical offload identifier grammar".to_owned(),
            ));
        }
        self.handlers.insert(handler);
        Ok(())
    }

    #[must_use]
    pub fn contains(&self, handler: &str) -> bool {
        self.handlers.contains(handler)
    }
}

#[must_use]
pub fn plan_image(
    image: &ImageConfiguration,
    backend: &BackendCandidate,
    handlers: &OffloadHandlerRegistry,
) -> ImageExecutionPlan {
    let validation = match backend.class() {
        BackendClass::FullVirtualMachine => image.validate_linux_guest_metadata(),
        BackendClass::PortableOffload | BackendClass::NativeLinux => image.validate(),
    };
    if let Err(error) = validation {
        return ImageExecutionPlan::Rejected {
            reason: error.to_string(),
        };
    }
    if image.config.entrypoint.is_empty() && image.config.cmd.is_empty() {
        return ImageExecutionPlan::Rejected {
            reason: OciError::MissingCommand.to_string(),
        };
    }

    match backend.class() {
        BackendClass::PortableOffload => match OffloadContract::from_image(image) {
            Ok(Some(contract)) => {
                if !handlers.contains(&contract.handler) {
                    return ImageExecutionPlan::Rejected {
                        reason: format!(
                            "native offload handler is not bound by the host: {}",
                            contract.handler
                        ),
                    };
                }
                let mut requirements = contract.requirements;
                requirements.push(CapabilityRequirement::any(Capability::CommandOffload));
                if let Err(error) = backend.profile().require_all(&requirements) {
                    return ImageExecutionPlan::Rejected {
                        reason: error.to_string(),
                    };
                }
                ImageExecutionPlan::NativeOffload {
                    handler: contract.handler,
                    command: image_command(image),
                }
            }
            Err(error) => ImageExecutionPlan::Rejected {
                reason: error.to_string(),
            },
            Ok(None) => ImageExecutionPlan::Rejected {
                reason: "image has no bound rish native-offload contract".to_owned(),
            },
        },
        BackendClass::NativeLinux => {
            if backend.profile().level(Capability::LinuxElf) != rish_core::SupportLevel::Native {
                return ImageExecutionPlan::Rejected {
                    reason: "native backend lacks exact Linux ELF evidence".to_owned(),
                };
            }
            ImageExecutionPlan::NativeLinux {
                command: image_command(image),
            }
        }
        BackendClass::FullVirtualMachine => {
            let Some(guest_architecture) = backend.verified_guest_architecture() else {
                return ImageExecutionPlan::Rejected {
                    reason: "full VM backend lacks a verified guest architecture".to_owned(),
                };
            };
            if image.architecture != guest_architecture {
                return ImageExecutionPlan::Rejected {
                    reason: OciError::GuestArchitectureMismatch {
                        image: image.architecture.clone(),
                        guest: guest_architecture.to_owned(),
                    }
                    .to_string(),
                };
            }
            let profile = backend.profile();
            let has_exec = profile.level(Capability::LinuxElf)
                == rish_core::SupportLevel::Virtualized
                || profile.level(Capability::CommandOffload)
                    == rish_core::SupportLevel::Virtualized;
            if profile.level(Capability::FullVirtualMachine) != rish_core::SupportLevel::Virtualized
                || !has_exec
            {
                return ImageExecutionPlan::Rejected {
                    reason: "full VM backend lacks exact VM/exec evidence".to_owned(),
                };
            }
            ImageExecutionPlan::VirtualMachine {
                command: image_command(image),
            }
        }
    }
}

fn image_command(image: &ImageConfiguration) -> GuestCommand {
    let mut words = image.config.entrypoint.clone();
    words.extend(image.config.cmd.clone());

    let program = words.first().cloned().unwrap_or_default();
    let args = words.into_iter().skip(1).collect();
    let env = image
        .config
        .env
        .iter()
        .filter_map(|entry| entry.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();

    GuestCommand {
        program,
        args,
        env,
        cwd: if image.config.working_dir.is_empty() {
            "/".to_owned()
        } else {
            image.config.working_dir.clone()
        },
        stdin: Vec::new(),
    }
}

fn valid_handler(handler: &str) -> bool {
    !handler.is_empty()
        && handler.len() <= MAX_HANDLER_LENGTH
        && handler
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && handler
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_environment_entry(entry: &str) -> bool {
    let Some((name, _value)) = entry.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !name.contains('\0')
        && !name.contains('=')
        && name.bytes().all(|byte| !byte.is_ascii_control())
}

fn validate_sha256_diff_id(diff_id: &str) -> Result<(), OciError> {
    let Some(encoded) = diff_id.strip_prefix("sha256:") else {
        return Err(OciError::UnsupportedDiffId(diff_id.to_owned()));
    };
    if encoded.len() != 64
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(OciError::InvalidDiffId(diff_id.to_owned()));
    }
    Ok(())
}

fn parse_requirements(
    value: Option<&String>,
    kernel_semantics_required: bool,
) -> Result<Vec<CapabilityRequirement>, OciError> {
    value
        .map_or("", String::as_str)
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| {
            Capability::from_str(item)
                .map(|capability| CapabilityRequirement {
                    capability,
                    kernel_semantics_required,
                })
                .map_err(|_| OciError::UnknownCapability(item.to_owned()))
        })
        .collect()
}

#[derive(Debug, Error)]
pub enum OciError {
    #[error("invalid rish OCI contract: {0}")]
    InvalidContract(String),

    #[error("unknown capability in OCI contract: {0}")]
    UnknownCapability(String),

    #[error("unsupported OCI platform: {os}/{architecture}")]
    UnsupportedPlatform { os: String, architecture: String },

    #[error("OCI image architecture {image} does not match verified guest architecture {guest}")]
    GuestArchitectureMismatch { image: String, guest: String },

    #[error("invalid OCI root filesystem: {0}")]
    InvalidRootFilesystem(String),

    #[error("unsupported OCI diff ID algorithm: {0}")]
    UnsupportedDiffId(String),

    #[error("invalid OCI diff ID: {0}")]
    InvalidDiffId(String),

    #[error("OCI WorkingDir must be empty or absolute: {0}")]
    InvalidWorkingDirectory(String),

    #[error("OCI Env entries must have a non-empty NAME=VALUE form")]
    InvalidEnvironment,

    #[error("OCI image has neither Entrypoint nor Cmd")]
    MissingCommand,
}

#[cfg(test)]
mod tests {
    use rish_core::{Capability, GuestCommand, HostReply, Platform, PrivilegeMode};
    use rish_guest_protocol::{
        CURRENT_PROTOCOL_VERSION, Capability as GuestCapability, CapabilityStatus, Envelope,
        GuestCapabilities, GuestLimits, HandshakeOutcome, HelloAck, Message, PeerInfo,
        capability_name,
    };
    use rish_runtime::portable_offload_profile;
    use rish_vm::{
        GuestChannel, GuestKernelContract, GuestKernelEvidence, GuestSession, KernelEvidenceSource,
        VmAcceleration, VmCandidate, VmConfig, VmDevice, VmEngine, VmError, VmProbe,
    };

    use super::*;

    struct TestEngine;

    struct TestChannel {
        architecture: String,
    }

    impl VmEngine for TestEngine {
        fn probe(&self) -> VmProbe {
            VmProbe::available([VmAcceleration::Interpreter])
        }

        fn boot(&self, config: &VmConfig) -> Result<Box<dyn GuestChannel>, VmError> {
            Ok(Box::new(TestChannel {
                architecture: config.architecture.clone(),
            }))
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
                        session_id: "oci-architecture-test".to_owned(),
                        peer: PeerInfo {
                            name: "guest".to_owned(),
                            version: "0.1.0".to_owned(),
                            platform: "linux".to_owned(),
                            architecture: self.architecture.clone(),
                        },
                        capabilities: Box::new(GuestCapabilities {
                            kernel_release: "6.12".to_owned(),
                            architecture: self.architecture.clone(),
                            init_system: "systemd".to_owned(),
                            cgroup_version: Some(2),
                            container_runtimes: vec!["youki".to_owned()],
                            features: vec![GuestCapability {
                                name: capability_name::EXEC.to_owned(),
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
                "oci-architecture-test",
                KernelEvidenceSource::BuildManifest {
                    kernel_release: "6.12".to_owned(),
                    sha256: "a".repeat(64),
                },
                ["CONFIG_BINFMT_ELF"],
            )
        }

        fn execute(&self, _command: &GuestCommand) -> Result<HostReply, VmError> {
            Err(VmError::Guest("not used by OCI planner tests".to_owned()))
        }
    }

    fn full_vm_backend(guest_architecture: &str) -> BackendCandidate {
        let vm = VmCandidate::new(
            Platform::Ios,
            VmConfig {
                architecture: guest_architecture.to_owned(),
                vcpus: 1,
                memory_mib: 512,
                kernel_path: "/app/kernel".to_owned(),
                initrd_path: None,
                root_disk_path: "/app/root.img".to_owned(),
                acceleration: VmAcceleration::Interpreter,
                devices: vec![VmDevice::Console],
            },
            GuestKernelContract::new(["CONFIG_BINFMT_ELF"], [Capability::LinuxElf]),
        )
        .boot(&TestEngine)
        .unwrap();
        BackendCandidate::full_virtual_machine(&vm, 0).unwrap()
    }

    fn portable_backend(platform: Platform) -> BackendCandidate {
        BackendCandidate::portable_offload(
            portable_offload_profile(platform, PrivilegeMode::AppSandbox),
            0,
        )
        .unwrap()
    }

    fn image(labels: BTreeMap<String, String>) -> ImageConfiguration {
        ImageConfiguration {
            architecture: "arm64".to_owned(),
            os: "linux".to_owned(),
            config: ImageConfig {
                entrypoint: vec!["/usr/bin/demo".to_owned()],
                cmd: vec!["serve".to_owned()],
                labels,
                ..ImageConfig::default()
            },
            rootfs: RootFilesystem {
                kind: "layers".to_owned(),
                diff_ids: Vec::new(),
            },
        }
    }

    #[test]
    fn metadata_validation_allows_a_runtime_command_override() {
        let mut image = image(BTreeMap::new());
        image.config.entrypoint.clear();
        image.config.cmd.clear();

        image.validate().unwrap();

        let backend = portable_backend(Platform::Ios);
        assert!(matches!(
            plan_image(&image, &backend, &OffloadHandlerRegistry::default()),
            ImageExecutionPlan::Rejected { reason }
                if reason == OciError::MissingCommand.to_string()
        ));
    }

    #[test]
    fn amd64_guest_metadata_does_not_widen_portable_or_native_planning() {
        let mut image = image(BTreeMap::new());
        image.architecture = "amd64".to_owned();

        image.validate_linux_guest_metadata().unwrap();
        assert!(matches!(
            image.validate(),
            Err(OciError::UnsupportedPlatform { architecture, .. })
                if architecture == "amd64"
        ));

        let portable = portable_backend(Platform::Ios);
        assert!(matches!(
            plan_image(
                &image,
                &portable,
                &OffloadHandlerRegistry::default()
            ),
            ImageExecutionPlan::Rejected { reason }
                if reason.contains("unsupported OCI platform")
        ));
    }

    #[test]
    fn full_vm_executes_only_an_exact_verified_guest_architecture_match() {
        let arm64 = full_vm_backend("aarch64");
        let amd64 = full_vm_backend("x86_64");
        let mut arm64_image = image(BTreeMap::new());
        let mut amd64_image = image(BTreeMap::new());
        amd64_image.architecture = "amd64".to_owned();

        assert!(matches!(
            plan_image(&arm64_image, &arm64, &OffloadHandlerRegistry::default()),
            ImageExecutionPlan::VirtualMachine { .. }
        ));
        assert!(matches!(
            plan_image(&amd64_image, &amd64, &OffloadHandlerRegistry::default()),
            ImageExecutionPlan::VirtualMachine { .. }
        ));

        for (candidate, guest, candidate_image, image_architecture) in [
            (&arm64, "arm64", &amd64_image, "amd64"),
            (&amd64, "amd64", &arm64_image, "arm64"),
        ] {
            assert!(matches!(
                plan_image(
                    candidate_image,
                    candidate,
                    &OffloadHandlerRegistry::default()
                ),
                ImageExecutionPlan::Rejected { reason }
                    if reason == OciError::GuestArchitectureMismatch {
                        image: image_architecture.to_owned(),
                        guest: guest.to_owned(),
                    }
                    .to_string()
            ));
        }

        arm64_image.os = "darwin".to_owned();
        assert!(matches!(
            plan_image(
                &arm64_image,
                &arm64,
                &OffloadHandlerRegistry::default()
            ),
            ImageExecutionPlan::Rejected { reason }
                if reason.contains("unsupported OCI platform")
        ));
    }

    #[test]
    fn portable_backend_accepts_declared_native_offload() {
        let labels = BTreeMap::from([(HANDLER_LABEL.to_owned(), "sample.demo".to_owned())]);
        let backend = portable_backend(Platform::Ios);
        let mut handlers = OffloadHandlerRegistry::default();
        handlers.register("sample.demo").unwrap();

        assert!(matches!(
            plan_image(&image(labels), &backend, &handlers),
            ImageExecutionPlan::NativeOffload { handler, .. } if handler == "sample.demo"
        ));
    }

    #[test]
    fn image_labels_cannot_register_their_own_native_handler() {
        let labels = BTreeMap::from([(HANDLER_LABEL.to_owned(), "sample.demo".to_owned())]);
        let backend = portable_backend(Platform::Ios);

        assert!(matches!(
            plan_image(
                &image(labels),
                &backend,
                &OffloadHandlerRegistry::default()
            ),
            ImageExecutionPlan::Rejected { reason } if reason.contains("not bound")
        ));
    }

    #[test]
    fn portable_backend_rejects_unknown_elf() {
        let backend = portable_backend(Platform::Ios);

        assert!(matches!(
            plan_image(
                &image(BTreeMap::new()),
                &backend,
                &OffloadHandlerRegistry::default()
            ),
            ImageExecutionPlan::Rejected { .. }
        ));
    }

    #[test]
    fn native_requirement_rejects_emulated_namespace() {
        let labels = BTreeMap::from([
            (HANDLER_LABEL.to_owned(), "sample.demo".to_owned()),
            (
                REQUIRES_KERNEL_LABEL.to_owned(),
                "network_namespace".to_owned(),
            ),
        ]);
        let backend = portable_backend(Platform::Android);
        let mut handlers = OffloadHandlerRegistry::default();
        handlers.register("sample.demo").unwrap();

        assert!(matches!(
            plan_image(&image(labels), &backend, &handlers),
            ImageExecutionPlan::Rejected { .. }
        ));
    }

    #[test]
    fn rejects_malformed_offload_handler() {
        let labels = BTreeMap::from([(HANDLER_LABEL.to_owned(), "sample.demo\nforged".to_owned())]);
        let backend = portable_backend(Platform::Ios);

        assert!(matches!(
            plan_image(
                &image(labels),
                &backend,
                &OffloadHandlerRegistry::default()
            ),
            ImageExecutionPlan::Rejected { reason } if reason.contains("offload handler")
        ));
    }

    #[test]
    fn rejects_invalid_image_process_configuration() {
        let backend = portable_backend(Platform::Ios);
        let mut invalid = image(BTreeMap::new());
        invalid.config.working_dir = "relative".to_owned();

        assert!(matches!(
            plan_image(&invalid, &backend, &OffloadHandlerRegistry::default()),
            ImageExecutionPlan::Rejected { reason } if reason.contains("WorkingDir")
        ));

        invalid.config.working_dir = "/".to_owned();
        invalid.config.env = vec!["MISSING_EQUALS".to_owned()];
        assert!(matches!(
            plan_image(&invalid, &backend, &OffloadHandlerRegistry::default()),
            ImageExecutionPlan::Rejected { reason } if reason.contains("Env")
        ));
    }

    #[test]
    fn accepts_canonical_sha256_diff_ids() {
        let mut valid = image(BTreeMap::new());
        valid.rootfs.diff_ids = vec![format!("sha256:{}", "a".repeat(64))];

        valid.validate().unwrap();
    }
}
