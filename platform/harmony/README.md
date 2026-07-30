# HarmonyOS/OpenHarmony native-offload SDK

`RishBridge.ets` exposes the Rust N-API planner and an ArkTS host-call
dispatcher as separate steps. The N-API module returns a plan; ArkTS must
extract `plan.call`, decode it with `RishHostCodec`, and pass it through the
fixed allow-list.

Build `rish_napi.cpp` with the included `CMakeLists.txt`, place the Rust
`librish_ffi.so` under `libs/<OHOS_ARCH>`, and package both it and the
resulting `librish_napi.so` wrapper in the HAP. ArkTS imports
`librish_napi.so`; the wrapper resolves the Rust ABI from the sibling
`librish_ffi.so`.

## Wire contract

ArkTS uses camel-case in memory and the codec maps it to Rust snake-case JSON.
`stdin`, `stdout`, and `stderr` are `Uint8Array`; the wire codec converts them
to unsigned JSON integer arrays compatible with Rust `Vec<u8>`. ArkTS JSON
numbers cannot safely represent every Rust `u64`, so call ids outside the JSON
safe-integer range are rejected. `protocolVersion()` exposes the linked Rust
ABI version for an early startup check.

```typescript
const planned = JSON.parse(planJson(planRequest));
const callJson = JSON.stringify(planned.plan.call);
const cancellation = new RishCancellationToken();
const dispatcher = new RishHostDispatcher();

const pendingReply = dispatchHostCall(
  callJson,
  dispatcher,
  cancellation
);

// The owning Ability/Page lifecycle cleanup may call:
// cancellation.cancel();
const replyJson = await pendingReply;
```

## Portable Rust applets

The app creates configuration from its own
`UIAbilityContext/ApplicationContext.filesDir`. `executePortableApplet`
generates a Harmony/app-sandbox plan itself and reaches the N-API applet
wrapper only for an exact matching `portable_applet` plan:

```typescript
const configuration = RishAppletConfiguration.inAppFiles(context);
const responseJson = executePortableApplet(
  {
    program: 'sha256sum',
    args: [],
    env: {},
    cwd: '/',
    stdin: new Uint8Array([0, 255])
  },
  configuration
);
```

There is no public configuration constructor that accepts an arbitrary guest
root. Pass the platform `Context` itself, not `filesDir`, command payload, or UI
text. The constructor creates and verifies one direct child of
`Context.filesDir`. The native ABI is synchronous and bounded, so run
filesystem-heavy applets in a Worker/taskpool rather than the UI thread.
stdout/stderr stay as JSON byte arrays.

The low-level N-API functions are trusted HAP implementation details. Do not
expose `executeAppletJson` to a WebView, downloaded script, Guest process, or
other untrusted JSON source; only `executePortableApplet` should be reachable
from product code handling Guest requests.

## Implemented operations

- `service.systemctl` is a HAP-local in-memory unit state map supporting
  `start`, `stop`, `restart`, `status`, `is-active`, and `list-units`. It is
  not systemd, PID 1, a unit-file engine, a process launcher, or cgroup
  management.
- `container.docker_api` returns `docker_api_unavailable` by default. The
  constructor accepts only the dedicated Docker API handler slot, suitable for
  a separately authorized remote or future VM guest API. No `dockerd` is
  started by this bridge.
- Unknown operations and calls requesting Linux-kernel semantics fail closed.

Handlers are Promise-based, use `Uint8Array` for arbitrary output, and receive
a cooperative cancellation token that long-running platform work must check.
The example caps wire/input sizes and the in-memory unit table; injected
transports must impose their own response, timeout, and concurrency limits.

## Permission and kernel boundary

The HAP/system application must independently enforce user consent, network
permissions, file access, secure credential storage, background lifecycle, and
device/OEM policy. A host call does not grant any Harmony permission.

This ordinary HAP adapter does not provide Linux namespaces, delegated cgroups,
kernel modules, arbitrary device nodes, a privileged host container, systemd,
Docker-in-Docker, or a full network namespace. Use a real Linux guest for those
semantics, or a separately probed OEM/system backend where policy explicitly
permits it. Never label guest-only privilege as host privilege.
