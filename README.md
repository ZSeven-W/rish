# rish

`rish` 是面向 iOS、Android 和鸿蒙的 Rust Linux 命令与容器语义运行时。

项目采用 **native-offload first、Linux VM fallback** 的路线：

- 已知 Guest 命令由 Rust 拦截并分派给 Swift/Objective-C、Kotlin/Java、
  ArkTS 或系统特权服务。
- OCI 镜像可以声明原生 handler 契约，无需解释执行其中的 ELF。
- 未知 ELF、真正的 systemd、Docker-in-Docker、内核模块和完整网络
  namespace 必须进入真实 Linux guest 或经过探测的宿主 Linux 后端。
- 能力不足时失败关闭，不会把“模拟成功”伪装成真实内核隔离。

> 当前状态：P0 架构原型。能力模型、命令规划、OCI offload 契约、VM
> guest 合同、三端 C ABI 和控制面模型已经可编译测试；完整 VM、镜像拉取、
> Youki/Docker guest 和平台应用尚未实现。

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
  rish-runtime/    路由、后端选择、namespace/cgroup/device/service 模型
  rish-oci/        OCI image config 和原生 offload 契约
  rish-vm/         VM/guest trait、设备配置和容器 guest 内核合同
  rish-ffi/        Swift/JNI/N-API 可调用的稳定 JSON C ABI
  rish-cli/        命令规划调试工具
platform/
  ios/             Swift 接入示例
  android/         Kotlin/JNI 接入契约
  harmony/         ArkTS/N-API 接入契约
```

## 快速验证

```bash
cargo test --workspace
cargo run -p rish-cli -- ios systemctl start demo
cargo run -p rish-cli -- ios docker ps
```

第一条命令会输出 `service.systemctl` host call；第二条会输出
`container.docker_api`。真实 `dockerd` 会因为 stock iOS 不具备
guest/native kernel 后端而被拒绝：

```bash
cargo run -p rish-cli -- ios dockerd
```

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
[offload-first 决策](docs/decisions/0001-offload-first.md) 和
[路线图](docs/roadmap.md)。

## 许可证

项目代码采用 MIT License。不得复制 iSH/OpenMinis/PRoot 的 GPL 实现到
本仓库；若后续分发 QEMU 或 Linux guest，需分别履行其 GPL 和源码提供义务。
