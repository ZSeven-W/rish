//! End-to-end serial control transport test.
//!
//! A scripted provider plays the emulated x86_64 machine while the real
//! rish_guest_agent library plays the guest. Both sides speak the production
//! framed protocol through the bounded control channel, and the host side
//! runs the full VmCandidate boot chain: probe, Hello handshake, live kernel
//! evidence (uname + zcat /proc/config.gz), capability mapping, and command
//! execution including attached stdin.

use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    sync::{Arc, Mutex},
};

use rish_core::{GuestCommand, Platform};
use rish_guest_agent::{GuestAgent, HandlerReply, OperationHandler};
use rish_guest_protocol::{
    Capability as GuestCapability, CapabilityStatus, DEFAULT_MAX_FRAME_SIZE, ErrorCode, EventKind,
    FrameDecoder, FrameEncoder, GuestCapabilities, GuestLimits, Operation, PeerInfo, RemoteError,
    RequestId, ResponsePayload, StreamChannel, capability_name,
};
use rish_softvm_x86_64::{
    EngineLimits, MachineProvider, MachineState, ProviderBuildInfo, ProviderIo, ProviderKind,
    ProviderMachine, ProviderRequest, ProviderRun, ProviderSnapshot, SoftVmError,
    X86_64SoftwareEngine,
};
use rish_vm::{GuestKernelContract, VmAcceleration, VmCandidate, VmConfig, VmDevice};
use tempfile::tempdir;

struct ScriptedExec {
    program: String,
    exit_code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

struct FakeHandler {
    script: Arc<Mutex<VecDeque<ScriptedExec>>>,
    next_execution: u64,
}

impl FakeHandler {
    fn new(script: Arc<Mutex<VecDeque<ScriptedExec>>>) -> Self {
        Self {
            script,
            next_execution: 1,
        }
    }
}

impl OperationHandler for FakeHandler {
    fn handle(
        &mut self,
        _request_id: &RequestId,
        operation: &Operation,
    ) -> Result<HandlerReply, RemoteError> {
        match operation {
            Operation::Stream(request) => Ok(HandlerReply {
                response: ResponsePayload::StreamAccepted {
                    execution_id: request.execution_id.clone(),
                },
                events: Vec::new(),
            }),
            Operation::Exec(request) => {
                let entry = self
                    .script
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .pop_front()
                    .ok_or_else(|| {
                        RemoteError::new(ErrorCode::NotFound, "no scripted execution remains")
                    })?;
                if entry.program != request.argv[0] {
                    return Err(RemoteError::new(
                        ErrorCode::NotFound,
                        format!(
                            "scripted program {} does not match requested {}",
                            entry.program, request.argv[0]
                        ),
                    ));
                }
                let execution_id = format!("scripted-{}", self.next_execution);
                self.next_execution = self.next_execution.saturating_add(1);
                let mut events = Vec::new();
                events.push(EventKind::ExecutionStarted {
                    execution_id: execution_id.clone(),
                    pid: 1234,
                });
                if !entry.stdout.is_empty() {
                    events.push(EventKind::Stream {
                        execution_id: execution_id.clone(),
                        channel: StreamChannel::Stdout,
                        stream_sequence: 1,
                        data_base64: encode(&entry.stdout),
                        eof: true,
                    });
                }
                if !entry.stderr.is_empty() {
                    events.push(EventKind::Stream {
                        execution_id: execution_id.clone(),
                        channel: StreamChannel::Stderr,
                        stream_sequence: 1,
                        data_base64: encode(&entry.stderr),
                        eof: true,
                    });
                }
                events.push(EventKind::ProcessExited {
                    execution_id: execution_id.clone(),
                    exit_code: Some(entry.exit_code),
                    signal: None,
                });
                Ok(HandlerReply {
                    response: ResponsePayload::ExecStarted {
                        execution_id,
                        pid: 1234,
                    },
                    events,
                })
            }
            _ => Err(RemoteError::new(
                ErrorCode::UnsupportedOperation,
                "scripted guest only supports Exec and Stream",
            )),
        }
    }
}

fn encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// One emulated x86_64 machine whose "guest" is the real agent library.
struct FakeGuestMachine {
    io: ProviderIo,
    agent: GuestAgent<FakeHandler>,
    decoder: FrameDecoder,
    encoder: FrameEncoder,
    units: u64,
}

impl ProviderMachine for FakeGuestMachine {
    fn snapshot(&mut self) -> Result<ProviderSnapshot, SoftVmError> {
        Ok(ProviderSnapshot {
            state: MachineState::Running,
            pc: Some(0x100000 + self.units),
            total_units: self.units,
        })
    }

    fn run_quantum(&mut self, max_units: u64) -> Result<ProviderRun, SoftVmError> {
        self.units = self.units.saturating_add(max_units);
        let mut buffer = [0_u8; 65536];
        loop {
            let read = self.io.control.read_input(&mut buffer);
            if read == 0 {
                break;
            }
            self.decoder
                .push(&buffer[..read])
                .map_err(|error| SoftVmError::ControlChannel(error.to_string()))?;
        }
        let mut envelopes = Vec::new();
        while let Some(frame) = self
            .decoder
            .next_frame()
            .map_err(|error| SoftVmError::ControlChannel(error.to_string()))?
        {
            envelopes.extend(
                self.agent
                    .handle(frame)
                    .map_err(|error| SoftVmError::ControlChannel(error.to_string()))?,
            );
        }
        envelopes.extend(self.agent.poll());
        for envelope in envelopes {
            let bytes = self
                .encoder
                .encode(&envelope)
                .map_err(|error| SoftVmError::ControlChannel(error.to_string()))?;
            if self.io.control.write_output(&bytes) != bytes.len() {
                return Err(SoftVmError::ControlChannel(
                    "guest control write was truncated".to_owned(),
                ));
            }
        }
        if let Some(version) = self.agent.negotiated_version() {
            self.decoder.set_expected_version(Some(version));
        }
        if let Some(max_frame_size) = self.agent.negotiated_max_frame_size() {
            let max_frame_size = max_frame_size as usize;
            self.decoder
                .set_max_frame_size(max_frame_size)
                .map_err(|error| SoftVmError::ControlChannel(error.to_string()))?;
            self.encoder
                .set_max_frame_size(max_frame_size)
                .map_err(|error| SoftVmError::ControlChannel(error.to_string()))?;
        }
        Ok(ProviderRun {
            executed_units: max_units,
            snapshot: ProviderSnapshot {
                state: MachineState::Running,
                pc: Some(0x100000 + self.units),
                total_units: self.units,
            },
        })
    }

    fn request_stop(&mut self) -> Result<(), SoftVmError> {
        Ok(())
    }
}

struct ScriptedProvider {
    build: ProviderBuildInfo,
    script: Arc<Mutex<VecDeque<ScriptedExec>>>,
}

impl ScriptedProvider {
    fn new(script: Vec<ScriptedExec>) -> Self {
        Self {
            build: ProviderBuildInfo {
                kind: ProviderKind::QemuTcti,
                qemu_version: "10.0.2".to_owned(),
                source_revision: "37ba092d59aff24900dfd0d5e01d4ed68441ba07".to_owned(),
                build_id: "scripted-serial-control-test".to_owned(),
                guest_architecture: "x86_64".to_owned(),
                host_architecture: "aarch64".to_owned(),
                target_list: "x86_64-softmmu".to_owned(),
                compiled_features: rish_softvm_x86_64::abi::REQUIRED_FEATURES
                    | rish_softvm_x86_64::abi::FEATURE_INITRD,
                min_memory_mib: 128,
                max_memory_mib: 2048,
                max_vcpus: 1,
            },
            script: Arc::new(Mutex::new(script.into())),
        }
    }
}

impl MachineProvider for ScriptedProvider {
    fn build_info(&self) -> &ProviderBuildInfo {
        &self.build
    }

    fn create(
        &self,
        _request: ProviderRequest,
        io: ProviderIo,
    ) -> Result<Box<dyn ProviderMachine>, SoftVmError> {
        let agent = GuestAgent::new(
            FakeHandler::new(Arc::clone(&self.script)),
            PeerInfo {
                name: "rish-guest-agent".to_owned(),
                version: "0.1.0".to_owned(),
                platform: "linux".to_owned(),
                architecture: "x86_64".to_owned(),
            },
            guest_capabilities(),
            GuestLimits {
                max_frame_size: 1024 * 1024,
                max_concurrent_exec: 4,
                max_port_forwards: 8,
                max_stream_chunk_size: 65536,
            },
            "test-session-1".to_owned(),
        );
        Ok(Box::new(FakeGuestMachine {
            io,
            agent,
            decoder: FrameDecoder::new(DEFAULT_MAX_FRAME_SIZE)
                .expect("default frame size is valid"),
            encoder: FrameEncoder::new(DEFAULT_MAX_FRAME_SIZE)
                .expect("default frame size is valid"),
            units: 0,
        }))
    }
}

fn guest_capabilities() -> GuestCapabilities {
    let names = [
        capability_name::EXEC,
        capability_name::OCI,
        capability_name::VIRTUAL_FILESYSTEM,
        capability_name::NAMESPACES,
        capability_name::NETWORK_NAMESPACES,
        capability_name::CGROUPS_V2,
        capability_name::PRIVILEGED_CONTAINERS,
        capability_name::MODULES,
        capability_name::DEVICES,
        capability_name::SYSTEMD,
        capability_name::NESTED_CONTAINERS,
        capability_name::PORT_FORWARDING,
        capability_name::RAW_SOCKETS,
        capability_name::TUN_TAP,
    ];
    GuestCapabilities {
        kernel_release: "6.18.35-0-virt".to_owned(),
        architecture: "x86_64".to_owned(),
        init_system: "none".to_owned(),
        cgroup_version: Some(2),
        container_runtimes: vec!["crun".to_owned()],
        features: names
            .into_iter()
            .map(|name| GuestCapability {
                name: name.to_owned(),
                version: 1,
                status: CapabilityStatus::Available,
                attributes: BTreeMap::new(),
                reason: None,
            })
            .collect(),
    }
}

fn config(kernel: &std::path::Path, root: &std::path::Path) -> VmConfig {
    VmConfig {
        architecture: "x86_64".to_owned(),
        vcpus: 1,
        memory_mib: 256,
        kernel_path: kernel.to_string_lossy().into_owned(),
        initrd_path: None,
        root_disk_path: root.to_string_lossy().into_owned(),
        acceleration: VmAcceleration::Interpreter,
        devices: vec![VmDevice::Console],
        command_line: String::new(),
    }
}

fn linux_bzimage_header() -> Vec<u8> {
    let mut image = vec![0; 0x238];
    image[0x1fe..0x200].copy_from_slice(&[0x55, 0xaa]);
    image[0x202..0x206].copy_from_slice(b"HdrS");
    image[0x236..0x238].copy_from_slice(&1_u16.to_le_bytes());
    image
}

#[test]
fn real_agent_handshake_evidence_and_exec_over_control_serial() {
    let directory = tempdir().unwrap();
    let kernel = directory.path().join("bzImage");
    let root = directory.path().join("root.img");
    fs::write(&kernel, linux_bzimage_header()).unwrap();
    fs::write(&root, vec![0x5a; 4096]).unwrap();

    let contract = GuestKernelContract::container_host();
    let mut config_text = String::new();
    for symbol in contract.required_kconfig() {
        config_text.push_str(symbol);
        config_text.push_str(
            "=y
",
        );
    }
    config_text.push_str(
        "# CONFIG_NOT_NEEDED is not set
",
    );
    config_text.push_str(
        "CONFIG_OPTIONAL_MODULE=m
",
    );

    let script = vec![
        ScriptedExec {
            program: "uname".to_owned(),
            exit_code: 0,
            stdout: b"6.18.35-0-virt
"
            .to_vec(),
            stderr: Vec::new(),
        },
        ScriptedExec {
            program: "zcat".to_owned(),
            exit_code: 0,
            stdout: config_text.into_bytes(),
            stderr: Vec::new(),
        },
        ScriptedExec {
            program: "echo".to_owned(),
            exit_code: 0,
            stdout: b"hello
"
            .to_vec(),
            stderr: Vec::new(),
        },
        ScriptedExec {
            program: "cat".to_owned(),
            exit_code: 0,
            stdout: b"stdin-was-attached
"
            .to_vec(),
            stderr: Vec::new(),
        },
    ];
    let provider = Arc::new(ScriptedProvider::new(script));
    let engine = X86_64SoftwareEngine::new(provider, EngineLimits::default())
        .expect("scripted provider must pass the production gate");

    let candidate = VmCandidate::new(Platform::Android, config(&kernel, &root), contract);
    let vm = candidate
        .boot(&engine)
        .expect("full boot evidence chain must succeed");

    assert_eq!(vm.session().id(), "test-session-1");
    assert_eq!(
        vm.kernel_evidence().source().kernel_release(),
        "6.18.35-0-virt"
    );

    let reply = vm
        .execute(&GuestCommand::new("echo", ["hello".to_owned()]))
        .expect("guest echo must succeed");
    assert_eq!(reply.exit_code, 0);
    assert_eq!(
        reply.stdout,
        b"hello
"
    );
    assert!(reply.stderr.is_empty());

    let with_stdin = vm
        .execute(&GuestCommand {
            program: "cat".to_owned(),
            args: Vec::new(),
            env: Default::default(),
            cwd: "/".to_owned(),
            stdin: b"abc".to_vec(),
        })
        .expect("guest cat with stdin must succeed");
    assert_eq!(with_stdin.exit_code, 0);
    assert_eq!(
        with_stdin.stdout,
        b"stdin-was-attached
"
    );
}
