use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rish_guest_protocol::{
    CURRENT_PROTOCOL_VERSION, CancelRequest, ExecRequest, Hello, Message, PingRequest,
    ProtocolVersion, ResponseOutcome, StreamChannel,
};

use super::*;

mod interactive;

fn host_hello(max_frame_size: u32, versions: Vec<ProtocolVersion>) -> Hello {
    Hello {
        request_id: request_id("hello-1"),
        role: PeerRole::Host,
        peer: PeerInfo {
            name: "test-host".to_owned(),
            version: "1".to_owned(),
            platform: "test".to_owned(),
            architecture: "arm64".to_owned(),
        },
        supported_versions: versions,
        requested_capabilities: Vec::new(),
        max_frame_size,
    }
}

fn request_id(value: &str) -> RequestId {
    RequestId::new(value).unwrap()
}

fn handshake<H: OperationHandler>(agent: &mut GuestAgent<H>) -> Vec<Envelope> {
    let output = agent
        .handle(Envelope::new(Message::Hello(host_hello(
            1024 * 1024,
            vec![CURRENT_PROTOCOL_VERSION],
        ))))
        .unwrap();
    assert!(matches!(
        output[0].message,
        Message::HelloAck(HelloAck {
            outcome: HandshakeOutcome::Accepted { .. },
            ..
        })
    ));
    output
}

#[derive(Debug, Default)]
struct PermissiveHandler {
    called: bool,
}

impl OperationHandler for PermissiveHandler {
    fn handle(
        &mut self,
        _request_id: &RequestId,
        _operation: &Operation,
    ) -> Result<HandlerReply, RemoteError> {
        self.called = true;
        Ok(HandlerReply {
            response: ResponsePayload::Ack,
            events: Vec::new(),
        })
    }
}

fn capability(name: &str, version: u16, status: CapabilityStatus) -> Capability {
    Capability {
        name: name.to_owned(),
        version,
        status,
        attributes: BTreeMap::new(),
        reason: (status != CapabilityStatus::Available).then(|| "policy denied".to_owned()),
    }
}

fn permissive_agent(features: Vec<Capability>) -> GuestAgent<PermissiveHandler> {
    GuestAgent::new(
        PermissiveHandler::default(),
        PeerInfo {
            name: "test-agent".to_owned(),
            version: "1".to_owned(),
            platform: "linux".to_owned(),
            architecture: "aarch64".to_owned(),
        },
        GuestCapabilities {
            kernel_release: "test".to_owned(),
            architecture: "aarch64".to_owned(),
            init_system: "test".to_owned(),
            cgroup_version: Some(2),
            container_runtimes: Vec::new(),
            features,
        },
        GuestLimits {
            max_frame_size: 1024 * 1024,
            max_concurrent_exec: 1,
            max_port_forwards: 0,
            max_stream_chunk_size: 1024,
        },
        "test-session",
    )
}

fn exec_request(script: &str, attach_stdout: bool, attach_stderr: bool) -> ExecRequest {
    ExecRequest {
        argv: vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()],
        env: BTreeMap::new(),
        cwd: None,
        user: None,
        tty: false,
        attach_stdin: false,
        attach_stdout,
        attach_stderr,
        timeout_ms: None,
    }
}

fn start_exec(handler: &mut NativeOperationHandler, id: &str, request: ExecRequest) -> String {
    let reply = handler
        .handle(&request_id(id), &Operation::Exec(request))
        .unwrap();
    match reply.response {
        ResponsePayload::ExecStarted { execution_id, .. } => execution_id,
        response => panic!("unexpected exec response: {response:?}"),
    }
}

fn poll_until_exit(
    handler: &mut NativeOperationHandler,
    expected_execution_id: &str,
) -> Vec<HandlerEvent> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut events = Vec::new();
    loop {
        if let Some(event) = handler.poll_event() {
            let exited = matches!(
                &event.event,
                EventKind::ProcessExited { execution_id, .. }
                    if execution_id == expected_execution_id
            );
            events.push(event);
            if exited {
                return events;
            }
        } else {
            assert!(
                Instant::now() < deadline,
                "execution {expected_execution_id} did not finish"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
}

fn stream_bytes(events: &[HandlerEvent], channel: StreamChannel) -> Vec<u8> {
    let mut bytes = Vec::new();
    for event in events {
        if let EventKind::Stream {
            channel: actual,
            data_base64,
            ..
        } = &event.event
        {
            if *actual == channel {
                bytes.extend(BASE64.decode(data_base64).unwrap());
            }
        }
    }
    bytes
}

fn rejected_hello(error_code: ErrorCode, hello: Hello, features: Vec<Capability>) {
    let mut agent = permissive_agent(features);
    let output = agent.handle(Envelope::new(Message::Hello(hello))).unwrap();
    assert_eq!(agent.negotiated_version(), None);
    assert!(matches!(
        &output[0].message,
        Message::HelloAck(HelloAck {
            outcome: HandshakeOutcome::Rejected {
                error: RemoteError { code, .. }
            },
            ..
        }) if *code == error_code
    ));
}

#[test]
fn rejects_request_before_handshake() {
    let mut agent = bootstrap_agent();
    let request = Request {
        id: request_id("request-1"),
        operation: Operation::Ping(PingRequest {
            nonce: "abc".to_owned(),
        }),
    };

    assert!(matches!(
        agent.handle(Envelope::new(Message::Request(request))),
        Err(AgentError::HandshakeRequired)
    ));
}

#[test]
fn handshake_and_ping_round_trip() {
    let mut agent = bootstrap_agent();
    handshake(&mut agent);
    let request = Request {
        id: request_id("request-1"),
        operation: Operation::Ping(PingRequest {
            nonce: "abc".to_owned(),
        }),
    };
    let output = agent
        .handle(Envelope::new(Message::Request(request)))
        .unwrap();

    assert!(matches!(
        &output[0].message,
        Message::Response(Response {
            outcome: ResponseOutcome::Success {
                result: ResponsePayload::Pong { nonce }
            },
            ..
        }) if nonce == "abc"
    ));
}

#[test]
fn incompatible_handshake_is_rejected() {
    let hello = host_hello(
        MIN_NEGOTIATED_FRAME_SIZE as u32,
        vec![ProtocolVersion::new(99, 0)],
    );
    rejected_hello(ErrorCode::VersionMismatch, hello, Vec::new());
}

#[test]
fn hello_envelope_version_must_be_supported_and_advertised() {
    let mut agent = bootstrap_agent();
    let received = ProtocolVersion::new(99, 0);
    let hello = host_hello(
        MIN_NEGOTIATED_FRAME_SIZE as u32,
        vec![CURRENT_PROTOCOL_VERSION, received],
    );
    let output = agent
        .handle(Envelope::with_version(received, Message::Hello(hello)))
        .unwrap();

    assert_eq!(agent.negotiated_version(), None);
    assert!(matches!(
        &output[0].message,
        Message::HelloAck(HelloAck {
            outcome: HandshakeOutcome::Rejected {
                error: RemoteError {
                    code: ErrorCode::VersionMismatch,
                    ..
                }
            },
            ..
        })
    ));
}

#[test]
fn handshake_rejects_a_frame_limit_below_the_minimum() {
    let hello = host_hello(
        MIN_NEGOTIATED_FRAME_SIZE as u32 - 1,
        vec![CURRENT_PROTOCOL_VERSION],
    );
    rejected_hello(ErrorCode::InvalidRequest, hello, Vec::new());
}

#[test]
fn handshake_negotiates_the_smaller_frame_limit() {
    let mut agent = bootstrap_agent();
    let host_limit = 256 * 1024;
    let output = agent
        .handle(Envelope::new(Message::Hello(host_hello(
            host_limit,
            vec![CURRENT_PROTOCOL_VERSION],
        ))))
        .unwrap();

    assert_eq!(agent.negotiated_version(), Some(CURRENT_PROTOCOL_VERSION));
    assert_eq!(agent.negotiated_max_frame_size(), Some(host_limit));
    assert!(matches!(
        &output[0],
        Envelope {
            message: Message::HelloAck(HelloAck {
                outcome: HandshakeOutcome::Accepted {
                    limits: GuestLimits { max_frame_size, .. },
                    ..
                },
                ..
            }),
            ..
        } if *max_frame_size == host_limit
    ));
}

#[test]
fn duplicate_hello_and_wrong_session_version_are_rejected() {
    let mut agent = bootstrap_agent();
    handshake(&mut agent);
    assert!(matches!(
        agent.handle(Envelope::new(Message::Hello(host_hello(
            1024 * 1024,
            vec![CURRENT_PROTOCOL_VERSION],
        )))),
        Err(AgentError::DuplicateHandshake)
    ));

    let received = ProtocolVersion::new(2, 0);
    let request = Request {
        id: request_id("request-version-mismatch"),
        operation: Operation::Ping(PingRequest {
            nonce: "abc".to_owned(),
        }),
    };
    assert!(matches!(
        agent.handle(Envelope::with_version(received, Message::Request(request))),
        Err(AgentError::ProtocolVersionMismatch {
            expected: CURRENT_PROTOCOL_VERSION,
            received: actual,
        }) if actual == received
    ));
}

#[test]
fn requested_capabilities_reject_invalid_names_and_duplicates() {
    let available = vec![capability(
        capability_name::EXEC,
        1,
        CapabilityStatus::Available,
    )];
    for requested in [
        vec![String::new()],
        vec![" ".to_owned()],
        vec!["x".repeat(MAX_CAPABILITY_NAME_LENGTH + 1)],
        vec![
            capability_name::EXEC.to_owned(),
            capability_name::EXEC.to_owned(),
        ],
    ] {
        let mut hello = host_hello(1024 * 1024, vec![CURRENT_PROTOCOL_VERSION]);
        hello.requested_capabilities = requested;
        rejected_hello(ErrorCode::InvalidRequest, hello, available.clone());
    }
}

#[test]
fn requested_capabilities_reject_unknown_unavailable_and_unknown_versions() {
    for (requested, features) in [
        ("unknown.capability", Vec::new()),
        (
            capability_name::OCI,
            vec![capability(
                capability_name::OCI,
                1,
                CapabilityStatus::Unavailable,
            )],
        ),
        (
            capability_name::EXEC,
            vec![capability(
                capability_name::EXEC,
                2,
                CapabilityStatus::Available,
            )],
        ),
    ] {
        let mut hello = host_hello(1024 * 1024, vec![CURRENT_PROTOCOL_VERSION]);
        hello.requested_capabilities = vec![requested.to_owned()];
        rejected_hello(ErrorCode::CapabilityUnavailable, hello, features);
    }
}

#[test]
fn requested_available_capability_establishes_the_session() {
    let mut agent = permissive_agent(vec![capability(
        capability_name::EXEC,
        1,
        CapabilityStatus::Available,
    )]);
    let mut hello = host_hello(1024 * 1024, vec![CURRENT_PROTOCOL_VERSION]);
    hello.requested_capabilities = vec![capability_name::EXEC.to_owned()];
    let output = agent.handle(Envelope::new(Message::Hello(hello))).unwrap();
    assert!(matches!(
        output[0].message,
        Message::HelloAck(HelloAck {
            outcome: HandshakeOutcome::Accepted { .. },
            ..
        })
    ));
}

#[test]
fn central_capability_gate_checks_status_and_version() {
    for features in [
        Vec::new(),
        vec![capability(
            capability_name::OCI,
            1,
            CapabilityStatus::Restricted,
        )],
        vec![capability(
            capability_name::OCI,
            2,
            CapabilityStatus::Available,
        )],
    ] {
        let mut agent = permissive_agent(features);
        handshake(&mut agent);
        let request = Request {
            id: request_id("oci-run"),
            operation: Operation::OciRun(rish_guest_protocol::OciRunRequest {
                container_id: "demo".to_owned(),
                attach: false,
            }),
        };
        let output = agent
            .handle(Envelope::new(Message::Request(request)))
            .unwrap();
        assert!(!agent.handler.called);
        assert!(matches!(
            &output[0].message,
            Message::Response(Response {
                outcome: ResponseOutcome::Error {
                    error: RemoteError {
                        code: ErrorCode::CapabilityUnavailable,
                        ..
                    }
                },
                ..
            })
        ));
    }
}

#[test]
fn exec_returns_immediately_and_ping_and_cancel_remain_responsive() {
    let mut agent = bootstrap_agent();
    handshake(&mut agent);
    let exec_request_id = request_id("long-exec");
    let started_at = Instant::now();
    let output = agent
        .handle(Envelope::new(Message::Request(Request {
            id: exec_request_id.clone(),
            operation: Operation::Exec(exec_request("sleep 30", true, true)),
        })))
        .unwrap();
    assert!(started_at.elapsed() < Duration::from_secs(1));
    let execution_id = match &output[0].message {
        Message::Response(Response {
            outcome:
                ResponseOutcome::Success {
                    result: ResponsePayload::ExecStarted { execution_id, .. },
                },
            ..
        }) => execution_id.clone(),
        message => panic!("unexpected exec response: {message:?}"),
    };

    let ping_output = agent
        .handle(Envelope::new(Message::Request(Request {
            id: request_id("ping-during-exec"),
            operation: Operation::Ping(PingRequest {
                nonce: "still-responsive".to_owned(),
            }),
        })))
        .unwrap();
    assert!(matches!(
        &ping_output[0].message,
        Message::Response(Response {
            outcome: ResponseOutcome::Success {
                result: ResponsePayload::Pong { nonce }
            },
            ..
        }) if nonce == "still-responsive"
    ));

    let cancel_output = agent
        .handle(Envelope::new(Message::Request(Request {
            id: request_id("cancel-long-exec"),
            operation: Operation::Cancel(CancelRequest {
                target_request_id: exec_request_id.clone(),
                execution_id: Some(execution_id.clone()),
                signal: None,
            }),
        })))
        .unwrap();
    assert!(matches!(
        &cancel_output[0].message,
        Message::Response(Response {
            outcome: ResponseOutcome::Success {
                result: ResponsePayload::Cancelled { target_request_id }
            },
            ..
        }) if *target_request_id == exec_request_id
    ));

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let exited = agent.poll().into_iter().any(|envelope| {
            matches!(
                envelope.message,
                Message::Event(Event {
                    request_id: Some(ref owner),
                    event: EventKind::ProcessExited {
                        execution_id: ref event_execution_id,
                        ..
                    },
                    ..
                }) if owner == &exec_request_id && event_execution_id == &execution_id
            )
        });
        if exited {
            break;
        }
        assert!(Instant::now() < deadline, "cancelled child was not reaped");
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn timeout_kills_and_reaps_the_execution() {
    let mut handler = NativeOperationHandler::default();
    let mut request = exec_request("sleep 30", true, true);
    request.timeout_ms = Some(30);
    let execution_id = start_exec(&mut handler, "timeout-exec", request);
    let events = poll_until_exit(&mut handler, &execution_id);

    assert_eq!(handler.active_execution_count(), 0);
    assert!(events.iter().any(|event| matches!(
        &event.event,
        EventKind::ProcessExited {
            signal: Some(9),
            ..
        }
    )));
}

#[test]
fn max_concurrent_exec_and_duplicate_active_request_are_rejected() {
    let mut handler = NativeOperationHandler::new(NativeExecutionConfig {
        max_concurrent_exec: 1,
        ..NativeExecutionConfig::default()
    })
    .unwrap();
    let first = start_exec(
        &mut handler,
        "active-request",
        exec_request("sleep 30", false, false),
    );
    let duplicate = handler
        .handle(
            &request_id("active-request"),
            &Operation::Exec(exec_request(":", false, false)),
        )
        .unwrap_err();
    assert_eq!(duplicate.code, ErrorCode::AlreadyExists);
    let exhausted = handler
        .handle(
            &request_id("another-request"),
            &Operation::Exec(exec_request(":", false, false)),
        )
        .unwrap_err();
    assert_eq!(exhausted.code, ErrorCode::ResourceExhausted);

    handler
        .handle(
            &request_id("cancel-active"),
            &Operation::Cancel(CancelRequest {
                target_request_id: request_id("active-request"),
                execution_id: Some(first.clone()),
                signal: None,
            }),
        )
        .unwrap();
    poll_until_exit(&mut handler, &first);
}

#[test]
fn attached_empty_streams_emit_explicit_eof_events() {
    let mut handler = NativeOperationHandler::default();
    let execution_id = start_exec(&mut handler, "empty-streams", exec_request(":", true, true));
    let events = poll_until_exit(&mut handler, &execution_id);
    let streams = events
        .iter()
        .filter_map(|event| match &event.event {
            EventKind::Stream {
                channel,
                data_base64,
                eof,
                ..
            } => Some((*channel, data_base64.as_str(), *eof)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        streams,
        vec![
            (StreamChannel::Stdout, "", true),
            (StreamChannel::Stderr, "", true),
        ]
    );
}

#[test]
fn exec_emits_only_requested_streams_and_never_inherits_control_stdin() {
    let mut handler = NativeOperationHandler::default();
    let execution_id = start_exec(
        &mut handler,
        "stream-selection",
        exec_request(
            "if read value; then printf inherited; else printf eof >&2; fi",
            false,
            true,
        ),
    );
    let events = poll_until_exit(&mut handler, &execution_id);
    assert!(stream_bytes(&events, StreamChannel::Stdout).is_empty());
    assert_eq!(stream_bytes(&events, StreamChannel::Stderr), b"eof");
    assert!(!events.iter().any(|event| matches!(
        &event.event,
        EventKind::Stream {
            channel: StreamChannel::Stdout,
            ..
        }
    )));
}

#[test]
fn output_is_streamed_with_a_hard_cumulative_limit() {
    let mut handler = NativeOperationHandler::new(NativeExecutionConfig {
        max_stream_chunk_size: 256,
        max_stdout_bytes: 1024,
        max_stderr_bytes: 1024,
        ..NativeExecutionConfig::default()
    })
    .unwrap();
    let script = "\
        i=0; \
        while [ \"$i\" -lt 8192 ]; do \
            printf 0123456789abcdef; \
            printf fedcba9876543210 >&2; \
            i=$((i + 1)); \
        done";
    let execution_id = start_exec(
        &mut handler,
        "bounded-output",
        exec_request(script, true, true),
    );
    let events = poll_until_exit(&mut handler, &execution_id);

    let stdout_len = stream_bytes(&events, StreamChannel::Stdout).len();
    let stderr_len = stream_bytes(&events, StreamChannel::Stderr).len();
    assert!(stdout_len <= 1024);
    assert!(stderr_len <= 1024);
    assert!(stdout_len == 1024 || stderr_len == 1024);
    assert_eq!(handler.active_execution_count(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn escaped_background_pipe_holder_cannot_block_completion() {
    let mut handler = NativeOperationHandler::default();
    let started = Instant::now();
    let execution_id = start_exec(
        &mut handler,
        "escaped-descendant",
        exec_request("setsid sh -c 'sleep 1' &", true, true),
    );
    let events = poll_until_exit(&mut handler, &execution_id);

    assert!(started.elapsed() < Duration::from_millis(900));
    assert!(
        events
            .iter()
            .any(|event| matches!(&event.event, EventKind::Stream { eof: true, .. }))
    );
    assert_eq!(handler.active_execution_count(), 0);
}

#[test]
fn minimum_frame_size_fits_a_full_stream_chunk() {
    let encoder = rish_guest_protocol::FrameEncoder::new(MIN_NEGOTIATED_FRAME_SIZE).unwrap();
    let envelope = Envelope::new(Message::Event(Event::now(
        1,
        Some(request_id("stream-frame")),
        EventKind::Stream {
            execution_id: "exec-frame".to_owned(),
            channel: StreamChannel::Stdout,
            stream_sequence: 0,
            data_base64: BASE64.encode(vec![0_u8; DEFAULT_STREAM_CHUNK_SIZE as usize]),
            eof: false,
        },
    )));
    encoder.encode(&envelope).unwrap();
}
