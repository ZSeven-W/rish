//! Cross-crate control-channel contract.
//!
//! The guest agent talks to the host over one 16550 that it drives by direct
//! port I/O. Two host paths sit on the other end of that UART:
//!
//! - the pure-Rust interpreter (the mobile-bridge rish_vm_run_docker_json
//!   surface behind the vm_smoke example, and the pure_rust_guest example),
//!   whose machine is the rish-softvm-core Cpu;
//! - the QEMU diagnostic oracle (rish-guest-boot and the Docker language-image
//!   harness), whose machine is described by guest/x86_64/boot-manifest.json.
//!
//! Neither path exercises the other, so this file pins what they must agree
//! on: the control UART sits at the port the agent polls with the IRQ the
//! manifest declares, the console markers the hosts wait for are the markers
//! the guest prints, and the agent's port initialization keeps its documented
//! destructive behavior (FCR clear-RX) so host-side sequencing stays honest.

use std::{fs, path::Path};

use rish_guest_agent::{CONTROL_PORT, CONTROL_READY_MARKER};
use rish_softvm_core::Cpu;
use rish_softvm_x86_64::{
    AGENT_READY_MARKER, BOOT_FAILED_MARKER, BOOT_OK_MARKER, MachineProvider, PureRustProvider, abi,
    guest_failed, guest_ready,
};
use serde_json::Value;

/// The QEMU path's machine description, checked in next to the guest build.
fn boot_manifest() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../guest/x86_64/boot-manifest.json");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} is not valid JSON: {error}", path.display()))
}

fn manifest_port(serial: &Value) -> u16 {
    let text = serial["io_port"]
        .as_str()
        .expect("serial io_port is a string");
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .expect("serial io_port is hexadecimal");
    u16::from_str_radix(digits, 16).expect("serial io_port fits a port number")
}

fn manifest_irq(serial: &Value) -> u8 {
    u8::try_from(serial["irq"].as_u64().expect("serial irq is an integer"))
        .expect("serial irq fits a line number")
}

fn manifest_marker<'a>(manifest: &'a Value, key: &str) -> &'a [u8] {
    manifest["boot"][key]
        .as_str()
        .unwrap_or_else(|| panic!("boot.{key} is a string"))
        .as_bytes()
}

#[test]
fn interpreter_uarts_match_the_boot_manifest_and_the_agent() {
    let manifest = boot_manifest();
    let console = &manifest["machine"]["serial"];
    let control = &manifest["machine"]["control_serial"];
    let cpu = Cpu::new(128, 0).unwrap();

    // The QEMU machine and the interpreter must place the control UART where
    // the agent polls it, and on the same IRQ line.
    assert_eq!(manifest_port(control), CONTROL_PORT);
    assert_eq!(manifest_irq(control), cpu.uart_control.irq_line());
    assert_eq!(manifest_port(console), 0x3F8);
    assert_eq!(manifest_irq(console), cpu.uart_console.irq_line());
    assert_ne!(manifest_port(console), manifest_port(control));
}

#[test]
fn interpreter_answers_the_control_port_the_agent_polls() {
    // The agent polls COM2 directly through inb/outb; no kernel serial driver
    // is involved on the control channel. If the machine answers that port
    // with anything else, the handshake stalls until the step budget runs out.
    let mut cpu = Cpu::new(128, 0).unwrap();
    let console_port = 0x3F8;

    // Host-to-guest bytes queued at the control port surface on port reads.
    cpu.uart_control.push_input(b"R");
    assert_ne!(cpu.io_read(CONTROL_PORT + 5, 1).unwrap() as u8 & 0x01, 0);
    assert_eq!(cpu.io_read(CONTROL_PORT, 1).unwrap() as u8, b'R');

    // The console port carries the console, not the control channel.
    cpu.uart_console.push_input(b"Y");
    assert_ne!(cpu.io_read(console_port + 5, 1).unwrap() as u8 & 0x01, 0);
    assert_eq!(cpu.io_read(console_port, 1).unwrap() as u8, b'Y');

    // A control-port transmit stays on the control device.
    cpu.io_write(CONTROL_PORT, 1, u32::from(b'Q')).unwrap();
    assert_eq!(cpu.uart_control.drain_output(), b"Q");
    assert!(cpu.uart_console.drain_output().is_empty());
}

#[test]
fn control_uart_fcr_reset_discards_queued_host_input() {
    // Regression contract for the vm_smoke breakage: the agent writes FCR=0x07
    // (enable + clear RX) while initializing the port, so a Hello frame the
    // host queued before the agent-ready marker is destroyed and never
    // answered. Hosts must therefore wait for AGENT_READY_MARKER (guest_ready)
    // before writing the first frame. If the device ever stops honoring the
    // RX clear, or a host starts writing earlier again, this test names the
    // exact hazard.
    let mut cpu = Cpu::new(128, 0).unwrap();
    cpu.uart_control.push_input(b"early hello frame bytes");
    cpu.io_write(CONTROL_PORT + 2, 1, 0x07).unwrap();
    assert_eq!(cpu.io_read(CONTROL_PORT + 5, 1).unwrap() as u8 & 0x01, 0);
    assert_eq!(cpu.io_read(CONTROL_PORT, 1).unwrap(), 0);
}

#[test]
fn host_readiness_markers_match_the_agent_and_the_boot_manifest() {
    // The marker the agent prints after initializing the control port is the
    // marker both hosts wait for; a spelling change on any side must break
    // this test instead of the handshake.
    let manifest = boot_manifest();
    assert_eq!(CONTROL_READY_MARKER.as_bytes(), AGENT_READY_MARKER);
    assert_eq!(
        manifest_marker(&manifest, "control_ready_serial_marker"),
        AGENT_READY_MARKER
    );
    assert_eq!(
        manifest_marker(&manifest, "expected_serial_marker"),
        BOOT_OK_MARKER
    );
    assert_eq!(
        manifest_marker(&manifest, "failure_serial_marker"),
        BOOT_FAILED_MARKER
    );

    // Both markers are required: the boot marker alone must not open the
    // control channel.
    let mut console = Vec::new();
    console.extend_from_slice(BOOT_OK_MARKER);
    assert!(!guest_ready(&console));
    assert!(!guest_failed(&console));
    console.extend_from_slice(AGENT_READY_MARKER);
    assert!(guest_ready(&console));
}

#[test]
fn the_shared_provider_declares_both_serials() {
    // Every interpreter entry point constructs its machine through
    // PureRustProvider; the provider must keep advertising the serial pair the
    // handshake depends on.
    let provider = PureRustProvider::new();
    let build = provider.build_info();
    assert!(build.supports(abi::FEATURE_SERIAL_16550));
    assert!(build.supports(abi::FEATURE_CONTROL_SERIAL));
}
