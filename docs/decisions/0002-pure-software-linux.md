# ADR 0002: Stock mobile devices use a pure software Linux machine

Status: accepted

## Context

The product must run unmodified Linux programs that depend on namespaces,
cgroups, systemd, device nodes, kernel modules, privileged containers,
Docker-in-Docker, network namespaces, and syscalls not implemented by the
portable command layer.

Those features require a Linux kernel. Calling Swift, Objective-C, Java,
Kotlin, or ArkTS implementations can cover selected commands, but it cannot
turn the mobile host kernel into Linux or provide arbitrary Guest ELF
compatibility.

Stock iOS applications do not have a public hardware-virtualization API.
Ordinary Android and OpenHarmony applications also cannot rely on KVM or an
OEM virtualization service being available. A backend that requires such an
API therefore cannot be the common baseline.

## Decision

The common Full VM backend is a pure software full-system emulator:

- the CPU interpreter, MMU, interrupt controller, timer, and virtual devices
  run inside the application process;
- a real, pinned Linux kernel provides all Linux syscall and isolation
  semantics;
- no KVM, AVF, Hypervisor.framework, private entitlement, root access, or JIT
  permission is required;
- guest privilege never grants privilege over the mobile host;
- platform UI and networking remain native, with guest traffic crossing an
  explicit user-mode NAT and port-forwarding boundary.

The first executable architecture is x86-64 and consumes `linux/amd64` OCI
manifests. This is the compatibility target explicitly exposed by the product;
a 32-bit x86 emulator does not satisfy it.

The image, lifecycle, policy, guest protocol, and mobile integration layers
remain Rust. The CPU backend is selected through the Rust `VmEngine` boundary.
The first production backend is a no-JIT x86-64 full-system interpreter. The
mobile candidate is the UTM SE QEMU TCTI (threaded-code-interpreter) build,
integrated through a small Rust-owned C ABI. There is no mature pure-Rust
x86-64 full-system engine that boots a modern 64-bit Linux kernel today. The
integration must pin the reviewed SE/no-JIT source and configuration, reject
TCG JIT and hardware-virtualization accelerators, and keep its source, GPL,
and reproducible-build obligations explicit. A userspace ELF emulator or a
32-bit-only PC emulator cannot be substituted.

A future pure-Rust x86-64 interpreter can replace that provider behind the same
engine, device, guest protocol, and OCI lifecycle interfaces after it passes
the identical evidence gates.

## Required evidence

Each milestone must produce observable evidence; a host command, mock channel,
or synthetic terminal response does not count.

1. The interpreter passes deterministic x86-64 ISA, long-mode, privilege,
   exception, interrupt, and four-level paging tests.
2. A checksum-pinned kernel and initramfs reach an `/init` UART marker.
3. The real guest agent completes the versioned handshake and reports evidence
   tied to the running kernel and session.
4. A verified `linux/amd64` OCI graph is imported with ordered layer,
   whiteout, metadata, digest, and diff-ID validation.
5. The guest OCI runtime starts the image and a PTY reaches its real
   `/bin/sh`.
6. The same backend boots in the foreground on iOS, Android, and OpenHarmony.

The verified VM capability profile remains unavailable until the guest
handshake and kernel contract pass. Early UART boot harnesses are diagnostics,
not a runnable Full VM candidate.

## Consequences

- The requested Linux semantics come from a real guest kernel rather than a
  growing, incomplete syscall translation layer.
- The baseline is portable across stock mobile devices and fails closed when
  a kernel or guest capability is missing.
- Startup, CPU throughput, memory use, heat, and battery consumption will be
  materially worse than hardware virtualization.
- iOS may suspend the VM when the app leaves the foreground; the design cannot
  promise an always-running background daemon.
- The AMD64 interpreter will be much slower and less energy-efficient than a
  native ARM64 guest. Registry platform selection must be explicit and the UI
  must show `linux/amd64`; selecting a host architecture is not acceptable.
- Guest kernel and distribution artifacts have their own source and license
  obligations and must be shipped with checksums, build metadata, and notices.
- Hardware-accelerated Android/OEM backends remain optional optimizations and
  must pass the same evidence gates before selection.
