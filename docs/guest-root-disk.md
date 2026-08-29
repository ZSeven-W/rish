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
  get a generated `RISH0001...` short name plus an LFN;
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

## How far root_disk_path actually works (fail-closed findings)

### The parameter plumbing is complete (verified line by line)

| Layer | Location | Behavior |
|---|---|---|
| FFI JSON | `crates/rish-ffi/src/vm_ffi.rs` (`VmRunRequest.root_disk_path`, `boot_channel`) | Both `rish_vm_boot_session` and `rish_vm_run_docker_json` accept an optional `root_disk_path`; when omitted a 64 MiB scratch file is materialized (because `VmConfig` requires a non-empty value) |
| Config | `crates/rish-vm/src/config.rs:88` | `root_disk_path` must be non-empty |
| Artifact check | `crates/rish-softvm-x86_64/src/artifacts.rs:62-65` | Must be a regular, non-empty file of at most 16 GiB (`max_root_disk_bytes`) |
| Provider | `crates/rish-softvm-x86_64/src/provider.rs:268-276` | The TCTI (QEMU) provider forwards the path into `RishTctiConfigV1.root_disk_path` across the C ABI; `PureRustProvider` reads only kernel + initrd and emulates no block device (`pure_rust.rs`: "It does not declare virtio-block") |
| Device gate | `crates/rish-softvm-x86_64/src/engine.rs` (`validate_engine_config`) | Only Console/Network devices are admitted today |
| Swift | `platform/ios/RishBridge.swift` (`RishVMRunRequest.rootDiskPath`) | Encodes as `root_disk_path` |

### Format and mount-point semantics

rish does **not** parse the image, does **not** require any filesystem format,
and does **not** choose a mount point: `root_disk_path` is just a path to an
image file. The format is a guest-side consumption contract (FAT16 as defined
here), and the mount point is up to the guest init (convention: mount
`/dev/vda` and copy over `/etc`, or mount at `/mnt/overlay`).

### Prerequisites that are not met yet (stated plainly)

1. **The pure-Rust interpreter emulates no block device.** The FFI path the
   app actually uses (`vm_ffi.rs` builds a `PureRustProvider`) accepts and
   validates `root_disk_path`, but the guest has no `/dev/vda` and never sees
   the image content (confirmed dynamically below).
2. **The kernel drivers are modules.** Alpine virt 6.18.35 has
   `CONFIG_VIRTIO_BLK=m`, `CONFIG_VFAT_FS=m`, `CONFIG_EXT4_FS=m` (checked in
   `config-6.18.35-0-virt`). The container initramfs ships no modules; the
   docker initramfs ships modloop. Even with a block device present, the
   container guest still needs module loading plus an init step that detects
   `/dev/vda`, mounts it, and overlays `/etc`.
3. **The TCTI (QEMU) path is only verified up to C ABI argument passing**;
   attaching a disk was not exercised against a QEMU provider.

## Verification record (what was actually checked)

- Static: every code path in the table above was read and confirmed.
- Dynamic A (FFI JSON, real boot): `vm_smoke` (the
  `rish_vm_run_docker_json` example) booted the kernel + container initramfs
  with a FAT16 root disk built by `build-root-disk.sh`. The boot succeeded
  (1,172,500,000 boot units) but the guest `/dev` contained only
  `console`, `null`, `ttyS0` and `cat /etc/apk/repositories` failed with
  "No such file or directory": the parameter is accepted and the file is
  validated, but the pure-Rust path does not attach the disk.
- Dynamic B (iOS simulator, C ABI + Swift): `run-vm-simulator.sh` staged
  `rish-root-disk.img` into the demo bundle and passed it as
  `rootDiskPath`; the in-guest probe shows the same absence of a block
  device.
- Not verified: real devices / arm64, QEMU TCTI disk attach, and an actual
  in-guest vfat mount (module gap above).
