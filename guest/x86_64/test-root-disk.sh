#!/bin/sh
# End-to-end test for build-root-disk.sh.
#
# Checks: the tool's own unit tests; a realistic overlay (etc/apk/
# repositories, etc/pip.conf, .npmrc) round-trips through the image;
# byte-identical output across two builds; content readability through the
# macOS vfat kernel driver (hdiutil) when available; and rejection of
# symlinks. Run from anywhere; it only writes to a temporary directory.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

die() {
    printf 'test-root-disk: %s\n' "$*" >&2
    exit 1
}

temporary_dir=
cleanup() {
    if [ -n "$temporary_dir" ]; then
        rm -rf -- "$temporary_dir"
    fi
}

command -v rustc >/dev/null 2>&1 || die "rustc is required"
temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/rish-root-disk-test.XXXXXX")
trap cleanup EXIT HUP INT TERM

# 1. The tool's own unit tests (short names, LFN round trip, determinism,
#    symlink rejection, name validation).
rustc --edition=2021 --test -D warnings \
    "$script_dir/tools/mk-root-disk.rs" -o "$temporary_dir/unit-tests"
"$temporary_dir/unit-tests" >/dev/null

tool="$temporary_dir/mk-root-disk"
rustc --edition=2021 -C opt-level=2 -D warnings \
    "$script_dir/tools/mk-root-disk.rs" -o "$tool"

# 2. A realistic overlay: the files the app stages from
#    Application Support/rish-guest-overlay/.
overlay="$temporary_dir/overlay"
mkdir -p "$overlay/etc/apk"
cat >"$overlay/etc/apk/repositories" <<'EOF'
https://mirrors.example/alpine/v3.24/main
https://mirrors.example/alpine/v3.24/community
EOF
cat >"$overlay/etc/pip.conf" <<'EOF'
[global]
index-url = https://mirrors.example/pypi/simple
EOF
cat >"$overlay/.npmrc" <<'EOF'
registry=https://mirrors.example/npm
EOF

# 3. Build twice: the images must be byte-identical.
"$script_dir/build-root-disk.sh" "$overlay" "$temporary_dir/root-disk.img"
"$script_dir/build-root-disk.sh" "$overlay" "$temporary_dir/root-disk-2.img"
cmp "$temporary_dir/root-disk.img" "$temporary_dir/root-disk-2.img"

# 4. Extract the image back and compare every file.
"$tool" extract "$temporary_dir/root-disk.img" "$temporary_dir/extracted"
diff -r "$overlay" "$temporary_dir/extracted"

# 5. On macOS, mount the image with the real kernel vfat driver and read the
#    staged repositories file back through the mount.
if [ "$(uname)" = "Darwin" ] && command -v hdiutil >/dev/null 2>&1; then
    mount_output=$(hdiutil attach -nobrowse -readonly "$temporary_dir/root-disk.img")
    device=$(printf '%s\n' "$mount_output" | awk 'NR == 1 { print $1 }')
    mountpoint=$(printf '%s\n' "$mount_output" | awk '/\/Volumes\// { print $NF; exit }')
    [ -n "$device" ] || die "hdiutil attach produced no device"
    [ -n "$mountpoint" ] || die "hdiutil attach produced no mount point"
    cmp "$mountpoint/etc/apk/repositories" "$overlay/etc/apk/repositories"
    cmp "$mountpoint/etc/pip.conf" "$overlay/etc/pip.conf"
    cmp "$mountpoint/.npmrc" "$overlay/.npmrc"
    hdiutil detach "$device" >/dev/null
fi

# 6. Input validation: symlinks must fail the build.
mkdir -p "$temporary_dir/bad/etc"
cat >"$temporary_dir/bad/etc/repositories" <<'EOF'
https://mirrors.example/alpine/v3.24/main
EOF
ln -s /etc/passwd "$temporary_dir/bad/etc/escape"
if "$script_dir/build-root-disk.sh" "$temporary_dir/bad" "$temporary_dir/bad.img" \
    2>"$temporary_dir/bad.err"; then
    die "an overlay containing a symlink was accepted"
fi
grep -q "symlink" "$temporary_dir/bad.err"

printf 'test-root-disk: all checks passed\n'
