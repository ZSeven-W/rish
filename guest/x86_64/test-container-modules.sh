#!/bin/sh
# Unit tests plus validation of an already-built container initramfs.
set -eu
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/rish-container-modules-test.XXXXXX")
trap 'rm -rf -- "$temporary_dir"' EXIT HUP INT TERM
rustc --edition=2021 --test -D warnings \
    "$script_dir/tools/verify-container-modules.rs" -o "$temporary_dir/tests"
"$temporary_dir/tests"
rustc --edition=2021 -D warnings \
    "$script_dir/tools/verify-container-modules.rs" -o "$temporary_dir/verifier"
"$temporary_dir/verifier" "${1:-$script_dir/out/rish-container.cpio}"
