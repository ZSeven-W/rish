#!/bin/sh
set -eu

demo_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
rish_root=$(CDPATH= cd -- "${demo_directory}/../.." && pwd -P)
native_sdk=${RISH_OHOS_NATIVE_SDK:-}

if [ -z "${native_sdk}" ]; then
  printf '%s\n' \
    'RISH_OHOS_NATIVE_SDK must point to the OpenHarmony native SDK.' >&2
  exit 2
fi

clang="${native_sdk}/llvm/bin/clang"
sysroot="${native_sdk}/sysroot"
if [ ! -x "${clang}" ] || [ ! -d "${sysroot}" ]; then
  printf 'Invalid OpenHarmony native SDK: %s\n' "${native_sdk}" >&2
  exit 2
fi

rustup target add \
  --toolchain 1.85.0 \
  aarch64-unknown-linux-ohos
rish_separator=$(printf '\037')
rish_rustflags="-Clink-arg=--target=aarch64-linux-ohos"
rish_rustflags="${rish_rustflags}${rish_separator}"
rish_rustflags="${rish_rustflags}-Clink-arg=--sysroot=${sysroot}"

(
  cd "${rish_root}"
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER="${clang}" \
    CARGO_ENCODED_RUSTFLAGS="${rish_rustflags}" \
    cargo +1.85.0 build \
      --locked \
      --release \
      --target aarch64-unknown-linux-ohos \
      -p rish-ffi
)

library_directory="${demo_directory}/entry/libs/arm64-v8a"
mkdir -p "${library_directory}"
cp \
  "${rish_root}/target/aarch64-unknown-linux-ohos/release/librish_ffi.so" \
  "${library_directory}/librish_ffi.so"
printf 'Installed %s/librish_ffi.so\n' "${library_directory}"
