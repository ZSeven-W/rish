#!/bin/sh
set -eu

umask 022

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
downloads=${1:-"$script_dir/out/downloads"}
output_dir=${2:-"$script_dir/out"}
rootfs_name=alpine-minirootfs-3.24.1-x86_64.tar.gz
config_name=config-6.18.35-0-virt
temporary_dir=

die() {
    printf 'build-initramfs: %s\n' "$*" >&2
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

command -v rustc >/dev/null 2>&1 || die "rustc is required"
command -v tar >/dev/null 2>&1 || die "tar is required"

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

temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/rish-x86-initramfs.XXXXXX")
trap cleanup EXIT HUP INT TERM
rootfs="$temporary_dir/rootfs"
mkdir -p -- "$rootfs"
tar -xzf "$rootfs_archive" -C "$rootfs" --no-same-owner
install -m 0755 "$script_dir/rootfs-overlay/init" "$rootfs/init"

packer="$temporary_dir/pack-newc"
rustc \
    --edition=2021 \
    -C opt-level=2 \
    -D warnings \
    "$script_dir/tools/pack-newc.rs" \
    -o "$packer"

expected_id=${EXPECTED_ID:-rish-alpine-diagnostic-initramfs}
tab=$(printf '\t')
derived_id=
while IFS="$tab" read -r candidate_id filename expected_size expected_sha extra; do
    case "$candidate_id" in
        ''|'#'*) continue ;;
    esac
    [ "$candidate_id" = "$expected_id" ] || continue
    [ -z "${extra:-}" ] || die "unexpected field in derived.lock.tsv"
    [ -z "$derived_id" ] || die "derived.lock.tsv has a duplicate $expected_id"
    derived_id=$candidate_id
    output_name=$filename
    output_size=$expected_size
    output_sha=$expected_sha
done <"$script_dir/derived.lock.tsv"
[ -n "$derived_id" ] || die "derived.lock.tsv has no $expected_id artifact"
case "$output_name" in
    ''|*/*|.|..) die "unsafe derived output filename: $output_name" ;;
esac
case "$output_size" in
    ''|*[!0-9]*) die "invalid derived output size" ;;
esac
[ "${#output_sha}" -eq 64 ] || die "invalid derived SHA-256 length"
case "$output_sha" in
    *[!0-9a-f]*) die "invalid derived SHA-256" ;;
esac

candidate="$temporary_dir/$output_name"
"$packer" "$rootfs" "$candidate"
actual_size=$(wc -c <"$candidate" | tr -d '[:space:]')
[ "$actual_size" = "$output_size" ] ||
    die "derived size mismatch: expected $output_size, got $actual_size"
actual_sha=$(sha256_file "$candidate")
[ "$actual_sha" = "$output_sha" ] ||
    die "derived SHA-256 mismatch: expected $output_sha, got $actual_sha"

mkdir -p -- "$output_dir"
output_dir=$(CDPATH= cd -- "$output_dir" && pwd)
mv -f -- "$candidate" "$output_dir/$output_name"
printf 'verified  %s (%s bytes, sha256:%s)\n' \
    "$output_dir/$output_name" "$actual_size" "$actual_sha"