use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rish_guest_protocol::{
    ContainerState, ErrorCode, OciDeleteRequest, OciPrepareRequest, OciRunRequest, OciStopRequest,
    Operation, ResponsePayload,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::process::{ProcessRuntimeRunner, RuntimeCommandRunner, RuntimeOutput};
use super::{OciRuntimeBackend, OciRuntimeConfig};

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
type RecordedCalls = Arc<Mutex<Vec<(Vec<OsString>, Duration)>>>;

#[derive(Clone, Debug)]
struct MockHandle {
    calls: RecordedCalls,
}

#[derive(Debug)]
struct MockRunner {
    outputs: VecDeque<RuntimeOutput>,
    handle: MockHandle,
}

impl MockRunner {
    fn scripted(outputs: Vec<RuntimeOutput>) -> (Self, MockHandle) {
        let handle = MockHandle {
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        (
            Self {
                outputs: outputs.into(),
                handle: handle.clone(),
            },
            handle,
        )
    }
}

impl RuntimeCommandRunner for MockRunner {
    fn run(
        &mut self,
        args: &[OsString],
        timeout: Duration,
    ) -> Result<RuntimeOutput, rish_guest_protocol::RemoteError> {
        self.handle
            .calls
            .lock()
            .unwrap()
            .push((args.to_vec(), timeout));
        Ok(self
            .outputs
            .pop_front()
            .expect("mock OCI runtime script was exhausted"))
    }
}

struct Harness {
    _temporary: TempDir,
    runtime: PathBuf,
    bundle_root: PathBuf,
    runtime_root: PathBuf,
    bundle: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let runtime = temporary.path().join("youki");
        fs::write(&runtime, b"test runtime placeholder").unwrap();
        make_executable(&runtime);
        let bundle_root = temporary.path().join("bundles");
        let runtime_root = temporary.path().join("runtime-state");
        let bundle = bundle_root.join("demo");
        fs::create_dir_all(bundle.join("rootfs")).unwrap();
        fs::create_dir_all(&runtime_root).unwrap();
        fs::write(
            bundle.join(".rish-verified-rootfs.json"),
            serde_json::to_vec(&json!({
                "protocol": "dev.rish.verified-rootfs",
                "version": 1,
                "image_digest": DIGEST
            }))
            .unwrap(),
        )
        .unwrap();
        let runtime = runtime.canonicalize().unwrap();
        let bundle_root = bundle_root.canonicalize().unwrap();
        let runtime_root = runtime_root.canonicalize().unwrap();
        let bundle = bundle.canonicalize().unwrap();
        Self {
            _temporary: temporary,
            runtime,
            bundle_root,
            runtime_root,
            bundle,
        }
    }

    fn config(&self) -> OciRuntimeConfig {
        OciRuntimeConfig::new(&self.runtime, &self.bundle_root)
            .with_runtime_root(&self.runtime_root)
            .with_timeouts(
                Duration::from_secs(1),
                Duration::from_millis(50),
                Duration::from_millis(50),
            )
    }

    fn prepare(&self) -> OciPrepareRequest {
        OciPrepareRequest {
            container_id: "demo".to_owned(),
            image_reference: format!("docker.io/library/alpine@{DIGEST}"),
            expected_digest: Some(DIGEST.to_owned()),
            bundle_path: self.bundle.to_string_lossy().into_owned(),
            oci_spec: spec("rootfs", false),
            replace: false,
        }
    }
}

#[test]
fn full_lifecycle_invokes_real_oci_cli_shape_without_a_shell() {
    let harness = Harness::new();
    let outputs = vec![
        missing(),
        success(b""),
        state("demo", "created", Some(410)),
        state("demo", "created", Some(410)),
        success(b""),
        state("demo", "running", Some(410)),
        state("demo", "running", Some(410)),
        success(b""),
        state("demo", "stopped", None),
        state("demo", "stopped", None),
        success(b""),
        missing(),
    ];
    let (runner, handle) = MockRunner::scripted(outputs);
    let mut backend = OciRuntimeBackend::with_runner(harness.config(), Box::new(runner)).unwrap();

    let prepared = backend
        .dispatch(&Operation::OciPrepare(harness.prepare()))
        .unwrap();
    assert!(matches!(
        prepared.response,
        ResponsePayload::OciPrepared {
            ref container_id,
            ref image_digest
        } if container_id == "demo" && image_digest == DIGEST
    ));
    let written: Value =
        serde_json::from_slice(&fs::read(harness.bundle.join("config.json")).unwrap()).unwrap();
    assert_eq!(written, spec("rootfs", false));

    let running = backend
        .dispatch(&Operation::OciRun(OciRunRequest {
            container_id: "demo".to_owned(),
            attach: false,
        }))
        .unwrap();
    assert_state(&running.response, ContainerState::Running, Some(410));

    let stopped = backend
        .dispatch(&Operation::OciStop(OciStopRequest {
            container_id: "demo".to_owned(),
            signal: Some("term".to_owned()),
            timeout_ms: Some(50),
        }))
        .unwrap();
    assert_state(&stopped.response, ContainerState::Stopped, None);

    let deleted = backend
        .dispatch(&Operation::OciDelete(OciDeleteRequest {
            container_id: "demo".to_owned(),
            force: false,
        }))
        .unwrap();
    assert_state(&deleted.response, ContainerState::Deleted, None);

    let calls = handle.calls.lock().unwrap();
    let words = calls
        .iter()
        .map(|(args, _)| {
            args.iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let root = harness.runtime_root.to_string_lossy().into_owned();
    let bundle = harness.bundle.to_string_lossy().into_owned();
    assert_eq!(
        words,
        vec![
            vec!["--root", &root, "state", "demo"],
            vec!["--root", &root, "create", "--bundle", &bundle, "demo"],
            vec!["--root", &root, "state", "demo"],
            vec!["--root", &root, "state", "demo"],
            vec!["--root", &root, "start", "demo"],
            vec!["--root", &root, "state", "demo"],
            vec!["--root", &root, "state", "demo"],
            vec!["--root", &root, "kill", "demo", "SIGTERM"],
            vec!["--root", &root, "state", "demo"],
            vec!["--root", &root, "state", "demo"],
            vec!["--root", &root, "delete", "demo"],
            vec!["--root", &root, "state", "demo"],
        ]
    );
}

#[test]
fn create_failure_attempts_forced_runtime_cleanup() {
    let harness = Harness::new();
    let (runner, handle) =
        MockRunner::scripted(vec![missing(), failure("invalid config"), success(b"")]);
    let mut backend = OciRuntimeBackend::with_runner(harness.config(), Box::new(runner)).unwrap();

    let error = backend.prepare(&harness.prepare()).unwrap_err();

    assert_eq!(error.code, ErrorCode::Oci);
    assert!(error.message.contains("create failed"));
    let calls = handle.calls.lock().unwrap();
    let cleanup = calls[2]
        .0
        .iter()
        .map(|argument| argument.to_string_lossy())
        .collect::<Vec<_>>();
    assert_eq!(cleanup[cleanup.len() - 3..], ["delete", "--force", "demo"]);
}

#[test]
fn unsafe_ids_paths_digests_and_rootfs_are_rejected_before_runtime_dispatch() {
    let harness = Harness::new();
    let (runner, handle) = MockRunner::scripted(Vec::new());
    let mut backend = OciRuntimeBackend::with_runner(harness.config(), Box::new(runner)).unwrap();

    let mut request = harness.prepare();
    request.container_id = "../escape".to_owned();
    assert_eq!(
        backend.prepare(&request).unwrap_err().code,
        ErrorCode::InvalidRequest
    );

    let mut request = harness.prepare();
    request.bundle_path = harness.bundle_root.join("other").display().to_string();
    assert_eq!(
        backend.prepare(&request).unwrap_err().code,
        ErrorCode::PermissionDenied
    );

    let mut request = harness.prepare();
    request.expected_digest =
        Some("sha256:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB".into());
    assert_eq!(
        backend.prepare(&request).unwrap_err().code,
        ErrorCode::InvalidRequest
    );

    let mut request = harness.prepare();
    request.oci_spec = spec("../outside", false);
    assert_eq!(
        backend.prepare(&request).unwrap_err().code,
        ErrorCode::InvalidRequest
    );
    assert!(handle.calls.lock().unwrap().is_empty());
}

#[test]
fn terminal_specs_and_attach_fail_closed_until_console_streaming_exists() {
    let harness = Harness::new();
    let (runner, handle) = MockRunner::scripted(Vec::new());
    let mut backend = OciRuntimeBackend::with_runner(harness.config(), Box::new(runner)).unwrap();

    let mut request = harness.prepare();
    request.oci_spec = spec("rootfs", true);
    assert_eq!(
        backend.prepare(&request).unwrap_err().code,
        ErrorCode::UnsupportedOperation
    );
    assert_eq!(
        backend
            .run(&OciRunRequest {
                container_id: "demo".to_owned(),
                attach: true,
            })
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedOperation
    );
    assert!(handle.calls.lock().unwrap().is_empty());
}

#[test]
fn prepare_requires_a_matching_guest_verified_rootfs_record() {
    let harness = Harness::new();
    fs::remove_file(harness.bundle.join(".rish-verified-rootfs.json")).unwrap();
    let (runner, handle) = MockRunner::scripted(Vec::new());
    let mut backend = OciRuntimeBackend::with_runner(harness.config(), Box::new(runner)).unwrap();

    let error = backend.prepare(&harness.prepare()).unwrap_err();

    assert_eq!(error.code, ErrorCode::Oci);
    assert!(error.message.contains("verified import record"));
    assert!(handle.calls.lock().unwrap().is_empty());
}

#[test]
fn force_delete_is_idempotent_for_an_absent_container() {
    let harness = Harness::new();
    let (runner, _) = MockRunner::scripted(vec![missing()]);
    let mut backend = OciRuntimeBackend::with_runner(harness.config(), Box::new(runner)).unwrap();
    let reply = backend
        .delete(&OciDeleteRequest {
            container_id: "demo".to_owned(),
            force: true,
        })
        .unwrap();
    assert_state(&reply.response, ContainerState::Deleted, None);
}

#[cfg(unix)]
#[test]
fn process_runner_kills_a_timed_out_runtime_command_group() {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempfile::tempdir().unwrap();
    let script = temporary.path().join("slow-runtime");
    fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let output = temporary.path().join("output");
    fs::create_dir(&output).unwrap();
    let mut runner = ProcessRuntimeRunner::new(script, output.clone(), 1024);

    let started = Instant::now();
    let error = runner
        .run(
            &[OsString::from("state"), OsString::from("demo")],
            Duration::from_millis(30),
        )
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::DeadlineExceeded);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(fs::read_dir(output).unwrap().count(), 0);
}

fn spec(root: &str, terminal: bool) -> Value {
    json!({
        "ociVersion": "1.1.0",
        "process": {
            "terminal": terminal,
            "args": ["/bin/sh"],
            "cwd": "/",
            "env": ["PATH=/usr/bin:/bin"]
        },
        "root": {
            "path": root,
            "readonly": false
        }
    })
}

fn success(stdout: &[u8]) -> RuntimeOutput {
    RuntimeOutput {
        exit_code: Some(0),
        stdout: stdout.to_vec(),
        stderr: Vec::new(),
    }
}

fn failure(stderr: &str) -> RuntimeOutput {
    RuntimeOutput {
        exit_code: Some(1),
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

fn missing() -> RuntimeOutput {
    failure("container does not exist")
}

fn state(id: &str, status: &str, pid: Option<u32>) -> RuntimeOutput {
    success(
        serde_json::to_string(&json!({
            "ociVersion": "1.1.0",
            "id": id,
            "status": status,
            "pid": pid
        }))
        .unwrap()
        .as_bytes(),
    )
}

fn assert_state(payload: &ResponsePayload, expected: ContainerState, pid: Option<u32>) {
    assert!(matches!(
        payload,
        ResponsePayload::OciState {
            container_id,
            state,
            pid: actual_pid
        } if container_id == "demo" && *state == expected && *actual_pid == pid
    ));
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}
