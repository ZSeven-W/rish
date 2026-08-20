#!/bin/zsh
set -euo pipefail

PULL_SCRIPT_DIR=${0:A:h}
PULL_REPOSITORY_ROOT=${PULL_SCRIPT_DIR:h:h}
PULL_DEVICE_ID=${1:-}
PULL_REFERENCE=${2:-alpine:latest}
PULL_PLATFORM=${3:-${RISH_PULL_PLATFORM:-linux/arm64/v8}}
PULL_BUNDLE_ID=dev.rish.demo
PULL_TIMEOUT_SECONDS=${RISH_PULL_TIMEOUT_SECONDS:-300}

case "${PULL_PLATFORM}" in
    linux/arm64/v8)
        PULL_EXPECTED_ARCHITECTURE=arm64
        PULL_EXPECTED_VARIANT=v8
        ;;
    linux/amd64)
        PULL_EXPECTED_ARCHITECTURE=amd64
        PULL_EXPECTED_VARIANT=
        ;;
    *)
        print -u2 "platform must be linux/arm64/v8 or linux/amd64"
        exit 2
        ;;
esac

if [[ -z "${PULL_DEVICE_ID}" ]]; then
    PULL_DEVICE_ID=$(
        xcrun simctl list devices available |
            awk -F '[()]' '/iPhone/ { print $2; exit }'
    )
fi
if [[ -z "${PULL_DEVICE_ID}" ]]; then
    print -u2 "no available iPhone Simulator was found"
    exit 2
fi
if ! [[ "${PULL_TIMEOUT_SECONDS}" =~ '^[1-9][0-9]*$' ]]; then
    print -u2 "RISH_PULL_TIMEOUT_SECONDS must be a positive integer"
    exit 2
fi

"${PULL_SCRIPT_DIR}/run-simulator.sh" "${PULL_DEVICE_ID}"

PULL_CONTAINER=$(
    xcrun simctl get_app_container \
        "${PULL_DEVICE_ID}" \
        "${PULL_BUNDLE_ID}" \
        data
)
PULL_RESULT="${PULL_CONTAINER}/Documents/RishPullResult.json"
PULL_PREVIOUS_SIGNATURE=missing
if [[ -f "${PULL_RESULT}" ]]; then
    PULL_PREVIOUS_SIGNATURE=$(stat -f '%i:%m:%c:%z' "${PULL_RESULT}")
fi

xcrun simctl terminate \
    "${PULL_DEVICE_ID}" \
    "${PULL_BUNDLE_ID}" >/dev/null 2>&1 || true
xcrun simctl launch \
    "${PULL_DEVICE_ID}" \
    "${PULL_BUNDLE_ID}" \
    --rish-demo-run-id \
    "pull-$(uuidgen | tr '[:upper:]' '[:lower:]')" \
    --rish-auto-pull \
    "${PULL_REFERENCE}" \
    --rish-auto-platform \
    "${PULL_PLATFORM}"

PULL_ATTEMPTS=$((PULL_TIMEOUT_SECONDS * 4))
for _ in {1..${PULL_ATTEMPTS}}; do
    if [[ -f "${PULL_RESULT}" ]]; then
        PULL_SIGNATURE=$(stat -f '%i:%m:%c:%z' "${PULL_RESULT}")
        if [[ "${PULL_SIGNATURE}" != "${PULL_PREVIOUS_SIGNATURE}" ]]; then
            break
        fi
    fi
    sleep 0.25
done

if [[ ! -f "${PULL_RESULT}" ]]; then
    print -u2 "the app did not publish a pull result"
    exit 1
fi
PULL_SIGNATURE=$(stat -f '%i:%m:%c:%z' "${PULL_RESULT}")
if [[ "${PULL_SIGNATURE}" == "${PULL_PREVIOUS_SIGNATURE}" ]]; then
    print -u2 "the pull did not finish within ${PULL_TIMEOUT_SECONDS} seconds"
    exit 1
fi

jq -e \
    --arg architecture "${PULL_EXPECTED_ARCHITECTURE}" \
    --arg variant "${PULL_EXPECTED_VARIANT}" '
    .protocol_version == 1
    and .ok == true
    and .platform.os == "linux"
    and .platform.architecture == $architecture
    and (
        if $variant == ""
        then .platform.variant == null
        else .platform.variant == null or .platform.variant == $variant
        end
    )
    and (.resolved_digest | startswith("sha256:"))
    and (.cas_pin | length > 0)
    and (.verified_bytes > 0)
' "${PULL_RESULT}" >/dev/null || {
    command jq . "${PULL_RESULT}"
    exit 1
}

PULL_PIN=$(jq -er '.cas_pin' "${PULL_RESULT}")
PULL_VERIFIED_BYTES=$(jq -er '.verified_bytes' "${PULL_RESULT}")
PULL_STORE="${PULL_CONTAINER}/rish-oci-store"
PULL_PIN_DIRECTORY="${PULL_STORE}/pins/${PULL_PIN}"
if [[ ! -d "${PULL_PIN_DIRECTORY}" ]]; then
    print -u2 "verified CAS pin directory is missing"
    exit 1
fi

PULL_SUM=0
PULL_COUNT=0
for marker in "${PULL_PIN_DIRECTORY}"/*; do
    [[ -f "${marker}" ]] || continue
    PULL_DIGEST=${marker:t}
    PULL_BLOB="${PULL_STORE}/blobs/sha256/${PULL_DIGEST}"
    if [[ ! -f "${PULL_BLOB}" ]]; then
        print -u2 "pinned blob is missing: ${PULL_DIGEST}"
        exit 1
    fi
    PULL_ACTUAL=$(shasum -a 256 "${PULL_BLOB}" | awk '{ print $1 }')
    if [[ "${PULL_ACTUAL}" != "${PULL_DIGEST}" ]]; then
        print -u2 "pinned blob digest mismatch: ${PULL_DIGEST}"
        exit 1
    fi
    PULL_SUM=$((PULL_SUM + $(stat -f %z "${PULL_BLOB}")))
    PULL_COUNT=$((PULL_COUNT + 1))
done

if (( PULL_COUNT == 0 || PULL_SUM != PULL_VERIFIED_BYTES )); then
    print -u2 \
        "verified graph mismatch: blobs=${PULL_COUNT} bytes=${PULL_SUM}, receipt=${PULL_VERIFIED_BYTES}"
    exit 1
fi

if [[ -n "${RISH_PULL_SCREENSHOT:-}" ]]; then
    xcrun simctl io \
        "${PULL_DEVICE_ID}" \
        screenshot \
        "${RISH_PULL_SCREENSHOT}"
fi

print "simulator=${PULL_DEVICE_ID}"
print "platform=${PULL_PLATFORM}"
print "result=${PULL_RESULT}"
print "cas=${PULL_STORE}"
print "verified_blobs=${PULL_COUNT}"
print "verified_bytes=${PULL_SUM}"
command jq . "${PULL_RESULT}"
