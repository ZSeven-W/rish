#!/bin/sh
set -eu

demo_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
rish_root=$(CDPATH= cd -- "${demo_directory}/../.." && pwd -P)
generated_directory="${demo_directory}/entry/src/main/ets/rish"

mkdir -p "${generated_directory}"
cp "${rish_root}/platform/harmony/RishBridge.ets" \
  "${generated_directory}/RishBridge.ets"
cp "${rish_root}/platform/harmony/rish_ffi.d.ts" \
  "${generated_directory}/rish_ffi.d.ts"

printf 'Prepared HarmonyOS ArkTS bridge in %s\n' "${generated_directory}"
