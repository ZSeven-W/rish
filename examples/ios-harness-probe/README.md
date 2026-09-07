# On-device Harness compatibility probe

This development-only iOS application boots a bundled x86-64 Linux guest with
the pure-Rust interpreter, then runs a bounded list of guest commands. It uses
the same public C ABI as the app's pinned XCFramework. It does not run any
Harness on the Mac, collect credential files, or modify
the Rish app's workspace. It is not a subscription-login implementation.

Guest networking defaults to disabled. An explicit `"network": "user-nat"`
configuration enables the guest's on-device outbound network backend for
official authentication experiments. Stream callbacks display process output
before exit; the guest process retains ownership of its own credential files.

`Documents/harness-probe.json` records a unique run ID, device OS, process ID,
elapsed stage timings, command arguments, and actual guest replies. A missing
`probe-finished` means execution did not finish. Inspect each `exit_code`:
finishing the probe does not imply that every program succeeded.

## Prepare and run

Prerequisites: Xcode, XcodeGen, Python 3.9+, an iOS-capable rish XCFramework
with `rish_vm_session_exec_stream_json`, the pinned Alpine kernel, and an initramfs built with
`guest/x86_64/build-container-initramfs.sh`.

```sh
python3 examples/ios-harness-probe/prepare.py \
  --kernel guest/x86_64/out/downloads/vmlinuz-virt-6.18.35 \
  --initrd guest/x86_64/out/rish-container.cpio \
  --xcframework /absolute/path/rish_ffi.xcframework \
  --header platform/rish.h \
  --configuration examples/ios-harness-probe/baseline.json \
  --team YOUR_TEAM_ID \
  --output /tmp/rish-harness-probe

xcodebuild -project /tmp/rish-harness-probe/RishHarnessProbe.xcodeproj \
  -scheme RishHarnessProbe -configuration Release \
  -destination 'generic/platform=iOS' \
  -derivedDataPath /tmp/rish-harness-probe-build \
  -allowProvisioningUpdates build
```

Install the built app with `devicectl`, launch its bundle ID, and copy the JSON
receipt from its app data container. `prepare.py --bundle-id` allows an
explicitly selected test-app identity when a free developer profile is at its
installed-app limit. Do not use the product's bundle ID. Retain the original
test runner bundle and reinstall it when the experiment ends.

For official Harness compatibility, stage the unmodified vendor Linux x64
binary and its shared-library closure into a separate initramfs. Verify the
archive against the vendor package's integrity digest, record its version and
SHA256, and use a configuration that runs `--version` and `--help` before any
authentication. A shell boot, package extraction, or API request alone is not
evidence that an official Harness can execute.

No guest artifacts, vendor binaries, auth files, or generated Xcode projects
belong in the source commit. Preserve each experiment's input manifest and
receipt with the product's acceptance records.

When the Mac's download route is unreliable, a separate `downloads`
configuration can prepare the same fixtures through native iOS HTTPS. Each
descriptor requires `name` (`codex.tgz` or `claude.tgz`), the official npm
tarball `url`, and its base64 `sha512` from package metadata. Do not include
`commands` in this mode. Successful archives are written into the probe's
Documents directory only after digest verification. Download receipts are
labelled `fixture-verified`; they do not demonstrate guest execution.
