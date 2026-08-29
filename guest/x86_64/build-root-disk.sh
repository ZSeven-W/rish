#!/bin/sh
# Builds the deterministic FAT16 root-disk image for the guest overlay.
#
# The app stages its runtime overlay (Application Support/rish-guest-overlay/,
# e.g. etc/apk/repositories) and passes the produced image as root_disk_path
# in the boot request. See docs/guest-root-disk.md for the format contract and
# the root_disk_path wiring.
#
# Output: a fixed 4 MiB FAT16 image. Equal inputs produce byte-identical
# output: entry timestamps are pinned, cluster allocation is sequential after
# a deterministic sort, and the volume serial/label are constants.
set -eu

umask 022

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

usage() {
    cat >&2 <<'EOF'
usage: build-root-disk.sh OVERLAY_DIR OUTPUT_IMG

Builds a 4 MiB FAT16 root-disk image from the files under OVERLAY_DIR.
Symlinks, special files, non-UTF-8 names, and unsafe path components are
rejected. OUTPUT_IMG must not already exist.
EOF
}

die() {
    printf 'build-root-disk: %s\n' "$*" >&2
    exit 1
}

cleanup() {
    if [ -n "$temporary_dir" ]; then
        rm -rf -- "$temporary_dir"
    fi
}

[ "$#" -eq 2 ] || {
    usage
    exit 2
}
overlay=$1
output=$2

[ -d "$overlay" ] || die "overlay is not a directory: $overlay"
[ -e "$output" ] && die "output already exists: $output"
command -v rustc >/dev/null 2>&1 || die "rustc is required"

temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/rish-root-disk.XXXXXX")
trap cleanup EXIT HUP INT TERM

tool="$temporary_dir/mk-root-disk"
rustc --edition=2021 -C opt-level=2 -D warnings \
    "$script_dir/tools/mk-root-disk.rs" -o "$tool"

"$tool" build "$overlay" "$output"
