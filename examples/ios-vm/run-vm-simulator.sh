#!/bin/zsh
# Builds and runs the full-VM iOS demo on the iPhone Simulator: it boots the
# pure-Rust x86_64 interpreter with a bundled kernel and initramfs and runs
# `uname -a` inside the Linux guest through rish_vm_run_docker_json.
set -euo pipefail

VM_SCRIPT_DIR=${0:A:h}
VM_REPOSITORY_ROOT=${VM_SCRIPT_DIR:h:h}
VM_TARGET=aarch64-apple-ios-sim
VM_BUILD_ROOT="${VM_REPOSITORY_ROOT}/target/ios-vm-demo"
VM_APP="${VM_BUILD_ROOT}/RishVMDemo.app"
VM_BUNDLE_ID=dev.rish.vmdemo
VM_RUN_ID=$(uuidgen | tr '[:upper:]' '[:lower:]')

# Guest images: overridable, default to the in-repo kernel and the minimal
# port-I/O agent initramfs staged at /tmp/rish-mini.cpio.
VM_KERNEL=${RISH_KERNEL:-"${VM_REPOSITORY_ROOT}/guest/x86_64/out/downloads/vmlinuz-virt-6.18.35"}
# The interactive container initramfs is built reproducibly by
# guest/x86_64/build-container-initramfs.sh. Override RISH_INITRD to boot a
# different guest (e.g. the full docker-in-guest image).
VM_INITRD=${RISH_INITRD:-"${VM_REPOSITORY_ROOT}/guest/x86_64/out/rish-container.cpio"}
VM_INITRD_NAME=${RISH_INITRD_NAME:-rish-container.cpio}
# Optional root-disk image (guest/x86_64/build-root-disk.sh); staged when
# RISH_ROOT_DISK names an existing file or the default path exists.
VM_ROOT_DISK=${RISH_ROOT_DISK:-"${VM_REPOSITORY_ROOT}/guest/x86_64/out/root-disk.img"}
VM_ROOT_DISK_NAME=${RISH_ROOT_DISK_NAME:-rish-root-disk.img}

if ! rustup target list --installed | grep -qx "${VM_TARGET}"; then
    # A workspace-local sysroot (per-target RUSTFLAGS with --sysroot) can also
    # supply the target std without a rustup install.
    if [[ "${CARGO_TARGET_AARCH64_APPLE_IOS_SIM_RUSTFLAGS:-}" != *"--sysroot"* ]]; then
        print -u2 "missing Rust target: ${VM_TARGET} (rustup target add ${VM_TARGET})"
        exit 2
    fi
fi
for image in "${VM_KERNEL}" "${VM_INITRD}"; do
    if [[ ! -f "${image}" ]]; then
        print -u2 "missing guest image: ${image}"
        exit 2
    fi
done

VM_DEVICE_ID=${1:-}
if [[ -z "${VM_DEVICE_ID}" ]]; then
    VM_DEVICE_ID=$(
        xcrun simctl list devices available |
            awk -F '[()]' '/iPhone/ { print $2; exit }'
    )
fi
if [[ -z "${VM_DEVICE_ID}" ]]; then
    print -u2 "no available iPhone Simulator was found"
    exit 2
fi

# Release build: the interpreter is CPU-bound, so a debug library would make the
# guest boot take minutes.
cargo build --release --target "${VM_TARGET}" -p rish-ffi

rm -rf "${VM_APP}"
mkdir -p "${VM_APP}"
cp "${VM_SCRIPT_DIR}/Info.plist" "${VM_APP}/Info.plist"
# Stage the guest kernel and initramfs as bundle resources; the Swift demo
# resolves them with Bundle.main.path and passes the paths across the ABI.
cp "${VM_KERNEL}" "${VM_APP}/vmlinuz-virt-6.18.35"
cp "${VM_INITRD}" "${VM_APP}/${VM_INITRD_NAME}"
if [[ -n "${VM_ROOT_DISK}" && -f "${VM_ROOT_DISK}" ]]; then
    cp "${VM_ROOT_DISK}" "${VM_APP}/${VM_ROOT_DISK_NAME}"
fi

# Keep clang's module cache inside the build root so the compile also
# works in sandboxes that deny writes to the shared per-user cache.
mkdir -p "${VM_BUILD_ROOT}/ModuleCache"
xcrun --sdk iphonesimulator swiftc \
    -target arm64-apple-ios18.0-simulator \
    -O \
    -parse-as-library \
    -Xcc -fmodules-cache-path="${VM_BUILD_ROOT}/ModuleCache" \
    -import-objc-header "${VM_REPOSITORY_ROOT}/platform/rish.h" \
    "${VM_REPOSITORY_ROOT}/platform/ios/RishBridge.swift" \
    "${VM_SCRIPT_DIR}/RishVMDemo.swift" \
    "${VM_REPOSITORY_ROOT}/target/${VM_TARGET}/release/librish_ffi.a" \
    -framework Foundation \
    -framework UIKit \
    -o "${VM_APP}/RishVMDemo"
codesign --force --sign - "${VM_APP}"

xcrun simctl boot "${VM_DEVICE_ID}" >/dev/null 2>&1 || true
xcrun simctl bootstatus "${VM_DEVICE_ID}" -b
xcrun simctl terminate "${VM_DEVICE_ID}" "${VM_BUNDLE_ID}" >/dev/null 2>&1 || true
# Uninstall first so a previous run's transcript file cannot be mistaken for
# this run's output.
xcrun simctl uninstall "${VM_DEVICE_ID}" "${VM_BUNDLE_ID}" >/dev/null 2>&1 || true
xcrun simctl install "${VM_DEVICE_ID}" "${VM_APP}"
xcrun simctl launch "${VM_DEVICE_ID}" "${VM_BUNDLE_ID}"

# This is an interactive terminal: it boots the x86-64 Linux guest once (up to a
# few minutes) and then waits for commands typed in the simulator. Wait until
# the guest has booted and the auto-run `uname` has landed, then hand off — the
# app keeps running for interactive use.
print "launched — booting the x86-64 Linux guest (this takes ~40-60s)…"
VM_RESULT=
for _ in {1..3000}; do
    VM_CONTAINER=$(
        xcrun simctl get_app_container "${VM_DEVICE_ID}" "${VM_BUNDLE_ID}" data 2>/dev/null || true
    )
    if [[ -n "${VM_CONTAINER}" ]]; then
        VM_RESULT="${VM_CONTAINER}/Documents/RishVMResult.txt"
        if [[ -f "${VM_RESULT}" ]] && grep -q 'x86_64' "${VM_RESULT}"; then
            break
        fi
    fi
    sleep 0.2
done

print "simulator=${VM_DEVICE_ID}"
if [[ -n "${VM_RESULT}" && -f "${VM_RESULT}" ]]; then
    print "guest booted — transcript so far:"
    print
    command cat "${VM_RESULT}"
    print
    print "The app is live: type shell commands in the simulator to run them in the guest."
else
    print -u2 "the guest did not report boot within the timeout"
    exit 1
fi
