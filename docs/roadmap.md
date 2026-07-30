# Roadmap

## P0：控制面和真实性模型

状态：进行中。

- [x] Rust workspace 和本地 Git 仓库
- [x] 六级 capability profile
- [x] host-call 命令注册表
- [x] namespace、cgroup、service、device 控制面模型
- [x] OCI native-offload labels
- [x] VM trait、virtio 设备配置与 guest kernel contract
- [x] Swift/JNI/N-API 共用 JSON C ABI
- [x] stock 平台失败关闭测试
- [ ] 可执行的 JNI 和 Harmony N-API shim
- [ ] iOS XCFramework、Android AAR、Harmony HAR 打包

验收：三端能规划相同命令，产生一致 host call；真实内核要求不能进入
portable backend。

## P1：OCI content pipeline

- Registry v2/OAuth/token authentication
- `linux/arm64` index/manifest 选择
- digest CAS、lease 和 garbage collection
- 安全 layer 解包及 whiteout
- 签名/attestation hook
- Keychain/Keystore/安全存储凭据适配

验收：能拉取并验证 Alpine/BusyBox OCI layout，但尚不执行未知 ELF。

## P2：Native offload SDK

- 版本化 handler schema
- 生成 Swift/Kotlin/ArkTS 类型
- permission broker
- streaming stdin/stdout/stderr
- cancellation、deadline、backpressure
- platform handler conformance suite

验收：同一个带 `io.rish.offload.handler` 的镜像可在三端运行，输出一致。

## P3：Full VM boot

- 软件 ARM64 CPU/MMU/exception interpreter
- GICv3、timer 和 PSCI
- virtio-blk、console、rng、net、vsock
- Linux kernel/initramfs/ext4 可复现构建
- guest Rust agent
- 用户态 NAT、DNS、TCP/UDP 端口转发
- suspend/checkpoint/restore

现实工程策略：先用成熟全系统后端验证 guest 协议，再并行推进纯 Rust
解释器。若引入 QEMU，发行和链接必须单独完成 GPL 合规评审。

验收：三端 stock 设备能够启动同一 Linux guest、执行 shell，并在前台
保持稳定；iOS 不承诺后台常驻。

## P4：真实 OCI container

- guest 中集成 Youki/containerd
- cgroup v2 delegation
- OverlayFS 和独立 writable layers
- mount/user/PID/UTS/IPC/network namespaces
- seccomp/capabilities
- volumes、signals、TTY 和 lifecycle

验收：通过 OCI runtime 核心测试并运行常用 `linux/arm64` 镜像。

## P5：systemd、网络和 privileged

- systemd 作为 guest PID 1
- veth、bridge、TUN、nftables/NAT
- guest devtmpfs/udev
- kernel modules 与 kernel build ID 成套发布
- privileged workload 独立 VM 策略

验收：systemd service、真实 netns、模块加载和 guest `/dev` 测试通过。

## P6：Docker 和 DinD

- Docker API 兼容 facade
- guest dockerd/containerd
- `docker:dind` 专用 VM
- `/var/lib/docker` 独立 ext4 block volume
- Docker CLI、Compose 和 build 基础兼容

验收：在 guest 范围运行 `docker:dind`，容器能够联网、端口转发和嵌套
启动；文档明确它不能控制移动宿主。

## P7：硬件加速和受控设备

- Android AVF/crosvm 系统/OEM backend
- Android/鸿蒙 KVM probe 与专用 SELinux domain
- 受控 ROM 的 Native Linux/Youki backend
- OEM device assignment
- 性能、电量、内存和热管理

验收：硬件后端只有在完整 capability contract 通过时启用，否则自动回退
VM 软件解释器或拒绝。
