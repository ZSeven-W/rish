//! Boots one interactive guest session and runs a sequence of commands over the
//! same open channel, mirroring the iOS demo's host-side shell: it mounts the
//! pseudo filesystems and tracks the working directory across commands so `cd`
//! sticks. Proves `cd` persistence and that ps/free/df see `/proc`.

use rish_ffi::vm_ffi::{vm_boot_session, vm_session_exec_json};
use serde_json::{Value, json};

/// Record separator that brackets the reported `pwd`, matching RishVMDemo.swift.
const CWD_MARK: char = '\u{1E}';

fn main() {
    let kernel = std::env::var("RISH_KERNEL")
        .unwrap_or_else(|_| "guest/x86_64/out/downloads/vmlinuz-virt-6.18.35".to_owned());
    let initrd =
        std::env::var("RISH_INITRD").unwrap_or_else(|_| "/tmp/rish-container.cpio".to_owned());

    let request = json!({
        "kernel_path": kernel,
        "initrd_path": initrd,
        "command": ["true"],
        "memory_mib": 1024,
        "command_line": "console=ttyS0,115200n8 rdinit=/init panic=-1 oops=panic nokaslr cgroup_no_v1=all 8250.nr_uarts=1",
        "boot_budget_units": 60_000_000_000u64,
        "handshake_budget_units": 40_000_000_000u64,
    });

    let session = match vm_boot_session(&request.to_string()) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("boot failed: {error}");
            std::process::exit(1);
        }
    };

    // The same script every command gets in the demo.
    let commands = [
        "pwd",
        "cd /root",
        "pwd",
        "ls -la",
        "cd ..",
        "pwd",
        "cd /etc && cat hostname",
        "pwd",
        "ps | head -3",
        "echo done",
    ];

    let mut cwd = String::from("/");
    for command in commands {
        let setup = "mount -t proc proc /proc 2>/dev/null;mount -t sysfs sysfs /sys 2>/dev/null";
        let script = format!(
            "{setup}\ncd '{cwd}' 2>/dev/null\n{command}\n__rish_rc=$?\nprintf '{CWD_MARK}%s{CWD_MARK}' \"$(pwd)\"\nexit $__rish_rc"
        );
        let reply = vm_session_exec_json(
            &session,
            &json!({ "command": ["sh", "-lc", script] }).to_string(),
        );
        let value: Value = serde_json::from_str(&reply).unwrap_or(Value::Null);
        let mut stdout = value["stdout"].as_str().unwrap_or("").to_owned();
        if let Some(dir) = take_cwd(&mut stdout) {
            if !dir.is_empty() {
                cwd = dir;
            }
        }
        let stderr = value["stderr"].as_str().unwrap_or("");
        println!("{cwd} $ {command}");
        if !stdout.is_empty() {
            print!("{stdout}");
            if !stdout.ends_with('\n') {
                println!();
            }
        }
        if !stderr.is_empty() {
            eprint!("[stderr] {stderr}");
        }
    }
}

/// Pulls the trailing `\u{1E}pwd\u{1E}` marker out of `out`, matching the demo.
fn take_cwd(out: &mut String) -> Option<String> {
    let first = out.find(CWD_MARK)?;
    let rest = first + CWD_MARK.len_utf8();
    let second = out[rest..].find(CWD_MARK)? + rest;
    let dir = out[rest..second].to_owned();
    out.replace_range(first..second + CWD_MARK.len_utf8(), "");
    Some(dir)
}
