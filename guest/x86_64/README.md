# Reproducible x86_64 Linux diagnostic guest

This directory builds a small, real x86_64 Linux guest for the pure-software
rish engine. It does not use iOS Hypervisor or Virtualization APIs. The mobile
app supplies an ordinary software CPU/device emulator; the emulated machine
then boots the pinned Alpine Linux kernel and initramfs.

Current status is deliberately narrow: **diagnostic serial boot**. A successful
boot mounts devtmpfs, devpts, proc, sysfs, tmpfs, and cgroup v2, prints
`RISH_X86_64_BOOT_OK`, then enters an interactive BusyBox shell on `ttyS0`.
It is not yet the full OCI/systemd container guest.

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
  protocol on ttyS1.

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

The harness spawns QEMU with two serial ports (console on stdio, the guest
protocol on a control socket), negotiates the session, collects live kernel
evidence, and streams the requested command. It is a diagnostic tool: the
verified Full VM capability profile still requires the TCTI provider gate.

## Root-disk overlay image

`build-root-disk.sh` turns a runtime overlay directory (the app's
`Application Support/rish-guest-overlay/`, e.g. `etc/apk/repositories`)
into a deterministic 4 MiB FAT16 image that the boot request consumes as
`root_disk_path`. See `docs/guest-root-disk.md` for the format contract,
the root_disk_path support matrix, and the honest list of unmet
prerequisites.

```sh
./build-root-disk.sh overlay-dir out/root-disk.img
./test-root-disk.sh
```

## Offline apk repository in the container initramfs

`build-container-initramfs.sh` now also carries the real Alpine `apk` (its
binary and full runtime closure, extracted from the same pinned minirootfs)
plus a small **offline** repository so the guest can install packages with no
network:

- `assets.lock.tsv` pins the v3.24 `main/x86_64` `APKINDEX.tar.gz` (with
  its embedded `.SIGN.RSA` index signature) and two packages:
  `musl-1.2.6-r2.apk`, `tree-2.3.2-r0.apk`;
- the repo is staged at `/opt/rish-apk-repo/main` and
  `container-overlay/etc/apk/repositories` points apk at
  `file:///opt/rish-apk-repo/main`;
- signature verification uses the minirootfs's own `/etc/apk/keys`, so no
  `--allow-untrusted` is needed;
- the apk database ships empty on purpose: the initramfs payload sits outside
  apk's bookkeeping, and `apk add` resolves dependencies from the local
  repository (installing musl as a dependency of tree in the recorded proof).

This proves configuration-to-`apk` consumption inside the guest. It does
**not** prove remote mirrors (no guest network emulation), disk-injected
configuration (no virtio-blk), or persistence (initramfs rootfs is RAM and
every boot starts fresh). See `docs/guest-apk-offline.md` for the dynamic
verification record, verbatim guest output, and the app-side prerequisites.

## What is not claimed yet

The Alpine `virt` kernel config builds several container/storage/network
features as modules, while this diagnostic initramfs intentionally ships no
module payload. In particular, virtio block/network, ext4, overlayfs, nftables,
veth, bridge, TUN, and packet sockets cannot be claimed by this artifact.

The next container-guest asset must pin and verify:

1. a matching module set or a rebuilt kernel with required drivers built in;
2. a writable root disk and virtio block device;
3. systemd only if it remains a product requirement;
4. the rish guest agent and a real OCI runtime such as youki/runc;
5. virtio-net plus user-mode NAT and explicit port-forward policy.

Only after that guest boots and passes integration tests may the UI offer
“start container” or “enter container”. The current shell is the diagnostic
guest shell, not a container shell.

Source correspondence, redistribution obligations, and the pinned UTM QEMU
source lineage are recorded in `SOURCES-AND-LICENSES.md`.