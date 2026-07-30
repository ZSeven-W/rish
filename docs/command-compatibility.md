# Linux command compatibility

Rish routes commands to the narrowest backend that can provide their requested
semantics:

1. a bounded native Rust applet for portable file and stream operations;
2. a typed Swift/Kotlin/ArkTS broker for an explicitly supported host product
   API;
3. a probed native-Linux backend; or
4. a verified full Linux VM.

An unknown command or unsupported option never falls through to a mobile host
shell. A stock mobile backend fails closed. A verified Linux backend executes
the installed Linux binary in that backend and is the path for broad
distribution command compatibility.

Portable and typed-offload dispatch accepts only an explicit bare command
name. Path-bearing programs such as `/usr/bin/ls`, `./docker`, or an executable
resolved from a future Guest `PATH` never acquire applet/offload identity from
their basename. A future shell must preserve that provenance with a trusted
resolution ticket.

## Native portable applets

The first compatibility set contains 47 command entry points:

| Family | Commands |
| --- | --- |
| basic and identity | `[`, `basename`, `date`, `dirname`, `echo`, `env`, `false`, `groups`, `hostname`, `id`, `printenv`, `printf`, `pwd`, `seq`, `test`, `true`, `uname`, `whoami` |
| text and streams | `cat`, `cut`, `grep`, `head`, `sort`, `tail`, `tee`, `tr`, `uniq`, `wc` |
| scoped filesystem | `chmod`, `cp`, `du`, `find`, `ln`, `ls`, `mkdir`, `mktemp`, `mv`, `readlink`, `realpath`, `rm`, `rmdir`, `stat`, `touch` |
| encoding and checksums | `base64`, `cksum`, `sha256sum`, `sha512sum` |

These are native implementations compiled into `rish-applets`; no Guest ELF
is decoded or interpreted. They implement common POSIX/GNU option subsets. An
unrecognized option is an error instead of an approximate result.

- Paths are resolved below one app-owned absolute sandbox root.
- `..` cannot escape that root and applets do not follow symbolic links.
- Removing the sandbox root is forbidden.
- Recursive walks, input, output, argv and environment data are bounded.
- The identity from `id`, `whoami`, `groups`, `hostname` and `uname` is a Rish
  virtual identity, not the mobile host kernel identity.
- Portable `uname` reports `Rish`, not `Linux`.
- `date` uses a deterministic UTC personality and refuses clock changes.
- `chmod` changes only files in the applet sandbox.
- `mktemp -u` is deliberately rejected.
- Commands that would launch another executable do not invoke a mobile shell.

Default limits are 8 MiB input, 8 MiB combined output, 10,000 filesystem
entries and recursion depth 64. Platform applications may lower these limits
per invocation.

The applet root must already exist as a canonical, non-symlink directory and
must remain private to the application while an operation is running. Calls
are serialized and root device/inode identity is checked before and after each
call. Path validation prevents Guest-controlled lexical and symlink escape; it
is not an OS mount namespace or a defense against a trusted host process
deliberately racing filesystem entries.

## Commands that require Linux

The following command families are not portable applets because their normal
meaning depends on Linux kernel state:

| Required semantics | Examples |
| --- | --- |
| namespaces and mounts | `mount`, `umount`, `unshare`, `nsenter`, `chroot`, `pivot_root` |
| cgroups and processes | full `ps`, `top`, `free`, `lsof`, cgroup tools |
| network namespaces/netlink | `ip`, `ss`, `route`, `iptables`, `nft`, `tc`, `bridge` |
| devices and block storage | `mknod`, `losetup`, `lsblk`, `blkid`, `fdisk`, `mkfs`, `udevadm` |
| modules and kernel tracing | `modprobe`, `insmod`, `rmmod`, `dmesg`, `strace`, `perf`, eBPF tools |
| real service management | `systemctl`, `journalctl`, `loginctl` |
| local container daemons | `dockerd`, `containerd`, `ctr`, `runc`, Docker-in-Docker |

On a verified VM profile, all commands—including names also available as
portable applets—prefer `vm.exec`. `VerifiedVmRuntime` keeps the live
`BootedVm` borrowed through execution, so a generic host bridge cannot forge a
VM result. This preserves the exact flags and behavior of the Linux
distribution installed in the Guest. A probed native-Linux profile may plan
`linux.exec` only after an active child-exec probe is implemented; the current
conservative production probe leaves `LinuxElf` unavailable, so it cannot
create a runnable native candidate. Capability checks for systemd,
PID/user/mount/network/UTS/IPC/cgroup/
time namespaces, cgroups, devices and modules still run before dispatch.

On stock iOS, Android or HarmonyOS, these commands are rejected unless an
explicit typed product-semantic handler exists. Such a handler is labelled as
bridged or emulated and is not reported as Linux kernel support.

## Shell and distribution tools

`sh`, Bash control flow, package managers, language runtimes, compilers and
arbitrary programs remain Guest-Linux operations. A future portable `rish-sh`
may compose applets using pipelines and redirections, but it must route every
external command through the same capability planner; it will not spawn the
mobile host shell.

## Native ABI

`rish_plan_json` returns either `portable_applet`, a typed host call, or an
error. `rish_execute_applet_json` executes a portable applet from a versioned
JSON request containing a host-selected absolute sandbox root. Both C calls
take an explicit input byte length and reject requests above 8 MiB. The mobile
FFI caps applet input/output at 1 MiB, 10,000 entries and depth 64 before JSON
output can amplify.

This C ABI is a trusted embedding-host boundary, not a Guest API. The root is
application configuration and must never be copied from Guest-controlled
payload data or exposed through a WebView/script bridge. Binary stdin, stdout
and stderr are JSON byte arrays. Platform code should use its typed
Context/container constructor.
