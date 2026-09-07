use super::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

struct ScriptIo<'a> {
    packets: RefCell<VecDeque<Vec<u8>>>,
    observed: &'a Cell<usize>,
    steps: Cell<usize>,
    require_early_output: bool,
    drop_on_advance: bool,
}

impl SessionIo for ScriptIo<'_> {
    fn write(&self, bytes: &[u8]) -> usize {
        bytes.len()
    }
    fn advance(&self) -> Result<Vec<u8>, String> {
        if self.require_early_output && self.steps.get() == 1 {
            assert_eq!(
                self.observed.get(),
                1,
                "login output must arrive before process exit"
            );
        }
        self.steps.set(self.steps.get() + 1);
        Ok(self.packets.borrow_mut().pop_front().unwrap_or_default())
    }
    fn dropped_output(&self) -> u64 {
        u64::from(self.drop_on_advance && self.steps.get() > 0)
    }
}

fn packet(event: EventKind) -> Vec<u8> {
    FrameEncoder::new(DEFAULT_MAX_FRAME_SIZE)
        .unwrap()
        .encode(&Envelope::new(Message::Event(Event::now(1, None, event))))
        .unwrap()
}

fn stream(id: &str) -> EventKind {
    EventKind::Stream {
        execution_id: id.into(),
        channel: StreamChannel::Stdout,
        stream_sequence: 0,
        data_base64: encode_base64(b"device-code"),
        eof: false,
    }
}

fn exit() -> EventKind {
    EventKind::ProcessExited {
        execution_id: "login".into(),
        exit_code: Some(0),
        signal: None,
    }
}

#[test]
fn output_is_observed_before_the_next_guest_advance() {
    let observed = Cell::new(0);
    let io = ScriptIo {
        packets: RefCell::new(VecDeque::from([packet(stream("login")), packet(exit())])),
        observed: &observed,
        steps: Cell::new(0),
        require_early_output: true,
        drop_on_advance: false,
    };
    let mut client = SessionClient::new(5).unwrap();
    let mut events = Vec::new();
    client
        .collect_until_exit_observed("login", &mut events, &io, &mut |event| {
            if matches!(event.event, EventKind::Stream { .. }) {
                observed.set(observed.get() + 1);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(observed.get(), 1);
    assert_eq!(events.len(), 2);
}

#[test]
fn another_executions_output_is_not_observed() {
    let observed = Cell::new(0);
    let mut first = packet(stream("other"));
    first.extend(packet(stream("login")));
    let io = ScriptIo {
        packets: RefCell::new(VecDeque::from([first, packet(exit())])),
        observed: &observed,
        steps: Cell::new(0),
        require_early_output: false,
        drop_on_advance: false,
    };
    SessionClient::new(5)
        .unwrap()
        .collect_until_exit_observed("login", &mut Vec::new(), &io, &mut |event| {
            if let EventKind::Stream { execution_id, .. } = &event.event {
                assert_eq!(execution_id, "login");
                observed.set(observed.get() + 1);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(observed.get(), 1);
}

#[test]
fn bytes_dropped_during_advance_reject_before_observation() {
    let observed = Cell::new(0);
    let io = ScriptIo {
        packets: RefCell::new(VecDeque::from([packet(stream("login")), packet(exit())])),
        observed: &observed,
        steps: Cell::new(0),
        require_early_output: false,
        drop_on_advance: true,
    };
    let result = SessionClient::new(5).unwrap().collect_until_exit_observed(
        "login",
        &mut Vec::new(),
        &io,
        &mut |_| {
            observed.set(observed.get() + 1);
            Ok(())
        },
    );
    assert!(matches!(result, Err(SessionError::DroppedBytes)));
    assert_eq!(observed.get(), 0);
}

#[test]
fn full_execute_observes_decoded_spawn_output_before_exit() {
    let observed = Cell::new(0);
    let response = Response::success(
        RequestId::new("vm-req-1").unwrap(),
        ResponsePayload::ExecStarted {
            execution_id: "login".into(),
            pid: 123,
        },
    );
    let mut first = FrameEncoder::new(DEFAULT_MAX_FRAME_SIZE)
        .unwrap()
        .encode(&Envelope::new(Message::Response(response)))
        .unwrap();
    first.extend(packet(stream("other")));
    first.extend(packet(stream("login")));
    let io = ScriptIo {
        packets: RefCell::new(VecDeque::from([first, packet(exit())])),
        observed: &observed,
        steps: Cell::new(0),
        require_early_output: true,
        drop_on_advance: false,
    };
    let mut client = SessionClient::new(5).unwrap();
    client.negotiated = Some(NegotiatedSession {
        session_id: "test".into(),
        version: crate::CURRENT_PROTOCOL_VERSION,
        max_frame_size: DEFAULT_MAX_FRAME_SIZE as u32,
    });
    let request = crate::ExecRequest {
        argv: vec!["codex".into(), "login".into()],
        env: Default::default(),
        cwd: None,
        user: None,
        tty: false,
        attach_stdin: false,
        attach_stdout: true,
        attach_stderr: true,
        timeout_ms: None,
    };
    let result = client
        .execute_observed(request, &[], &io, &mut |channel, data| {
            assert_eq!(channel, StreamChannel::Stdout);
            assert_eq!(data, b"device-code");
            observed.set(observed.get() + 1);
        })
        .unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout, b"device-code");
    assert_eq!(observed.get(), 1);
}
