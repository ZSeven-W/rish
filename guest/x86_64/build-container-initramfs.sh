#!/bin/sh
# Builds the interactive container initramfs used by the iOS/Android VM demos:
# a minimal busybox userland, the apk package manager, a small offline APK
# repository, the six kernel modules the root disk needs, plus the rish guest
# agent -- small enough to bundle as a mobile app resource. Unlike
# build-docker-initramfs.sh this ships no container runtime; the only modules
# are the virtio-blk driver and the VFAT filesystem stack (plus the NLS
# codepages the fat driver requests at mount time), all from the pinned
# netboot initramfs. The guest agent execs shell commands straight into the
# interpreter's Linux guest.
#
# The offline repository lets the guest run `apk add` from pinned, hash-locked
# Alpine packages with no network: /etc/apk/repositories (container-overlay)
# points apk at the file:// repository staged under /opt/rish-apk-repo/main.
# Signature verification follows Alpine convention: the pinned APKINDEX.tar.gz
# carries the .SIGN.RSA signature and the minirootfs provides the matching
# /etc/apk/keys public key, so no --allow-untrusted is required.
#
# Output: out/rish-container.cpio (uncompressed newc, ready as rdinit fodder).
set -eu

umask 022

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
downloads=${1:-"$script_dir/out/downloads"}
output_dir=${2:-"$script_dir/out"}
rootfs_name=alpine-minirootfs-3.24.1-x86_64.tar.gz
netboot_initramfs=initramfs-virt
kernel_version=6.18.35-0-virt
output_name=rish-container.cpio
temporary_dir=

die() {
    printf 'build-container-initramfs: %s\n' "$*" >&2
    exit 1
}

cleanup() {
    if [ -n "$temporary_dir" ]; then
        rm -rf -- "$temporary_dir"
    fi
}

command -v rustc >/dev/null 2>&1 || die "rustc is required"
command -v cargo >/dev/null 2>&1 || die "cargo is required"
command -v tar >/dev/null 2>&1 || die "tar is required"
command -v gzip >/dev/null 2>&1 || die "gzip is required"
command -v cpio >/dev/null 2>&1 || die "cpio is required"

"$script_dir/fetch-assets.sh" --offline "$downloads"

rootfs_archive="$downloads/$rootfs_name"
[ -f "$rootfs_archive" ] || die "missing rootfs archive: $rootfs_archive"

temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/rish-x86-container.XXXXXX")
trap cleanup EXIT HUP INT TERM
rootfs="$temporary_dir/rootfs"
mkdir -p -- "$rootfs/bin" "$rootfs/lib" "$rootfs/usr/bin" \
    "$rootfs/proc" "$rootfs/sys" "$rootfs/dev" "$rootfs/tmp" "$rootfs/root"

# 1. Pull the busybox binary, the musl loader, and the apk runtime closure out
# of the pinned Alpine minirootfs; the overlay init recreates the applet links
# at boot. apk needs libapk, openssl (signature verification), and zlib; the
# libc symlink and signing keys ship alongside it.
tar -xzf "$rootfs_archive" -C "$rootfs" --no-same-owner \
    bin/busybox lib/ld-musl-x86_64.so.1 lib/libc.musl-x86_64.so.1 \
    sbin/apk usr/lib/libapk.so.3.0.0 usr/lib/libssl.so.3 usr/lib/libcrypto.so.3 \
    usr/lib/libz.so.1.3.2 etc/apk/keys etc/apk/arch ||
    die "cannot extract busybox/apk closure from $rootfs_name"
[ -f "$rootfs/bin/busybox" ] || die "busybox missing after extract"
[ -f "$rootfs/lib/ld-musl-x86_64.so.1" ] || die "ld-musl missing after extract"
[ -f "$rootfs/sbin/apk" ] || die "apk missing after extract"
ln -s libz.so.1.3.2 "$rootfs/usr/lib/libz.so.1"

# 2. Root-disk kernel modules. The pinned virt kernel ships the block and
# vfat drivers as modules; the emulated machine exposes root_disk_path as a
# virtio-mmio block device, so the guest insmods these before use. They come
# from the pinned netboot initramfs (the same asset the docker guest uses),
# not from the modloop squashfs: gzip + cpio can extract them deterministically
# on every host, including macOS where the case-colliding modloop tree cannot
# be unpacked. virtio_blk has no module dependencies (the virtio core,
# virtio_ring, and virtio_mmio transport are built in); vfat needs fat.ko and
# the fat driver requests the cp437 codepage and the utf8 iocharset at mount
# time, so all three NLS modules ship too.
netboot_dir="$temporary_dir/netboot"
mkdir -p -- "$netboot_dir"
gzip -dc "$downloads/$netboot_initramfs" | (cd "$netboot_dir" && cpio -id --quiet)
modules_dir="$rootfs/lib/modules/$kernel_version"
mkdir -p -- "$modules_dir/kernel/drivers/block" "$modules_dir/kernel/fs/fat"     "$modules_dir/kernel/fs/nls"
for module_path in \
    kernel/drivers/block/virtio_blk.ko \
    kernel/fs/fat/fat.ko \
    kernel/fs/fat/vfat.ko \
    kernel/fs/nls/nls_cp437.ko \
    kernel/fs/nls/nls_ascii.ko \
    kernel/fs/nls/nls_utf8.ko; do
    [ -f "$netboot_dir/usr/lib/modules/$kernel_version/$module_path" ] ||
        die "netboot initramfs is missing $module_path"
    install -m 0644 "$netboot_dir/usr/lib/modules/$kernel_version/$module_path" \
        "$modules_dir/$module_path"
done

# 3. The guest agent, cross-compiled for the musl x86_64 guest.
(cd "$repo_root" && cargo build --release --target x86_64-unknown-linux-musl -p rish-guest-agent)
agent="$repo_root/target/x86_64-unknown-linux-musl/release/rish-guest-agent"
[ -f "$agent" ] || die "guest agent binary not found: $agent"
install -m 0755 "$agent" "$rootfs/usr/bin/rish-guest-agent"

# 4. Offline APK repository: the pinned, hash-locked main/x86_64 index and two
# packages (musl, tree). The empty installed database is deliberate: the
# initramfs payload sits outside apk's bookkeeping, so `apk add tree` resolves
# its libc dependency against this repository, the same flow a persistent
# guest would use.
apk_repo="$rootfs/opt/rish-apk-repo/main/x86_64"
mkdir -p -- "$rootfs/lib/apk/db" "$rootfs/var/cache/apk" "$apk_repo"
touch "$rootfs/lib/apk/db/installed"
for apk_asset in APKINDEX.tar.gz musl-1.2.6-r2.apk tree-2.3.2-r0.apk; do
    [ -f "$downloads/$apk_asset" ] || die "missing locked APK asset: $downloads/$apk_asset"
    install -m 0644 "$downloads/$apk_asset" "$apk_repo/$apk_asset"
done

# 5. Overlay: PID 1, /etc identity files (so whoami/id resolve), the
# apk repository configuration, and the namespace container demo.
cp -a "$script_dir/container-overlay/." "$rootfs/"
chmod 0755 "$rootfs/init" "$rootfs/container-demo.sh"

# 6. Pack an uncompressed newc archive with the shared tool.
packer="$temporary_dir/pack-newc"
rustc --edition=2021 -C opt-level=2 -D warnings \
    "$script_dir/tools/pack-newc.rs" -o "$packer"

mkdir -p -- "$output_dir"
output_dir=$(CDPATH= cd -- "$output_dir" && pwd)
candidate="$temporary_dir/$output_name"
"$packer" "$rootfs" "$candidate"
size=$(wc -c <"$candidate" | tr -d '[:space:]')
mv -f -- "$candidate" "$output_dir/$output_name"
printf 'built  %s (%s bytes)\n' "$output_dir/$output_name" "$size"
