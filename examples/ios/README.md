# iOS Simulator demo

This app links the real `rish-ffi` Rust static library and the production iOS
bridges. Its first tab can stream a real OCI image pull over HTTPS into the
app-owned content store, with bounded response sizes, SHA-256 verification,
per-blob progress, cancellation, and a verified receipt. The platform picker
selects exact `linux/arm64/v8` or `linux/amd64` index entries. Pulling
content does not start a container or execute image binaries.

The Runtime tab checks that `grep` plans as a `portable_applet`, executes the
Rust `echo` applet inside an app-owned container directory, and renders plus
persists the returned stdout.

Run it from the repository root:

```sh
rustup +1.85.0 target add aarch64-apple-ios-sim
examples/ios/run-simulator.sh
```

Pass a Simulator UDID as the first argument to select a particular device.
The script builds and signs a Simulator `.app`, boots the selected device,
installs and launches the app, then reads `RishDemoResult.txt` from its data
container. A successful run ends with:

```text
RISH_DEMO run_id=<fresh UUID>
RISH_DEMO planner.kind=portable_applet planner.name=grep
RISH_DEMO applet.kind=portable_applet applet.name=echo
RISH_DEMO applet.exit_code=0
RISH_DEMO applet.stdout=hello-from-rish-ios
RISH_DEMO PASS
```

Each launch carries a fresh run identifier, so the script cannot accept a
result left behind by an earlier Simulator process. The script does not call
`simctl uninstall`.

Normal launches never start a network request. UI automation may opt in once
per process by passing:

```text
--rish-auto-pull alpine:latest
--rish-auto-platform linux/amd64
```

The platform flag is optional and defaults to `linux/arm64/v8`; any other
token fails closed.

The app atomically writes a short versioned result to
`Documents/RishPullResult.json` when that pull succeeds, fails, or is
cancelled. The regular `run-simulator.sh` success check remains the local
Runtime self-test and does not wait for an image pull.

For a real end-to-end pull check, including a fresh result, every pinned CAS
blob's SHA-256, and the receipt's verified byte total, run:

```sh
examples/ios/run-pull-simulator.sh \
    <Simulator-UDID> \
    alpine:latest \
    linux/amd64
```

The third argument accepts only `linux/arm64/v8` or `linux/amd64` and
defaults to ARM64/v8.

Set `RISH_PULL_SCREENSHOT` to an absolute `.png` path to capture the completed
screen. The script uses the host's existing network and proxy configuration;
it does not weaken TLS or install a registry certificate.

This demonstrates portable userspace command semantics. It does not claim
iOS namespaces, cgroups, device nodes, kernel modules, `systemd`, or a
privileged Linux container.
