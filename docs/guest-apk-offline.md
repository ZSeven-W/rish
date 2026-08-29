# Offline apk consumption in the guest: what is and is not proven

Status: **verified for a build-time-baked offline repository only.** Nothing
here proves remote mirror access, block-device configuration injection, or
persistence.

## The question

The app-side plan wants to mount a persistent guest and feed it configuration
(`/etc/apk/repositories`, signing keys, package sets). Before any of that,
this work proves the weakest link inside the guest: that a real `apk` binary,
given a local repository, actually *consumes* its configuration and installs
packages end to end -- inside the pure-Rust interpreter, with no network.

## What was proven (dynamic)

- The container initramfs now carries the real Alpine `apk` plus its complete
  runtime closure (`libapk`, `libssl`, `libcrypto`, `libz`, musl loader),
  extracted from the pinned `alpine-minirootfs-3.24.1-x86_64.tar.gz`
  (`assets.lock.tsv`). apk is Alpine's own binary; nothing was reimplemented.
- An offline repository is baked into the image at
  `/opt/rish-apk-repo/main/x86_64/`: the pinned Alpine v3.24 `main` index
  (`APKINDEX.tar.gz`, carrying its `.SIGN.RSA` signature) plus two
  hash-locked packages: `musl-1.2.6-r2.apk` and `tree-2.3.2-r0.apk`.
- `/etc/apk/repositories` points at `file:///opt/rish-apk-repo/main`; the
  minirootfs provides `/etc/apk/arch` and the `/etc/apk/keys` public keys.
  Signature verification follows Alpine convention (trusted key + signed
  index), so the proof runs **without** `--allow-untrusted`.
- The guest boots in the pure-Rust interpreter and, in-guest, `apk add tree`
  resolves the package, verifies the index, installs **musl as a dependency**
  and tree itself, updates the database and world, and the installed
  `/usr/bin/tree` binary runs. Raw output is quoted below.
- Reproducibility: two consecutive builds produced byte-identical archives.

## What was NOT proven (fail-closed)

1. **Remote mirror URLs are unavailable.** The pure-Rust interpreter has no
   virtio-net and no user-mode NAT/DNS (the roadmap item is still unchecked).
   Only the `file://` repository works; an `https://dl-cdn...` mirror would
   fail. This proof says nothing about network-based `apk update`.
2. **Block-device configuration injection is unavailable.** The interpreter
   emulates no virtio-blk; `root_disk_path` is accepted and validated but the
   guest sees no disk (see `docs/guest-root-disk.md`). Configuration cannot
   reach the guest through a root disk today; it must be baked at build time,
   as this work does.
3. **Nothing persists.** The initramfs rootfs lives in RAM. Installed packages
   vanish at power-off; the `apk` database starts empty by design (the
   initramfs payload sits outside apk's bookkeeping). A persistent guest still
   needs block-device emulation plus the kernel modules listed in
   `docs/guest-root-disk.md`.
4. **Only a two-package snapshot is pinned.** The index is a frozen `main`
   snapshot: no community repo, no version drift follow-up, no `apk upgrade`
   path. Expanding the package set means pinning more assets the same way.
5. **No cross-version libc upgrade was exercised.** The pinned repo musl is
   1.2.6-r2, the same version the minirootfs ships, so the in-guest libc
   overwrite during `apk add` was byte-identical content.

## Build and boot record

Build (twice, same result) with the workspace-local toolchain workaround:

```sh
export CARGO_HOME="$PWD/.toolchain/cargo-home"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$PWD/.toolchain/bin/zig-musl-cc"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="--sysroot $PWD/.toolchain/sysroot -C link-self-contained=no"
guest/x86_64/build-container-initramfs.sh
shasum -a 256 guest/x86_64/out/rish-container.cpio
```

Build #1 and build #2 both produced:

- 9,835,008 bytes
- `152905238ade87b7e1cd495508ff1bb92807b4440cc0ed056030e4dfd72caea0`

Boot and command (the `vm_smoke` example of `rish_vm_run_docker_json`):

```sh
export RISH_KERNEL=guest/x86_64/out/downloads/vmlinuz-virt-6.18.35
export RISH_INITRD=guest/x86_64/out/rish-container.cpio
export RISH_BOOT_BUDGET=80000000000
export RISH_HANDSHAKE_BUDGET=50000000000
cargo run --release -p rish-ffi --example vm_smoke -- /bin/sh -c '<script below>'
```

## Raw guest output (verbatim)

Response of `vm_smoke`: `ok`: true, `exit_code`: 0,
`boot_units`: 1,197,000,000, `stderr`: empty. The `stdout` field, decoded:

```text
== /etc/apk/repositories ==
file:///opt/rish-apk-repo/main
== /etc/apk/arch ==
x86_64
== apk version ==
apk-tools 3.0.6-r0, compiled for x86_64.
== initial installed-db bytes ==
0 /lib/apk/db/installed
== apk add tree ==
(1/2) Installing musl (1.2.6-r2)
(2/2) Installing tree (2.3.2-r0)
OK: 743736 B in 2 packages
APK_ADD_EXIT=0
== tree binary ==
-rwxr-xr-x    1 root     root         73424 Mar 27  2026 /usr/bin/tree
== tree runs ==
tree v2.3.2 (c) 1996 - 2026 by Steve Baker, Thomas Moore, Francesc Rocher, Florian Sesser, Kyosuke Tokoro
== loader ==
-rwxr-xr-x    1 root     root        670312 Apr 11  2026 /lib/ld-musl-x86_64.so.1
== world ==
tree
== installed ==
P:musl
P:tree
PROOF_END
```

Notes on the output: no `UNTRUSTED` warning appeared, so the signed index was
verified against the shipped `/etc/apk/keys`; the installed sizes
(`/usr/bin/tree` 73,424 bytes, musl loader 670,312 bytes) match the
`APKINDEX` `I:` fields exactly; the database went from empty to exactly the
two packages, with musl resolved as tree's declared dependency.

## Fresh-boot check (no persistence), verbatim

A second boot of the same archive, `ok`: true, `exit_code`: 0, identical
`boot_units`: 1,197,000,000:

```text
== fresh boot: tree from previous boot ==
ls: /usr/bin/tree: No such file or directory
== apk present ==
-rwxr-xr-x    1 root     root        115096 Jan  1  1970 /sbin/apk
== db still empty at boot ==
0 /lib/apk/db/installed
== repositories intact ==
file:///opt/rish-apk-repo/main
== installing again works ==
(1/2) Installing musl (1.2.6-r2)
(2/2) Installing tree (2.3.2-r0)
OK: 743736 B in 2 packages
APK_ADD_EXIT=0
tree v2.3.2 (c) 1996 - 2026 by Steve Baker, Thomas Moore, Francesc Rocher, Florian Sesser, Kyosuke Tokoro
PERSIST_END
```

The first boot's installed files are gone on reboot, confirming the RAM-rootfs
boundary: configuration and package state must come from a persistent disk or
from a rebuild, not from the previous boot.

## App-side prerequisites (what must exist before the app can rely on this)

- A persistent writable guest disk with virtio-blk emulation and the matching
  kernel modules, **or** accept the build-time-bake loop: regenerate the
  initramfs whenever the app changes repository configuration or package sets.
- Network emulation (virtio-net + user-mode NAT/DNS) before any remote
  mirror or `apk update` claim.
- A pinned, license-audited package set in `assets.lock.tsv` for anything the
  product actually ships (see `guest/x86_64/SOURCES-AND-LICENSES.md`).
- A guest-side handshake step that applies the app's `/etc/apk`
  configuration into the persistent rootfs before the first `apk add`.

## Reference

- `guest/x86_64/build-container-initramfs.sh` -- apk closure extraction and
  repository staging.
- `guest/x86_64/assets.lock.tsv` -- pinned sizes and SHA-256 of the index and
  both packages.
- `guest/x86_64/container-overlay/etc/apk/` -- repositories and empty world.
- `docs/roadmap.md` -- network emulation remains unchecked.
- `docs/guest-root-disk.md` -- root disk support matrix and unmet
  prerequisites.
