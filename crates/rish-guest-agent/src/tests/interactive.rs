use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
#[cfg(target_os = "linux")]
use rish_guest_protocol::EventKind;
use rish_guest_protocol::{
    ErrorCode, Operation, ResponsePayload, StreamAction, StreamChannel, StreamRequest,
};

use super::*;

fn stream(
    handler: &mut NativeOperationHandler,
    request_name: &str,
    execution_id: &str,
    action: StreamAction,
) -> Result<HandlerReply, RemoteError> {
    handler.handle(
        &request_id(request_name),
        &Operation::Stream(StreamRequest {
            execution_id: execution_id.to_owned(),
            action,
        }),
    )
}

fn write_stdin(
    handler: &mut NativeOperationHandler,
    execution_id: &str,
    bytes: &[u8],
) -> Result<HandlerReply, RemoteError> {
    stream(
        handler,
        "stdin-write",
        execution_id,
        StreamAction::WriteStdin {
            data_base64: BASE64.encode(bytes),
        },
    )
}

fn close_stdin(
    handler: &mut NativeOperationHandler,
    execution_id: &str,
) -> Result<HandlerReply, RemoteError> {
    stream(
        handler,
        "stdin-close",
        execution_id,
        StreamAction::CloseStdin,
    )
}

fn assert_stream_accepted(reply: HandlerReply, execution_id: &str) {
    assert!(matches!(
        reply.response,
        ResponsePayload::StreamAccepted {
            execution_id: accepted
        } if accepted == execution_id
    ));
}

#[test]
fn attached_pipe_stdin_is_streamed_and_closed_without_blocking() {
    let mut handler = NativeOperationHandler::default();
    let mut request = exec_request(
        "IFS= read -r value; printf 'received:%s' \"$value\"",
        true,
        true,
    );
    request.attach_stdin = true;
    let execution_id = start_exec(&mut handler, "pipe-stdin", request);

    assert_stream_accepted(
        write_stdin(&mut handler, &execution_id, b"hello from host\n").unwrap(),
        &execution_id,
    );
    assert_stream_accepted(
        close_stdin(&mut handler, &execution_id).unwrap(),
        &execution_id,
    );

    let events = poll_until_exit(&mut handler, &execution_id);
    assert_eq!(
        stream_bytes(&events, StreamChannel::Stdout),
        b"received:hello from host"
    );
    assert_eq!(handler.active_execution_count(), 0);
}

#[test]
fn stream_requires_an_exact_active_execution_id() {
    let mut handler = NativeOperationHandler::default();
    let mut request = exec_request("sleep 30", false, false);
    request.attach_stdin = true;
    let execution_id = start_exec(&mut handler, "strict-stream-target", request);

    let error = write_stdin(
        &mut handler,
        &format!("{execution_id}-suffix"),
        b"do not deliver",
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);

    handler
        .handle(
            &request_id("cancel-strict-target"),
            &Operation::Cancel(CancelRequest {
                target_request_id: request_id("strict-stream-target"),
                execution_id: Some(execution_id.clone()),
                signal: None,
            }),
        )
        .unwrap();
    poll_until_exit(&mut handler, &execution_id);
}

#[test]
fn stream_rejects_missing_stdin_invalid_base64_and_oversized_chunks() {
    let mut handler = NativeOperationHandler::new(NativeExecutionConfig {
        max_stream_chunk_size: 8,
        ..NativeExecutionConfig::default()
    })
    .unwrap();
    let no_stdin = start_exec(
        &mut handler,
        "no-stdin",
        exec_request("sleep 30", false, false),
    );
    let missing = write_stdin(&mut handler, &no_stdin, b"x").unwrap_err();
    assert_eq!(missing.code, ErrorCode::InvalidRequest);

    let mut request = exec_request("sleep 30", false, false);
    request.attach_stdin = true;
    let with_stdin = start_exec(&mut handler, "bounded-stdin", request);
    let invalid = stream(
        &mut handler,
        "invalid-base64",
        &with_stdin,
        StreamAction::WriteStdin {
            data_base64: "AB==".to_owned(),
        },
    )
    .unwrap_err();
    assert_eq!(invalid.code, ErrorCode::InvalidRequest);
    let oversized = write_stdin(&mut handler, &with_stdin, b"123456789").unwrap_err();
    assert_eq!(oversized.code, ErrorCode::ResourceExhausted);

    for (target_request_id, execution_id) in [("no-stdin", no_stdin), ("bounded-stdin", with_stdin)]
    {
        handler
            .handle(
                &request_id("cleanup-stream-test"),
                &Operation::Cancel(CancelRequest {
                    target_request_id: request_id(target_request_id),
                    execution_id: Some(execution_id.clone()),
                    signal: None,
                }),
            )
            .unwrap();
        poll_until_exit(&mut handler, &execution_id);
    }
}

#[cfg(target_os = "linux")]
fn tty_request(script: &str) -> ExecRequest {
    let mut request = exec_request(script, true, true);
    request.tty = true;
    request.attach_stdin = true;
    request
}

#[cfg(target_os = "linux")]
#[test]
fn tty_exec_owns_a_real_controlling_pty_and_uses_console_streams() {
    let mut handler = NativeOperationHandler::default();
    let execution_id = start_exec(
        &mut handler,
        "real-pty",
        tty_request(
            "test -t 0 && test -t 1 && test -t 2 && test -r /dev/tty && printf 'tty-ok\\n'",
        ),
    );
    let events = poll_until_exit(&mut handler, &execution_id);

    assert!(
        stream_bytes(&events, StreamChannel::Console)
            .windows(b"tty-ok".len())
            .any(|window| window == b"tty-ok")
    );
    assert!(stream_bytes(&events, StreamChannel::Stdout).is_empty());
    assert!(stream_bytes(&events, StreamChannel::Stderr).is_empty());
    assert!(events.iter().any(|event| matches!(
        &event.event,
        EventKind::ProcessExited {
            exit_code: Some(0),
            ..
        }
    )));
}

#[cfg(target_os = "linux")]
#[test]
fn tty_stdin_close_delivers_eof_and_resize_updates_kernel_winsize() {
    let mut handler = NativeOperationHandler::default();
    let execution_id = start_exec(
        &mut handler,
        "interactive-pty",
        tty_request(
            "IFS= read -r first; \
             size=$(stty size); \
             if IFS= read -r second; then \
                 printf 'unexpected-second:%s' \"$second\"; \
             else \
                 printf 'value:%s size:%s' \"$first\" \"$size\"; \
             fi",
        ),
    );

    assert_stream_accepted(
        stream(
            &mut handler,
            "resize-pty",
            &execution_id,
            StreamAction::ResizeTty {
                rows: 41,
                columns: 101,
            },
        )
        .unwrap(),
        &execution_id,
    );
    assert_stream_accepted(
        write_stdin(&mut handler, &execution_id, b"hello\n").unwrap(),
        &execution_id,
    );
    assert_stream_accepted(
        close_stdin(&mut handler, &execution_id).unwrap(),
        &execution_id,
    );

    let events = poll_until_exit(&mut handler, &execution_id);
    let console = stream_bytes(&events, StreamChannel::Console);
    assert!(
        console
            .windows(b"value:hello size:41 101".len())
            .any(|window| window == b"value:hello size:41 101"),
        "unexpected PTY output: {}",
        String::from_utf8_lossy(&console)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn resize_rejects_non_tty_and_zero_dimensions() {
    let mut handler = NativeOperationHandler::default();
    let execution_id = start_exec(
        &mut handler,
        "resize-validation",
        exec_request("sleep 30", false, false),
    );
    let non_tty = stream(
        &mut handler,
        "resize-pipe",
        &execution_id,
        StreamAction::ResizeTty {
            rows: 24,
            columns: 80,
        },
    )
    .unwrap_err();
    assert_eq!(non_tty.code, ErrorCode::InvalidRequest);
    let zero = stream(
        &mut handler,
        "resize-zero",
        &execution_id,
        StreamAction::ResizeTty {
            rows: 0,
            columns: 80,
        },
    )
    .unwrap_err();
    assert_eq!(zero.code, ErrorCode::InvalidRequest);

    handler
        .handle(
            &request_id("cancel-resize-validation"),
            &Operation::Cancel(CancelRequest {
                target_request_id: request_id("resize-validation"),
                execution_id: Some(execution_id.clone()),
                signal: None,
            }),
        )
        .unwrap();
    poll_until_exit(&mut handler, &execution_id);
}

#[cfg(unix)]
#[test]
fn dropping_handler_terminates_and_reaps_attached_stdin_process() {
    let mut handler = NativeOperationHandler::default();
    let mut request = exec_request("sleep 30", false, false);
    request.attach_stdin = true;
    let reply = handler
        .handle(&request_id("drop-reap"), &Operation::Exec(request))
        .unwrap();
    let pid = match reply.response {
        ResponsePayload::ExecStarted { pid, .. } => pid,
        response => panic!("unexpected exec response: {response:?}"),
    };

    drop(handler);
    let status = std::process::Command::new("sh")
        .args(["-c", &format!("kill -0 {pid} 2>/dev/null")])
        .status()
        .unwrap();
    assert!(!status.success(), "child {pid} survived handler drop");
}
