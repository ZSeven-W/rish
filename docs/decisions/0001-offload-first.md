# ADR 0001: Native-offload first

Status: accepted

## Context

普通 iOS、Android 和 HarmonyOS App 不能获得一致的 Linux kernel API。
逐条解释 Guest ELF 和 syscall 可以提供 iSH 类 shell，但无法在合理周期内
完整实现 systemd、Docker-in-Docker、内核模块、netfilter 和所有复杂
syscall。

三个平台都能稳定调用本平台已签名的原生代码。

## Decision

Portable backend 不解释任意 Guest ELF。它将已知命令转为版本化 host call，
由 Swift/Objective-C、Java/Kotlin 或 ArkTS handler 完成。

OCI 镜像通过 `io.rish.offload.handler` 声明可移植原生实现。命令或镜像
没有 handler 时：

1. 若探测到 Native Linux backend，交给宿主 Linux。
2. 否则若 Full VM backend 可用，交给 Linux guest。
3. 否则拒绝执行。

要求 namespaces、cgroups、privileged、内核模块、真实 systemd、DinD 或
完整 netns 的请求必须声明真实内核语义；portable semantic model 不满足
该要求。

## Consequences

- 三端的控制面、权限和产品语义可以快速保持一致。
- 常用重任务可以使用平台原生性能。
- 不能宣称任意 Docker 镜像都能由 offload backend 运行。
- handler 需要独立实现和维护一致性测试。
- Full VM 仍然是未知 ELF 和完整 Linux 兼容性的必要后端。
- 原生 handler 必须经过 allow-list 和平台权限检查，不能把 Guest 参数
  直接拼接为宿主命令。
