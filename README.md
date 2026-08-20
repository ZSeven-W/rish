# rish

`rish` 是面向 iOS、Android 和鸿蒙的 Rust Linux 命令与容器语义运行时。

项目采用 **native-offload first、AMD64 Linux software-VM fallback** 的路线：

- 已知 Guest 命令由 Rust 拦截并分派给 Swift/Objective-C、Kotlin/Java、
  ArkTS 或系统特权服务。
- OCI 镜像可以请求原生 handler 契约；只有宿主 live allow-list 已绑定同名
  handler 时才会采用，无需解释执行其中的 ELF。
- 未知 ELF、真正的 systemd、Docker-in-Docker、内核模块和完整网络
  namespace 必须进入纯软件 x86_64 全系统模拟器中的真实 Linux guest，
  或经过探测的宿主 Linux 后端。
- 能力不足时失败关闭，不会把“模拟成功”伪装成真实内核隔离。

> 当前状态：P1 数据面原型 + P3 软模拟主线。能力模型、三端 native-offload
> SDK、OCI Registry/CAS、安全解层、事务 rootfs snapshot、Guest RPC 与
> bootstrap agent 已可编译测试；iOS Simulator 已真实拉取并校验 Docker Hub
> 的 `alpine:latest` `linux/amd64` 图。方向（ADR-0003）：iOS/Android 通过
> 仓库自研的纯 Rust 无 JIT x86_64 全系统解释器（rish-softvm-core）运行
> docker，不使用 UTM/QEMU。解释器核心（CPU/分页/中断/8259/8254/CMOS/16550
> 与首批指令子集）已落地并有 59 项测试；docker 诊断 guest（pinned kernel +
> 模块树 + 静态 Docker 工具链 + agent PID 1）已可复现构建。待办：SSE2/LAPIC
> /bzImage 装载与 Linux boot 调试。

## 架构

```text
Guest command / OCI image
             │
             ▼
  Rust capability negotiation
       ┌─────┼─────────────┐
       ▼     ▼             ▼
 Portable   Full VM      Native Linux
 offload    backend      backend
       │     │             │
 Swift/     Real Linux    Probed host
 Java/      guest         kernel
 ArkTS      kernel        features
```

### Portable offload

普通移动 App 的默认后端。它提供版本化 JSON host-call 协议，并实现逻辑
namespace、cgroup、设备和服务状态模型。

`systemctl`、`docker`、`mount`、`unshare`、`ip` 等可以映射为平台 handler；
`dockerd`、`runc`、`modprobe` 等要求真实内核语义的操作不会在这个后端
假装成功。

### Full VM

高级兼容性的主后端。在 Linux guest 内提供：

- PID/user/mount/UTS/IPC/network namespaces
- cgroup v2
- systemd PID 1
- Docker、containerd、Youki 和 DinD
- guest 内核模块、devtmpfs、virtio `/dev`
- veth、bridge、TUN、nftables 和端口转发

这里的 `privileged`、内核模块和设备权限只作用于 **guest Linux**，不会
突破 iOS、Android 或鸿蒙宿主。

### Native Linux

仅用于经过运行时探测的 Android/鸿蒙 root、OEM 或系统镜像。是否可用不
根据“设备已 root”猜测，而是逐项探测 namespaces、cgroups、SELinux、
KVM、设备和内核配置。

## 当前仓库内容

```text
crates/
  rish-core/       公共命令、能力、后端与 host-call 协议
  rish-content/    SHA-256 CAS、process-local lease、persistent pin 与 GC
  rish-registry/   OCI 引用、manifest/index、Bearer challenge 与响应校验
  rish-pull/       有资源上限的 linux/arm64/v8 与 linux/amd64 拉取流水线
  rish-layer/      tar/gzip、diff-id、whiteout 与防路径逃逸的安全解层
  rish-snapshot/   私有 staging 和 atomic no-replace rootfs 发布
  rish-runtime/    路由、后端选择、namespace/cgroup/device/service 模型
  rish-oci/        OCI image config 和原生 offload 契约
  rish-vm/         evidence-gated VM 启动、设备配置和 guest 内核合同
  rish-guest-protocol/  Host/guest 握手、执行、OCI、端口和 checkpoint RPC
  rish-guest-agent/     Linux guest 内的失败关闭 bootstrap agent
  rish-guest-importer/  Guest 内流式校验、解层和 Linux 元数据发布
  rish-softvm-x86_64/   无 JIT x86_64 TCTI provider 的 Rust 安全边界
  rish-ffi/        Swift/JNI/N-API 可调用的稳定 JSON C ABI
  rish-cli/        命令规划调试工具
platform/
  ios/             Swift 接入示例
  android/         Kotlin/JNI 接入契约
  harmony/         ArkTS/N-API 接入契约
examples/
  ios/             可直接构建并启动的 iOS Simulator App
  android/         可直接构建、安装并启动的 Android APK
  harmony/         HarmonyOS Stage/HAP 工程与受限宿主烟测
```

## 快速验证

```bash
cargo test --workspace
cargo run -p rish-cli -- plan ios grep needle
cargo run -p rish-cli -- plan ios docker ps
cargo run -p rish-cli -- image-ref alpine
```

第一条命令会输出 `portable_applet` plan。通用 planner 未绑定 live Docker
handler，因此第二条失败关闭；后续 dispatcher-bound capability token 才能启用
`container.docker_api`。真实 `systemctl` 和 `dockerd` 也会因为 stock iOS
不具备 guest/native kernel 后端而被拒绝：

```bash
cargo run -p rish-cli -- plan ios systemctl status demo
cargo run -p rish-cli -- plan ios dockerd
```

## 三端 Demo

三个 Demo 都使用正式平台桥接和同一个 Rust `rish-ffi` ABI，合计验证
`grep` 规划、`echo` 与 `sha256sum` 等 portable applet：

```bash
# iOS：构建、签名、安装并启动 iPhone Simulator App
examples/ios/run-simulator.sh

# Android：构建、签名、安装并启动 arm64 APK（存在 adb 设备时）
examples/android/run-demo.sh

# 鸿蒙：准备 Stage/HAP 工程；需 DevEco、HarmonyOS SDK 和设备才能真机运行
examples/harmony/prepare_hap.sh
RISH_OHOS_NATIVE_SDK=/path/to/native examples/harmony/build_rust_ohos.sh

# 没有鸿蒙 SDK 时，只验证相同 Rust C ABI，不冒充 HAP/设备运行
examples/harmony/run_host_smoke.sh
```

具体依赖、输出和能力边界见各目录 README。这里演示的是移动应用沙箱内的
Rust 原生命令语义，不代表宿主拥有 Linux namespaces、cgroups、systemd、
内核模块、privileged container 或 Docker-in-Docker。

Swift、Kotlin/Java 和 ArkTS 侧现已包含二进制安全 HostCall/HostReply codec、
固定 allow-list dispatcher、协作式取消，以及明确标记为“非真实 systemd”
的 App 内 service supervisor。未知 operation 和真实内核语义均失败关闭。

Guest bootstrap exec 使用有界非阻塞监督器：长进程不会阻塞 Ping，Cancel、
timeout、进程组清理、stdout/stderr 限额、streaming stdin/stdout/stderr 和
Linux PTY 均已接入。它们尚未连接到真实启动的移动端 x86_64 guest。

## Linux 命令兼容层

`rish-applets` 现提供首批 47 个共享 Rust 原生命令入口，覆盖常用文本流、
校验和、虚拟身份与 app-owned 文件系统操作。它们不解释 Guest ELF，并通过
`rish_execute_applet_json` 从 Swift、Kotlin/Java 和 ArkTS 桥调用。路径被限制
在应用创建的 canonical sandbox root 内，输入、输出、递归深度和文件数量均有
硬上限。只有显式 bare command name 进入 applet；带路径的 Guest 程序不会因
basename 相同而被原生实现截获。

需要 `/proc`、netlink、namespace、cgroup、设备、模块、真实 systemd 或容器
daemon 的命令不会使用近似 applet；它们通过绑定 live `BootedVm` 的 Full VM
执行。Native Linux 已有不可伪造的保守 probe token；在主动 child-exec probe
和 OEM executor 接入前，它不声明 `LinuxElf`，也不能成为可运行候选。完整命令分层和当前列表见
[Linux 命令兼容说明](docs/command-compatibility.md)。

移动端 C ABI 是可信宿主嵌入边界，不是 Guest API：请求使用显式字节长度且上限
8 MiB，FFI applet 的输入/输出上限为 1 MiB。平台应用必须从自己的
Context/container 构造根目录，不能把 Guest JSON 的路径转发给原始 native ABI。

## OCI 原生契约

已知镜像可以通过 OCI config labels 声明平台 handler：

```json
{
  "config": {
    "Labels": {
      "io.rish.offload.handler": "media.ffmpeg",
      "io.rish.requires": "port_forwarding",
      "io.rish.requires-kernel": "network_namespace"
    }
  }
}
```

镜像 label 只是未受信请求，不能自行注册实现。`plan_image` 还要求
evidence-gated `BackendCandidate` 和宿主构造的 `OffloadHandlerRegistry`；
未绑定 handler 会失败关闭。

- `io.rish.requires` 允许桥接或语义模拟。
- `io.rish.requires-kernel` 只接受宿主原生或完整 VM guest 的真实内核语义。
- 没有 handler 的未知 ELF 只能进入 Native Linux/Full VM，否则拒绝。

## 平台承诺

| 能力 | 普通 App offload | Full Linux VM | Native Linux |
|---|---:|---:|---:|
| 已知命令原生实现 | 支持 | 支持 | 支持 |
| OCI 元数据和控制面 | 支持 | 支持 | 支持 |
| namespace/cgroup | 语义模拟 | guest 内真实 | 探测后真实 |
| systemd | 兼容 API | guest 内真实 | 探测后真实 |
| privileged/DinD | 不支持 | guest 内支持 | 仅受控设备 |
| 内核模块与 `/dev` | 显式设备代理 | guest 内支持 | 仅受控设备 |
| 未知复杂 ELF/syscall | 不支持 | Linux 内核处理 | Linux 内核处理 |

详细设计见 [架构说明](docs/architecture.md)、[平台能力矩阵](docs/platform-matrix.md)、
[OCI 数据面](docs/oci-pipeline.md)、[Linux 命令兼容说明](docs/command-compatibility.md)、
[offload-first 决策](docs/decisions/0001-offload-first.md) 和
[纯软件 Linux 决策](docs/decisions/0002-pure-software-linux.md)、
[路线图](docs/roadmap.md)。

## 许可证

项目自有 Rust 代码采用 MIT License。可选 QEMU TCTI provider、Linux kernel
和 guest 发行物保持各自许可证；链接或分发 QEMU 的产品必须单独履行 GPLv2
源码与再分发义务。