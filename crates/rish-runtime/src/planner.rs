use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use rish_applets::is_portable_command;
use rish_core::{
    Capability, CapabilityProfile, CapabilityRequirement, ExecutionOutcome, ExecutionPath,
    GuestCommand, HostBridge, HostCall, RuntimeError, SupportLevel,
};
use rish_vm::BootedVm;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::selector::{BackendCandidate, BackendCandidateError, BackendClass};

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
                [CapabilityRequirement::kernel(Capability::MountNamespace)],
            ),
        );
        registry.register(
            "umount",
            OffloadSpec::new(
                "kernel.umount",
                [CapabilityRequirement::kernel(Capability::MountNamespace)],
            ),
        );
        registry.register(
            "systemctl",
            OffloadSpec::new(
                "service.systemctl",
                [CapabilityRequirement::kernel(Capability::Systemd)],
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
                [CapabilityRequirement::kernel(Capability::DeviceNodes)],
            ),
        );
        for command in ["ip", "iptables", "nft"] {
            registry.register(
                command,
                OffloadSpec::new(
                    "network.admin",
                    [CapabilityRequirement::kernel(Capability::NetworkNamespace)],
                ),
            );
        }
        registry.register(
            "unshare",
            OffloadSpec::new(
                "kernel.unshare",
                [CapabilityRequirement::kernel(Capability::LinuxElf)],
            ),
        );
        registry.register(
            "nsenter",
            OffloadSpec::new(
                "kernel.nsenter",
                [CapabilityRequirement::kernel(Capability::LinuxElf)],
            ),
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
    PortableApplet { name: String },
    HostCall { call: HostCall },
}

pub struct Planner {
    candidate: BackendCandidate,
    registry: OffloadRegistry,
    next_call_id: AtomicU64,
}

impl Planner {
    #[must_use]
    pub fn new(candidate: BackendCandidate, registry: OffloadRegistry) -> Self {
        Self {
            candidate,
            registry,
            next_call_id: AtomicU64::new(1),
        }
    }

    #[must_use]
    pub fn profile(&self) -> &CapabilityProfile {
        self.candidate.profile()
    }

    #[must_use]
    pub fn backend_class(&self) -> BackendClass {
        self.candidate.class()
    }

    pub fn plan(&self, command: &GuestCommand) -> Result<CommandPlan, RuntimeError> {
        match self.backend_class() {
            BackendClass::PortableOffload => self.plan_portable(command),
            BackendClass::NativeLinux | BackendClass::FullVirtualMachine => {
                self.plan_kernel_backend(command)
            }
        }
    }

    fn plan_portable(&self, command: &GuestCommand) -> Result<CommandPlan, RuntimeError> {
        if is_portable_command(command) {
            return Ok(CommandPlan::PortableApplet {
                name: command.basename().to_owned(),
            });
        }

        if command.program != command.basename() {
            return Err(RuntimeError::UnknownExecutable(command.program.clone()));
        }

        if let Some(spec) = self.registry.resolve(command.basename()) {
            let mut requirements = spec.requirements.clone();
            if matches!(command.basename(), "unshare" | "nsenter") {
                requirements.extend(namespace_requirements(command));
            }
            self.profile().require_all(&requirements)?;
            return Ok(CommandPlan::HostCall {
                call: self.host_call(command, spec.operation.clone(), requirements),
            });
        }

        Err(RuntimeError::UnknownExecutable(command.program.clone()))
    }

    fn plan_kernel_backend(&self, command: &GuestCommand) -> Result<CommandPlan, RuntimeError> {
        let (operation, backend_requirement, exact_capability, exact_level) =
            match self.backend_class() {
                BackendClass::NativeLinux => (
                    "linux.exec",
                    CapabilityRequirement::kernel(Capability::LinuxElf),
                    Capability::LinuxElf,
                    SupportLevel::Native,
                ),
                BackendClass::FullVirtualMachine => (
                    "vm.exec",
                    CapabilityRequirement::any(Capability::FullVirtualMachine),
                    Capability::FullVirtualMachine,
                    SupportLevel::Virtualized,
                ),
                BackendClass::PortableOffload => {
                    return Err(RuntimeError::InvalidRequest(
                        "portable backend cannot enter kernel execution planning".to_owned(),
                    ));
                }
            };

        let actual_level = self.profile().level(exact_capability);
        if actual_level != exact_level {
            return Err(RuntimeError::InvalidRequest(format!(
                "{:?} backend requires {exact_capability} at exact level {exact_level:?}, actual level is {actual_level:?}",
                self.backend_class()
            )));
        }

        let mut requirements = vec![backend_requirement];
        if self.backend_class() == BackendClass::FullVirtualMachine {
            let exec_capability = if self.profile().level(Capability::LinuxElf)
                == SupportLevel::Virtualized
            {
                Capability::LinuxElf
            } else if self.profile().level(Capability::CommandOffload) == SupportLevel::Virtualized
            {
                Capability::CommandOffload
            } else {
                return Err(RuntimeError::InvalidRequest(
                    "verified VM has no executable guest command channel".to_owned(),
                ));
            };
            requirements.push(CapabilityRequirement::any(exec_capability));
        }
        if command.program == command.basename() {
            if let Some(spec) = self.registry.resolve(command.basename()) {
                requirements.extend(spec.requirements.iter().cloned());
                if matches!(command.basename(), "unshare" | "nsenter") {
                    requirements.extend(namespace_requirements(command));
                }
            }
        }
        self.profile().require_all(&requirements)?;
        Ok(CommandPlan::HostCall {
            call: self.host_call(command, operation.to_owned(), requirements),
        })
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
                "platform": self.profile().platform,
                "privilege": self.profile().privilege,
                "backend": self.profile().backend,
                "backend_class": self.backend_class(),
            }),
        }
    }
}

pub struct Runtime<B> {
    planner: Planner,
    bridge: B,
}

/// Executes commands only through the live VM that produced the verified
/// profile. This is deliberately separate from the generic `HostBridge`,
/// which cannot prove that a reply came from the selected VM session.
pub struct VerifiedVmRuntime<'a> {
    planner: Planner,
    vm: &'a BootedVm,
}

impl<'a> VerifiedVmRuntime<'a> {
    pub fn new(vm: &'a BootedVm, registry: OffloadRegistry) -> Result<Self, BackendCandidateError> {
        let candidate = BackendCandidate::full_virtual_machine(vm, 0)?;
        Ok(Self {
            planner: Planner::new(candidate, registry),
            vm,
        })
    }

    pub fn execute(&self, command: &GuestCommand) -> Result<ExecutionOutcome, RuntimeError> {
        let CommandPlan::HostCall { call } = self.planner.plan(command)? else {
            return Err(RuntimeError::InvalidRequest(
                "verified VM planner returned a non-VM command".to_owned(),
            ));
        };
        if call.operation != "vm.exec" {
            return Err(RuntimeError::InvalidRequest(format!(
                "verified VM executor cannot dispatch {}",
                call.operation
            )));
        }
        let reply = self.vm.execute(command).map_err(|error| {
            RuntimeError::HostBridge(format!("verified VM exec failed: {error}"))
        })?;
        Ok(ExecutionOutcome {
            exit_code: reply.exit_code,
            stdout: reply.stdout,
            stderr: reply.stderr,
            path: ExecutionPath::VirtualMachine,
            warnings: support_warnings(self.planner.profile(), &call.requirements),
        })
    }
}

impl<B: HostBridge> Runtime<B> {
    #[must_use]
    pub fn new(planner: Planner, bridge: B) -> Self {
        Self { planner, bridge }
    }

    pub fn execute(&self, command: &GuestCommand) -> Result<ExecutionOutcome, RuntimeError> {
        match self.planner.plan(command)? {
            CommandPlan::PortableApplet { name } => Err(RuntimeError::NotImplemented(format!(
                "portable applet {name} requires an AppletExecutor"
            ))),
            CommandPlan::HostCall { call } => {
                let operation = call.operation.clone();
                if matches!(operation.as_str(), "linux.exec" | "vm.exec") {
                    return Err(RuntimeError::InvalidRequest(format!(
                        "{operation} requires a live backend-bound executor"
                    )));
                }
                let reply = self.bridge.invoke(&call)?;
                let path = ExecutionPath::NativeOffload {
                    operation: operation.clone(),
                };
                Ok(ExecutionOutcome {
                    exit_code: reply.exit_code,
                    stdout: reply.stdout,
                    stderr: reply.stderr,
                    path,
                    warnings: support_warnings(self.planner.profile(), &call.requirements),
                })
            }
        }
    }
}

fn namespace_requirements(command: &GuestCommand) -> Vec<CapabilityRequirement> {
    let mut capabilities = std::collections::BTreeSet::new();
    for argument in &command.args {
        if argument == "--" {
            break;
        }
        let long = argument.strip_prefix("--").and_then(|value| {
            value
                .split_once('=')
                .map_or(Some(value), |(name, _)| Some(name))
        });
        if let Some(name) = long {
            if command.basename() == "nsenter" && name == "all" {
                capabilities.extend(all_namespace_capabilities());
                continue;
            }
            if command.basename() == "unshare" && matches!(name, "mount-binfmt" | "load-interp") {
                capabilities.insert(Capability::UserNamespace);
                capabilities.insert(Capability::MountNamespace);
                continue;
            }
            if let Some(capability) = namespace_long_flag(command.basename(), name) {
                capabilities.insert(capability);
            } else if !namespace_neutral_long_flag(command.basename(), name) {
                add_conservative_unknown_option_requirements(&mut capabilities);
            }
            continue;
        }
        if let Some(short) = argument.strip_prefix('-') {
            for flag in short.chars() {
                if command.basename() == "nsenter" && flag == 'a' {
                    capabilities.extend(all_namespace_capabilities());
                    continue;
                }
                if command.basename() == "unshare" && flag == 'l' {
                    capabilities.insert(Capability::UserNamespace);
                    capabilities.insert(Capability::MountNamespace);
                    continue;
                }
                if let Some(capability) = namespace_short_flag(command.basename(), flag) {
                    capabilities.insert(capability);
                } else if !namespace_neutral_short_flag(command.basename(), flag) {
                    add_conservative_unknown_option_requirements(&mut capabilities);
                }
            }
        }
    }
    capabilities
        .into_iter()
        .map(CapabilityRequirement::kernel)
        .collect()
}

fn all_namespace_capabilities() -> [Capability; 8] {
    [
        Capability::ProcessNamespace,
        Capability::UserNamespace,
        Capability::MountNamespace,
        Capability::NetworkNamespace,
        Capability::UtsNamespace,
        Capability::IpcNamespace,
        Capability::CgroupNamespace,
        Capability::TimeNamespace,
    ]
}

fn add_conservative_unknown_option_requirements(
    capabilities: &mut std::collections::BTreeSet<Capability>,
) {
    capabilities.extend(all_namespace_capabilities());
    capabilities.insert(Capability::CgroupsV2);
}

fn namespace_long_flag(command: &str, value: &str) -> Option<Capability> {
    match value {
        "pid" => Some(Capability::ProcessNamespace),
        "user" => Some(Capability::UserNamespace),
        "mount" => Some(Capability::MountNamespace),
        "net" => Some(Capability::NetworkNamespace),
        "uts" => Some(Capability::UtsNamespace),
        "ipc" => Some(Capability::IpcNamespace),
        "cgroup" => Some(Capability::CgroupNamespace),
        "time" | "monotonic" | "boottime" => Some(Capability::TimeNamespace),
        "map-user" | "map-group" | "map-root-user" | "map-current-user" | "map-auto"
        | "map-users" | "map-groups" | "map-subids" | "setgroups" | "owner"
            if command == "unshare" =>
        {
            Some(Capability::UserNamespace)
        }
        "mount-proc" | "propagation" if command == "unshare" => Some(Capability::MountNamespace),
        "net-socket" if command == "nsenter" => Some(Capability::NetworkNamespace),
        "user-parent" if command == "nsenter" => Some(Capability::UserNamespace),
        "join-cgroup" if command == "nsenter" => Some(Capability::CgroupsV2),
        _ => None,
    }
}

fn namespace_neutral_long_flag(command: &str, value: &str) -> bool {
    match command {
        "unshare" => matches!(
            value,
            "fork"
                | "keep-caps"
                | "kill-child"
                | "root"
                | "wd"
                | "setuid"
                | "setgid"
                | "help"
                | "version"
        ),
        "nsenter" => matches!(
            value,
            "target"
                | "setuid"
                | "setgid"
                | "preserve-credentials"
                | "keep-caps"
                | "root"
                | "wd"
                | "wdns"
                | "no-fork"
                | "follow-context"
                | "help"
                | "version"
        ),
        _ => false,
    }
}

fn namespace_short_flag(command: &str, value: char) -> Option<Capability> {
    match value {
        'p' => Some(Capability::ProcessNamespace),
        'U' => Some(Capability::UserNamespace),
        'm' => Some(Capability::MountNamespace),
        'n' => Some(Capability::NetworkNamespace),
        'u' => Some(Capability::UtsNamespace),
        'i' => Some(Capability::IpcNamespace),
        'C' => Some(Capability::CgroupNamespace),
        'T' => Some(Capability::TimeNamespace),
        'r' | 'c' if command == "unshare" => Some(Capability::UserNamespace),
        'N' if command == "nsenter" => Some(Capability::NetworkNamespace),
        'c' if command == "nsenter" => Some(Capability::CgroupsV2),
        _ => None,
    }
}

fn namespace_neutral_short_flag(command: &str, value: char) -> bool {
    match command {
        "unshare" => matches!(value, 'f' | 'R' | 'w' | 'S' | 'G' | 'h' | 'V'),
        "nsenter" => matches!(
            value,
            't' | 'S' | 'G' | 'r' | 'w' | 'W' | 'F' | 'Z' | 'h' | 'V'
        ),
        _ => false,
    }
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
    use crate::{BackendCandidate, VerifiedNativeProfile, portable_offload_profile};

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

    fn portable_planner(platform: Platform, privilege: PrivilegeMode) -> Planner {
        let profile = portable_offload_profile(platform, privilege);
        let candidate = BackendCandidate::portable_offload(profile, 0).unwrap();
        Planner::new(candidate, OffloadRegistry::portable_defaults())
    }

    #[test]
    fn optional_docker_handler_is_not_promised_without_a_live_binding() {
        let planner = portable_planner(Platform::Ios, PrivilegeMode::AppSandbox);
        let runtime = Runtime::new(planner, EchoBridge);

        let docker = GuestCommand::new("docker", ["ps".to_owned()]);
        assert!(matches!(
            runtime.execute(&docker),
            Err(RuntimeError::MissingCapability(_))
        ));

        let dockerd = GuestCommand::new("dockerd", Vec::<String>::new());
        assert!(matches!(
            runtime.execute(&dockerd),
            Err(RuntimeError::MissingCapability(_))
        ));
    }

    #[test]
    fn unshare_requires_real_namespace_semantics() {
        let planner = portable_planner(Platform::Android, PrivilegeMode::AppSandbox);
        let runtime = Runtime::new(planner, EchoBridge);
        let command = GuestCommand::new("unshare", ["--net".to_owned(), "--pid".to_owned()]);

        assert!(matches!(
            runtime.execute(&command),
            Err(RuntimeError::MissingCapability(_))
        ));
    }

    #[test]
    fn namespace_flags_preserve_cgroup_time_and_all_requirements() {
        let command = GuestCommand::new(
            "unshare",
            [
                "--cgroup".to_owned(),
                "-T".to_owned(),
                "--map-auto".to_owned(),
            ],
        );
        let requirements = namespace_requirements(&command);
        assert!(requirements.contains(&CapabilityRequirement::kernel(Capability::CgroupNamespace)));
        assert!(requirements.contains(&CapabilityRequirement::kernel(Capability::TimeNamespace)));
        assert!(requirements.contains(&CapabilityRequirement::kernel(Capability::UserNamespace)));

        let all = namespace_requirements(&GuestCommand::new("nsenter", ["-a".to_owned()]));
        assert_eq!(all.len(), all_namespace_capabilities().len());
        for capability in all_namespace_capabilities() {
            assert!(all.contains(&CapabilityRequirement::kernel(capability)));
        }

        let implied = namespace_requirements(&GuestCommand::new(
            "unshare",
            [
                "-rc".to_owned(),
                "--map-users=auto".to_owned(),
                "--map-groups=auto".to_owned(),
                "--map-subids".to_owned(),
                "--owner=0".to_owned(),
                "--mount-binfmt".to_owned(),
                "--load-interp=/bin/qemu".to_owned(),
                "--kill-child".to_owned(),
            ],
        ));
        assert!(implied.contains(&CapabilityRequirement::kernel(Capability::UserNamespace)));
        assert!(implied.contains(&CapabilityRequirement::kernel(Capability::MountNamespace)));
        assert!(!implied.contains(&CapabilityRequirement::kernel(Capability::ProcessNamespace)));

        for option in ["--mount-binfmt", "--load-interp=/bin/qemu", "-l"] {
            let requirements =
                namespace_requirements(&GuestCommand::new("unshare", [option.to_owned()]));
            assert!(
                requirements.contains(&CapabilityRequirement::kernel(Capability::UserNamespace))
            );
            assert!(
                requirements.contains(&CapabilityRequirement::kernel(Capability::MountNamespace))
            );
        }
        let abbreviated =
            namespace_requirements(&GuestCommand::new("unshare", ["--ne".to_owned()]));
        assert_eq!(abbreviated.len(), all_namespace_capabilities().len() + 1);
        assert!(abbreviated.contains(&CapabilityRequirement::kernel(Capability::CgroupsV2)));

        let nsenter = namespace_requirements(&GuestCommand::new(
            "nsenter",
            [
                "-Nc".to_owned(),
                "--net-socket=3".to_owned(),
                "--user-parent".to_owned(),
                "--join-cgroup".to_owned(),
            ],
        ));
        assert!(nsenter.contains(&CapabilityRequirement::kernel(Capability::NetworkNamespace)));
        assert!(nsenter.contains(&CapabilityRequirement::kernel(Capability::UserNamespace)));
        assert!(nsenter.contains(&CapabilityRequirement::kernel(Capability::CgroupsV2)));
        assert!(!nsenter.contains(&CapabilityRequirement::kernel(Capability::CgroupNamespace)));
    }

    #[test]
    fn common_commands_plan_as_portable_applets() {
        let planner = portable_planner(Platform::Ios, PrivilegeMode::AppSandbox);
        let command = GuestCommand::new("grep", ["needle".to_owned()]);

        assert_eq!(
            planner.plan(&command).unwrap(),
            CommandPlan::PortableApplet {
                name: "grep".to_owned()
            }
        );
    }

    #[test]
    fn unknown_elf_fails_closed_on_portable_backend() {
        let planner = portable_planner(Platform::Harmony, PrivilegeMode::AppSandbox);
        let command = GuestCommand::new("/usr/bin/python3", Vec::<String>::new());

        assert!(matches!(
            planner.plan(&command),
            Err(RuntimeError::UnknownExecutable(_))
        ));
    }

    #[test]
    fn unknown_elf_cannot_impersonate_an_applet_by_basename() {
        let planner = portable_planner(Platform::Ios, PrivilegeMode::AppSandbox);
        let command = GuestCommand::new("/untrusted/root/rm", ["-r".to_owned(), "/".to_owned()]);

        assert!(matches!(
            planner.plan(&command),
            Err(RuntimeError::UnknownExecutable(program))
                if program == "/untrusted/root/rm"
        ));
    }

    #[test]
    fn executable_path_cannot_impersonate_a_semantic_offload() {
        let planner = portable_planner(Platform::Ios, PrivilegeMode::AppSandbox);
        let command = GuestCommand::new("/untrusted/docker", ["ps".to_owned()]);

        assert!(matches!(
            planner.plan(&command),
            Err(RuntimeError::UnknownExecutable(program))
                if program == "/untrusted/docker"
        ));
    }

    #[test]
    fn native_candidate_routes_only_native_linux_elf_to_linux_exec() {
        let profile = CapabilityProfile::new(Platform::Linux, PrivilegeMode::Root, "native-linux")
            .with(Capability::LinuxElf, SupportLevel::Native)
            .with(Capability::FullVirtualMachine, SupportLevel::Unavailable);
        let verified = VerifiedNativeProfile::for_test(profile);
        let candidate = BackendCandidate::native_linux(&verified, 0).unwrap();
        let planner = Planner::new(candidate, OffloadRegistry::portable_defaults());
        let command = GuestCommand::new("/usr/bin/python3", Vec::<String>::new());

        assert!(matches!(
            planner.plan(&command).unwrap(),
            CommandPlan::HostCall { call } if call.operation == "linux.exec"
        ));
    }

    #[test]
    fn emulated_or_bridged_linux_elf_never_upgrades_portable_to_linux_exec() {
        for level in [SupportLevel::Emulated, SupportLevel::Bridged] {
            let profile = portable_offload_profile(Platform::Android, PrivilegeMode::AppSandbox)
                .with(Capability::LinuxElf, level);
            let candidate = BackendCandidate::portable_offload(profile, 0).unwrap();
            let planner = Planner::new(candidate, OffloadRegistry::portable_defaults());
            let command = GuestCommand::new("/usr/bin/python3", Vec::<String>::new());

            assert!(matches!(
                planner.plan(&command),
                Err(RuntimeError::UnknownExecutable(_))
            ));
        }
    }

    #[test]
    fn emulated_or_bridged_vm_support_cannot_create_a_planner_candidate() {
        for level in [SupportLevel::Emulated, SupportLevel::Bridged] {
            let profile = portable_offload_profile(Platform::Android, PrivilegeMode::AppSandbox)
                .with(Capability::FullVirtualMachine, level);

            assert!(BackendCandidate::portable_offload(profile, 0).is_err());
        }
    }
}
