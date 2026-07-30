use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use rish_core::{
    Capability, CapabilityProfile, CapabilityRequirement, ExecutionOutcome, ExecutionPath,
    GuestCommand, HostBridge, HostCall, RuntimeError,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OffloadSpec {
    pub operation: String,
    #[serde(default)]
    pub requirements: Vec<CapabilityRequirement>,
}

impl OffloadSpec {
    #[must_use]
    pub fn new(
        operation: impl Into<String>,
        requirements: impl IntoIterator<Item = CapabilityRequirement>,
    ) -> Self {
        Self {
            operation: operation.into(),
            requirements: requirements.into_iter().collect(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct OffloadRegistry {
    commands: BTreeMap<String, OffloadSpec>,
}

impl OffloadRegistry {
    #[must_use]
    pub fn portable_defaults() -> Self {
        let mut registry = Self::default();

        registry.register(
            "mount",
            OffloadSpec::new(
                "kernel.mount",
                [CapabilityRequirement::any(Capability::MountNamespace)],
            ),
        );
        registry.register(
            "umount",
            OffloadSpec::new(
                "kernel.umount",
                [CapabilityRequirement::any(Capability::MountNamespace)],
            ),
        );
        registry.register(
            "systemctl",
            OffloadSpec::new(
                "service.systemctl",
                [CapabilityRequirement::any(Capability::Systemd)],
            ),
        );
        registry.register(
            "init",
            OffloadSpec::new(
                "service.init",
                [CapabilityRequirement::kernel(Capability::Systemd)],
            ),
        );
        registry.register(
            "docker",
            OffloadSpec::new(
                "container.docker_api",
                [CapabilityRequirement::any(Capability::OciImages)],
            ),
        );
        for command in ["dockerd", "containerd"] {
            registry.register(
                command,
                OffloadSpec::new(
                    "container.nested_daemon",
                    [
                        CapabilityRequirement::kernel(Capability::NestedContainers),
                        CapabilityRequirement::kernel(Capability::CgroupsV2),
                    ],
                ),
            );
        }
        registry.register(
            "runc",
            OffloadSpec::new(
                "container.oci_runtime",
                [
                    CapabilityRequirement::kernel(Capability::PrivilegedContainers),
                    CapabilityRequirement::kernel(Capability::ProcessNamespace),
                    CapabilityRequirement::kernel(Capability::MountNamespace),
                ],
            ),
        );
        for command in ["modprobe", "insmod", "rmmod"] {
            registry.register(
                command,
                OffloadSpec::new(
                    "kernel.module",
                    [CapabilityRequirement::kernel(Capability::KernelModules)],
                ),
            );
        }
        registry.register(
            "mknod",
            OffloadSpec::new(
                "device.mknod",
                [CapabilityRequirement::any(Capability::DeviceNodes)],
            ),
        );
        for command in ["ip", "iptables", "nft"] {
            registry.register(
                command,
                OffloadSpec::new(
                    "network.admin",
                    [CapabilityRequirement::any(Capability::NetworkNamespace)],
                ),
            );
        }
        registry.register(
            "unshare",
            OffloadSpec::new("kernel.unshare", std::iter::empty()),
        );
        registry.register(
            "nsenter",
            OffloadSpec::new("kernel.nsenter", std::iter::empty()),
        );

        registry
    }

    pub fn register(&mut self, command: impl Into<String>, spec: OffloadSpec) {
        self.commands.insert(command.into(), spec);
    }

    #[must_use]
    pub fn resolve(&self, command: &str) -> Option<&OffloadSpec> {
        self.commands.get(command)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CommandPlan {
    Builtin { outcome: ExecutionOutcome },
    HostCall { call: HostCall },
}

pub struct Planner {
    profile: CapabilityProfile,
    registry: OffloadRegistry,
    next_call_id: AtomicU64,
}

impl Planner {
    #[must_use]
    pub fn new(profile: CapabilityProfile, registry: OffloadRegistry) -> Self {
        Self {
            profile,
            registry,
            next_call_id: AtomicU64::new(1),
        }
    }

    #[must_use]
    pub fn profile(&self) -> &CapabilityProfile {
        &self.profile
    }

    pub fn plan(&self, command: &GuestCommand) -> Result<CommandPlan, RuntimeError> {
        if let Some(outcome) = plan_builtin(command, &self.profile) {
            return Ok(CommandPlan::Builtin { outcome });
        }

        if let Some(spec) = self.registry.resolve(command.basename()) {
            let mut requirements = spec.requirements.clone();
            if matches!(command.basename(), "unshare" | "nsenter") {
                requirements.extend(namespace_requirements(command));
            }
            self.profile.require_all(&requirements)?;
            return Ok(CommandPlan::HostCall {
                call: self.host_call(command, spec.operation.clone(), requirements),
            });
        }

        if self.profile.level(Capability::LinuxElf).is_runnable() {
            let requirements = vec![CapabilityRequirement::any(Capability::LinuxElf)];
            return Ok(CommandPlan::HostCall {
                call: self.host_call(command, "linux.exec".to_owned(), requirements),
            });
        }

        if self
            .profile
            .level(Capability::FullVirtualMachine)
            .is_runnable()
        {
            let requirements = vec![CapabilityRequirement::any(Capability::FullVirtualMachine)];
            return Ok(CommandPlan::HostCall {
                call: self.host_call(command, "vm.exec".to_owned(), requirements),
            });
        }

        Err(RuntimeError::UnknownExecutable(command.program.clone()))
    }

    fn host_call(
        &self,
        command: &GuestCommand,
        operation: String,
        requirements: Vec<CapabilityRequirement>,
    ) -> HostCall {
        HostCall {
            protocol_version: 1,
            id: self.next_call_id.fetch_add(1, Ordering::Relaxed),
            operation,
            command: command.clone(),
            requirements,
            payload: json!({
                "platform": self.profile.platform,
                "privilege": self.profile.privilege,
                "backend": self.profile.backend,
            }),
        }
    }
}

pub struct Runtime<B> {
    planner: Planner,
    bridge: B,
}

impl<B: HostBridge> Runtime<B> {
    #[must_use]
    pub fn new(planner: Planner, bridge: B) -> Self {
        Self { planner, bridge }
    }

    pub fn execute(&self, command: &GuestCommand) -> Result<ExecutionOutcome, RuntimeError> {
        match self.planner.plan(command)? {
            CommandPlan::Builtin { outcome } => Ok(outcome),
            CommandPlan::HostCall { call } => {
                let operation = call.operation.clone();
                let reply = self.bridge.invoke(&call)?;
                Ok(ExecutionOutcome {
                    exit_code: reply.exit_code,
                    stdout: reply.stdout,
                    stderr: reply.stderr,
                    path: ExecutionPath::NativeOffload { operation },
                    warnings: support_warnings(&self.planner.profile, &call.requirements),
                })
            }
        }
    }
}

fn plan_builtin(command: &GuestCommand, profile: &CapabilityProfile) -> Option<ExecutionOutcome> {
    let output = match command.basename() {
        "true" => Vec::new(),
        "false" => {
            return Some(ExecutionOutcome {
                exit_code: 1,
                stdout: Vec::new(),
                stderr: Vec::new(),
                path: ExecutionPath::Builtin,
                warnings: Vec::new(),
            });
        }
        "echo" => {
            let mut line = command.args.join(" ").into_bytes();
            line.push(b'\n');
            line
        }
        "pwd" => {
            let mut line = command.cwd.as_bytes().to_vec();
            line.push(b'\n');
            line
        }
        "uname" => format!(
            "Linux rish 0.1.0 {} {:?}\n",
            match profile.platform {
                rish_core::Platform::Ios
                | rish_core::Platform::Android
                | rish_core::Platform::Harmony => "aarch64",
                rish_core::Platform::Linux => std::env::consts::ARCH,
            },
            profile.platform
        )
        .into_bytes(),
        _ => return None,
    };

    Some(ExecutionOutcome::success(output, ExecutionPath::Builtin))
}

fn namespace_requirements(command: &GuestCommand) -> Vec<CapabilityRequirement> {
    let mut requirements = Vec::new();
    let joined = command.args.join(" ");
    for (flags, capability) in [
        (["--pid", "-p"], Capability::ProcessNamespace),
        (["--user", "-U"], Capability::UserNamespace),
        (["--mount", "-m"], Capability::MountNamespace),
        (["--net", "-n"], Capability::NetworkNamespace),
        (["--uts", "-u"], Capability::UtsNamespace),
        (["--ipc", "-i"], Capability::IpcNamespace),
    ] {
        if flags
            .iter()
            .any(|flag| joined.split_whitespace().any(|arg| arg == *flag))
        {
            requirements.push(CapabilityRequirement::any(capability));
        }
    }
    requirements
}

fn support_warnings(
    profile: &CapabilityProfile,
    requirements: &[CapabilityRequirement],
) -> Vec<String> {
    requirements
        .iter()
        .filter_map(|requirement| {
            let level = profile.level(requirement.capability);
            (level == rish_core::SupportLevel::Emulated).then(|| {
                format!(
                    "{} is semantic emulation, not host-kernel isolation",
                    requirement.capability
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use rish_core::{HostReply, Platform, PrivilegeMode};

    use super::*;
    use crate::portable_offload_profile;

    struct EchoBridge;

    impl HostBridge for EchoBridge {
        fn invoke(&self, call: &HostCall) -> Result<HostReply, RuntimeError> {
            Ok(HostReply {
                exit_code: 0,
                stdout: call.operation.as_bytes().to_vec(),
                stderr: Vec::new(),
                payload: serde_json::Value::Null,
            })
        }
    }

    #[test]
    fn docker_cli_routes_to_semantic_api_but_dockerd_fails_closed() {
        let profile = portable_offload_profile(Platform::Ios, PrivilegeMode::AppSandbox);
        let planner = Planner::new(profile, OffloadRegistry::portable_defaults());
        let runtime = Runtime::new(planner, EchoBridge);

        let docker = GuestCommand::new("docker", ["ps".to_owned()]);
        let outcome = runtime.execute(&docker).unwrap();
        assert_eq!(outcome.stdout, b"container.docker_api");

        let dockerd = GuestCommand::new("dockerd", Vec::<String>::new());
        assert!(matches!(
            runtime.execute(&dockerd),
            Err(RuntimeError::MissingCapability(_))
        ));
    }

    #[test]
    fn unshare_reports_semantic_namespace_warning() {
        let profile = portable_offload_profile(Platform::Android, PrivilegeMode::AppSandbox);
        let planner = Planner::new(profile, OffloadRegistry::portable_defaults());
        let runtime = Runtime::new(planner, EchoBridge);
        let command = GuestCommand::new("unshare", ["--net".to_owned(), "--pid".to_owned()]);

        let outcome = runtime.execute(&command).unwrap();
        assert_eq!(outcome.warnings.len(), 2);
    }

    #[test]
    fn unknown_elf_fails_closed_on_portable_backend() {
        let profile = portable_offload_profile(Platform::Harmony, PrivilegeMode::AppSandbox);
        let planner = Planner::new(profile, OffloadRegistry::portable_defaults());
        let command = GuestCommand::new("/usr/bin/python3", Vec::<String>::new());

        assert!(matches!(
            planner.plan(&command),
            Err(RuntimeError::UnknownExecutable(_))
        ));
    }
}
