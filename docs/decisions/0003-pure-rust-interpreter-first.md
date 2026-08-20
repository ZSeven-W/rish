# ADR 0003: The pure-Rust interpreter is the primary mobile backend

Status: accepted (user decision, 2026-08-20)

## Context

ADR-0002 chose the UTM SE QEMU TCTI build as the first mobile candidate and
described a pure-Rust x86-64 interpreter only as a future replacement.

The user has now decided that iOS and Android must run Docker through the
project's own software emulation: a pure-Rust, no-JIT x86_64 full-system
interpreter built in this repository. The UTM/QEMU route is not wanted for
the product.

## Decision

- The primary Full VM backend is the in-repository pure-Rust interpreter
  (crates/rish-softvm-core): CPU, MMU, interrupt controller, timers, and
  virtual devices implemented in Rust, no JIT, no executable-memory
  translation, no QEMU, no UTM, no hypervisor.
- It targets the same pinned x86_64 Alpine guest, the same versioned guest
  protocol, and the same evidence gates as ADR-0002.
- The QEMU TCTI adapter (crates/rish-softvm-x86_64) remains in the tree as a
  documented, gated alternative provider boundary, but it is not a product
  path.
- macOS stays a development machine only; the hosts that matter are iOS and
  Android (simulators/emulators for CI, devices for release).

## Required evidence

The ADR-0002 evidence list still applies, with "the interpreter" meaning the
pure-Rust one:

1. deterministic x86-64 ISA, long-mode, privilege, exception, interrupt, and
   four/five-level paging tests;
2. the checksum-pinned kernel and initramfs reach the /init UART marker;
3. the real guest agent completes the versioned handshake with kernel-tied
   evidence;
4. a verified linux/amd64 OCI graph is imported with full validation;
5. the guest OCI runtime starts the image and a PTY reaches /bin/sh;
6. the same backend runs docker (dockerd in the guest) on iOS and Android.

## Consequences

- Boot time and throughput are bounded by a from-scratch interpreter; the
  instruction set must grow until the pinned kernel boots (SSE2, LAPIC,
  ACPI tables, bzImage loading, and more).
- No third-party emulator license obligations for the engine itself; guest
  kernel, module, and Docker payload licenses still apply as recorded in
  guest/x86_64/SOURCES-AND-LICENSES.md.
- The QEMU/UTM documentation stays for reference; the roadmap reorders P3 to
  make the pure-Rust interpreter the primary path.
