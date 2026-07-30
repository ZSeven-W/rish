# rish guest protocol

This crate defines the control protocol between a mobile rish host and its
Linux VM guest. It is transport-neutral and has no async or platform SDK
dependency, so the same state machine can run over AF_VSOCK or virtio-serial.

## Framing

Each frame consists of:

```text
  0                   1                   2                   3
  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 |              JSON payload length (big endian)                 |
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 |                       JSON payload ...                        |
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

The length excludes the four-byte prefix. The default payload limit is 8 MiB.
Peers advertise their own limit during the handshake and must use the smaller
of the two values for the session. A decoder is poisoned after malformed,
oversized, wrong-protocol, or wrong-version input; the transport should close
and reconnect rather than attempting byte-stream resynchronization.
The incremental decoder also caps total queued input at two maximum-sized
frames. Callers should limit each transport read to
`FrameDecoder::remaining_buffer_capacity()` and drain complete frames between
reads.

`Hello` and `HelloAck` use the 8 MiB bootstrap limit. The negotiated
post-handshake limit must be at least 64 KiB; a peer advertising less is
rejected. Bootstrap stream chunks are at most 32 KiB, leaving space for base64
expansion and the surrounding event envelope within that minimum frame.

Stream bytes use base64 strings in JSON. This costs bandwidth but keeps the
wire format identical for Rust, Swift, Java, and ArkTS without adding a second
binary framing mode.

## Session flow

1. The host sends `Message::Hello` with explicit supported versions and desired
   capabilities.
2. The guest selects an exact common version and returns
   `Message::HelloAck`, including kernel/runtime capabilities and limits.
3. Both sides configure `FrameDecoder::set_expected_version` with the selected
   version.
4. The host sends `Message::Request`; the guest returns one correlated
   `Message::Response` and may emit correlated `Message::Event` values.

Request IDs are bounded strings to avoid JSON integer precision differences.
Event sequence numbers are monotonic within one negotiated session.

The guest must reject a handshake if any requested capability is unknown,
unavailable, restricted, duplicated, malformed, or has a schema version the
guest does not implement. Advertising a capability is not enough: every
privileged operation is checked again at request dispatch.

An `ExecStarted` response means that the process was accepted by the guest
supervisor; it does not mean the process has exited. The guest transport must
continue polling its handler while input is idle so stream events, deadlines,
cancellation, and `ProcessExited` can progress. Events emitted after the
initial response retain the originating exec request ID.
