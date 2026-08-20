#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
config=${1:-"$script_dir/out/downloads/config-6.18.35-0-virt"}

die() {
    printf 'verify-kernel-config: %s\n' "$*" >&2
    exit 1
}

[ -f "$config" ] || die "missing kernel config: $config"

require_builtin() {
    grep -qx "$1=y" "$config" ||
        die "$1 must be built into the diagnostic kernel"
}

for option in \
    CONFIG_64BIT \
    CONFIG_X86_64 \
    CONFIG_BLK_DEV_INITRD \
    CONFIG_CGROUPS \
    CONFIG_MEMCG \
    CONFIG_CGROUP_PIDS \
    CONFIG_CGROUP_BPF \
    CONFIG_NAMESPACES \
    CONFIG_UTS_NS \
    CONFIG_IPC_NS \
    CONFIG_USER_NS \
    CONFIG_PID_NS \
    CONFIG_NET_NS \
    CONFIG_SECCOMP \
    CONFIG_SECCOMP_FILTER \
    CONFIG_DEVTMPFS \
    CONFIG_DEVTMPFS_MOUNT \
    CONFIG_SERIAL_8250 \
    CONFIG_SERIAL_8250_CONSOLE \
    CONFIG_PROC_FS \
    CONFIG_SYSFS \
    CONFIG_TMPFS
do
    require_builtin "$option"
done

printf 'verified  minimal x86_64 serial/initramfs kernel configuration\n'

