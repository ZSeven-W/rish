#!/usr/bin/env bash
set -euo pipefail

demo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "${demo_dir}/../.." && pwd)"
android_sdk="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}"
ndk_version="27.1.12297006"
build_tools_version="36.1.0"
rust_target="aarch64-linux-android"
android_abi="arm64-v8a"

if [[ -z "${android_sdk}" || ! -d "${android_sdk}" ]]; then
  echo "ANDROID_SDK_ROOT or ANDROID_HOME must name an installed Android SDK" >&2
  exit 2
fi

case "$(uname -s)" in
  Darwin) ndk_host="darwin-x86_64" ;;
  Linux) ndk_host="linux-x86_64" ;;
  *)
    echo "unsupported NDK host: $(uname -s)" >&2
    exit 2
    ;;
esac

ndk_root="${android_sdk}/ndk/${ndk_version}"
linker="${ndk_root}/toolchains/llvm/prebuilt/${ndk_host}/bin/aarch64-linux-android23-clang"
if [[ ! -x "${linker}" ]]; then
  echo "Android NDK ${ndk_version} is required: missing ${linker}" >&2
  exit 2
fi

echo "[1/4] Building Rust cdylib for ${rust_target}"
(
  cd "${repository_root}"
  CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="${linker}" \
    cargo +1.85.0 build --locked -p rish-ffi --target "${rust_target}"
)

jni_directory="${demo_dir}/app/src/main/jniLibs/${android_abi}"
mkdir -p "${jni_directory}"
cp \
  "${repository_root}/target/${rust_target}/debug/librish_ffi.so" \
  "${jni_directory}/librish_ffi.so"

build_tools="${android_sdk}/build-tools/${build_tools_version}"
android_jar="${android_sdk}/platforms/android-35/android.jar"
cmake_bin="${android_sdk}/cmake/3.22.1/bin/cmake"
ninja_bin="${android_sdk}/cmake/3.22.1/bin/ninja"
default_kotlin_home="/Applications/IntelliJ IDEA.app/Contents/plugins/Kotlin/kotlinc"
if [[ ! -d "${default_kotlin_home}" ]]; then
  default_kotlin_home="/Applications/Android Studio.app/Contents/plugins/Kotlin/kotlinc"
fi
kotlin_home="${KOTLIN_HOME:-${default_kotlin_home}}"
kotlinc_bin="${KOTLINC_BIN:-${kotlin_home}/bin/kotlinc}"
kotlin_lib="${KOTLIN_LIB_DIR:-${kotlin_home}/lib}"

required_tools=(
  "${build_tools}/aapt2"
  "${build_tools}/d8"
  "${build_tools}/apksigner"
  "${build_tools}/zipalign"
  "${cmake_bin}"
  "${ninja_bin}"
  "${kotlinc_bin}"
)
for required_tool in "${required_tools[@]}"; do
  if [[ ! -x "${required_tool}" ]]; then
    echo "required Android build tool is missing: ${required_tool}" >&2
    exit 2
  fi
done
if [[ ! -f "${android_jar}" ]]; then
  echo "Android SDK platform 35 is required: missing ${android_jar}" >&2
  exit 2
fi

manual_build="${demo_dir}/app/build/manual"
native_build="${manual_build}/native"
dex_build="${manual_build}/dex"
package_staging="${manual_build}/package"
classes_jar="${manual_build}/demo-classes.jar"
base_apk="${manual_build}/base.apk"
unaligned_apk="${manual_build}/app-debug-unaligned.apk"
aligned_apk="${manual_build}/app-debug-aligned.apk"
apk="${demo_dir}/app/build/outputs/apk/debug/app-debug.apk"

rm -rf "${manual_build}"
mkdir -p \
  "${native_build}" \
  "${dex_build}" \
  "${package_staging}/lib/${android_abi}" \
  "$(dirname "${apk}")"

echo "[2/4] Compiling the production Kotlin bridge and demo Activity"
"${kotlinc_bin}" \
  -Werror \
  -jvm-target 17 \
  -classpath "${android_jar}" \
  -d "${classes_jar}" \
  "${repository_root}/platform/android/RishBridge.kt" \
  "${demo_dir}/app/src/main/java/dev/rish/demo/MainActivity.kt"

"${build_tools}/d8" \
  --lib "${android_jar}" \
  --min-api 23 \
  --output "${dex_build}" \
  "${classes_jar}" \
  "${kotlin_lib}/kotlin-stdlib.jar" \
  "${kotlin_lib}/kotlin-stdlib-jdk7.jar" \
  "${kotlin_lib}/kotlin-stdlib-jdk8.jar"

echo "[3/4] Building JNI and assembling a signed APK"
"${cmake_bin}" \
  -S "${demo_dir}/app/src/main/cpp" \
  -B "${native_build}" \
  -G Ninja \
  "-DCMAKE_MAKE_PROGRAM=${ninja_bin}" \
  "-DCMAKE_TOOLCHAIN_FILE=${ndk_root}/build/cmake/android.toolchain.cmake" \
  "-DANDROID_ABI=${android_abi}" \
  -DANDROID_PLATFORM=android-23 \
  -DCMAKE_BUILD_TYPE=Debug
"${cmake_bin}" --build "${native_build}"

"${build_tools}/aapt2" link \
  -o "${base_apk}" \
  -I "${android_jar}" \
  --manifest "${demo_dir}/app/src/main/AndroidManifest.xml" \
  --min-sdk-version 23 \
  --target-sdk-version 35 \
  --version-code 1 \
  --version-name 0.1

cp "${dex_build}/classes.dex" "${package_staging}/classes.dex"
cp \
  "${jni_directory}/librish_ffi.so" \
  "${package_staging}/lib/${android_abi}/librish_ffi.so"
cp \
  "${native_build}/librish_jni.so" \
  "${package_staging}/lib/${android_abi}/librish_jni.so"
cp "${base_apk}" "${unaligned_apk}"
(
  cd "${package_staging}"
  zip -q -u -r "${unaligned_apk}" classes.dex lib
)

"${build_tools}/zipalign" -f -p 4 "${unaligned_apk}" "${aligned_apk}"
debug_keystore="${RISH_ANDROID_KEYSTORE:-${HOME}/.android/debug.keystore}"
if [[ ! -f "${debug_keystore}" ]]; then
  debug_keystore="${manual_build}/debug.keystore"
  keytool -genkeypair \
    -keystore "${debug_keystore}" \
    -storepass android \
    -keypass android \
    -alias androiddebugkey \
    -keyalg RSA \
    -keysize 2048 \
    -validity 10000 \
    -dname "CN=Android Debug,O=Android,C=US"
fi
"${build_tools}/apksigner" sign \
  --ks "${debug_keystore}" \
  --ks-pass pass:android \
  --key-pass pass:android \
  --out "${apk}" \
  "${aligned_apk}"
"${build_tools}/apksigner" verify --verbose "${apk}"
echo "APK: ${apk}"

adb_bin="${android_sdk}/platform-tools/adb"
requested_device="${RISH_ANDROID_SERIAL:-}"
online_devices="$(
  "${adb_bin}" devices |
    awk 'NR > 1 && $2 == "device" { print $1 }'
)"
if [[ -n "${requested_device}" ]]; then
  if ! printf '%s\n' "${online_devices}" |
    grep -Fqx "${requested_device}"; then
    echo "RISH_ANDROID_SERIAL is not an online device: ${requested_device}" >&2
    exit 2
  fi
  device="${requested_device}"
else
  device_count="$(
    printf '%s\n' "${online_devices}" |
      awk 'NF { count += 1 } END { print count + 0 }'
  )"
  if [[ "${device_count}" -gt 1 ]]; then
    echo "multiple Android devices are online; set RISH_ANDROID_SERIAL" >&2
    printf '%s\n' "${online_devices}" >&2
    exit 2
  fi
  device="${online_devices}"
fi
if [[ -z "${device}" ]]; then
  echo "[4/4] No online Android device; APK build is complete."
  exit 0
fi

echo "[4/4] Installing and running on ${device}"
"${adb_bin}" -s "${device}" install -r "${apk}"
"${adb_bin}" -s "${device}" shell am force-stop dev.rish.demo
"${adb_bin}" -s "${device}" shell am start -W -n dev.rish.demo/.MainActivity
demo_pid="$(
  "${adb_bin}" -s "${device}" shell pidof dev.rish.demo |
    tr -d '\r' |
    awk '{ print $1 }'
)"
if [[ -z "${demo_pid}" ]]; then
  echo "Android demo process did not start" >&2
  exit 1
fi

demo_log=""
for _ in {1..50}; do
  demo_log="$(
    "${adb_bin}" -s "${device}" logcat \
      -d \
      --pid="${demo_pid}" \
      -s RishAndroidDemo:I '*:S' 2>/dev/null || true
  )"
  if [[ "${demo_log}" == *"PASS — rish Android demo"* ]]; then
    break
  fi
  sleep 0.2
done
printf '%s\n' "${demo_log}"
if [[ "${demo_log}" != *"PASS — rish Android demo"* ]]; then
  echo "Android demo did not report PASS" >&2
  exit 1
fi
