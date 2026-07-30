# iOS Simulator demo

This minimal app links the real `rish-ffi` Rust static library and the
production `RishBridge.swift`. It checks that `grep` plans as a
`portable_applet`, executes the Rust `echo` applet inside an app-owned
container directory, and renders plus persists the returned stdout.

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

This demonstrates portable userspace command semantics. It does not claim
iOS namespaces, cgroups, device nodes, kernel modules, `systemd`, or a
privileged Linux container.
