# HarmonyOS Stage demo

This is a minimal Stage-model HAP that exercises the same Rust ABI used by
the product adapter:

1. plan `grep` and require `portable_applet:grep`;
2. execute `echo hello from HarmonyOS`;
3. execute `sha256sum` over `hello from HarmonyOS\n` on stdin.

The page displays the three results. These are portable Rust applets running
inside the HAP sandbox. They are not Guest ELF execution or Linux kernel
semantics.

## Prepare the HAP

Run:

```sh
./prepare_hap.sh
```

This copies the canonical ArkTS bridge and its N-API declaration from
`platform/harmony` into the demo module. The copied files are generated and
git-ignored so the demo cannot silently fork the production bridge.

Build the Rust cdylib with an OpenHarmony native SDK:

```sh
RISH_OHOS_NATIVE_SDK=/absolute/path/to/openharmony/native \
  ./build_rust_ohos.sh
```

`RISH_OHOS_NATIVE_SDK` must contain `llvm/bin/clang` and `sysroot`. The script
builds `rish-ffi` for `aarch64-unknown-linux-ohos` and installs
`librish_ffi.so` under `entry/libs/arm64-v8a`, the HAP's packaged native
library directory.

Open this directory as a project in a compatible DevEco Studio installation,
select the installed HarmonyOS SDK, configure debug signing, and run the
`entry` module on a phone, emulator, or remote simulator. The included CMake
target builds `librish_napi.so` and links it to the Rust library.

The SDK/toolchain setup follows Rust's
[OpenHarmony target guide](https://doc.rust-lang.org/stable/rustc/platform-support/openharmony.html).
The project and module JSON5 layout follows the HarmonyOS
[Stage configuration model](https://developer.huawei.com/consumer/cn/doc/doccenter-getting-started/application-configuration-file-overview-stage).

## Host smoke test

When no HarmonyOS SDK or device is available, run:

```sh
./run_host_smoke.sh
```

This compiles a small macOS harness, links the real `rish-ffi` cdylib, sends
the same three versioned JSON operations, and validates their output. It tests
the Rust C ABI and command behavior only. A passing host smoke test is not a
HarmonyOS HAP build, emulator run, or device run.

## Expected UI output

```text
protocol: 1
grep plan: portable_applet:grep
echo: hello from HarmonyOS
sha256sum: 455788b5b41c3bcbb2cd5ef572079294a44a8ed1b7b74c17ec0680cd2bf97f27  -
```

The low-level `executeAppletJson` export remains a trusted HAP implementation
detail. The demo UI invokes only fixed commands through
`executePortableApplet`; it does not accept an arbitrary sandbox root or
program from UI input.
