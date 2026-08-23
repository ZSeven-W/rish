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
Linux guest in well under a minute and drops you into an **interactive shell** —
type a command, it executes in the guest.

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

- **CPU**: 64-bit long mode, the general-purpose instruction set, SSE/SSE2, and
  the SSE3/SSSE3/SSE4.1 subset exercised by Bun 1.4.0; complete post-SSE2
  extension sets are not advertised through CPUID. The **x87 FPU** provides an
  80-bit extended stack plus `FXSAVE`/`FXRSTOR` context save; string ops are
  interpreted as well.
- **Memory**: 4-level paging with page-boundary-correct operand access, an MTRR/MCE
  MSR surface, a generation-invalidated translation cache
- **Platform**: IDT/GDT, `syscall`/`sysret`, 8259 PIC, 8254 PIT, local APIC + IOAPIC,
  CMOS/RTC, ACPI PM, dual 16550 UARTs
- **Boot**: unpacks and runs a real pinned Alpine `bzImage` from `startup_64`,
  correct `boot_params` zero-page layout (cmdline / E820 / ramdisk)

Speed is tuned with `lto=fat`, `codegen-units=1`, host-scoped
`target-cpu=native`, and a mapped decode fast path. A cached instruction skips
redundant fetch translation only while its TLB invalidation epoch, execution
permission context, exact linear address, and physical code-page generation all
remain unchanged; `invlpg`, CR3/control changes, and code writes still force the
validated slow path. Cache hits return only decoded semantics; raw instruction
bytes are re-read for tracing or fatal diagnostics instead of crossing every
hot return path. LAPIC-to-CPU delivery stays in the owning interpreter thread,
avoiding cross-thread synchronization at every interrupt-recognition boundary.

## Container compatibility and startup benchmark

The pinned Docker diagnostic guest passed real compile/execute checks for Java,
Python, Go, Rust, Node.js, and Bun. The table records the exact cached
`linux/amd64` image resolved on 2026-08-23, so the result can be reproduced
without relying on a floating tag:

| Runtime | Resolved image digest | Executed compatibility check |
|---|---|---|
| Java 21.0.12 | `eclipse-temurin@sha256:6ea5548706b60ac0a602eaf48af74792cbab012d90e811ca8db6184b16b5c3d6` | `javac` + `java` passed on `amd64` |
| Python 3.13.15 | `python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d` | CPython + pip + JSON/hashlib/zlib/SQLite passed on `x86_64 linux` |
| Go 1.25.14 | `golang@sha256:1ae0735f00daffa3aaf1363a5184c0d2dc55c78e3db4ec70241cdac97bf84b59` | `go run` passed on `linux/amd64` |
| Rust 1.98.0 | `rust@sha256:a10e64dd139b7387337c7fbe8aca31b959b57b2fd4c8ae20a02cf1d6ea424dce` | `rustc` output executed successfully |
| Node.js 22.23.2 | `node@sha256:c610fcdfb1d5b4740dd70c284ed3cb16bb857e0f7166196e36a5501df7a3aa32` | Node + npm passed on `x64 linux` |
| Bun 1.4.0 | `oven/bun@sha256:07235578f79ef8c6f97d94aee7938e76f5cdba5f21ae5dbfdd3d3d38058437eb` | `bun --version` + JavaScript passed with the QEMU Nehalem oracle |

The official Bun 1.4.0 x64 Alpine baseline executable was also tested directly
on the pure-software rish CPU. The 74,507,808-byte executable has SHA-256
`805ecd8b91244de1c14d8d7e24841add8cb15c4eefffd17ce3d93cb87b3162ed`;
`bun --version` returned `1.4.0`, and `bun -e` returned
`RISH_BUN_OK x64 linux 42`. This is evidence for that tested Bun binary and
execution path, not a claim that rish implements or advertises every SSE4.2
instruction.

### Cached-image timing

The startup benchmark was host-timed on QEMU 11.1.0 TCG using a Nehalem CPU
profile with `thread=single`, one vCPU, 4 GiB RAM, Linux 6.18.35, Docker 29.7.2,
overlay2, and cgroup v2 with the `cgroupfs` driver. All six exact image digests
were cached and no containers existed before the run. Each result below is
P50 / P95 in milliseconds from ten measured samples after two warmups, with an
unmeasured 100 ms settle interval between samples:

| Runtime | Container lifecycle | Fresh minimal command | Same-container warm exec | P50 reduction |
|---|---:|---:|---:|---:|
| Java | 1006.5 / 1082.2 ms | 1360.2 / 1493.9 ms | 735.8 / 783.3 ms | 45.9% |
| Python | 990.8 / 1053.0 ms | 1312.1 / 1361.8 ms | 553.8 / 606.1 ms | 57.8% |
| Go | 987.5 / 1040.9 ms | 1013.4 / 1059.1 ms | 396.4 / 406.7 ms | 60.9% |
| Rust | 981.9 / 1064.1 ms | 1269.1 / 1338.3 ms | 640.6 / 672.7 ms | 49.5% |
| Node.js | 978.3 / 1017.8 ms | 1279.8 / 1332.0 ms | 685.1 / 736.9 ms | 46.5% |
| Bun | 977.0 / 1032.3 ms | 995.3 / 1057.2 ms | 379.7 / 472.7 ms | 61.9% |

“Container lifecycle” overrides the image entrypoint with `/bin/true` and
includes create, start, exit, and removal. “Fresh minimal command” runs one
minimal runtime command in a new container. “Warm exec” runs the same command
in one already-running container. Warm reuse is therefore valid only for the
same verified image, tenant, policy, and security context; mutable state must be
reset or retained by an explicit contract.

Two guest changes made the matrix stable and removed avoidable startup pressure:

- The pinned `docker-init`/tini process now remains PID 1, reaps descendants,
  and forwards signals to `rish-guest-agent`. A stress run that had accumulated
  568 orphaned `containerd-shim` zombies ended with zero zombies after this
  change, with the guest task count near 59.
- `/var/lib/docker` uses a `noatime` tmpfs capped at 75% of guest RAM. With the
  six-image matrix, this changed the tested 4 GiB guest from an approximately
  1.9 GiB filesystem at 94% use (about 122 MiB free) to approximately 2.9 GiB
  at 63% use (about 1.1 GiB free). In an exploratory live Bun A/B, lifecycle
  P50 fell from 1809.5 ms to 1355.0 ms (25.1%) and warm-exec P50 from 540.7 ms
  to 458.3 ms (15.2%); the clean-run table above is the authoritative matrix.

A five-sample Python `/bin/true` boundary experiment found no worthwhile safe
container-flag shortcut:

| Configuration | P50 / P95 | Result |
|---|---:|---|
| Baseline before controls | 980.2 / 1166.7 ms | Reference |
| `--log-driver none` | 975.6 / 1007.2 ms | No material P50 gain; persistent logs are lost |
| `--cgroupns host` | 978.7 / 1022.9 ms | No material P50 gain; namespace isolation is weaker |
| `--security-opt seccomp=unconfined` | 891.8 / 905.5 ms | About 8.7% faster, rejected because isolation is weaker |
| `--privileged` | 1048.8 / 1105.2 ms | Slower and substantially weaker isolation |
| Baseline after controls | 972.7 / 990.4 ms | Control for run-to-run drift |

Reproduce the compatibility checks and benchmark through the diagnostic
loopback-only Docker API with:

```bash
DOCKER_HOST=tcp://127.0.0.1:12375 \
RISH_REMOVE_TEST_IMAGES=1 \
sh guest/x86_64/test-language-images.sh

DOCKER_HOST=tcp://127.0.0.1:12375 \
python3 guest/x86_64/benchmark-language-images.py \
  --warm-exec --runs 10 --warmups 2 --settle-ms 100 \
  --output guest/x86_64/out/language-startup-benchmark.json
```

The reproducible Docker initramfs produced for this run is
`guest/x86_64/out/rish-alpine-3.24.1-x86_64-docker-initramfs.cpio`:
285,680,128 bytes, SHA-256
`0b762ca4d0837b609d4a3f98c820a8a96bf37f210b55895718e7ed347a1c995b`.
The unauthenticated diagnostic Docker API is disabled by default; the guest
documentation explains how to opt into a loopback-forwarded test setup.

These timing numbers characterize the QEMU development oracle used to exercise
Docker deterministically. QEMU is not a production runtime dependency, and the
numbers are neither pure-rish interpreter throughput nor native-kernel support.
See the [x86-64 guest documentation](guest/x86_64/README.md) for the complete
artifact, network, proxy, and Bun CPU-contract details.

## Repository layout

```text
crates/
  rish-softvm-core/     no-JIT x86-64 full-system interpreter (CPU / paging / devices)
  rish-softvm-x86_64/   interpreter control plane, engine gate, bounded quanta
  rish-vm/              evidence-gated VM boot, device config, guest kernel contract
  rish-guest-agent/     fail-closed bootstrap agent behind the guest PID 1 reaper
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
