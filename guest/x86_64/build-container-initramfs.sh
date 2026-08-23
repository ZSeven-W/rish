#!/bin/sh
# Builds the interactive container initramfs used by the iOS/Android VM demos:
# a minimal busybox userland plus the rish guest agent, small enough to bundle
# as a mobile app resource. Unlike build-docker-initramfs.sh this ships no
# container runtime or kernel modules -- the guest agent execs shell commands
# straight into the interpreter's Linux guest.
#
# Output: out/rish-container.cpio (uncompressed newc, ready as rdinit fodder).
set -eu

umask 022

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
downloads=${1:-"$script_dir/out/downloads"}
output_dir=${2:-"$script_dir/out"}
rootfs_name=alpine-minirootfs-3.24.1-x86_64.tar.gz
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

"$script_dir/fetch-assets.sh" --offline "$downloads"

rootfs_archive="$downloads/$rootfs_name"
[ -f "$rootfs_archive" ] || die "missing rootfs archive: $rootfs_archive"

temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/rish-x86-container.XXXXXX")
trap cleanup EXIT HUP INT TERM
rootfs="$temporary_dir/rootfs"
mkdir -p -- "$rootfs/bin" "$rootfs/lib" "$rootfs/usr/bin" \
    "$rootfs/proc" "$rootfs/sys" "$rootfs/dev" "$rootfs/tmp" "$rootfs/root"

# 1. Pull just the busybox binary and the musl loader out of the pinned Alpine
# minirootfs; the overlay init recreates the applet links at boot.
tar -xzf "$rootfs_archive" -C "$rootfs" --no-same-owner \
    bin/busybox lib/ld-musl-x86_64.so.1 ||
    die "cannot extract busybox/ld-musl from $rootfs_name"
[ -f "$rootfs/bin/busybox" ] || die "busybox missing after extract"
[ -f "$rootfs/lib/ld-musl-x86_64.so.1" ] || die "ld-musl missing after extract"

# 2. The guest agent, cross-compiled for the musl x86_64 guest.
(cd "$repo_root" && cargo build --release --target x86_64-unknown-linux-musl -p rish-guest-agent)
agent="$repo_root/target/x86_64-unknown-linux-musl/release/rish-guest-agent"
[ -f "$agent" ] || die "guest agent binary not found: $agent"
install -m 0755 "$agent" "$rootfs/usr/bin/rish-guest-agent"

# 3. Overlay: PID 1, /etc identity files (so whoami/id resolve), and the
# namespace container demo.
cp -a "$script_dir/container-overlay/." "$rootfs/"
chmod 0755 "$rootfs/init" "$rootfs/container-demo.sh"

# 4. Pack an uncompressed newc archive with the shared tool.
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
