//! Console-marker contract shared by every host entry point that boots the
//! pure-Rust guest.
//!
//! The guest init prints BOOT_OK_MARKER and then hands PID 1 to the guest
//! agent. The agent prints AGENT_READY_MARKER only once it is polling the
//! control 16550 directly. A host must observe both before writing the first
//! framed Hello: while configuring the control UART the agent resets the
//! receive FIFO (FCR clear-RX), which discards any frame the host queued
//! earlier. A Hello destroyed that way is never answered, so the handshake
//! spins until the session step budget runs out.

/// Guest init success marker, printed by /init before it execs the agent.
pub const BOOT_OK_MARKER: &[u8] = b"RISH_X86_64_BOOT_OK";

/// Guest init failure marker, printed by /init before the emergency shell.
pub const BOOT_FAILED_MARKER: &[u8] = b"RISH_X86_64_BOOT_FAILED";

/// Guest agent control-channel readiness marker. The host must not write a
/// framed Hello before this marker appears on the console: the agent clears
/// the control UART's receive FIFO while initializing it and any earlier
/// frame is lost.
pub const AGENT_READY_MARKER: &[u8] = b"RISH_GUEST_AGENT_READY";

/// Whether the console observed so far proves the guest init and the agent
/// are both up. Both markers are required; seeing only the boot marker is not
/// enough to open the control channel safely.
#[must_use]
pub fn guest_ready(console: &[u8]) -> bool {
    contains(console, BOOT_OK_MARKER) && contains(console, AGENT_READY_MARKER)
}

/// Whether the guest init reported failure on the console.
#[must_use]
pub fn guest_failed(console: &[u8]) -> bool {
    contains(console, BOOT_FAILED_MARKER)
}

/// Reports whether a marker appears anywhere in the buffer. The caller keeps
/// the full console buffer across pump chunks, so a marker spanning two
/// chunks is still detected.
#[must_use]
pub fn contains(buffer: &[u8], marker: &[u8]) -> bool {
    if marker.is_empty() {
        return false;
    }
    buffer.windows(marker.len()).any(|window| window == marker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_ok_alone_does_not_open_the_control_channel() {
        let mut console = Vec::new();
        console.extend_from_slice(BOOT_OK_MARKER);
        console.extend_from_slice(b"Linux 6.18.35 on x86_64\n");
        assert!(!guest_ready(&console));
        assert!(!guest_failed(&console));
    }

    #[test]
    fn both_markers_open_the_control_channel_even_across_chunks() {
        let mut console = Vec::new();
        console.extend_from_slice(&BOOT_OK_MARKER[..10]);
        console.extend_from_slice(&BOOT_OK_MARKER[10..]);
        console.extend_from_slice(&AGENT_READY_MARKER[..8]);
        console.extend_from_slice(&AGENT_READY_MARKER[8..]);
        assert!(guest_ready(&console));
    }

    #[test]
    fn failure_marker_wins_over_readiness() {
        let mut console = Vec::new();
        console.extend_from_slice(BOOT_OK_MARKER);
        console.extend_from_slice(BOOT_FAILED_MARKER);
        console.extend_from_slice(AGENT_READY_MARKER);
        assert!(guest_failed(&console));
        assert!(guest_ready(&console));
    }

    #[test]
    fn contains_does_not_match_an_empty_marker_or_short_buffer() {
        assert!(!contains(b"", b""));
        assert!(!contains(b"short", b"a much longer marker"));
        assert!(contains(b"xxRISH_X86_64_BOOT_OKyy", BOOT_OK_MARKER));
    }
}
