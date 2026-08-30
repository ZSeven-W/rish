//! End-to-end smoke test of the `rish_vm_run_docker_json` C ABI surface:
//! boots the pure-Rust interpreter and runs one guest command via the mobile
//! bridge entry point, printing the JSON response.
use std::ffi::{CStr, CString};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command: Vec<String> = if args.is_empty() {
        vec!["uname".to_owned(), "-a".to_owned()]
    } else {
        args
    };
    let kernel = std::env::var("RISH_KERNEL")
        .unwrap_or_else(|_| "guest/x86_64/out/downloads/vmlinuz-virt-6.18.35".to_owned());
    let initrd =
        std::env::var("RISH_INITRD").unwrap_or_else(|_| "/tmp/rish-portio.cpio".to_owned());
    let env_u64 = |name: &str, default: u64| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    let request = serde_json::json!({
        "kernel_path": kernel,
        "initrd_path": initrd,
        "root_disk_path": "/tmp/rish-rootdisk.img",
        "memory_mib": env_u64("RISH_MEMORY_MIB", 1024),
        "command": command,
        "network": if std::env::var("RISH_NETWORK").as_deref() == Ok("user-nat") {
            "user-nat"
        } else {
            "disabled"
        },
        "command_line": "console=ttyS0,115200n8 rdinit=/init panic=-1 oops=panic nokaslr cgroup_no_v1=all 8250.nr_uarts=1",
        "boot_budget_units": env_u64("RISH_BOOT_BUDGET", 25_000_000_000),
        "handshake_budget_units": env_u64("RISH_HANDSHAKE_BUDGET", 15_000_000_000),
    })
    .to_string();
    let input = CString::new(request).unwrap();
    let ptr = unsafe { rish_ffi::rish_vm_run_docker_json(input.as_ptr(), input.as_bytes().len()) };
    let response = unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    println!("{response}");
    unsafe { rish_ffi::rish_string_free(ptr) };
}
