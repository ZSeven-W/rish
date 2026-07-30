# iOS native-offload SDK

`RishBridge.swift` contains two separate stages:

1. `plan(request:)` calls the Rust planner through `rish.h`.
2. `RishHostDispatcher` executes a decoded `HostCall` only when its operation
   appears in the fixed Swift allow-list.

Planning never grants an iOS entitlement or starts work automatically. Add
`platform/rish.h` to the target bridging header, link the iOS build of
`librish_ffi`, and keep the Rust-owned string/free pairing unchanged.

## Wire contract

`RishHostCall` and `RishHostReply` map to the Rust structs field-for-field.
Swift coding keys map camel-case properties to `protocol_version`,
`kernel_semantics_required`, and `exit_code`. `stdin`, `stdout`, and `stderr`
are `[UInt8]`, so JSON encodes them as integer arrays compatible with Rust
`Vec<u8>`; using `Data` directly would incorrectly produce Base64.
`RishBridge.protocolVersion` exposes the linked Rust ABI version for an early
startup compatibility check.

A host app extracts `plan.call` from a successful planner response, serializes
that object as JSON, then decodes and dispatches it:

```swift
let planned = try RishBridge.plan(request: requestJSON)
let root = try JSONSerialization.jsonObject(with: planned) as! [String: Any]
let plan = root["plan"] as! [String: Any]
let callJSON = try JSONSerialization.data(
    withJSONObject: plan["call"] as! [String: Any]
)

let call = try RishBridge.decodeHostCall(callJSON)
let task = RishHostDispatcher().submit(call)
// Retain `task`; the owning lifecycle calls `task.cancel()` if needed.
let reply = await task.value
let replyJSON = try RishBridge.encodeHostReply(reply)
```

Production code should replace the forced casts in this compact example with
normal error handling.

## Implemented operations

- `service.systemctl` is an actor-backed, in-memory state machine. It supports
  `start`, `stop`, `restart`, `status`, `is-active`, and `list-units`. State is
  lost when the supervisor is released. It does not launch processes, create
  cgroups, run PID 1, parse unit files, or implement systemd.
- `container.docker_api` returns `docker_api_unavailable` unless the app injects
  the dedicated `dockerAPIHandler`. Injection is a place to connect an
  explicitly authorized remote API or a future VM guest service; it does not
  imply that `dockerd` exists on iOS.
- Every other operation returns `operation_not_allow_listed`. Calls requesting
  real kernel semantics are rejected before handler dispatch.

Handlers are `async`, return arbitrary binary output, and should call
`Task.checkCancellation()` around long operations. `submit` returns the Swift
`Task` used as the cancellation handle. The example also caps JSON calls,
stdin, argument/environment counts, field sizes, and the in-memory unit table;
production transports should apply their own tighter workload limits.

## Permission and kernel boundary

The app must perform its own UI consent, entitlement, Keychain, file bookmark,
and network policy checks inside or before an injected handler. A Rust
capability profile is not an iOS permission.

This SDK does not provide Linux namespaces, cgroups, kernel modules, Linux
device nodes, a privileged host container, systemd, Docker-in-Docker, or a
network namespace on stock iOS. Those require a real Linux guest backend; any
privilege there remains scoped to that guest.
