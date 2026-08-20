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
