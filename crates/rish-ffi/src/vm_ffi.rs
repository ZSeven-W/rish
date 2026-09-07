//! Full-VM docker surface for the mobile bridge.
//!
//! `rish_vm_run_docker_json` boots the in-repository pure-Rust x86_64
//! interpreter with an app-supplied kernel and initramfs, waits for the guest
//! init to report on the console, brings up the framed control channel, and
//! runs one command inside the guest — the same path the desktop
//! `pure_rust_guest` example drives. Everything crosses the ABI as UTF-8 JSON;
//! the guest kernel and initramfs are named by filesystem path so the large
//! binaries never travel through the JSON envelope.

use std::sync::Arc;

use rish_core::HostReply;
use rish_guest_protocol::{DEFAULT_MAX_FRAME_SIZE, Envelope, Hello, Message, PeerInfo, RequestId};
use rish_softvm_x86_64::{
    EngineLimits, MachineState, PureRustProvider, SerialGuestTransport, X86_64SoftwareEngine,
};
use rish_vm::{GuestChannel, VmAcceleration, VmConfig, VmDevice, VmNetworkMode};
use serde::{Deserialize, Serialize};

const BOOT_OK_MARKER: &[u8] = b"RISH_X86_64_BOOT_OK";
const BOOT_FAILED_MARKER: &[u8] = b"RISH_X86_64_BOOT_FAILED";
const BOOT_QUANTUM_UNITS: u64 = 500_000;

/// One docker-run request. Kernel and initramfs are paths (the app stages them
/// as bundle resources); the command is the argv executed in the guest.
#[derive(Deserialize)]
struct VmRunRequest {
    kernel_path: String,
    initrd_path: String,
    #[serde(default)]
    root_disk_path: Option<String>,
    #[serde(default = "default_memory_mib")]
    memory_mib: u32,
    command: Vec<String>,
    #[serde(default)]
    command_line: Option<String>,
    /// Optional network mode: "disabled" (default) or "user-nat".
    #[serde(default)]
    network: Option<String>,
    #[serde(default = "default_boot_budget")]
    boot_budget_units: u64,
    #[serde(default = "default_handshake_budget")]
    handshake_budget_units: u64,
}

fn default_memory_mib() -> u32 {
    1024
}
fn default_boot_budget() -> u64 {
    25_000_000_000
}
fn default_handshake_budget() -> u64 {
    15_000_000_000
}

#[derive(Serialize)]
struct VmRunResponse {
    protocol_version: u32,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    boot_units: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl VmRunResponse {
    fn failure(error: impl Into<String>) -> Self {
        Self {
            protocol_version: 1,
            ok: false,
            exit_code: None,
            stdout: None,
            stderr: None,
            boot_units: None,
            error: Some(error.into()),
        }
    }
}

/// JSON entry point wrapped by the C ABI export in `lib.rs`.
pub fn vm_run_docker_json(request: &str) -> String {
    let response = match serde_json::from_str::<VmRunRequest>(request) {
        Ok(request) => run(request).unwrap_or_else(VmRunResponse::failure),
        Err(error) => VmRunResponse::failure(format!("invalid request JSON: {error}")),
    };
    serde_json::to_string(&response).unwrap_or_else(|_| {
        r#"{"protocol_version":1,"ok":false,"error":"serialize failed"}"#.into()
    })
}

/// A booted guest whose framed control channel stays open, so many commands
/// run without rebooting — the interactive-shell surface for the mobile bridge.
pub struct VmSession {
    channel: SerialGuestTransport,
    boot_units: u64,
    _scratch_disk: Option<tempfile::NamedTempFile>,
}

/// Boots the guest and brings up the bootstrapped control channel, returning it
/// with any throwaway root disk and the boot instruction count.
fn boot_channel(
    request: &VmRunRequest,
) -> Result<(SerialGuestTransport, Option<tempfile::NamedTempFile>, u64), String> {
    let limits = EngineLimits {
        max_units_per_request: request.handshake_budget_units,
        ..EngineLimits::default()
    };
    let provider = PureRustProvider::new();
    let engine = X86_64SoftwareEngine::new(Arc::new(provider), limits.clone())
        .map_err(|error| error.to_string())?;

    // An initramfs-only boot never reads the root disk, but the VM config
    // requires a path. Materialize a throwaway file when the caller (a mobile
    // app with no scratch disk) omits it; it must outlive the guest.
    let scratch_disk: Option<tempfile::NamedTempFile>;
    let root_disk_path = match &request.root_disk_path {
        Some(path) if !path.is_empty() => {
            scratch_disk = None;
            path.clone()
        }
        _ => {
            let file = tempfile::NamedTempFile::new()
                .map_err(|error| format!("cannot create a scratch root disk: {error}"))?;
            file.as_file()
                .set_len(64 * 1024 * 1024)
                .map_err(|error| format!("cannot size the scratch root disk: {error}"))?;
            let path = file.path().to_string_lossy().into_owned();
            scratch_disk = Some(file);
            path
        }
    };

    let mut devices = vec![VmDevice::Console];
    match request.network.as_deref() {
        None | Some("disabled") => {}
        Some("user-nat") => devices.push(VmDevice::Network {
            mode: VmNetworkMode::UserNat,
        }),
        Some(other) => return Err(format!("unknown network mode \"{other}\"")),
    }
    let config = VmConfig {
        architecture: "x86_64".to_owned(),
        vcpus: 1,
        memory_mib: request.memory_mib,
        kernel_path: request.kernel_path.clone(),
        initrd_path: Some(request.initrd_path.clone()),
        root_disk_path,
        acceleration: VmAcceleration::Interpreter,
        devices,
        command_line: request.command_line.clone().unwrap_or_default(),
    };

    // Stage 1: run until the guest init prints the boot marker.
    let machine = engine.launch(&config).map_err(|error| error.to_string())?;
    let mut executed = 0_u64;
    let mut console = Vec::new();
    loop {
        let report = machine
            .run_units(BOOT_QUANTUM_UNITS)
            .map_err(|error| error.to_string())?;
        executed += report.executed_units;
        console.extend_from_slice(&report.console);
        if contains(&console, BOOT_FAILED_MARKER) {
            return Err("guest init reported RISH_X86_64_BOOT_FAILED".to_owned());
        }
        if contains(&console, BOOT_OK_MARKER) {
            break;
        }
        match report.snapshot.state {
            MachineState::Running => {}
            other => return Err(format!("guest machine {other:?} during boot")),
        }
        if executed > request.boot_budget_units {
            return Err(format!(
                "boot budget exhausted after {executed} units without the boot marker"
            ));
        }
    }
    let boot_units = executed;

    // Stage 2: framed control channel over the second serial; bootstrap it.
    let channel = SerialGuestTransport::new(machine, limits).map_err(|error| error.to_string())?;
    let hello = Envelope::new(Message::Hello(Hello::host(
        RequestId::new("vm-bootstrap-1").map_err(|error| error.to_string())?,
        PeerInfo {
            name: "rish-host".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
        },
        Vec::new(),
        DEFAULT_MAX_FRAME_SIZE as u32,
    )));
    channel
        .bootstrap(&hello)
        .map_err(|error| error.to_string())?;
    Ok((channel, scratch_disk, boot_units))
}

/// Runs one command over an already-bootstrapped channel.
fn exec_on(channel: &SerialGuestTransport, argv: &[String]) -> Result<HostReply, String> {
    exec_on_observed(channel, argv, &mut |_, _| {})
}

fn exec_on_observed(
    channel: &SerialGuestTransport,
    argv: &[String],
    observer: &mut dyn FnMut(rish_guest_protocol::StreamChannel, &[u8]),
) -> Result<HostReply, String> {
    let command = rish_core::GuestCommand {
        program: argv[0].clone(),
        args: argv[1..].to_vec(),
        env: Default::default(),
        cwd: "/".to_owned(),
        stdin: Vec::new(),
    };
    channel
        .execute_observed(&command, observer)
        .map_err(|error| error.to_string())
}

fn run(request: VmRunRequest) -> Result<VmRunResponse, String> {
    if request.command.is_empty() {
        return Err("command must have at least one element".to_owned());
    }
    let (channel, _scratch, boot_units) = boot_channel(&request)?;
    let reply = exec_on(&channel, &request.command)?;
    Ok(VmRunResponse {
        protocol_version: 1,
        ok: reply.exit_code == 0,
        exit_code: Some(reply.exit_code),
        stdout: Some(String::from_utf8_lossy(&reply.stdout).into_owned()),
        stderr: Some(String::from_utf8_lossy(&reply.stderr).into_owned()),
        boot_units: Some(boot_units),
        error: None,
    })
}

/// Boots a session for the interactive surface. The command field is ignored.
pub fn vm_boot_session(request_json: &str) -> Result<Box<VmSession>, String> {
    let request: VmRunRequest = serde_json::from_str(request_json)
        .map_err(|error| format!("invalid request JSON: {error}"))?;
    let (channel, scratch, boot_units) = boot_channel(&request)?;
    Ok(Box::new(VmSession {
        channel,
        boot_units,
        _scratch_disk: scratch,
    }))
}

/// Runs one command in a live session and returns the JSON result. The request
/// is `{"command":["argv0","argv1",...]}`.
pub fn vm_session_exec_json(session: &VmSession, request_json: &str) -> String {
    vm_session_exec_observed_json(session, request_json, &mut |_, _| {})
}

pub fn vm_session_exec_observed_json(
    session: &VmSession,
    request_json: &str,
    observer: &mut dyn FnMut(rish_guest_protocol::StreamChannel, &[u8]),
) -> String {
    #[derive(Deserialize)]
    struct ExecRequest {
        command: Vec<String>,
    }
    let response = match serde_json::from_str::<ExecRequest>(request_json) {
        Ok(request) if request.command.is_empty() => {
            VmRunResponse::failure("command must have at least one element")
        }
        Ok(request) => match exec_on_observed(&session.channel, &request.command, observer) {
            Ok(reply) => VmRunResponse {
                protocol_version: 1,
                ok: reply.exit_code == 0,
                exit_code: Some(reply.exit_code),
                stdout: Some(String::from_utf8_lossy(&reply.stdout).into_owned()),
                stderr: Some(String::from_utf8_lossy(&reply.stderr).into_owned()),
                boot_units: Some(session.boot_units),
                error: None,
            },
            Err(error) => VmRunResponse::failure(error),
        },
        Err(error) => VmRunResponse::failure(format!("invalid exec JSON: {error}")),
    };
    serde_json::to_string(&response).unwrap_or_else(|_| {
        r#"{"protocol_version":1,"ok":false,"error":"serialize failed"}"#.into()
    })
}

fn contains(buffer: &[u8], marker: &[u8]) -> bool {
    buffer.windows(marker.len()).any(|window| window == marker)
}

#[cfg(test)]
mod tests {
    use super::vm_run_docker_json;
    use serde_json::Value;

    #[test]
    fn malformed_request_returns_a_json_error() {
        let response: Value = serde_json::from_str(&vm_run_docker_json("not json")).unwrap();
        assert_eq!(response["ok"], Value::Bool(false));
        assert_eq!(response["protocol_version"], Value::from(1));
        assert!(
            response["error"]
                .as_str()
                .unwrap()
                .contains("invalid request JSON")
        );
    }

    #[test]
    fn empty_command_is_rejected_before_boot() {
        let request = r#"{"kernel_path":"/dev/null","initrd_path":"/dev/null","command":[]}"#;
        let response: Value = serde_json::from_str(&vm_run_docker_json(request)).unwrap();
        assert_eq!(response["ok"], Value::Bool(false));
        assert!(
            response["error"]
                .as_str()
                .unwrap()
                .contains("command must have at least one element")
        );
    }
}
