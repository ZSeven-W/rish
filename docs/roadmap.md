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
- [x] Swift/Kotlin/ArkTS HostCall codec 与固定 allow-list dispatcher
- [x] 首批 47 个 bounded Rust portable applets 与版本化执行 ABI
- [x] Linux/VM backend 优先的完整命令路由
- [x] live `BootedVm` 绑定的 verified VM command executor
- [x] 不可伪造且保守失败关闭的 Native Linux probe token
- [x] stock 平台失败关闭测试
- [x] JNI 和 Harmony N-API shim 源码
- [ ] 在真实 SDK 工程中编译并运行 JNI/Harmony N-API shim
- [ ] iOS XCFramework、Android AAR、Harmony HAR 打包

验收：三端能规划相同命令，产生一致 host call；真实内核要求不能进入
portable backend。

## P1：OCI content pipeline

- [x] OCI/Docker 引用、Registry 请求和 Bearer challenge 解析
- [x] descriptor/header/body digest、size 和 media type 校验
- [x] 严格 `linux/arm64/v8` 与 `linux/amd64` index/manifest 选择
- [x] digest CAS、lease、persistent pin 和 garbage collection
- [x] 安全 tar/gzip layer 解包、diff-id 及 whiteout
- [x] 私有 staging 与 atomic no-replace rootfs snapshot
- [x] manifest/config/layer count/单层/总下载资源上限
- [x] iOS URLSession streaming transport 与 Docker Bearer token 获取
- [ ] Android OkHttp、ArkTS HTTP transport 与 token 获取
- [ ] 签名/attestation hook
- [ ] Keychain/Keystore/安全存储凭据适配

验收：能拉取并验证 Alpine/BusyBox OCI layout，但尚不执行未知 ELF。

## P2：Native offload SDK

- [x] 版本化 HostCall/HostReply schema
- [x] Swift/Kotlin/ArkTS 二进制安全类型与 codec
- [x] 固定 allow-list、未知 operation 失败关闭
- [x] 协作式 cancellation contract
- [ ] permission broker
- [ ] streaming stdin/stdout/stderr、deadline 与 backpressure
- [ ] platform handler conformance suite
- [ ] portable `rish-sh`（pipeline、redirection、变量和控制流）
- [ ] tar/gzip/xargs 与 applet differential conformance suite

验收：同一个带 `io.rish.offload.handler` 的镜像可在三端运行，输出一致。

## P3：AMD64 Full VM boot

- [ ] 无 JIT 的 x86_64 全系统软件解释器
- [ ] long mode、四级页表、APIC/PIC/PIT/RTC 与 PCI
- [ ] virtio-blk、16550 console、rng、net 与独立 guest control channel
- [ ] x86_64 Linux kernel/initramfs/ext4 可复现构建
- [x] 版本化 Guest RPC 协议
- [x] evidence-gated VM probe/boot/HelloAck/Kconfig/capability profile
- [x] bootstrap Rust guest agent、严格握手和 capability gate
- [x] 非阻塞 exec、Cancel、timeout、进程组清理、streaming 与 Linux PTY
- [x] 固定 `linux/amd64` 镜像平台贯穿 pull、记录、FFI 与 Full VM 规划
- [ ] Youki/systemd capability probe 与完整 guest agent handler
- [ ] Native Linux OEM executor 与执行时主动 syscall 重验
- [ ] 用户态 NAT、DNS、TCP/UDP 端口转发
- [ ] suspend/checkpoint/restore

现实工程策略：Rust 负责 OCI、生命周期、安全门和 provider ABI；首个可用
provider 采用 UTM QEMU TCTI 的 no-JIT x86_64 full-system 路径。纯 Rust
provider 可并行实验，但必须通过同一真实 boot/handshake gate。QEMU 链接和
发行必须单独完成 GPL 合规评审。

验收：三端 stock 设备能够启动同一 Linux guest、执行 shell，并在前台
保持稳定；iOS 不承诺后台常驻。

## P4：真实 OCI container

- guest 中集成 Youki/containerd
- cgroup v2 delegation
- OverlayFS 和独立 writable layers
- mount/user/PID/UTS/IPC/network namespaces
- seccomp/capabilities
- volumes、signals、TTY 和 lifecycle

验收：通过 OCI runtime 核心测试并运行常用 `linux/amd64` 镜像。

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
