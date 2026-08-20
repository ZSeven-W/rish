# rish-softvm-x86_64

This crate is the Rust control plane for the `linux/amd64` software VM. It
plugs interchangeable no-JIT interpreter providers into the same evidence
chain: bounded quanta, the versioned guest control transport over the second
16550, and the `VmEngine`/`GuestChannel` contract of `rish-vm`.

## Providers

### ExperimentalPureRust (product backend)

`PureRustProvider` wraps the in-repository `rish-softvm-core` interpreter: a
pure-Rust, no-JIT x86_64 full-system emulator (CPU, paging, exceptions,
interrupts, 8259/8254/CMOS/16550 chipset) that boots the pinned Alpine Linux
bzImage plus initramfs directly in this process. One execution unit is one
retired guest instruction. This is the primary mobile backend per ADR-0003.

The provider contract gate (`ProviderBuildInfo::validate_pure_rust`) requires:

- provider kind `experimental_pure_rust`;
- non-empty build id and source revision (build.rs records the workspace
  `git rev-parse HEAD`, overridable with `RISH_SOURCE_REVISION`);
- full-system x86_64, bounded-run, cancel-poll, both 16550 channels, and
  initrd feature bits;
- no JIT/hypervisor/private feature bits;
- exactly one vCPU and 128..=1024 MiB guest memory.

The interpreter exposes the console and control serial ports only. It does
not declare virtio-block or user networking, so the engine rejects root-disk
and network device requests instead of emulating fake success. The pinned
docker diagnostic guest is initramfs-only and matches this device surface.

### QemuTcti (documented fallback boundary)

The C ABI adapter validates the pinned UTM QEMU 10.0.2 threaded-code
interpreter build (`--enable-tcg-threaded-interpreter`, no JIT, no
executable-memory translation, no HVF/KVM/private API):

- tag `v10.0.2-utm`;
- commit `37ba092d59aff24900dfd0d5e01d4ed68441ba07`;
- source `qemu-10.0.2-utm.tar.xz`, 136751908 bytes;
- SHA-256 `f1d7357547a71ae3339a115d5c8f2b72e3b0089531d67c2aca43326d320ac6ca`;
- target list `x86_64-softmmu`;
- host architecture `aarch64`.

ADR-0003 keeps this adapter in the tree as a documented alternative provider
boundary; it is not a product path.

## C provider contract

[`include/rish_tcti_provider.h`](include/rish_tcti_provider.h) declares the
versioned function table. The provider is constructed and owned on a
dedicated worker thread. Rust supplies bounded 16550 UART queues and a
cancellation callback. Calls to `run_quantum` are split into short bounded
quanta, and `cancel()` is visible both between calls and inside the provider.

The crate validates and attaches an AMD64 Linux `bzImage`, optional initrd,
and root disk. The pure-Rust provider boots the bzImage and initramfs
itself; the guest control transport is the versioned rish protocol over the
second 16550 (`ttyS1`), driven by the shared `SessionClient` from
`rish-guest-protocol`.

QEMU and its linked dependencies are not relicensed by this MIT Rust crate.
Applications that distribute the TCTI provider must satisfy the applicable
GPL/LGPL source and notice obligations. The pure-Rust interpreter has no
third-party emulator license obligations of its own; guest kernel, module,
and Docker payload licenses still apply as recorded in
`guest/x86_64/SOURCES-AND-LICENSES.md`.
