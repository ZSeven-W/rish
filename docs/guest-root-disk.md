# Guest root disk: image format and root_disk_path wiring

## Why this exists

dsh-app's LocalMirrorsModule stages Alpine/PyPI/npm mirror configuration into
`Application Support/rish-guest-overlay/` (the receipt records
`guest_runtime_mounted: false`), while the boot initramfs is a read-only bundle
resource. The runtime injection path is `root_disk_path`: the app assembles the
staged overlay into a small disk image and passes it in the boot request. This
document defines that image's format contract and records, honestly, how far
the rish side supports `root_disk_path` today.

## Image format (FAT16 / VFAT)

`guest/x86_64/build-root-disk.sh` produces a fixed **4 MiB** (8192 x 512-byte
sectors) FAT16 image:

- BPB: 512-byte sectors, 1 sector per cluster, 4 reserved sectors, 2 FATs
  (33 sectors each), 512 root-directory entries, volume label
  `RISHOVERLAY`, constant volume serial;
- Names: UTF-8 names are stored as VFAT LFNs (UTF-16); names that do not fit
  8.3 (long, lowercase, dotfiles, multiple extensions, ...) deterministically
  get a generated `RISH0001...` short name plus an LFN. Each LFN carries the
  spec-defined short-name checksum (rotate-right accumulator over the 8.3
  bytes) and NUL-pads the unused characters of its final slot, both of which
  Linux and Windows validate strictly; an earlier build used a left-rotating
  checksum and 0xFFFF filler, which macOS (lenient on both) read fine while
  Linux vfat either dropped the long names or rendered the filler as `?` —
  fixed in `tools/mk-root-disk.rs` with reference-value unit tests;
- Every directory-entry timestamp is pinned to **2020-01-01 00:00:00**; FAT
  stores no file uid (the uid is a mount-time option), so timestamps and uids
  are reproducible by construction;
- Entries are sorted by path bytes, clusters are allocated sequentially, and
  all other bytes are constants, so equal inputs produce byte-identical
  images (the test suite builds twice and `cmp`s);
- The data area holds about 4.0 MiB; overflow is an error; an existing output
  file is never overwritten;
- Input validation (fail-closed): symlinks, special files, non-UTF-8 names,
  VFAT-forbidden characters, and names over 255 characters are rejected.

FAT16 was chosen because generating it needs no host `mkfs`/`e2fsprogs` tools
(iOS has none either): the whole writer is dependency-free Rust that ports
directly to Swift, and macOS can mount the result with `hdiutil`, which the
test suite uses as a real-kernel check. The Linux guest reads it with the
vfat driver (busybox `mount -t vfat`), subject to the prerequisites below.

## Usage

```sh
# Reference implementation (this repository, host build)
guest/x86_64/build-root-disk.sh OVERLAY_DIR OUTPUT_IMG
guest/x86_64/test-root-disk.sh   # unit + end-to-end tests

# App side (dsh-app, runtime)
# Generate the same image format from Application Support/rish-guest-overlay/
# and put its path into the boot JSON as root_disk_path. This script is the
# reference implementation; the app needs the writer ported to Swift (or
# packaged as a library).
```

`test-root-disk.sh` covers: the tool's unit tests (short names, LFN round
trip, determinism, symlink rejection, name validation), two builds of a
realistic overlay compared byte-for-byte, extraction compared file-by-file,
macOS `hdiutil` mount + kernel vfat read-back, and symlink rejection.

## How far root_disk_path actually works (status)

### The parameter plumbing is complete (verified line by line)

| Layer | Location | Behavior |
|---|---|---|
| FFI JSON | `crates/rish-ffi/src/vm_ffi.rs` (`VmRunRequest.root_disk_path`, `boot_channel`) | Both `rish_vm_boot_session` and `rish_vm_run_docker_json` accept an optional `root_disk_path`; when omitted a 64 MiB scratch file is materialized (because `VmConfig` requires a non-empty value) |
| Config | `crates/rish-vm/src/config.rs` | `root_disk_path` must be non-empty; the command line must not contain NUL bytes (which would hide appended device declarations from the kernel) |
| Artifact check | `crates/rish-softvm-x86_64/src/artifacts.rs` | Must be a regular, non-empty file of at most 16 GiB (`max_root_disk_bytes`), checked on the path **and** on the opened inode; the validated handle is what the provider serves |
| Provider (pure Rust) | `crates/rish-softvm-x86_64/src/pure_rust.rs` | Binds the validated root-disk handle as the virtio-blk backend and attaches the emulated block device |
| Provider (TCTI/QEMU) | `crates/rish-softvm-x86_64/src/provider.rs` | Forwards the path into `RishTctiConfigV1.root_disk_path` across the C ABI |
| Device gate | `crates/rish-softvm-x86_64/src/engine.rs` (`validate_engine_config`) | Console, root block (implicit), and optional user NAT are admitted |
| Swift | `platform/ios/RishBridge.swift` (`RishVMRunRequest.rootDiskPath`) | Encodes as `root_disk_path` |

### Format and mount-point semantics

rish does **not** parse the image, does **not** require any filesystem format,
and does **not** choose a mount point: `root_disk_path` is just a path to an
image file. The format is a guest-side consumption contract (FAT16 as defined
here), and the mount point is up to the guest init (convention: mount
`/dev/vda` and copy over `/etc`, or mount at `/mnt/overlay`).

### Prerequisites (status)

1. **The pure-Rust interpreter emulates the block device.** This document
   predates that work; `docs/guest-virtio-blk.md` records the device, the
   fail-closed servicing contract, and the end-to-end proof: the guest sees
   `/dev/vda`, mounts the FAT16 image, and reads and writes files through it.
2. **The kernel drivers are modules and are now baked in.** Alpine virt
   6.18.35 has `CONFIG_VIRTIO_BLK=m`, `CONFIG_VFAT_FS=m`; the container
   initramfs bakes `virtio_blk`, `fat`, `vfat`, `nls_cp437`, `nls_ascii`,
   and `nls_utf8` plus the virtio-net module stack (see
   `docs/guest-virtio-blk.md` and `docs/guest-virtio-net.md`). The overlay
   init insmods them, so the guest-side mount works; an init step that
   mounts `/dev/vda` and overlays `/etc` is the app's job.
3. **The TCTI (QEMU) path is only verified up to C ABI argument passing**;
   attaching a disk was not exercised against a QEMU provider.

## Verification record (what was actually checked)

- Static: every code path in the table above was read and confirmed.
- Dynamic A (FFI JSON, real boot, current): the proof recorded in
  `docs/guest-virtio-blk.md` — `vm_smoke` (the `rish_vm_run_docker_json`
  example) booted the kernel + container initramfs with a FAT16 root disk
  built by `build-root-disk.sh`; the guest's `/dev/vda` appeared, `mount -t
  vfat` succeeded, and the guest read the host-written marker file byte for
  byte, and wrote a file the host extractor read back intact.
- The earlier "Dynamic A" run recorded in the original version of this
  document (guest had no block device) is superseded by the virtio-blk
  milestone above.
- Dynamic B (iOS simulator, C ABI + Swift): `run-vm-simulator.sh` staged
  `rish-root-disk.img` into the demo bundle; in-guest probing on the iOS
  simulator path has not been re-run since the block device landed.
- Not verified: real devices / arm64, QEMU TCTI disk attach.
