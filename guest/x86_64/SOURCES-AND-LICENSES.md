# x86_64 guest source and license correspondence

This directory commits recipes, hashes, and a tiny `/init`; it does **not**
commit third-party kernel, root filesystem, ISO, or emulator binaries.

## Alpine Linux 3.24.1 guest

`assets.lock.tsv` pins byte size, SHA-256, and HTTPS release URL for:

- `vmlinuz-virt`, Linux 6.18.35-0-virt for x86_64;
- the exact published kernel configuration;
- Alpine 3.24.1 x86_64 minirootfs; and
- the optional Alpine virt ISO.

The release directory is:

<https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/>

The kernel and config are from its immutable versioned netboot directory:

<https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/netboot-3.24.1/>

The kernel package is `linux-virt` 6.18.35-r0, built from Alpine's `linux-lts`
packaging at aports commit:

`954487c7d11ef901fdf7350f9fa8638e7a45df4e`

Exact packaging recipe:

<https://gitlab.alpinelinux.org/alpine/aports/-/raw/954487c7d11ef901fdf7350f9fa8638e7a45df4e/main/linux-lts/APKBUILD>

That recipe is independently pinned here for audit as 13,896 bytes with
SHA-256:

`b56b1cc91a16f54f02fd07568e1fba6200c3c62683b636efc55710165ba37e22`

It identifies Linux 6.18 plus the 6.18.35 stable patch and the Alpine patches
and configuration used by `linux-virt`. Linux is
`GPL-2.0-only WITH Linux-syscall-note`; distributing the kernel requires the
corresponding license notices and source compliance.

The minirootfs is a collection of Alpine packages, not one uniformly licensed
work. Its package records and license fields are retained inside
`/lib/apk/db/installed` in the generated initramfs. A distributor must inventory
those packages, preserve their notices, and satisfy each package's license.
The OCI Distribution Specification does not replace payload licenses.

`rootfs-overlay/init` and the deterministic packer are part of rish and follow
the repository's own license.

## Emulator source provenance

The planned software-emulator lineage is UTM's QEMU 10.0.2-utm source at commit:

`37ba092d59aff24900dfd0d5e01d4ed68441ba07`

Source tarball:

<https://codeload.github.com/utmapp/qemu/tar.gz/37ba092d59aff24900dfd0d5e01d4ed68441ba07>

- byte size: `136751908`
- SHA-256: `f1d7357547a71ae3339a115d5c8f2b72e3b0089531d67c2aca43326d320ac6ca`

This pin documents emulator provenance; it is not downloaded by the guest asset
script and it does not mean QEMU is linked into this repository. QEMU is
primarily GPL-2.0 and includes components under other licenses. Any eventual
distribution must audit the exact configured source set, retain notices, and
meet the corresponding source obligations.

## Offline apk repository additions

`build-container-initramfs.sh` stages an offline Alpine v3.24 `main/x86_64`
repository from three more pinned inputs in `assets.lock.tsv`:

| Asset | Package | License |
|---|---|---|
| `APKINDEX.tar.gz` | repository index snapshot, signed with key `alpine-devel@lists.alpinelinux.org-6165ee59` (the `.SIGN.RSA` travels inside the archive) | index metadata; individual entries carry their own `L:` fields |
| `musl-1.2.6-r2.apk` | musl libc | MIT |
| `tree-2.3.2-r0.apk` | tree | GPL-2.0-or-later |

Source directory (a moving snapshot; the exact bytes are hash-pinned in the
lock file):

<https://dl-cdn.alpinelinux.org/alpine/v3.24/main/x86_64/>

The apk runtime itself is **not** re-downloaded: the `sbin/apk` binary,
`libapk.so.3.0.0`, `libssl.so.3`, `libcrypto.so.3`, `libz.so.1`, and the
signing keys are extracted from the pinned minirootfs and remain covered by its
`/lib/apk/db/installed` records (apk-tools GPL-2.0-only, openssl Apache-2.0,
zlib Zlib). Distributing the generated initramfs requires the license notices
of these packages; tree is GPL-2.0-or-later, so corresponding source must be
offered. The packages stay inside the emulated guest and grant no mobile-host
privilege.

## Docker diagnostic guest additions

`build-docker-initramfs.sh` adds two more pinned inputs to the same
`assets.lock.tsv`:

- `modloop-virt`, the Alpine kernel module squashfs for 6.18.35-0-virt
  (module binaries are Linux kernel code under GPL-2.0-only); and
- the static Docker 29.7.2 toolchain tarball from
  <https://download.docker.com/linux/static/stable/x86_64/>, whose
  components are Apache-2.0 (Moby, containerd, runc, ctr) with
  BSD-3-Clause parts (docker-init/tini, docker-proxy).

Distributing the generated docker initramfs requires the Alpine and Moby
license notices and, for the kernel modules, corresponding source. The
module set stays inside the emulated guest and grants no mobile-host
privilege.

## Capability and redistribution boundary

The generated image is a diagnostic initramfs. It contains no Alpine kernel
modules, systemd, Docker, containerd, runc/youki, rish guest agent, writable
root disk, or network configuration. Alpine's minirootfs uses BusyBox/OpenRC,
not systemd.

A later container guest must have its own pinned source and binary manifest for
all of the following:

- the exact kernel modules or rebuilt-in drivers for virtio block/network,
  ext4, overlayfs, nftables, veth, bridge, TUN, and packet sockets;
- a writable root filesystem and deterministic first-boot provisioning;
- systemd, if systemd is a product requirement;
- a version-matched rish guest agent and OCI runtime;
- virtio device models, user-mode NAT, and explicit host port-forward policy;
- an SBOM, package notices, and corresponding-source material.

Guest root does not grant iOS, Android, or OpenHarmony host privilege. Kernel
modules and `/dev` access remain confined to the emulated Linux guest.