#!/bin/sh
set -eu

demo_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
rish_root=$(CDPATH= cd -- "${demo_directory}/../.." && pwd -P)
demo_temp_root=${TMPDIR:-/tmp}
build_directory=$(mktemp -d "${demo_temp_root}/rish-harmony-build.XXXXXX")
sandbox_directory=$(mktemp -d "${demo_temp_root}/rish-harmony-sandbox.XXXXXX")
sandbox_directory=$(CDPATH= cd -- "${sandbox_directory}" && pwd -P)
demo_binary="${build_directory}/rish-harmony-host-smoke"

cleanup() {
  rm -f "${demo_binary}"
  rmdir "${build_directory}" "${sandbox_directory}" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

(
  cd "${rish_root}"
  cargo +1.85.0 build --locked -p rish-ffi
)

clang++ \
  -std=c++17 \
  -Wall \
  -Wextra \
  -Werror \
  "${demo_directory}/host_smoke.cpp" \
  -I"${rish_root}/platform" \
  -L"${rish_root}/target/debug" \
  -lrish_ffi \
  -Wl,-rpath,"${rish_root}/target/debug" \
  -o "${demo_binary}"

"${demo_binary}" "${sandbox_directory}"
