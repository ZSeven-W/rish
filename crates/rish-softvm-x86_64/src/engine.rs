use std::{fmt, sync::Arc};

use rish_vm::{
    GuestChannel, VmAcceleration, VmConfig, VmDevice, VmEngine, VmError, VmNetworkMode, VmProbe,
};

use crate::{
    EngineLimits, KernelFormat, MachineProvider, ProviderRequest, SerialGuestTransport,
    SoftVmError, ValidatedArtifacts, X86_64Machine, abi, config::GUEST_ARCHITECTURE,
    worker::spawn_worker,
};

/// x86-64 software VM engine backed by a reviewed provider.
///
/// [`Default`] is intentionally unavailable. A platform adapter must inject a
/// provider whose pinned build metadata passes the production gate.
#[derive(Clone, Default)]
pub struct X86_64SoftwareEngine {
    provider: Option<Arc<dyn MachineProvider>>,
    limits: EngineLimits,
}

impl X86_64SoftwareEngine {
    pub fn new(
        provider: Arc<dyn MachineProvider>,
        limits: EngineLimits,
    ) -> Result<Self, SoftVmError> {
        limits.validate()?;
        provider.build_info().validate_production()?;
        Ok(Self {
            provider: Some(provider),
            limits,
        })
    }

    #[must_use]
    pub fn limits(&self) -> &EngineLimits {
        &self.limits
    }

    #[must_use]
    pub fn provider_build_info(&self) -> Option<&crate::ProviderBuildInfo> {
        self.provider.as_ref().map(|provider| provider.build_info())
    }

    /// Loads a paused provider machine without claiming a verified guest.
    pub fn launch(&self, config: &VmConfig) -> Result<X86_64Machine, SoftVmError> {
        self.limits.validate()?;
        let provider = self
            .provider
            .as_ref()
            .ok_or_else(|| {
                SoftVmError::ProviderUnavailable(
                    "no pinned x86_64 TCTI provider was linked".to_owned(),
                )
            })?
            .clone();
        let build = provider.build_info();
        build.validate_production()?;
        let network_mode = validate_engine_config(config, build)?;
        let artifacts = ValidatedArtifacts::load(config, &self.limits)?;
        if artifacts.kernel_format != KernelFormat::LinuxBzImage {
            return Err(SoftVmError::InvalidKernel(
                "the production TCTI provider requires a 64-bit Linux bzImage".to_owned(),
            ));
        }
        if artifacts.initrd.is_some() && !build.supports(abi::FEATURE_INITRD) {
            return Err(SoftVmError::InvalidConfig(
                "the linked provider does not declare initrd support".to_owned(),
            ));
        }
        let request = ProviderRequest {
            memory_mib: config.memory_mib,
            vcpus: u32::from(config.vcpus),
            artifacts,
            network_mode,
        };
        let worker = spawn_worker(provider, request, &self.limits)?;
        Ok(X86_64Machine::new(worker, self.limits.clone()))
    }
}

impl fmt::Debug for X86_64SoftwareEngine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("X86_64SoftwareEngine")
            .field(
                "provider",
                &self.provider.as_ref().map(|provider| provider.build_info()),
            )
            .field("limits", &self.limits)
            .finish()
    }
}

impl VmEngine for X86_64SoftwareEngine {
    fn probe(&self) -> VmProbe {
        let Some(provider) = &self.provider else {
            return VmProbe::unavailable("no pinned x86_64 TCTI provider was linked");
        };
        match provider.build_info().validate_production() {
            Ok(()) => VmProbe::available([VmAcceleration::Interpreter]),
            Err(error) => VmProbe::unavailable(error.to_string()),
        }
    }

    fn boot(&self, config: &VmConfig) -> Result<Box<dyn GuestChannel>, VmError> {
        let machine = self
            .launch(config)
            .map_err(|error| VmError::Boot(error.to_string()))?;
        let transport = SerialGuestTransport::new(machine, self.limits.clone())
            .map_err(|error| VmError::Boot(error.to_string()))?;
        Ok(Box::new(transport))
    }
}

fn validate_engine_config(
    config: &VmConfig,
    build: &crate::ProviderBuildInfo,
) -> Result<u32, SoftVmError> {
    config
        .validate()
        .map_err(|error| SoftVmError::InvalidConfig(error.to_string()))?;
    if config.architecture != GUEST_ARCHITECTURE {
        return Err(SoftVmError::InvalidConfig(
            "this engine only executes linux/amd64 (x86_64) guests".to_owned(),
        ));
    }
    if config.acceleration != VmAcceleration::Interpreter {
        return Err(SoftVmError::InvalidConfig(
            "x86_64 software mode requires interpreter acceleration".to_owned(),
        ));
    }
    if u32::from(config.vcpus) > build.max_vcpus {
        return Err(SoftVmError::InvalidConfig(format!(
            "provider supports at most {} vCPUs",
            build.max_vcpus
        )));
    }
    if config.memory_mib < build.min_memory_mib || config.memory_mib > build.max_memory_mib {
        return Err(SoftVmError::InvalidConfig(format!(
            "provider memory range is {}..={} MiB",
            build.min_memory_mib, build.max_memory_mib
        )));
    }

    let mut network = None;
    for device in &config.devices {
        match device {
            VmDevice::Console => {}
            VmDevice::Network { mode } => {
                if network.replace(*mode).is_some() {
                    return Err(SoftVmError::InvalidConfig(
                        "only one network device is supported".to_owned(),
                    ));
                }
            }
            _ => {
                return Err(SoftVmError::InvalidConfig(
                    "the TCTI milestone supports only console, root block, and optional user NAT"
                        .to_owned(),
                ));
            }
        }
    }
    match network.unwrap_or(VmNetworkMode::Disabled) {
        VmNetworkMode::Disabled => Ok(abi::NETWORK_DISABLED),
        VmNetworkMode::UserNat if build.supports(abi::FEATURE_USER_NETWORK) => {
            Ok(abi::NETWORK_USER_NAT)
        }
        VmNetworkMode::UserNat => Err(SoftVmError::InvalidConfig(
            "the linked provider does not declare user-mode networking".to_owned(),
        )),
        VmNetworkMode::Tap => Err(SoftVmError::InvalidConfig(
            "tap networking is unavailable inside a stock mobile app".to_owned(),
        )),
    }
}
