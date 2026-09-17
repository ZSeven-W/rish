use super::*;
use rish_guest_protocol::{
    CURRENT_PROTOCOL_VERSION, DEFAULT_MAX_FRAME_SIZE, FrameEncoder, GuestCapabilities, GuestLimits,
    HandshakeOutcome, Hello, HelloAck, Message, PeerInfo, RequestId,
};
use rish_softvm_x86_64::{PureRustProvider, X86_64SoftwareEngine};
use rish_vm::{VmAcceleration, VmConfig, VmDevice};
use std::{cell::RefCell, sync::mpsc};

fn channel() -> (VmChannel, tempfile::TempDir, Arc<Cancellation>) {
    channel_with_output(&[])
}

fn channel_with_output(output: &[u8]) -> (VmChannel, tempfile::TempDir, Arc<Cancellation>) {
    let directory = tempfile::tempdir().unwrap();
    let kernel = directory.path().join("bzImage");
    let root = directory.path().join("root.raw");
    let mut image = crate::vm_cancel::tests::looping_kernel();
    if !output.is_empty() {
        let mut instructions = vec![0x66, 0xba, 0xf8, 0x02]; // mov dx, COM2
        for byte in output {
            instructions.extend_from_slice(&[0xb0, *byte, 0xee]);
        }
        instructions.extend_from_slice(&[0xe9, 0xfb, 0xff, 0xff, 0xff]);
        image.resize((0x800 + instructions.len()).max(image.len()), 0);
        image[0x800..0x800 + instructions.len()].copy_from_slice(&instructions);
    }
    std::fs::write(&kernel, image).unwrap();
    std::fs::write(&root, [0_u8; 4096]).unwrap();
    let limits = EngineLimits {
        max_units_per_request: 1_000_000_000,
        ..EngineLimits::default()
    };
    let engine =
        X86_64SoftwareEngine::new(Arc::new(PureRustProvider::new()), limits.clone()).unwrap();
    let machine = Arc::new(
        engine
            .launch(&VmConfig {
                architecture: "x86_64".into(),
                vcpus: 1,
                memory_mib: 128,
                kernel_path: kernel.to_string_lossy().into(),
                initrd_path: None,
                root_disk_path: root.to_string_lossy().into(),
                data_disk_path: None,
                acceleration: VmAcceleration::Interpreter,
                devices: vec![VmDevice::Console],
                command_line: String::new(),
            })
            .unwrap(),
    );
    let cancel = Arc::new(Cancellation::default());
    cancel.claim().unwrap();
    cancel.attach(&machine).unwrap();
    (
        VmChannel::new(machine, limits, cancel.clone()).unwrap(),
        directory,
        cancel,
    )
}

fn hello() -> Envelope {
    Envelope::new(Message::Hello(Hello::host(
        RequestId::new("test-hello").unwrap(),
        peer(),
        Vec::new(),
        DEFAULT_MAX_FRAME_SIZE as u32,
    )))
}

fn peer() -> PeerInfo {
    PeerInfo {
        name: "test".into(),
        version: "1".into(),
        platform: "linux".into(),
        architecture: "x86_64".into(),
    }
}

// Only negotiation is scripted. Subsequent commands advance the real spinning
// interpreter, which never sends a reply: cancellation must break that wait.
fn negotiate(channel: &VmChannel) {
    struct AckIo(RefCell<Vec<u8>>);
    impl SessionIo for AckIo {
        fn write(&self, bytes: &[u8]) -> usize {
            bytes.len()
        }
        fn advance(&self) -> Result<Vec<u8>, String> {
            Ok(self.0.take())
        }
        fn dropped_output(&self) -> u64 {
            0
        }
    }
    let ack = Envelope::new(Message::HelloAck(HelloAck {
        request_id: RequestId::new("test-hello").unwrap(),
        outcome: HandshakeOutcome::Accepted {
            selected_version: CURRENT_PROTOCOL_VERSION,
            session_id: "test-session".into(),
            peer: peer(),
            capabilities: Box::new(GuestCapabilities {
                kernel_release: "test".into(),
                architecture: "x86_64".into(),
                init_system: "test".into(),
                cgroup_version: None,
                container_runtimes: Vec::new(),
                features: Vec::new(),
            }),
            limits: GuestLimits {
                max_frame_size: DEFAULT_MAX_FRAME_SIZE as u32,
                max_concurrent_exec: 1,
                max_port_forwards: 0,
                max_stream_chunk_size: 65536,
            },
        },
    }));
    let bytes = FrameEncoder::new(DEFAULT_MAX_FRAME_SIZE)
        .unwrap()
        .encode(&ack)
        .unwrap();
    channel
        .client
        .lock()
        .unwrap()
        .bootstrap(&hello(), &AckIo(RefCell::new(bytes)))
        .unwrap();
}

fn request(timeout_ms: Option<u64>) -> ExecRequest {
    ExecRequest {
        argv: vec!["test".into()],
        env: Default::default(),
        cwd: None,
        user: None,
        tty: false,
        attach_stdin: false,
        attach_stdout: true,
        attach_stderr: true,
        timeout_ms,
    }
}

#[test]
fn cancellation_interrupts_blocked_handshake() {
    let (channel, _directory, cancel) = channel();
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || tx.send(channel.bootstrap(&hello())).unwrap());
    std::thread::sleep(Duration::from_millis(10));
    cancel.request();
    assert!(
        rx.recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap_err()
            .contains("E_VM_CANCELLED")
    );
    worker.join().unwrap();
}

#[test]
fn cancellation_interrupts_blocked_exec_and_drops_machine() {
    let (channel, _directory, cancel) = channel();
    negotiate(&channel);
    let weak = Arc::downgrade(&channel.machine);
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        tx.send(channel.execute_observed(request(None), None, &mut |_, _| {}))
            .unwrap();
    });
    std::thread::sleep(Duration::from_millis(10));
    cancel.request();
    assert!(
        rx.recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap_err()
            .contains("E_VM_CANCELLED")
    );
    worker.join().unwrap();
    assert!(weak.upgrade().is_none());
}

#[test]
fn host_deadline_stops_nonresponsive_guest_and_poisoned_session_is_not_reused() {
    let (channel, _directory, _cancel) = channel();
    negotiate(&channel);
    let started = Instant::now();
    let error = channel
        .execute_observed(request(Some(5)), None, &mut |_, _| {})
        .unwrap_err();
    assert_eq!(error, "E_VM_TIMEOUT");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(
        channel
            .execute_observed(request(None), None, &mut |_, _| {})
            .unwrap_err(),
        "E_VM_TIMEOUT"
    );
}

#[test]
fn exec_wire_omits_guest_deadline_but_host_budget_remains_active() {
    use rish_guest_protocol::{FrameDecoder, Operation};
    struct WireCapture(RefCell<Vec<u8>>);
    impl SessionIo for WireCapture {
        fn write(&self, bytes: &[u8]) -> usize {
            self.0.borrow_mut().extend_from_slice(bytes);
            bytes.len()
        }
        fn advance(&self) -> Result<Vec<u8>, String> {
            Err("wire captured".into())
        }
        fn dropped_output(&self) -> u64 {
            0
        }
    }
    let (channel, _directory, _cancel) = channel();
    negotiate(&channel);
    let started = Instant::now();
    let (request, deadline) = prepare_execution(request(Some(600_000)));
    let budget = deadline.unwrap().duration_since(started);
    assert!(budget >= Duration::from_secs(600));
    assert!(budget < Duration::from_secs(601));
    let wire = WireCapture(RefCell::new(Vec::new()));
    let result =
        channel
            .client
            .lock()
            .unwrap()
            .execute_observed(request, &[], &wire, &mut |_, _| {});
    assert!(result.is_err());
    let mut decoder = FrameDecoder::new(DEFAULT_MAX_FRAME_SIZE).unwrap();
    decoder.push(&wire.0.into_inner()).unwrap();
    let frame = decoder.next_frame().unwrap().unwrap();
    assert!(decoder.next_frame().unwrap().is_none());
    let Message::Request(request) = &frame.message else {
        panic!("expected exec request")
    };
    let Operation::Exec(exec) = &request.operation else {
        panic!("expected exec operation")
    };
    assert_eq!(exec.argv, ["test"]);
    assert_eq!(
        exec.timeout_ms, None,
        "a guest-clock timeout must not SIGKILL before the host deadline"
    );
}

#[test]
fn accidental_concurrent_exec_fails_instead_of_deadlocking() {
    let (channel, _directory, _cancel) = channel();
    let _guard = channel.client.lock().unwrap();
    assert_eq!(
        channel
            .execute_observed(request(None), None, &mut |_, _| {})
            .unwrap_err(),
        "E_VM_SESSION_BUSY"
    );
}

fn output_packets() -> Vec<u8> {
    use rish_guest_protocol::{Event, EventKind, Response, ResponsePayload, encode_base64};
    let messages = [
        Message::Response(Response::success(
            RequestId::new("vm-req-1").unwrap(),
            ResponsePayload::ExecStarted {
                execution_id: "exec-test".into(),
                pid: 123,
            },
        )),
        Message::Event(Event::now(
            1,
            None,
            EventKind::Stream {
                execution_id: "exec-test".into(),
                channel: StreamChannel::Stdout,
                stream_sequence: 0,
                data_base64: encode_base64(b"hello"),
                eof: false,
            },
        )),
        Message::Event(Event::now(
            2,
            None,
            EventKind::Stream {
                execution_id: "exec-test".into(),
                channel: StreamChannel::Stderr,
                stream_sequence: 0,
                data_base64: encode_base64(b"world"),
                eof: false,
            },
        )),
        Message::Event(Event::now(
            3,
            None,
            EventKind::ProcessExited {
                execution_id: "exec-test".into(),
                exit_code: Some(0),
                signal: None,
            },
        )),
    ];
    let encoder = FrameEncoder::new(DEFAULT_MAX_FRAME_SIZE).unwrap();
    messages
        .into_iter()
        .flat_map(|message| encoder.encode(&Envelope::new(message)).unwrap())
        .collect()
}

#[test]
fn real_uart_output_is_forwarded_and_legacy_reply_is_preserved() {
    let (channel, _directory, _) = channel_with_output(&output_packets());
    negotiate(&channel);
    let mut observed = Vec::new();
    let reply = channel
        .execute_observed(request(None), None, &mut |stream, bytes| {
            observed.push((stream, bytes.to_vec()));
        })
        .unwrap();
    assert_eq!(reply.exit_code, 0);
    assert_eq!(reply.stdout, b"hello");
    assert_eq!(reply.stderr, b"world");
    assert_eq!(observed.len(), 2);
}

#[test]
fn combined_output_cap_cancels_vm_before_forwarding_excess_chunk() {
    let (channel, _directory, cancel) = channel_with_output(&output_packets());
    negotiate(&channel);
    let mut observed = Vec::new();
    let error = channel
        .execute_observed(request(None), Some(8), &mut |_, bytes| {
            observed.extend_from_slice(bytes);
        })
        .unwrap_err();
    assert_eq!(error, "E_VM_OUTPUT_LIMIT");
    assert_eq!(observed, b"hello");
    assert_eq!(cancel.check().unwrap_err(), "E_VM_CANCELLED");
}
