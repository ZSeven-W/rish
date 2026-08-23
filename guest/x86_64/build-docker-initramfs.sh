#!/bin/sh
set -eu

umask 022

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null || die "cannot resolve repository root")
update_lock=0
while [ "$#" -gt 0 ] && [ "$1" = "--update-lock" ]; do
    update_lock=1
    shift
done
downloads=${1:-"$script_dir/out/downloads"}
output_dir=${2:-"$script_dir/out"}
rootfs_name=alpine-minirootfs-3.24.1-x86_64.tar.gz
config_name=config-6.18.35-0-virt
netboot_initramfs=initramfs-virt
modloop_name=modloop-virt
docker_tgz=docker-29.7.2.tgz
kernel_version=6.18.35-0-virt
expected_id=rish-alpine-docker-initramfs
output_name=rish-alpine-3.24.1-x86_64-docker-initramfs.cpio
temporary_dir=

die() {
    printf 'build-docker-initramfs: %s\n' "$*" >&2
    exit 1
}

cleanup() {
    if [ -n "$temporary_dir" ]; then
        rm -rf -- "$temporary_dir"
    fi
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        die "sha256sum or shasum is required"
    fi
}

[ "$#" -le 2 ] || die "usage: build-docker-initramfs.sh [--update-lock] [DOWNLOADS OUTPUT]"

command -v rustc >/dev/null 2>&1 || die "rustc is required"
command -v cargo >/dev/null 2>&1 || die "cargo is required"
command -v tar >/dev/null 2>&1 || die "tar is required"
command -v gzip >/dev/null 2>&1 || die "gzip is required"
command -v cpio >/dev/null 2>&1 || die "cpio is required"
command -v unsquashfs >/dev/null 2>&1 || die "unsquashfs is required (brew install squashfs)"

"$script_dir/fetch-assets.sh" --offline "$downloads"
"$script_dir/verify-kernel-config.sh" "$downloads/$config_name"

rootfs_archive="$downloads/$rootfs_name"
tar -tzf "$rootfs_archive" | awk '
    BEGIN { unsafe = 0 }
    /^\// {
        print "absolute archive path: " $0 > "/dev/stderr"
        unsafe = 1
    }
    {
        count = split($0, part, "/")
        for (item = 1; item <= count; item++) {
            if (part[item] == "..") {
                print "parent archive path: " $0 > "/dev/stderr"
                unsafe = 1
            }
        }
    }
    END { exit unsafe }
' || die "rootfs archive contains an unsafe path"

temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/rish-x86-docker.XXXXXX")
trap cleanup EXIT HUP INT TERM
rootfs="$temporary_dir/rootfs"
mkdir -p -- "$rootfs"
tar -xzf "$rootfs_archive" -C "$rootfs" --no-same-owner

modules_dir="$rootfs/lib/modules"
mkdir -p -- "$modules_dir"

# 1. Boot-critical modules and module metadata from the pinned netboot initramfs.
netboot_dir="$temporary_dir/netboot"
mkdir -p -- "$netboot_dir"
gzip -dc "$downloads/$netboot_initramfs" | (cd "$netboot_dir" && cpio -id --quiet)
[ -d "$netboot_dir/lib/modules/$kernel_version" ] ||
    die "netboot initramfs has no $kernel_version modules"
cp -a "$netboot_dir/lib/modules/$kernel_version" "$modules_dir/"

# 2. The pinned modloop squashfs ships inside the initramfs and is mounted
# by the guest init (loop + squashfs modules come from the netboot set).
# The host only verifies the listing because macOS APFS cannot represent
# the case-colliding module names inside the image.
modloop_listing="$temporary_dir/modloop.listing"
unsquashfs -ll "$downloads/$modloop_name" > "$modloop_listing" ||
    die "unsquashfs listing failed for $modloop_name"
for required in overlay.ko virtio_net.ko veth.ko nf_tables.ko br_netfilter.ko; do
    grep -q "/$required" "$modloop_listing" ||
        die "modloop listing is missing $required"
done
install -m 0644 "$downloads/$modloop_name" "$rootfs/modloop-virt"
[ -f "$modules_dir/$kernel_version/modules.dep" ] || die "modules.dep is missing"

# 3. Static Docker toolchain.
docker_dir="$temporary_dir/docker"
mkdir -p -- "$docker_dir"
tar -xzf "$downloads/$docker_tgz" -C "$docker_dir" --no-same-owner
for binary in dockerd containerd containerd-shim-runc-v2 runc ctr docker docker-proxy; do
    [ -x "$docker_dir/docker/$binary" ] || die "docker toolchain is missing $binary"
    install -m 0755 "$docker_dir/docker/$binary" "$rootfs/usr/bin/$binary"
done
install -d -m 0755 "$rootfs/usr/libexec/docker"
install -m 0755 "$docker_dir/docker/docker-init" "$rootfs/usr/libexec/docker/docker-init"
install -m 0755 "$docker_dir/docker/docker-init" "$rootfs/usr/bin/docker-init"

# 4. Static rish guest agent from the workspace.
(cd "$repo_root" && cargo build --release --target x86_64-unknown-linux-musl -p rish-guest-agent)
agent="$repo_root/target/x86_64-unknown-linux-musl/release/rish-guest-agent"
[ -x "$agent" ] || die "guest agent binary is missing"
install -m 0755 "$agent" "$rootfs/usr/bin/rish-guest-agent"

# 5. Docker guest overlay and supplementary group.
cp -a "$script_dir/docker-overlay/." "$rootfs/"
# PID 1 must be executable regardless of the checked-out mode of the overlay
# source (git does not reliably preserve the executable bit on all hosts).
chmod 0755 "$rootfs/init"
echo "docker:x:100:" >> "$rootfs/etc/group"

# 6. Deterministic newc pack.
packer="$temporary_dir/pack-newc"
rustc \
    --edition=2021 \
    -C opt-level=2 \
    -D warnings \
    "$script_dir/tools/pack-newc.rs" \
    -o "$packer"
candidate="$temporary_dir/$output_name"
"$packer" "$rootfs" "$candidate"
actual_size=$(wc -c <"$candidate" | tr -d '[:space:]')
actual_sha=$(sha256_file "$candidate")

# 7. Derived lock: verify, or rewrite the matching row under --update-lock.
tab=$(printf '\t')
lock="$script_dir/derived.lock.tsv"
if [ "$update_lock" -eq 1 ]; then
    tmp_lock="$temporary_dir/derived.lock.tsv"
    awk -F"$tab" -v id="$expected_id" -v name="$output_name" \
        -v size="$actual_size" -v sha="$actual_sha" '
        BEGIN { OFS = "\t"; replaced = 0 }
        /^#/ || /^$/ { print; next }
        $1 == id { print id, name, size, sha; replaced = 1; next }
        { print }
        END { if (!replaced) { print id, name, size, sha } }
    ' "$lock" > "$tmp_lock"
    mv -- "$tmp_lock" "$lock"
else
    expected_size=
    expected_sha=
    while IFS="$tab" read -r candidate_id filename expected_size_c expected_sha_c extra; do
        case "$candidate_id" in
            ''|'#'*) continue ;;
        esac
        [ "$candidate_id" = "$expected_id" ] || continue
        [ -z "${extra:-}" ] || die "unexpected field in derived.lock.tsv"
        [ -z "$expected_size" ] || die "duplicate $expected_id in derived.lock.tsv"
        expected_size=$expected_size_c
        expected_sha=$expected_sha_c
    done <"$lock"
    [ -n "$expected_size" ] || die "derived.lock.tsv has no $expected_id row"
    [ "$actual_size" = "$expected_size" ] ||
        die "derived size mismatch: expected $expected_size, got $actual_size"
    [ "$actual_sha" = "$expected_sha" ] ||
        die "derived SHA-256 mismatch: expected $expected_sha, got $actual_sha"
fi

mkdir -p -- "$output_dir"
output_dir=$(CDPATH= cd -- "$output_dir" && pwd)
mv -f -- "$candidate" "$output_dir/$output_name"
printf 'verified  %s (%s bytes, sha256:%s)\n' \
    "$output_dir/$output_name" "$actual_size" "$actual_sha"