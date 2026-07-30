# Architecture

## 目标

`rish` 不是把所有平台伪装成 Linux，而是为每次执行建立一份可审计的
能力合同，并选择能够满足该合同的最轻后端。

```text
OCI/command request
  → normalize command, env, cwd and mounts
  → derive capability requirements
  → negotiate fidelity
  → choose backend
  → execute or fail closed
```

能力级别由高到低不是简单的数值关系：

- `native`：宿主 Linux 内核原生能力。
- `virtualized`：真实 Linux guest 内核能力。
- `bridged`：由 Swift、Java/Kotlin、ArkTS 或系统服务完成。
- `emulated`：Rust 控制面提供语义模型，不具备宿主内核隔离。
- `planned`：设计存在但当前不能执行。
- `unavailable`：平台明确不支持。

要求“真实内核语义”的操作只接受 `native` 或 `virtualized`。

## Native offload

Guest 命令不会直接调用平台语言。Rust 首先产生版本化调用：

```json
{
  "protocol_version": 1,
  "id": 42,
  "operation": "service.systemctl",
  "command": {
    "program": "systemctl",
    "args": ["start", "demo.service"],
    "cwd": "/",
    "env": {},
    "stdin": []
  },
  "requirements": [
    {
      "capability": "systemd",
      "kernel_semantics_required": false
    }
  ]
}
```

平台层必须使用 allow-list 注册 handler，并在执行前再次检查用户权限。
Rust 返回 host call 不等于自动授予相机、文件、网络或系统权限。

推荐的平台 handler：

| Operation | iOS | Android | Harmony |
|---|---|---|---|
| `service.systemctl` | Swift supervisor | Kotlin service | ArkTS service |
| `container.docker_api` | Swift API facade | Kotlin API facade | ArkTS API facade |
| `network.admin` | Network.framework 代理 | Java/native socket 代理 | NetworkKit 代理 |
| `device.mknod` | 虚拟设备 registry | 虚拟/OEM device broker | 虚拟/OEM device broker |
| `kernel.module` | 拒绝 | root/OEM helper | system/OEM helper |

普通平台 handler 只能实现命令的产品语义。例如 `systemctl start demo`
可以启动 App 内托管服务，但不能声称创建了真正的 systemd unit、PID 1 或
cgroup。要求这些行为的调用必须标记 `requires-kernel`。

## OCI execution

```text
registry resolve/auth
  → choose linux/arm64 manifest
  → verify digest and size
  → content-addressed blob store
  → parse image config
  → native handler contract?
      yes → capability negotiation → platform handler
      no  → native Linux available?
              yes → OCI runtime
              no  → Full VM available?
                      yes → guest agent/Youki
                      no  → reject
```

Layer 解包阶段必须防御：

- 绝对路径和 `..` 穿越
- symlink/hardlink escape
- OCI whiteout 和 opaque directory 错误合并
- 解压炸弹与 inode/空间耗尽
- 未验证 digest 的缓存污染

## Full VM

完整能力采用一个持久 Linux guest，而不是每个容器一台 VM：

```text
Mobile host
  ├─ Rust policy/OCI/content store
  ├─ platform UI and permission broker
  ├─ user-mode NAT and port forwarding
  └─ VM engine
       ├─ software interpreter on stock devices
       ├─ AVF/crosvm on approved Android/OEM devices
       ├─ KVM on explicitly permitted devices
       └─ virtio block/net/console/rng/vsock
             │
             ▼
          Linux guest
             ├─ systemd PID 1
             ├─ rish guest agent
             ├─ containerd/Youki
             └─ optional dockerd/DinD
```

Guest kernel 合同位于 `rish-vm::GuestKernelContract`，包括 namespaces、
cgroup v2、OverlayFS、seccomp、modules、devtmpfs、TUN/veth/bridge 和
netfilter/nftables。

Full VM 的设备范围：

- guest 内 `/dev` 来自 devtmpfs 和 virtio。
- guest `.ko` 只能加载进匹配的 guest kernel。
- 相机、GPU、蓝牙、Secure Enclave 等宿主设备必须经过显式 broker。
- `--privileged` 只授予容器对 guest 的高权限。

## Native Linux

Android/鸿蒙原生后端启用前必须通过完整探测：

1. namespace clone/unshare 实测。
2. 可写且已委派的 cgroup v2 subtree。
3. overlayfs、seccomp、capabilities、veth/TUN/netfilter。
4. SELinux/AppArmor 策略允许所需操作。
5. 模块签名、KMI 和 `CAP_SYS_MODULE`。
6. `/dev/kvm` 不仅存在，而且允许完成 KVM API ioctl。

任何一项缺失都只降低该项能力；不能把一个 `root` 布尔值当作完整合同。

## 安全边界

- 所有未知命令默认拒绝。
- 原生 handler 必须 allow-list，参数采用结构化 schema。
- Registry 凭据分别进入 Keychain、Android Keystore 和鸿蒙安全存储。
- privileged workload 使用独立 VM，避免和普通 workload 共用 guest trust
  boundary。
- guest 对宿主文件系统只获得用户明确授权的目录。
- 宿主侧端口转发单独授权，默认仅监听 loopback。
- capability profile 随日志和执行结果持久化，便于审计真实执行路径。
