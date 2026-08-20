#!/bin/zsh
set -euo pipefail

DEMO_SCRIPT_DIR=${0:A:h}
DEMO_REPOSITORY_ROOT=${DEMO_SCRIPT_DIR:h:h}
DEMO_TOOLCHAIN=${RISH_RUST_TOOLCHAIN:-1.85.0}
DEMO_TARGET=aarch64-apple-ios-sim
DEMO_BUILD_ROOT="${DEMO_REPOSITORY_ROOT}/target/ios-demo"
DEMO_APP="${DEMO_BUILD_ROOT}/RishDemo.app"
DEMO_BUNDLE_ID=dev.rish.demo
DEMO_RUN_ID=$(uuidgen | tr '[:upper:]' '[:lower:]')
DEMO_PLATFORM_SOURCES=("${DEMO_REPOSITORY_ROOT}/platform/ios/"*.swift)
DEMO_APP_SOURCES=("${DEMO_SCRIPT_DIR}/"*.swift)

if ! rustup "+${DEMO_TOOLCHAIN}" target list --installed | grep -qx "${DEMO_TARGET}"; then
    print -u2 "missing Rust target: ${DEMO_TARGET}"
    print -u2 "install it with: rustup +${DEMO_TOOLCHAIN} target add ${DEMO_TARGET}"
    exit 2
fi

DEMO_DEVICE_ID=${1:-}
if [[ -z "${DEMO_DEVICE_ID}" ]]; then
    DEMO_DEVICE_ID=$(
        xcrun simctl list devices available |
            awk -F '[()]' '/iPhone/ { print $2; exit }'
    )
fi
if [[ -z "${DEMO_DEVICE_ID}" ]]; then
    print -u2 "no available iPhone Simulator was found"
    exit 2
fi

cargo "+${DEMO_TOOLCHAIN}" build \
    --locked \
    --target "${DEMO_TARGET}" \
    -p rish-ffi

mkdir -p "${DEMO_APP}"
cp "${DEMO_SCRIPT_DIR}/Info.plist" "${DEMO_APP}/Info.plist"
xcrun --sdk iphonesimulator swiftc \
    -target arm64-apple-ios18.0-simulator \
    -parse-as-library \
    -warnings-as-errors \
    -import-objc-header "${DEMO_REPOSITORY_ROOT}/platform/rish.h" \
    "${DEMO_PLATFORM_SOURCES[@]}" \
    "${DEMO_APP_SOURCES[@]}" \
    "${DEMO_REPOSITORY_ROOT}/target/${DEMO_TARGET}/debug/librish_ffi.a" \
    -framework Foundation \
    -framework UIKit \
    -o "${DEMO_APP}/RishDemo"
codesign --force --sign - "${DEMO_APP}"

xcrun simctl boot "${DEMO_DEVICE_ID}" >/dev/null 2>&1 || true
xcrun simctl bootstatus "${DEMO_DEVICE_ID}" -b
xcrun simctl terminate "${DEMO_DEVICE_ID}" "${DEMO_BUNDLE_ID}" >/dev/null 2>&1 || true
xcrun simctl install "${DEMO_DEVICE_ID}" "${DEMO_APP}"
xcrun simctl launch \
    "${DEMO_DEVICE_ID}" \
    "${DEMO_BUNDLE_ID}" \
    --rish-demo-run-id \
    "${DEMO_RUN_ID}"

DEMO_RESULT=
for _ in {1..100}; do
    DEMO_CONTAINER=$(
        xcrun simctl get_app_container \
            "${DEMO_DEVICE_ID}" \
            "${DEMO_BUNDLE_ID}" \
            data 2>/dev/null || true
    )
    if [[ -n "${DEMO_CONTAINER}" ]]; then
        DEMO_RESULT="${DEMO_CONTAINER}/Documents/RishDemoResult.txt"
        if [[ -f "${DEMO_RESULT}" ]] &&
            grep -Fqx "RISH_DEMO run_id=${DEMO_RUN_ID}" "${DEMO_RESULT}"; then
            break
        fi
    fi
    sleep 0.1
done

if [[ -z "${DEMO_RESULT}" || ! -f "${DEMO_RESULT}" ]]; then
    print -u2 "the app launched but did not write its result within 10 seconds"
    exit 1
fi

print "simulator=${DEMO_DEVICE_ID}"
print "result=${DEMO_RESULT}"
print
command cat "${DEMO_RESULT}"
grep -Fqx "RISH_DEMO run_id=${DEMO_RUN_ID}" "${DEMO_RESULT}"
grep -q '^RISH_DEMO PASS$' "${DEMO_RESULT}"
