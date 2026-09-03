//! Integration test through the mobile-bridge entry point used by the
//! vm_smoke example: boot the pure-Rust interpreter with the real pinned
//! Alpine kernel and the container initramfs, wait for the guest agent, and
//! run one command over the negotiated control channel.
//!
//! This is the regression test for the port-I/O control channel breakage: a
//! host that opens the control channel after only the boot marker (instead of
//! boot + agent-ready markers) destroys the first Hello frame in the agent's
//! FCR reset and exhausts the handshake budget. The test boots the real guest
//! whenever the built assets are present and skips cleanly where they are not
//! (a machine without guest/x86_64/out/ has not built them).

use std::path::{Path, PathBuf};

fn asset(kind: &str, default: &str) -> PathBuf {
    std::env::var_os(kind)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(default)
        })
}

fn guest_assets() -> Option<(PathBuf, PathBuf)> {
    let kernel = asset(
        "RISH_TEST_KERNEL",
        "guest/x86_64/out/downloads/vmlinuz-virt-6.18.35",
    );
    let initrd = asset("RISH_TEST_INITRD", "guest/x86_64/out/rish-container.cpio");
    if kernel.is_file() && initrd.is_file() {
        Some((kernel, initrd))
    } else {
        None
    }
}

#[test]
fn vm_run_docker_json_boots_and_runs_a_guest_command() {
    let Some((kernel, initrd)) = guest_assets() else {
        eprintln!(
            "skipping vm_smoke handshake test: guest assets are not built \
             (run guest/x86_64/fetch-assets.sh and build-container-initramfs.sh first)"
        );
        return;
    };
    let request = serde_json::json!({
        "kernel_path": kernel,
        "initrd_path": initrd,
        "memory_mib": 1024,
        "command": ["uname", "-a"],
        "network": "disabled",
        // The exact command line the vm_smoke example pins, including the
        // single-UART limit that keeps the kernel serial driver away from the
        // agent's direct port-I/O control channel.
        "command_line": "console=ttyS0,115200n8 rdinit=/init panic=-1 oops=panic nokaslr cgroup_no_v1=all 8250.nr_uarts=1",
        "boot_budget_units": 25_000_000_000_u64,
        "handshake_budget_units": 15_000_000_000_u64,
    })
    .to_string();
    let response: serde_json::Value =
        serde_json::from_str(&rish_ffi::vm_ffi::vm_run_docker_json(&request)).unwrap();
    assert_eq!(response["ok"], true, "unexpected response: {response}");
    assert_eq!(response["exit_code"], 0, "unexpected response: {response}");
    assert!(
        response["stdout"].as_str().unwrap().contains("Linux"),
        "unexpected response: {response}"
    );
}
