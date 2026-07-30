# rish Android demo

This APK compiles the production Kotlin and JNI bridges from `platform/android`
and links them to the Rust `rish-ffi` Android `cdylib`. On launch it:

1. plans `grep needle` and verifies `portable_applet/grep`;
2. executes `echo "hello from Android"` in an app-private sandbox;
3. executes `sha256sum` over the bytes `abc` and verifies the known digest.

Requirements are Android SDK API 35, build-tools `36.1.0`, NDK
`27.1.12297006`, CMake `3.22.1`, the `aarch64-linux-android` Rust target,
JDK 17+, and a Kotlin 2.0–2.2 compiler bundled with IntelliJ IDEA or Android
Studio. Run:

```bash
chmod +x run-demo.sh
./run-demo.sh
```

The script uses local SDK tools directly, so it does not need Gradle or Maven
network access. It builds the Rust library and APK. If `adb` has an online
arm64 device or emulator, it also installs the APK, launches the activity,
and prints the `RishAndroidDemo` logcat record. The same report is visible in
the activity. Set `KOTLIN_HOME`, `KOTLINC_BIN`, or `KOTLIN_LIB_DIR` when the
compiler is installed somewhere other than the default macOS application
locations. Build-tools `36.1.0` currently support Kotlin metadata through
2.2, so a newer compiler must not be selected for this direct-D8 build. When
more than one device is online, set `RISH_ANDROID_SERIAL` to the intended
`adb` serial; the script refuses to guess.

This demonstrates portable Rust applet semantics inside an Android
application sandbox. It does not claim Android kernel namespaces, cgroups,
systemd, kernel modules, privileged containers, or Docker-in-Docker.
