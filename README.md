<div align="center">

# rish

### Real Docker. Real x86-64 Linux. On your phone. In pure Rust.

**`rish` boots a genuine x86-64 Linux kernel inside a from-scratch, no-JIT Rust
interpreter — and runs real Docker containers on it, on iOS and Android.**
No QEMU. No KVM. No hypervisor entitlement. Just Rust.

[![language](https://img.shields.io/badge/built%20with-Rust-orange)](https://www.rust-lang.org)
[![license](https://img.shields.io/badge/license-MIT-blue)](#license)
[![platforms](https://img.shields.io/badge/targets-iOS%20·%20Android%20·%20HarmonyOS-black)](#three-platform-demos)
[![status](https://img.shields.io/badge/docker%20run-working-brightgreen)](#milestone)

</div>

---

## Milestone

> **`docker run hello-world` → `HELLO_FROM_DOCKER_CONTAINER`, exit 0 — executed by
> a pure-Rust x86-64 interpreter.** The full stack (dockerd · containerd ·
> containerd-shim · runc · docker CLI) runs unmodified on a real pinned Alpine
> kernel, booted from `startup_64` to userspace entirely in software.

And it runs **on the iOS Simulator**: an `iPhone 17 Pro` boots the same x86-64
Linux guest in ~40s and drops you into an **interactive shell** — type a command,
it executes in the guest.

```
root@rish-container
$ uname -a
Linux rish-container 6.18.35-0-virt #1-Alpine SMP PREEMPT_DYNAMIC x86_64 Linux
$ whoami
root
$ ps | head -3
PID   USER     TIME  COMMAND
    1 root      0:14 /usr/bin/rish-guest-agent
    2 root      0:00 [kthreadd]
$ cd /etc && cat hostname
rish-container
```

*A real Intel-syntax x86-64 instruction stream — SSE2, x87 FPU, paging, IDT/GDT,
APIC, PIT, 16550 UARTs — interpreted instruction by instruction, in safe Rust.*

---

## Why this is hard (and why it matters)

iOS and Android don't give apps a hypervisor. You cannot run KVM, you cannot get
the virtualization entitlement, and you cannot ship QEMU's JIT. The conventional
wisdom is that "real Linux on a stock phone" is impossible.

`rish` takes the other road: **a full-system x86-64 emulator written from scratch
in Rust, with no JIT and no unsafe execution of guest code.** Guest instructions
are decoded and interpreted; guest privilege, kernel modules, and namespaces
live entirely inside the emulated Linux — they never touch the host sandbox.

The interpreter is real enough to boot a stock distribution kernel and run the
entire container toolchain on top of it.

## How it works

`rish` is **native-offload first, software-VM fallback**:

```text
          Guest command / OCI image
                     │
                     ▼
          Rust capability negotiation
          ┌──────────┼───────────────┐
          ▼          ▼               ▼
      Portable    Full VM        Native Linux
      offload     backend        backend
          │          │               │
    Swift / Kotlin   pure-Rust    probed host
    / ArkTS handler  x86-64 Linux  kernel features
```

1. **Portable offload** — known commands (`grep`, `sha256sum`, `echo`, …) are
   intercepted by Rust and answered natively in the app sandbox. No emulation.
2. **Full VM** — anything needing a real kernel (`dockerd`, `runc`, `unshare`,
   kernel modules, namespaces) runs inside the **pure-Rust x86-64 Linux guest**.
3. **Native Linux** — on probed rooted/OEM hosts only, features are used
   directly. Availability is *probed*, never guessed from "device is rooted".

It **fails closed**: a capability that isn't really there is refused, never faked.
Semantic emulation is never reported as real kernel isolation.

## The pure-Rust interpreter

`rish-softvm-core` is a no-JIT, no-`unsafe`-guest-exec x86-64 full-system
interpreter. What's implemented and tested:

- **CPU**: 64-bit long mode, the general-purpose instruction set, SSE/SSE2, the
  **x87 FPU** (80-bit extended stack, `FXSAVE`/`FXRSTOR` context save), string ops
- **Memory**: 4-level paging with page-boundary-correct operand access, an MTRR/MCE
  MSR surface, a generation-invalidated translation cache
- **Platform**: IDT/GDT, `syscall`/`sysret`, 8259 PIC, 8254 PIT, local APIC + IOAPIC,
  CMOS/RTC, ACPI PM, dual 16550 UARTs
- **Boot**: unpacks and runs a real pinned Alpine `bzImage` from `startup_64`,
  correct `boot_params` zero-page layout (cmdline / E820 / ramdisk)

Speed is tuned with `lto=fat`, `codegen-units=1`, and host-scoped
`target-cpu=native` (~11% over baseline) — fast enough that a phone boots the
guest in well under a minute.

## Repository layout

```text
crates/
  rish-softvm-core/     no-JIT x86-64 full-system interpreter (CPU / paging / devices)
  rish-softvm-x86_64/   interpreter control plane, engine gate, bounded quanta
  rish-vm/              evidence-gated VM boot, device config, guest kernel contract
  rish-guest-agent/     fail-closed bootstrap agent (PID 1 inside the guest)
  rish-guest-protocol/  host↔guest handshake, exec, OCI, port and checkpoint RPC
  rish-ffi/             stable JSON C ABI callable from Swift / JNI / N-API
  rish-oci · rish-pull · rish-registry · rish-layer · rish-snapshot · rish-content
                        the OCI data plane: pull, verify, unpack, snapshot, CAS
  rish-applets · rish-runtime · rish-core · rish-cli
                        native command layer, backend routing, host-call protocol
platform/
  ios/ · android/ · harmony/   Swift / Kotlin-JNI / ArkTS-NAPI bridges + rish.h
examples/
  ios-vm/              interactive x86-64 Linux terminal on the iOS Simulator
  ios/ · android/ · harmony/   portable-offload demos on the real platform bridges
guest/x86_64/
  build-container-initramfs.sh   reproducible interactive-container initramfs
  build-docker-initramfs.sh      full docker-in-guest initramfs
```

## Quick start

```bash
# Type-check and test the workspace
cargo test --workspace

# Command planning (portable offload)
cargo run -p rish-cli -- plan ios grep needle      # → portable_applet
cargo run -p rish-cli -- plan ios dockerd          # → fails closed (no guest kernel)
```

### Interactive x86-64 Linux on the iOS Simulator

```bash
# Boots the guest and hands you a live shell in the simulator.
# Needs: Xcode + an iPhone simulator, and the musl-cross toolchain
#   brew install FiloSottile/musl-cross/musl-cross
guest/x86_64/build-container-initramfs.sh          # → out/rish-container.cpio
examples/ios-vm/run-vm-simulator.sh
```

The demo boots the guest once, then runs each command you type over the open
serial control channel — `whoami`, `ps`, `free -m`, `cd /etc && ls -la`, and a
`unshare + chroot` container demo all run inside the emulated Linux.

## Platform promises

| Capability | App offload | Full Linux VM | Native Linux |
|---|:---:|:---:|:---:|
| Known commands, native impl | ✅ | ✅ | ✅ |
| OCI metadata + control plane | ✅ | ✅ | ✅ |
| namespaces / cgroups | semantic | real, in guest | real, after probe |
| systemd | API-compatible | real, in guest | real, after probe |
| privileged / Docker-in-Docker | ✗ | real, in guest | controlled devices |
| kernel modules & `/dev` | device proxy | real, in guest | controlled devices |
| unknown complex ELF / syscalls | ✗ | Linux kernel | Linux kernel |

Everything in the **Full Linux VM** column is real inside the *guest* Linux — it
never breaks out of the iOS, Android, or HarmonyOS host sandbox.

## Documentation

[Architecture](docs/architecture.md) ·
[Platform matrix](docs/platform-matrix.md) ·
[OCI data plane](docs/oci-pipeline.md) ·
[Command compatibility](docs/command-compatibility.md) ·
[ADR-0001: offload-first](docs/decisions/0001-offload-first.md) ·
[ADR-0002: pure-software Linux](docs/decisions/0002-pure-software-linux.md) ·
[Roadmap](docs/roadmap.md)

## License

`rish`'s own Rust code is MIT-licensed. The optional QEMU TCTI provider, the
Linux kernel, and guest distribution artifacts keep their own licenses; anything
that links or ships QEMU must satisfy GPLv2 source and redistribution terms
separately.
