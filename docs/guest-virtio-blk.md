# Guest virtio-blk: root disk as a real block device in the pure-Rust VM

## Transport choice: virtio-mmio, not virtio-pci

The emulated block device speaks virtio-mmio (virtio spec v1.x, modern v2
register file) at MMIO 0xFEBF0000, IRQ 10. Reasons, checked against the pinned
kernel config (`guest/x86_64/out/downloads/config-6.18.35-0-virt`):

- The pinned virt kernel has the virtio-mmio **transport built in**
  (`CONFIG_VIRTIO_MMIO=y`) and supports command-line device discovery
  (`CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y`). The provider appends
  `virtio_mmio.device=1K@0xfebf0000:10` to the kernel command line, so the
  guest finds the device with no PCI work at all.
- virtio-pci would require PCI config-space enumeration (0xCF8/0xCFC or the
  ACPI MCFG), BAR mapping, capability parsing, and MSI/MSI-X or INTx routing
  before the first queue could be configured. None of that exists in the
  interpreter today, and all of it is skippable with virtio-mmio.
- The split virtqueue lives in ordinary guest RAM; the device walks it with
  checked arithmetic only.

Discovery therefore works exactly the way the kernel documents it: the
cmdline fragment creates a virtio-mmio platform device, the built-in
platform driver probes the register file (magic `virt`, version 2, device id
2 = block), and the guest's `virtio_blk` module binds to it when loaded.

## What is implemented (rish-softvm-core, `crates/rish-softvm-core/src/virtio/`)

- Modern virtio-mmio register file: magic/version/device/vendor IDs, feature
  negotiation (both 32-bit halves), queue selection/sizing, queue
  ready/notify, interrupt status/ack, device status with reset, and the
  block config space (capacity as two words, seg_max, blk_size).
- Feature bits offered: `VIRTIO_F_VERSION_1`, `VIRTIO_BLK_F_SEG_MAX`
  (126), `VIRTIO_BLK_F_BLK_SIZE` (512).
- One split virtqueue (max 128 descriptors) with `VIRTIO_BLK_T_IN` (read),
  `VIRTIO_BLK_T_OUT` (write), and `VIRTIO_BLK_T_GET_ID` (fixed serial
  `RISH-VIRTIO-BLK-01`, space-padded) processing. Multi-segment requests are
  supported; data is moved in bounded 64 KiB chunks so a hostile segment
  length cannot force a large allocation.
- Used-ring completions raise one interrupt edge on I/O APIC pin 10 through
  both interrupt controllers, drained on the CPU device tick, matching the
  16550 wiring.
- Fail-closed bounds checking everywhere: the three rings must fit in guest
  RAM before the first drain; every descriptor address is checked on access;
  chains that leave the ring, cycle, or exceed the device limit, and any
  guest-physical access outside RAM, latch a **sticky device fault** (the
  device stops servicing until the machine restarts) instead of panicking or
  reading past the end of memory. Requests past the backend capacity complete
  with `VIRTIO_BLK_S_IOERR` and move no data. Host backend I/O failures also
  complete the request with IOERR and keep the device alive.
- The host file is the backend (`FileBlockBackend`, opened read-write, with
  pread/pwrite on unix): the disk image is never held in memory. Sizes that
  are not a multiple of 512 bytes are rejected at attach time.

## Provider wiring (`crates/rish-softvm-x86_64/src/pure_rust.rs`)

`PureRustProvider::create` now opens the validated `root_disk_path` as a
`FileBlockBackend`, attaches it to the interpreter, appends the
`virtio_mmio.device` cmdline fragment, and declares
`abi::FEATURE_VIRTIO_BLOCK`. The existing artifact validation is untouched
(regular file, non-empty, at most 16 GiB, `artifacts.rs`). A command line
that already declares `virtio_mmio.device` is rejected (the provider would
otherwise double-attach the same window), and the provider keeps rejecting
user networking.

## Guest driver availability (`guest/x86_64/`)

The kernel's block and VFAT drivers are modules (`CONFIG_VIRTIO_BLK=m`,
`CONFIG_VFAT_FS=m`, `CONFIG_FAT_FS=m`, `CONFIG_NLS_*=m`).
`build-container-initramfs.sh` now bakes six `.ko` files extracted from the
pinned netboot initramfs (`initramfs-virt`, the same asset the docker guest
uses) into `/lib/modules/6.18.35-0-virt`: `virtio_blk.ko`, `fat.ko`,
`vfat.ko`, `nls_cp437.ko`, `nls_ascii.ko`, `nls_utf8.ko`. The modloop
squashfs is not needed for these six (and cannot be unpacked on macOS, where
its case-colliding module names are unrepresentable). The overlay `init`
insmods them after devtmpfs is mounted, so `/dev/vda` appears via devtmpfs as
soon as `virtio_blk` registers; insmod failures are tolerated so a disk-less
boot still reaches the agent.

The rebuilt initramfs is 10,252,288 bytes (deterministic across two builds),
sha256 `00077738b28950a7d95047d31dccb959b68df6abe3c5da13a05fc16d55f5e01e`.
The previous capability (offline `apk add tree`) still passes (see
`docs/guest-apk-offline.md`).

## Verified end to end (real run, not inference)

The disk was built with `guest/x86_64/build-root-disk.sh` from an overlay
containing `etc/apk/repositories` and `rish-marker.txt`, booted through
`cargo run --release -p rish-ffi --example vm_smoke` (the
`rish_vm_run_docker_json` entry point). See the verbatim output at the end
of this document.

## Explicitly not implemented / not verified (fail-closed inventory)

- **FLUSH** (write barriers), **DISCARD**, **WRITE_ZEROES**: feature bits not
  offered; a non-conforming request completes with `VIRTIO_BLK_S_UNSUPP`.
  A FAT16 overlay does not need flush for correctness here, but a journaled
  filesystem (ext4) would.
- **Multi-queue** (`VIRTIO_BLK_F_MQ`), **geometry/topology** reporting
  (`_GEOMETRY`, `_TOPOLOGY`), **size_max**, `VIRTIO_F_ACCESS_PLATFORM`,
  packed virtqueues, virtio-iommu, and config-space changes after reset are
  not implemented. There is exactly one queue.
- No live-migration/device-state save-restore; device reset is
  register-level only and the sticky fault latch survives a reset.
- The TCTI/QEMU provider path is unchanged and its disk attach remains
  unverified here (documented separately in `guest-root-disk.md`).
- The guest mount was tested with the FAT16 image format produced by
  `build-root-disk.sh`; other filesystems (e.g. ext4, whose driver is also a
  module) are not tested with this device.

## Roadmap meaning (P3/P4)

This closes the "pure-Rust interpreter emulates no block device" gap recorded
in `docs/guest-root-disk.md` and makes `root_disk_path` a real, verified data
plane on the product path. For P3 it removes the last reason the engine could
not admit block device requests on the pure-Rust provider (the feature bit is
now declared and contract-tested). For P4 it gives the guest a persistent,
host-backed writable volume: the stage for writable container layers,
volumes, and the runtime overlay. What it is not: discard/flush support,
multiple queues, or any performance work — a journaling root filesystem or
high-throughput container storage still needs those, and they must be
implemented (with the same bounds-checked servicing) before that is claimed.

## Raw guest output (verbatim)

One `vm_smoke` run on the final build: `apk add tree` regression first, then
the disk proof. `ok: true`, `exit_code: 0`, `boot_units: 1,291,500,000`.

```text
==apk_add_tree==
(1/2) Installing musl (1.2.6-r2)
(2/2) Installing tree (2.3.2-r0)
OK: 743736 B in 2 packages
APK_ADD_EXIT=0
tree v2.3.2 (c) 1996 - 2026 by Steve Baker, Thomas Moore, Francesc Rocher, Florian Sesser, Kyosuke Tokoro
===ls_dev===
...
brw-------    1 root     root      253,   0 Nov 30 00:00 vda
...
===mount===
MOUNT_EXIT=0
===disk_ls===
total 17
drwxr-xr-x    3 root     root         16384 Jan  1  1970 .
drwxr-xr-x    3 root     root            60 Nov 30 00:01 ..
drwxr-xr-x    3 root     root           512 Jan  1  2020 etc
-rwxr-xr-x    1 root     root            97 Jan  1  2020 rish-marker.txt
===cat_repositories===
file:///opt/rish-apk-repo/main
===cat_marker===
rish virtio-blk proof: the guest read this file byte-for-byte from a host-built root disk image.
===interrupts===
           CPU0
  0:        942  IO-APIC   0-edge      timer
  4:         20  IO-APIC   4-edge      ttyS0
  9:          0  IO-APIC   9-fasteoi   acpi
 10:         39  IO-APIC  10-edge      virtio0
NMI:          0   Non-maskable interrupts
LOC:      70008   Local timer interrupts
SPU:          0   Spurious interrupts
PMI:          0   Performance monitoring interrupts
===DONE===
```

The full `ls -l /dev` listing is the complete devtmpfs node set (console,
null, tty0..tty63, ttyS0, random/urandom, zero, kmsg, mem, ...) plus the one
block node `vda` (major 253, the virtio-blk driver's assigned major); the
elision above keeps the report readable, the disk line is verbatim. A second
run (the same command against a fresh image) additionally proved the write
path: the guest created `/mnt/rish-disk/guest-notes.txt`, and after the boot
the host-side extractor (`tools/mk-root-disk.rs extract`) read the file back
from the host image with its content (`guest-wrote-this`) intact, byte for
byte.

## Interrupt delivery note

The device raises its used-ring interrupt edge on both interrupt controllers
like the serials. The pinned guest kernel initializes only the **master**
8259 (its routing goes through the I/O APIC), which exposed a fail-closed gap
in the interpreter's PIC model: an uninitialized slave answered IRQ 10 with
vector 2 (base 0 + line 2), which the guest takes as an NMI (`Uhhuh. NMI
received for unknown reason`). Fixed in `pic8259.rs`: an uninitialized chip
delivers nothing (`NMI: 0` in the final run, with 39 interrupts served on
IO-APIC pin 10).
