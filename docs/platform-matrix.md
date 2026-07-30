# Platform capability matrix

本文件记录截至 2026-07-30 能够作为产品承诺的平台边界。所有能力都要在
运行时重新探测；系统版本、`root` 状态或 `/dev/kvm` 文件存在本身都不是
充分条件。

## 总览

| 环境 | Portable offload | 软件 Full VM | 加速 VM | Native Linux |
|---|---:|---:|---:|---:|
| stock iOS App | 支持 | 可行但慢 | 不作为产品基线 | 不可能 |
| 越狱/特殊 iOS | 支持 | 支持 | 设备/版本相关 | 不可能，宿主是 XNU |
| 普通 Android App | 支持 | 可行但慢 | 通常无权限 | 不可承诺 |
| Android OEM/系统 | 支持 | 支持 | AVF/crosvm/KVM | 定制内核后可选 |
| 商用 HarmonyOS HAP | 支持 | 可行但慢 | 无公开 API | 不可能 |
| OpenHarmony OEM | 支持 | 支持 | KVM/系统集成 | 定制系统后可选 |

## iOS

普通 iOS App 在沙盒内运行，不能把下载的 ARM64 ELF 当作本机代码直接
执行。Apple 的运行时安全模型限制动态生成/执行代码；公开的
Virtualization 和 Hypervisor framework 也不能作为普通 iOS App 的产品
基线：

- [Apple Platform Security: runtime process security](https://support.apple.com/guide/security/runtime-process-security-sec15bfe098e/web)
- [Apple Virtualization framework](https://developer.apple.com/documentation/virtualization)
- [Apple Hypervisor framework](https://developer.apple.com/documentation/hypervisor)
- [UTM iOS 后端与安装矩阵](https://docs.getutm.app/installation/ios/)

因此：

- 快速路径是 native offload 和语义控制面。
- 完整兼容路径是无 JIT 的全系统解释器，真实功能存在于 Linux guest。
- `privileged` 只能是 guest root。
- guest 模块不能控制 iPhone 的真实硬件。
- guest netns 可以完整，但宿主侧通常只能用户态 NAT/端口代理。
- 不能承诺 dockerd 在后台 24×7 常驻。

拉取并执行任意 OCI 内容还需要单独评估
[App Review Guidelines 2.5.2/4.7](https://developer.apple.com/app-store/review/guidelines/)；
模拟器可上架并不自动意味着通用 OCI Registry 一定通过审核。

## Android

普通 App 同时受到 UID sandbox、SELinux 和 seccomp 约束：

- [Android application sandbox](https://source.android.com/docs/security/app-sandbox)
- [Android SELinux](https://source.android.com/docs/security/features/selinux)
- [Android 16 app seccomp blocklist](https://android.googlesource.com/platform/bionic/+/refs/heads/android16-release/libc/SECCOMP_BLOCKLIST_APP.TXT)

当前 AOSP arm64 GKI baseline 虽包含 cgroups、KVM、overlayfs 和大量网络
功能，但 `CONFIG_PID_NS` 关闭，`CONFIG_USER_NS` 也不能作为设备通用假设：

- [Android GKI arm64 defconfig](https://android.googlesource.com/kernel/common/+/refs/heads/android16-6.12/arch/arm64/configs/gki_defconfig)
- [Android cgroups/task profiles](https://source.android.com/docs/core/perf/cgroups)

所以仅获得 root 或 Shizuku 不足以承诺 Docker host。主路线是：

1. OEM/预装环境使用 AVF/crosvm。
2. 明确允许 `/dev/kvm` 与相关 ioctl/SELinux policy 时使用 KVM VMM。
3. 其他设备回退软件 Full VM。
4. Native Linux 仅用于自有 kernel/init/SELinux 的受控硬件。

AVF 权限属于系统集成能力，并非普通第三方 App API：

- [AVF framework permissions](https://android.googlesource.com/platform/packages/modules/Virtualization/+/HEAD/libs/framework-virtualization/README.md)
- [Android Linux development environment](https://source.android.com/docs/core/virtualization/usecases#linux_development_environment)
- [crosvm](https://android.googlesource.com/platform/external/crosvm/+/refs/tags/android-17.0.0_r1/README.md)

## HarmonyOS / OpenHarmony

Rust 官方支持 `aarch64-unknown-linux-ohos`，但 Rust target 可编译并不表示
普通 HAP 获得 Linux 特权：

- [Rust OpenHarmony target](https://doc.rust-lang.org/rustc/platform-support/openharmony.html)
- [OpenHarmony application sandbox](https://gitcode.com/openharmony/docs/blob/master/en/application-dev/file-management/app-sandbox-directory.md)
- [NDK libc permission limitations](https://gitcode.com/openharmony/docs/blob/master/en/application-dev/reference/native-lib/guidance-on-ndk-libc-interfaces-affected-by-permissions.md)

普通 HAP 不能依赖 `setns`、`unshare`、`mount`、`pivot_root`、`mknod`、
`init_module` 或任意 `/dev`。HNP 可以把预先签名的 Python/Node/Java 等
程序随 HAP 分发，但不能把运行时拉取的未知 OCI ELF 变成原生可执行文件：

- [Harmony Native Package](https://gitcode.com/openharmony/startup_appspawn/tree/master/service/hnp)
- [OpenHarmony code signing/XPM](https://gitcode.com/openharmony/security_code_signature/blob/master/README.md)

OpenHarmony OEM 系统可以采用：

- root SystemAbility/init daemon + 定制 namespaces/cgroups/SELinux。
- KVM Linux guest，作为 systemd、DinD 和不受信任镜像的首选隔离边界。

官方 Linux 6.6 standard config 已有较完整的 namespace/cgroup 基础，但
具体板级配置仍可能缺 overlayfs、veth 或 bridge，必须逐项验证：

- [OpenHarmony Linux 6.6 standard defconfig](https://gitcode.com/openharmony/kernel_linux_config/blob/master/linux-6.6/type/standard_defconfig)
- [RK3568 board defconfig](https://gitcode.com/openharmony/kernel_linux_config/blob/master/linux-6.6/arch/arm64/configs/rk3568_standard_defconfig)
- [QEMU/KVM board defconfig](https://gitcode.com/openharmony/kernel_linux_config/blob/master/linux-6.6/arch/arm64/configs/qemu-arm-linux_standard_defconfig)

## 高级能力的准确作用域

| 用户可见能力 | stock App | Full VM | OEM Native |
|---|---|---|---|
| namespaces/cgroups | Rust 语义模型 | guest Linux 原生 | 宿主 Linux 原生 |
| privileged | 不支持 | guest 范围 | 宿主范围，高风险 |
| kernel module | handler 拒绝/虚拟设备 | guest `.ko` | 签名/KMI 匹配的宿主 `.ko` |
| `/dev` | allow-listed device proxy | devtmpfs/virtio | SELinux allow-listed 宿主设备 |
| systemd | API/supervisor 兼容层 | 真正 PID 1 | 取决于系统集成 |
| Docker-in-Docker | 不支持 | 支持 | 仅自控系统 |
| network namespace | 逻辑网络 + socket proxy | veth/bridge/nft | 宿主 netns/netlink |
| 复杂 syscall | 仅已知 handler | Linux guest kernel | Linux host kernel |
