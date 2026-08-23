use std::io;
use std::process::ExitCode;
use std::time::Duration;

use rish_guest_agent::{GuestAgent, NativeOperationHandler, bootstrap_agent};
use rish_guest_protocol::{Envelope, FrameDecoder, FrameEncoder};

const INPUT_CHUNK_SIZE: usize = 64 * 1024;

/// I/O base of the control 16550 (COM2 / ttyS1). The agent drives these
/// registers directly instead of reading and writing /dev/ttyS1, so the framed
/// binary protocol never passes through the kernel serial line discipline or
/// its receive-interrupt path — the two places that stall it.
const CONTROL_PORT: u16 = 0x2F8;
const REG_DATA: u16 = 0; // receive buffer / transmit holding register
const REG_LINE_STATUS: u16 = 5;
const LSR_DATA_READY: u8 = 1 << 0;
const LSR_THR_EMPTY: u8 = 1 << 5;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rish-guest-agent: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Reads a byte from a device I/O port.
///
/// # Safety
/// The caller must hold I/O permission for `port`, and `port` must be a real
/// device register. This is only ever used for the pinned control UART.
#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            out("al") value,
            in("dx") port,
            options(nomem, nostack, preserves_flags),
        );
    }
    value
}

/// Writes a byte to a device I/O port. See [`inb`] for the safety contract.
#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn outb(port: u16, value: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Requests I/O permission for the control UART's eight registers. A machine
/// that enforces the I/O privilege level requires this before any port access;
/// where it is not enforced the call still succeeds harmlessly. A failure is
/// non-fatal so the agent keeps working in environments that grant port access
/// another way.
#[cfg(target_arch = "x86_64")]
fn request_control_port_access() {
    // ioperm(from, num, turn_on) is x86-64 syscall 173.
    const SYS_IOPERM: usize = 173;
    unsafe {
        let ret: isize;
        core::arch::asm!(
            "syscall",
            inlateout("rax") SYS_IOPERM => ret,
            in("rdi") u64::from(CONTROL_PORT),
            in("rsi") 8_u64,
            in("rdx") 1_u64,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack, preserves_flags),
        );
        let _ = ret;
    }
}

// The agent only ever executes inside the x86_64 guest; these stubs let the
// crate still compile for the host toolchain (workspace checks) without the
// x86 port instructions.
#[cfg(not(target_arch = "x86_64"))]
unsafe fn inb(_port: u16) -> u8 {
    unreachable!("control-port I/O is only reachable on x86_64")
}
#[cfg(not(target_arch = "x86_64"))]
unsafe fn outb(_port: u16, _value: u8) {
    unreachable!("control-port I/O is only reachable on x86_64")
}
#[cfg(not(target_arch = "x86_64"))]
fn request_control_port_access() {}

/// Drains every byte the control UART currently holds into `buffer`, returning
/// how many were read. Bounded by the buffer length.
fn read_available(buffer: &mut [u8]) -> usize {
    let mut count = 0;
    while count < buffer.len() {
        if unsafe { inb(CONTROL_PORT + REG_LINE_STATUS) } & LSR_DATA_READY == 0 {
            break;
        }
        buffer[count] = unsafe { inb(CONTROL_PORT + REG_DATA) };
        count += 1;
    }
    count
}

/// Writes as much of `pending` as the transmit register will accept, dropping
/// the written prefix. Returns whether any byte was sent.
fn write_available(pending: &mut Vec<u8>) -> bool {
    let mut sent = 0;
    while sent < pending.len() {
        if unsafe { inb(CONTROL_PORT + REG_LINE_STATUS) } & LSR_THR_EMPTY == 0 {
            break;
        }
        unsafe { outb(CONTROL_PORT + REG_DATA, pending[sent]) };
        sent += 1;
    }
    if sent != 0 {
        pending.drain(..sent);
    }
    sent != 0
}

/// Single-threaded control loop driven by direct control-UART port I/O.
///
/// Talking to the UART registers directly bypasses the kernel serial driver
/// entirely: no line discipline mangling the binary frames, no receive-interrupt
/// enable dance, no blocking descriptor. Each pass drains all available input,
/// lets the agent react and stream output, then pushes out whatever the transmit
/// register accepts; the buffered output queue means a slow line never stalls
/// the input direction. When nothing moved it sleeps briefly so an idle agent
/// does not spin the guest CPU.
fn run() -> Result<(), Box<dyn std::error::Error>> {
    request_control_port_access();
    let mut decoder = FrameDecoder::default();
    let mut encoder = FrameEncoder::default();
    let mut agent = bootstrap_agent();
    let mut pending_output: Vec<u8> = Vec::new();
    let mut read_buffer = vec![0_u8; INPUT_CHUNK_SIZE];

    loop {
        let mut progressed = false;

        let read = read_available(&mut read_buffer);
        if read != 0 {
            progressed = true;
            process_input(
                &read_buffer[..read],
                &mut decoder,
                &mut encoder,
                &mut agent,
                &mut pending_output,
            )?;
        }

        if write_envelopes(&mut pending_output, &encoder, agent.poll())? {
            progressed = true;
        }

        if write_available(&mut pending_output) {
            progressed = true;
        }

        if !progressed {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

fn process_input(
    bytes: &[u8],
    decoder: &mut FrameDecoder,
    encoder: &mut FrameEncoder,
    agent: &mut GuestAgent<NativeOperationHandler>,
    output: &mut impl io::Write,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut offset = 0;
    let mut wrote_output = false;
    while offset < bytes.len() {
        if decoder.remaining_buffer_capacity() == 0 {
            let drained = drain_frames(decoder, encoder, agent, output)?;
            wrote_output |= drained;
            if !drained && decoder.remaining_buffer_capacity() == 0 {
                return Err("guest protocol decoder has no remaining buffer capacity".into());
            }
        }
        let accepted = decoder
            .remaining_buffer_capacity()
            .min(bytes.len() - offset);
        decoder.push(&bytes[offset..offset + accepted])?;
        offset += accepted;
        wrote_output |= drain_frames(decoder, encoder, agent, output)?;
    }
    Ok(wrote_output)
}

fn drain_frames(
    decoder: &mut FrameDecoder,
    encoder: &mut FrameEncoder,
    agent: &mut GuestAgent<NativeOperationHandler>,
    output: &mut impl io::Write,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut wrote_output = false;
    while let Some(envelope) = decoder.next_frame()? {
        wrote_output |= write_envelopes(output, encoder, agent.handle(envelope)?)?;
        apply_negotiated_limits(decoder, encoder, agent)?;
    }
    Ok(wrote_output)
}

fn write_envelopes(
    output: &mut impl io::Write,
    encoder: &FrameEncoder,
    envelopes: impl IntoIterator<Item = Envelope>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut wrote_output = false;
    for envelope in envelopes {
        output.write_all(&encoder.encode(&envelope)?)?;
        wrote_output = true;
    }
    Ok(wrote_output)
}

fn apply_negotiated_limits(
    decoder: &mut FrameDecoder,
    encoder: &mut FrameEncoder,
    agent: &GuestAgent<NativeOperationHandler>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(version) = agent.negotiated_version() {
        decoder.set_expected_version(Some(version));
    }
    if let Some(max_frame_size) = agent.negotiated_max_frame_size() {
        let max_frame_size = max_frame_size as usize;
        decoder.set_max_frame_size(max_frame_size)?;
        encoder.set_max_frame_size(max_frame_size)?;
    }
    Ok(())
}
