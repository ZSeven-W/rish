# Android native-offload SDK

`RishBridge.kt` keeps planning and execution separate. `planJson` calls the
Rust JNI ABI; `RishHostDispatcher` accepts the resulting `plan.call` only
through a fixed Kotlin `when` allow-list.

Place the Rust library at
`jniLibs/<abi>/librish_ffi.so`, build `rish_jni.cpp` with the included
`CMakeLists.txt`, and package both native libraries for every supported ABI.

## Wire contract

The Kotlin `RishHostCall` and `RishHostReply` models correspond to the Rust
types. `RishHostCodec` handles snake-case field names and serializes
`ByteArray` values as unsigned JSON integer arrays, matching Rust `Vec<u8>`
without converting binary output to text or Base64. Rust call ids that exceed
the positive Kotlin `Long` range fail closed. `RishBridge.protocolVersion()`
exposes the linked Rust ABI version for an early startup check.

An app using AndroidX lifecycle coroutines can connect the stages as follows
(the SDK itself does not depend on AndroidX or kotlinx-coroutines):

```kotlin
val planned = JSONObject(RishBridge.planJson(planRequest))
val callJson = planned
    .getJSONObject("plan")
    .getJSONObject("call")
    .toString()

val cancellation = RishCancellationToken()
val dispatcher = RishHostDispatcher()
val job = lifecycleScope.launch {
    val replyJson = RishBridge.dispatchHostCall(
        callJson,
        dispatcher,
        cancellation,
    )
    // Deliver replyJson to the Rust runtime integration.
}

// In the component's lifecycle cleanup:
// cancellation.cancel()
// job.cancel()
```

Injected handlers are `suspend` functions. A long-running implementation must
check `RishCancellationToken` before and after blocking platform work.

## Implemented operations

- `service.systemctl` stores unit names and active/inactive state in memory. It
  implements `start`, `stop`, `restart`, `status`, `is-active`, and
  `list-units`. It is not PID 1, systemd, a service process launcher, or a
  cgroup manager.
- `container.docker_api` fails with `docker_api_unavailable` unless the app
  injects the one named `dockerApiHandler` slot. Such a handler may connect to
  an explicitly authorized remote/VM API; this SDK does not start `dockerd`.
- Unknown operations and requests for real kernel semantics fail closed.

The example caps JSON calls, stdin, arguments, environment fields, capability
requirements, and the unit table. An injected transport should enforce its own
tighter request, response, timeout, and concurrency limits.

## Permission and kernel boundary

The application remains responsible for runtime permissions, URI grants,
Keystore credentials, foreground-service policy, network security
configuration, SELinux/OEM policy, and user consent. A Rust capability result
does not grant an Android permission.

An ordinary Android application does not gain namespaces, delegated cgroup v2,
kernel-module loading, arbitrary `/dev` access, a privileged host container,
systemd, Docker-in-Docker, or a complete network namespace from this bridge.
Those features require a real Linux guest or a separately probed and authorized
OEM/root backend. Guest privilege must not be reported as Android host
privilege.
