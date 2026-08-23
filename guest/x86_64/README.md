# Reproducible x86_64 Linux diagnostic guest

This directory builds a small, real x86_64 Linux guest for the pure-software
rish engine. It does not use iOS Hypervisor or Virtualization APIs. The mobile
app supplies an ordinary software CPU/device emulator; the emulated machine
then boots the pinned Alpine Linux kernel and initramfs.

There are two pinned artifacts. The minimal artifact provides a diagnostic
serial boot and BusyBox shell. The larger Docker diagnostic artifact adds the
matching kernel modules, Docker/containerd/runc, and the versioned guest agent.
Neither artifact is a verified Full VM product backend.

## Build

From this directory:

```sh
./fetch-assets.sh
./build-initramfs.sh
```

The fetcher accepts only the fixed HTTPS URLs in `assets.lock.tsv`. It verifies
both exact byte size and SHA-256 before atomically publishing a file. A
pre-existing corrupt file is rejected rather than silently replaced.

To prove a cache is complete without permitting network access:

```sh
./fetch-assets.sh --offline
./build-initramfs.sh
```

`build-initramfs.sh` always invokes the offline verifier. It extracts the
verified Alpine minirootfs into a temporary directory, adds the checked-in
`/init`, compiles the dependency-free deterministic `newc` packer, and rejects
the output unless it matches `derived.lock.tsv`.

Generated files live under ignored `out/`; no third-party binaries are committed.

| Output | Bytes | SHA-256 |
|---|---:|---|
| `out/downloads/vmlinuz-virt-6.18.35` | 12,575,744 | `1e6bf9027720c75c3ed0d79171f21b5791ee40ca9795d07c7c6e04dc5ea2ae90` |
| `out/rish-alpine-3.24.1-x86_64-initramfs.cpio` | 8,484,864 | `a92bf96c76dee0850db6662843d39bc52fe97762b6074f28b05ed6e01bf8b488` |
| `out/rish-alpine-3.24.1-x86_64-docker-initramfs.cpio` | 285,680,128 | `023593f5af064054998e14d290eaea0072efcbc9d6fc68e58fab55876d4047e1` |

Use `./fetch-assets.sh --all` only when the optional Alpine virt ISO is also
needed for provenance or later disk-image work.

## Boot contract

`boot-manifest.json` is the machine-readable contract for the emulator:

- architecture: x86_64 (`linux/amd64` for OCI selection);
- Linux x86 bzImage boot protocol;
- one CPU and at least 256 MiB RAM;
- a 16550A serial port at I/O `0x3f8`, IRQ 4, 115200 8N1;
- kernel and initramfs placement according to the x86 boot protocol;
- serial success marker `RISH_X86_64_BOOT_OK`.

The exact command line is:

```text
console=ttyS0,115200n8 rdinit=/init panic=-1 oops=panic nokaslr cgroup_no_v1=all
```

For comparison on a development machine with QEMU installed, the same assets
can be booted with software translation only:

```sh
qemu-system-x86_64 \
  -accel tcg,thread=single \
  -machine pc \
  -cpu qemu64 \
  -m 256 \
  -smp 1 \
  -kernel out/downloads/vmlinuz-virt-6.18.35 \
  -initrd out/rish-alpine-3.24.1-x86_64-initramfs.cpio \
  -append 'console=ttyS0,115200n8 rdinit=/init panic=-1 oops=panic nokaslr cgroup_no_v1=all' \
  -nographic \
  -no-reboot
```

This QEMU command is a reference oracle, not a runtime dependency of the mobile
implementation.

## What is real now

The pinned config is checked by `verify-kernel-config.sh`. The kernel has these
features built in:

- x86_64, initramfs, devtmpfs, proc, sysfs, tmpfs, and 8250 serial console;
- user, PID, mount, IPC, UTS, and network namespaces;
- cgroups, memory controller, PID controller, cgroup BPF;
- seccomp and seccomp filters.

Those are actual Linux kernel facilities inside the emulated guest. They are
not Swift/Java/ArkTS lookalikes and they do not imply mobile-host privilege.

## Docker diagnostic guest

`build-docker-initramfs.sh` extends the diagnostic rootfs into a guest that
carries a real container stack:

- the full 6.18.35-0-virt kernel module tree (boot modules and metadata from
  the pinned netboot initramfs, remaining modules from the pinned modloop
  squashfs);
- the static Docker 29.7.2 toolchain (dockerd, containerd, runc, ctr, docker
  CLI, docker-init, docker-proxy);
- the statically linked rish-guest-agent, started as PID 1 with its framed
  protocol polling the second 16550A UART directly at COM2 (`0x2f8`).

Build it from this directory:

```sh
./build-docker-initramfs.sh --update-lock
```

The first run records the derived digest in `derived.lock.tsv`; later runs
fail closed unless the bytes match. dockerd runs with bridge networking and
iptables disabled and overlay2 on tmpfs, so containers run on the guest
host network (`docker run --network host`). This is real dockerd and runc
inside a real Linux kernel, not a portable lookalike.

Boot the oracle on a development machine:

```sh
cargo run -p rish-guest-boot -- \
  --manifest guest/x86_64/boot-manifest.json \
  --exec docker version
```

The harness spawns QEMU with two serial ports (an output-only console log and a
control socket), waits for the guest-agent ready marker, negotiates the session,
collects live kernel evidence, and streams the requested command. The agent
initializes COM2 deterministically, both directions use FIFO-sized writes, and
the harness coalesces serial socket fragments over one bounded advance window.
Execution output is split into 1 KiB protocol chunks so sustained output does
not exhaust the session step budget. It is a diagnostic tool: the verified Full
VM capability profile still requires the TCTI provider gate.

The Docker guest sets `DOCKER_RAMDISK=1`, which makes Moby request runc's
`NoPivotRoot` mode; Linux cannot pivot away from its special initramfs rootfs.
It also loads `virtio-rng` so Go-based Docker tooling cannot block waiting for
early-boot entropy.

### Language image compatibility oracle

`test-language-images.sh` pulls `linux/amd64` toolchain images, records their
resolved digest, then compiles or executes a real program in each container:

```sh
DOCKER_HOST=tcp://127.0.0.1:12375 \
RISH_REMOVE_TEST_IMAGES=1 \
sh guest/x86_64/test-language-images.sh
```

The unauthenticated Docker API is disabled by default. A QEMU-only oracle may
enable it with the kernel arguments `rish.diagnostic_network=dhcp` and
`rish.diagnostic_docker_tcp=1`, forwarding guest port 2375 to host loopback.
If the host requires a proxy, `rish.diagnostic_proxy=HOST:PORT` configures only
the diagnostic dockerd process; credentials must never be placed on the kernel
command line. The QEMU machine must provide `virtio-rng`; 4 GiB RAM is
recommended while testing compiler images.

Observed on 2026-08-23 with Docker 29.7.2, Linux 6.18.35, overlay2, and cgroup
v2:

| Runtime | Resolved image digest | Executed result |
|---|---|---|
| Java 21.0.12 | `eclipse-temurin@sha256:6ea5548706b60ac0a602eaf48af74792cbab012d90e811ca8db6184b16b5c3d6` | `javac` + `java` passed on `amd64` |
| Go 1.25.14 | `golang@sha256:1ae0735f00daffa3aaf1363a5184c0d2dc55c78e3db4ec70241cdac97bf84b59` | `go run` passed on `linux/amd64` |
| Rust 1.98.0 | `rust@sha256:a10e64dd139b7387337c7fbe8aca31b959b57b2fd4c8ae20a02cf1d6ea424dce` | `rustc` output executed successfully |
| Node.js 22.23.2 | `node@sha256:c610fcdfb1d5b4740dd70c284ed3cb16bb857e0f7166196e36a5501df7a3aa32` | Node + npm passed on `x64 linux` |
| Bun 1.4.0 | `oven/bun@sha256:07235578f79ef8c6f97d94aee7938e76f5cdba5f21ae5dbfdd3d3d38058437eb` | `bun --version` + JavaScript passed with the QEMU Nehalem oracle |

The official Bun x64 Alpine image uses its baseline binary, whose minimum CPU
is [documented by Bun as SSE4.2](https://github.com/oven-sh/bun/blob/main/docs/installation.mdx).
The exact official Bun 1.4.0
`bun-linux-x64-musl-baseline` executable (74,507,808 bytes,
SHA-256 `805ecd8b91244de1c14d8d7e24841add8cb15c4eefffd17ce3d93cb87b3162ed`)
was also placed in a minimal guest and executed by the pure-software rish CPU:
`bun --version` returned `1.4.0`, and `bun -e` returned
`RISH_BUN_OK x64 linux 42`.

This is a tested Bun 1.4.0 compatibility claim, not a generic SSE4.2 claim. The
interpreter implements the SSE3/SSSE3/SSE4.1 instructions reached by those Bun
paths, while CPUID deliberately continues to withhold the complete SSE4.1 and
SSE4.2 feature bits. The language-image oracle now judges Bun by the real
process result instead of treating a `/proc/cpuinfo` flag as proof; QEMU
`qemu64` still faults, while its Nehalem profile supplies the complete hardware
contract.

## What is not claimed yet

The minimal shell artifact still carries no full module payload; the Docker
variant does. Neither variant provides a persistent root disk, systemd,
production network policy, or a verified Full VM capability profile. QEMU TCG
results are development-oracle evidence, not native kernel support and not
proof that every exercised userspace instruction is implemented by the
pure-software rish CPU.

Source correspondence, redistribution obligations, and the pinned UTM QEMU
source lineage are recorded in `SOURCES-AND-LICENSES.md`.
