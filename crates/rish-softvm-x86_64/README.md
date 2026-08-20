# rish-softvm-x86_64

This crate is the Rust control plane for the `linux/amd64` software VM. It is
not a CPU emulator by itself. The production provider contract is the
AArch64-hosted threaded-code interpreter (TCTI) from the reviewed UTM QEMU
10.0.2 fork:

- tag `v10.0.2-utm`;
- commit `37ba092d59aff24900dfd0d5e01d4ed68441ba07`;
- source `qemu-10.0.2-utm.tar.xz`, 136751908 bytes;
- SHA-256 `f1d7357547a71ae3339a115d5c8f2b72e3b0089531d67c2aca43326d320ac6ca`;
- target list `x86_64-softmmu`;
- `--enable-tcg-threaded-interpreter`;
- no JIT, executable-memory translation, HVF, KVM, or private API.

The adapter validates that source/build identity and feature contract before
`VmEngine::probe` can report interpreted execution. An unlinked provider, an
ordinary TCG/JIT build, a hypervisor build, a 32-bit emulator, or an
experimental pure-Rust provider all remain unavailable.

## C provider contract

[`include/rish_tcti_provider.h`](include/rish_tcti_provider.h) declares the
versioned function table. The provider is constructed and owned on a dedicated
worker thread. Rust supplies bounded 16550 UART queues and a cancellation
callback. Calls to `run_quantum` are split into short bounded quanta, and
`cancel()` is visible both between calls and inside the provider.

The current crate validates and attaches an AMD64 Linux `bzImage`, optional
initrd, and root disk. It can expose UART diagnostics, but UART output is not a
guest-agent handshake. The `GuestChannel` methods deliberately return
`ControlTransportUnavailable` until a real, framed guest transport is wired.
Consequently no verified Linux/container capability profile can be issued by
this milestone.

QEMU and its linked dependencies are not relicensed by this MIT Rust crate.
Applications that distribute the provider must satisfy the applicable
GPL/LGPL source and notice obligations.
