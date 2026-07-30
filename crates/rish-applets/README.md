# rish-applets

`rish-applets` provides native Rust implementations of bounded, portable
Linux/POSIX command semantics. The crate executes inside an app-owned
filesystem root and never interprets an ELF executable.

The catalog deliberately excludes commands whose meaning depends on Linux
kernel objects, including namespace, cgroup, device, module, process-table,
raw-network and service-manager operations. Those commands must be routed to a
verified native-Linux or full-VM backend.

Portable applets have bounded input, output, recursion and filesystem walks.
The root must already exist as a canonical, real directory. Path traversal and
symbolic-link following fail closed, calls are serialized, and the root
identity is checked before and after execution. The embedding app must keep
that directory private from unrelated host writers; this layer is not an OS
mount namespace and does not defend against a trusted host thread deliberately
racing path operations.

Only an explicit bare applet name has portable provenance. A path such as
`/image/bin/ls` or `./ls` is Guest executable input and is rejected here or
routed to a verified Linux backend. This is a command compatibility layer, not
a claim that the mobile host provides Linux kernel semantics.
