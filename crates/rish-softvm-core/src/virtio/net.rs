//! The virtio-mmio network device: register file, feature negotiation,
//! the RX/TX split virtqueues, and the virtio-net frame protocol.
//!
//! This module mirrors the virtio-blk device in crate::virtio::block:
//! modern (v2) virtio-mmio register layout, fail-closed queue servicing, a
//! sticky device fault on any out-of-RAM or malformed chain, and a used-ring
//! interrupt edge per drained batch. The difference is the payload: frames
//! are moved between the guest and a host-side user-mode network backend
//! (crate::net), which owns ARP, IPv4, DNS, and TCP termination.
//!
//! # virtio-net specifics
//!
//! - Device id 1, two split virtqueues: queue 0 receives (device writes
//!   frames into buffers the driver posts) and queue 1 transmits (the driver
//!   posts frames the device forwards to the backend).
//! - Offered features: VIRTIO_F_VERSION_1, VIRTIO_NET_F_MAC (the config
//!   space carries the fixed MAC), VIRTIO_NET_F_STATUS (LINK_UP). Deliberately
//!   absent: checksum/GSO offloads, MRG_RXBUF, MTU reporting, multi-queue,
//!   control virtqueue, VLAN filtering -- see virtio/mod.rs.
//! - The per-frame header is the 12-byte virtio_net_hdr_mrg_rxbuf
//!   (flags, gso_type, hdr_len, gso_size, csum_start, csum_offset,
//!   num_buffers). The pinned kernel's virtio_net (drivers/net/virtio_net.c,
//!   virtnet_probe) uses this size whenever VIRTIO_F_VERSION_1 is
//!   negotiated, even without VIRTIO_NET_F_MRG_RXBUF -- verified against the
//!   v6.18 source, and confirmed live: the guest's transmit buffers carry
//!   12 zero header bytes inline before the frame.
//! - Frames larger than the device MTU (1500-byte payload, 1514 bytes on
//!   the wire) are dropped and counted, never silently truncated.
//!
//! Queue servicing is driven by the memory dispatch: a QueueNotify write
//! sets a per-queue pending flag, and the CPU periodic device tick calls
//! Memory::poll_virtio_net, which drains transmit kicks, polls the backend
//! (throttled to host wall clock), fills receive buffers, and raises the
//! used-ring interrupt edge when anything completed.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::net::{NetBackend, NetCounters};
use crate::virtio::{
    GuestMemory, VirtioError,
    queue::{self, Descriptor, MAX_CHAIN_DESCRIPTORS, QueueLayout},
};

/// virtio-mmio register offsets (shared with the block device).
const MAGIC_VALUE: u64 = 0x000;
const VERSION: u64 = 0x004;
const DEVICE_ID: u64 = 0x008;
const VENDOR_ID: u64 = 0x00C;
const DEVICE_FEATURES: u64 = 0x010;
const DEVICE_FEATURES_SEL: u64 = 0x014;
const DRIVER_FEATURES: u64 = 0x020;
const DRIVER_FEATURES_SEL: u64 = 0x024;
const QUEUE_SEL: u64 = 0x030;
const QUEUE_NUM_MAX: u64 = 0x034;
const QUEUE_NUM: u64 = 0x038;
const QUEUE_READY: u64 = 0x044;
const QUEUE_NOTIFY: u64 = 0x050;
const INTERRUPT_STATUS: u64 = 0x060;
const INTERRUPT_ACK: u64 = 0x064;
const STATUS: u64 = 0x070;
const QUEUE_DESC_LOW: u64 = 0x080;
const QUEUE_DESC_HIGH: u64 = 0x084;
const QUEUE_AVAIL_LOW: u64 = 0x090;
const QUEUE_AVAIL_HIGH: u64 = 0x094;
const QUEUE_USED_LOW: u64 = 0x0A0;
const QUEUE_USED_HIGH: u64 = 0x0A4;
const CONFIG_GENERATION: u64 = 0x0FC;
/// Net config space starts right after the spec register block.
const CONFIG_SPACE: u64 = 0x100;

/// virtio-net device id.
pub const VIRTIO_NET_DEVICE_ID: u32 = 1;

/// Vendor id, spelling "RISH" little-endian.
const VENDOR_ID_RISH: u32 = 0x5249_5348;

/// Device status bit the device watches: the driver finished feature
/// negotiation (virtio spec section 2.1).
const STATUS_FEATURES_OK: u32 = 8;

/// Feature bits the device offers.
const VIRTIO_F_VERSION_1: u64 = 1 << 32;
const VIRTIO_NET_F_MAC: u64 = 1 << 5;
const VIRTIO_NET_F_STATUS: u64 = 1 << 16;

/// Offered features: modern transport, a config-space MAC, and a status
/// register reporting LINK_UP. Deliberately absent: CSUM/GSO offloads,
/// MRG_RXBUF, MTU, MQ, control virtqueue -- see virtio/mod.rs.
pub const OFFERED_FEATURES: u64 = VIRTIO_F_VERSION_1 | VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS;

/// Largest queue the device accepts.
pub const QUEUE_NUM_MAX_VALUE: u16 = 128;

/// Queue indices: 0 receives, 1 transmits.
pub const QUEUE_RX: usize = 0;
pub const QUEUE_TX: usize = 1;
const QUEUE_COUNT: usize = 2;

/// Link status reported in the config space: up.
const VIRTIO_NET_S_LINK_UP: u16 = 1;

/// Size of the virtio_net_hdr prepended to every frame. The pinned guest
/// kernel's virtio_net uses sizeof(struct virtio_net_hdr_mrg_rxbuf) = 12
/// whenever VIRTIO_F_VERSION_1 is negotiated (see virtnet_probe in
/// drivers/net/virtio_net.c), so this device must match that exactly.
pub const VIRTIO_NET_HDR_BYTES: usize = 12;

/// Ethernet frame limit: 14-byte header plus a 1500-byte IP payload. Frames
/// past this are dropped, never truncated.
pub const MAX_FRAME_BYTES: usize = 1514;

/// Receive-side backlog cap: frames the backend produced but the driver has
/// not yet collected. Oldest frames are dropped with a counter when full,
/// the way a real NIC drops on buffer exhaustion.
const RX_BACKLOG_CAP: usize = 128;

/// Receive buffers filled per poll, keeping the device tick bounded.
const RX_BATCH_PER_POLL: usize = 32;

/// Backend (host socket, timer) poll throttle: the CPU tick runs every 64
/// instructions, and hitting real host sockets on every tick would burn
/// syscalls for nothing. One millisecond of host wall time is plenty for
/// guest-perceived latency.
const BACKEND_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Used-ring interrupt bit in the ISR register.
const INTERRUPT_USED_RING: u32 = 0x1;

/// The virtio-mmio network device.
pub struct VirtioMmioNet {
    backend: Box<dyn NetBackend>,
    mac: [u8; 6],
    status: u32,
    device_feature_sel: u32,
    driver_feature_sel: u32,
    driver_features: [u32; 2],
    negotiated_features: u64,
    queue_sel: u32,
    queues: [QueueLayout; QUEUE_COUNT],
    queue_ready: [bool; QUEUE_COUNT],
    kick_pending: [bool; QUEUE_COUNT],
    last_seen_avail: [u16; QUEUE_COUNT],
    irq_status: u32,
    rx_backlog: VecDeque<Vec<u8>>,
    rx_dropped: u64,
    tx_dropped: u64,
    /// Notify writes seen, TX buffers drained, RX buffers filled -- for the
    /// RISH_DBG_NET diagnostics and honest operational counters.
    notify_count: u64,
    tx_drained: u64,
    rx_drained: u64,
    last_backend_poll: Instant,
    /// Sticky fail-closed latch, same discipline as the block device: once
    /// the guest handed the device an address outside its RAM or a
    /// structurally malformed chain, the device stops servicing until the
    /// machine restarts.
    fault: Option<String>,
}

impl VirtioMmioNet {
    pub fn new(backend: Box<dyn NetBackend>) -> Self {
        Self {
            mac: backend.mac(),
            backend,
            status: 0,
            device_feature_sel: 0,
            driver_feature_sel: 0,
            driver_features: [0; 2],
            negotiated_features: 0,
            queue_sel: 0,
            queues: [QueueLayout::default(); QUEUE_COUNT],
            queue_ready: [false; QUEUE_COUNT],
            kick_pending: [false; QUEUE_COUNT],
            last_seen_avail: [0; QUEUE_COUNT],
            irq_status: 0,
            rx_backlog: VecDeque::new(),
            rx_dropped: 0,
            tx_dropped: 0,
            notify_count: 0,
            tx_drained: 0,
            rx_drained: 0,
            last_backend_poll: Instant::now(),
            fault: None,
        }
    }

    /// The fixed MAC reported in the config space.
    #[must_use]
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// The sticky fail-closed fault, when the device latched one.
    #[must_use]
    pub fn fault(&self) -> Option<&str> {
        self.fault.as_deref()
    }

    /// Backend counters (ARP/ICMP/DNS/TCP totals), for diagnostics.
    #[must_use]
    pub fn backend_counters(&self) -> NetCounters {
        self.backend.counters()
    }

    /// One line of internal state for the RISH_DBG_NET diagnostics hook:
    /// queue selection, readiness, kick flags, and avail-ring positions.
    #[must_use]
    pub fn debug_state(&self) -> String {
        format!(
            "sel={} ready={:?} kick={:?} size={:?} last_seen={:?} backlog={} notify={} tx={} rx={} dropped={:?} counters={:?} fault={:?}",
            self.queue_sel,
            self.queue_ready,
            self.kick_pending,
            [self.queues[QUEUE_RX].size, self.queues[QUEUE_TX].size],
            self.last_seen_avail,
            self.rx_backlog.len(),
            self.notify_count,
            self.tx_drained,
            self.rx_drained,
            self.dropped(),
            self.backend.counters(),
            self.fault,
        )
    }

    /// Frames dropped because the driver left no receive buffer, the buffer
    /// was too small, or the frame exceeded the device MTU.
    #[must_use]
    pub fn dropped(&self) -> (u64, u64) {
        (self.rx_dropped, self.tx_dropped)
    }

    /// Reads one 32-bit word of the register file. offset may be any byte
    /// offset inside the register block; the word containing it is returned.
    pub fn mmio_read(&mut self, offset: u64) -> u32 {
        let word = offset & !3;
        let value = match word {
            MAGIC_VALUE => 0x7472_6976, // "virt", little-endian
            VERSION => 2,
            DEVICE_ID => VIRTIO_NET_DEVICE_ID,
            VENDOR_ID => VENDOR_ID_RISH,
            DEVICE_FEATURES => self.device_feature_word(),
            QUEUE_NUM_MAX => u32::from(QUEUE_NUM_MAX_VALUE),
            QUEUE_NUM => u32::from(self.queues[self.queue_sel as usize].size),
            QUEUE_READY => u32::from(self.queue_ready[self.queue_sel as usize]),
            INTERRUPT_STATUS => self.irq_status,
            STATUS => self.status,
            CONFIG_GENERATION => 0,
            word if word >= CONFIG_SPACE => self.config_word(word),
            _ => 0,
        };
        // Shift the word down for the requested byte offset: the driver
        // reads the MAC and status bytes one at a time, and an unshifted
        // word would repeat its first byte for every offset.
        value >> ((offset & 3) * 8)
    }

    /// Applies one 32-bit register write. A QueueNotify on a ready queue
    /// only sets the pending flag; the CPU device tick drains it.
    pub fn mmio_write(&mut self, offset: u64, value: u32) {
        let word = offset & !3;
        match word {
            DEVICE_FEATURES_SEL => self.device_feature_sel = value,
            DRIVER_FEATURES_SEL => self.driver_feature_sel = value,
            DRIVER_FEATURES => match self.driver_feature_sel {
                0 => self.driver_features[0] = value,
                1 => self.driver_features[1] = value,
                _ => {}
            },
            QUEUE_SEL => {
                if value < QUEUE_COUNT as u32 {
                    self.queue_sel = value;
                }
            }
            QUEUE_NUM => {
                if !self.queue_ready[self.queue_sel as usize] {
                    self.queues[self.queue_sel as usize].size =
                        if value > 0 && value <= u32::from(QUEUE_NUM_MAX_VALUE) {
                            value as u16
                        } else {
                            0
                        };
                }
            }
            QUEUE_READY => {
                let selected = self.queue_sel as usize;
                if value == 0 {
                    self.queue_ready[selected] = false;
                    self.last_seen_avail[selected] = 0;
                } else if value == 1 && self.queues[selected].size > 0 {
                    self.queue_ready[selected] = true;
                }
            }
            QUEUE_NOTIFY => {
                // The written value IS the queue index (the driver does
                // not select the queue first); any other value is invalid.
                if value < QUEUE_COUNT as u32 && self.queue_ready[value as usize] {
                    self.kick_pending[value as usize] = true;
                    self.notify_count = self.notify_count.saturating_add(1);
                }
            }
            INTERRUPT_ACK => self.irq_status &= !value,
            STATUS => {
                if value == 0 {
                    self.reset();
                } else {
                    self.status = value;
                    if value & STATUS_FEATURES_OK != 0 {
                        let driver = (u64::from(self.driver_features[1]) << 32)
                            | u64::from(self.driver_features[0]);
                        self.negotiated_features = OFFERED_FEATURES & driver;
                    }
                }
            }
            QUEUE_DESC_LOW => {
                let layout = &mut self.queues[self.queue_sel as usize];
                layout.desc = merge_low(layout.desc, value);
            }
            QUEUE_DESC_HIGH => {
                let layout = &mut self.queues[self.queue_sel as usize];
                layout.desc = merge_high(layout.desc, value);
            }
            QUEUE_AVAIL_LOW => {
                let layout = &mut self.queues[self.queue_sel as usize];
                layout.avail = merge_low(layout.avail, value);
            }
            QUEUE_AVAIL_HIGH => {
                let layout = &mut self.queues[self.queue_sel as usize];
                layout.avail = merge_high(layout.avail, value);
            }
            QUEUE_USED_LOW => {
                let layout = &mut self.queues[self.queue_sel as usize];
                layout.used = merge_low(layout.used, value);
            }
            QUEUE_USED_HIGH => {
                let layout = &mut self.queues[self.queue_sel as usize];
                layout.used = merge_high(layout.used, value);
            }
            // Read-only, unimplemented (shared-memory), and reserved
            // registers: discarded.
            _ => {}
        }
    }

    /// Services the device: drains transmit kicks, polls the host backend
    /// (wall-clock throttled), and fills receive buffers from the backlog.
    /// Returns true when at least one used entry was published and the
    /// used-ring interrupt edge should be raised. A malformed queue or an
    /// out-of-RAM access latches the device fault and stops servicing
    /// instead of guessing.
    pub fn poll<M: GuestMemory>(&mut self, memory: &mut M) -> Result<bool, VirtioError> {
        if self.fault.is_some() {
            return Ok(false);
        }
        match self.poll_inner(memory) {
            Ok(completed) => {
                if completed {
                    self.irq_status |= INTERRUPT_USED_RING;
                }
                Ok(completed)
            }
            Err(error) => {
                self.fault = Some(format!("virtio-net stopped servicing: {error}"));
                Ok(false)
            }
        }
    }

    fn poll_inner<M: GuestMemory>(&mut self, memory: &mut M) -> Result<bool, VirtioError> {
        let mut completed = false;
        // Transmit kicks are guest-driven: drain them promptly.
        if self.kick_pending[QUEUE_TX] {
            self.kick_pending[QUEUE_TX] = false;
            if self.queue_ready[QUEUE_TX] && self.queues[QUEUE_TX].size > 0 {
                completed |= self.drain_tx(memory)?;
            }
        }
        // The backend touches real host sockets: poll it on host wall clock,
        // not every 64-instruction device tick.
        let now = Instant::now();
        if now.duration_since(self.last_backend_poll) >= BACKEND_POLL_INTERVAL {
            self.last_backend_poll = now;
            let mut frames = Vec::new();
            self.backend.poll(&mut frames);
            for frame in frames {
                if self.rx_backlog.len() >= RX_BACKLOG_CAP {
                    self.rx_backlog.pop_front();
                    self.rx_dropped = self.rx_dropped.saturating_add(1);
                }
                self.rx_backlog.push_back(frame);
            }
        }
        // Receive buffers are host-driven: fill whenever both a backlog and
        // a ready queue exist, whether or not the driver kicked.
        if self.queue_ready[QUEUE_RX] && self.queues[QUEUE_RX].size > 0 {
            completed |= self.drain_rx(memory)?;
        }
        Ok(completed)
    }

    fn reset(&mut self) {
        self.status = 0;
        self.driver_features = [0; 2];
        self.driver_feature_sel = 0;
        self.negotiated_features = 0;
        self.queue_sel = 0;
        self.queues = [QueueLayout::default(); QUEUE_COUNT];
        self.queue_ready = [false; QUEUE_COUNT];
        self.kick_pending = [false; QUEUE_COUNT];
        self.last_seen_avail = [0; QUEUE_COUNT];
        self.irq_status = 0;
        self.rx_backlog.clear();
        // The fault latch deliberately survives a device reset: it marks a
        // machine-level integrity violation, not driver state.
    }

    fn device_feature_word(&self) -> u32 {
        match self.device_feature_sel {
            0 => OFFERED_FEATURES as u32,
            1 => (OFFERED_FEATURES >> 32) as u32,
            _ => 0,
        }
    }

    fn config_word(&self, offset: u64) -> u32 {
        match offset & !3 {
            // mac[0..4]
            0x100 => u32::from_le_bytes([self.mac[0], self.mac[1], self.mac[2], self.mac[3]]),
            // mac[4..6] then the status register.
            0x104 => {
                u32::from(self.mac[4])
                    | (u32::from(self.mac[5]) << 8)
                    | (u32::from(VIRTIO_NET_S_LINK_UP) << 16)
            }
            _ => 0,
        }
    }

    /// Drains transmit buffers published since the last kick. Every chain is
    /// validated in full before anything is consumed; a frame whose payload
    /// exceeds the device MTU is dropped and counted, and the buffer is
    /// still completed so the ring cannot wedge.
    fn drain_tx<M: GuestMemory>(&mut self, memory: &mut M) -> Result<bool, VirtioError> {
        let queue = self.queues[QUEUE_TX];
        queue.validate(memory.ram_bytes())?;
        let avail = queue::avail_index(memory, &queue)?;
        let mut completed = 0_u32;
        while self.last_seen_avail[QUEUE_TX] != avail {
            let head = queue::avail_head(memory, &queue, self.last_seen_avail[QUEUE_TX])?;
            let length = self.process_tx(memory, head)?;
            let used_index = queue::used_index(memory, &queue)?;
            queue::write_used(memory, &queue, used_index, u32::from(head), length)?;
            self.last_seen_avail[QUEUE_TX] = self.last_seen_avail[QUEUE_TX].wrapping_add(1);
            self.tx_drained = self.tx_drained.saturating_add(1);
            completed += 1;
        }
        Ok(completed > 0)
    }

    /// Services one transmit chain: device-readable descriptors holding the
    /// 12-byte virtio_net_hdr and the Ethernet frame, matching the pinned
    /// kernel's virtio_net exactly. With VIRTIO_F_VERSION_1 the driver may
    /// push the header inline with the data (one descriptor starting with
    /// 12 header bytes) or keep it in its own descriptor; both shapes are
    /// accepted. Returns the byte length published in the used ring.
    fn process_tx<M: GuestMemory>(
        &mut self,
        memory: &mut M,
        head: u16,
    ) -> Result<u32, VirtioError> {
        let queue = self.queues[QUEUE_TX];
        let mut chain = [Descriptor::default(); MAX_CHAIN_DESCRIPTORS];
        let count = queue::read_chain(memory, &queue, head, &mut chain)?;
        if count == 0 {
            return Err(VirtioError::BadQueue("transmit chain is empty"));
        }
        // Every descriptor must be device-readable.
        let mut total: u128 = 0;
        for descriptor in &chain[..count] {
            if descriptor.device_writable() {
                return Err(VirtioError::BadQueue(
                    "transmit descriptor is device-writable",
                ));
            }
            total += u128::from(descriptor.length);
        }
        // Validate the whole chain even when the frame will be dropped:
        // the device never touches memory it has not proven to be RAM.
        for descriptor in &chain[..count] {
            memory.check_range(descriptor.address, u64::from(descriptor.length))?;
        }
        // The header occupies the first 12 bytes of the chain. The split
        // shape (header in its own descriptor) is recognized by a first
        // descriptor of exactly the header size followed by payload.
        let split_header = count >= 2 && chain[0].length == VIRTIO_NET_HDR_BYTES as u32;
        let frame_bytes = if split_header || chain[0].length > VIRTIO_NET_HDR_BYTES as u32 {
            total - VIRTIO_NET_HDR_BYTES as u128
        } else {
            return Err(VirtioError::BadQueue(
                "transmit buffer is smaller than the virtio_net_hdr",
            ));
        };
        if frame_bytes > MAX_FRAME_BYTES as u128 {
            self.tx_dropped = self.tx_dropped.saturating_add(1);
            return Ok(total as u32);
        }
        let frame_len = frame_bytes as usize;
        // Drain the header (no offload features are offered, so its
        // contents carry no meaning) and gather the frame across the
        // descriptors, skipping the header bytes at the chain start.
        let mut header_bytes = [0_u8; VIRTIO_NET_HDR_BYTES];
        memory.read(chain[0].address, &mut header_bytes)?;
        let mut frame = vec![0_u8; frame_len];
        let mut written = 0_usize;
        for (index, descriptor) in chain[..count].iter().enumerate() {
            let start = if index == 0 {
                VIRTIO_NET_HDR_BYTES.min(descriptor.length as usize)
            } else {
                0
            };
            let step = (descriptor.length as usize)
                .saturating_sub(start)
                .min(frame_len - written);
            if step > 0 {
                memory.read(
                    descriptor.address + start as u64,
                    &mut frame[written..written + step],
                )?;
                written += step;
            }
        }
        if written != frame_len {
            return Err(VirtioError::BadQueue(
                "transmit chain does not cover the advertised frame length",
            ));
        }
        if frame_len > 0 {
            self.backend.enqueue(&frame);
        } else {
            self.tx_dropped = self.tx_dropped.saturating_add(1);
        }
        Ok(total as u32)
    }

    /// Fills posted receive buffers from the backlog: a 10-byte header and
    /// the frame, or a zero-length completion when the frame cannot fit.
    fn drain_rx<M: GuestMemory>(&mut self, memory: &mut M) -> Result<bool, VirtioError> {
        let queue = self.queues[QUEUE_RX];
        queue.validate(memory.ram_bytes())?;
        let mut completed = 0_u32;
        for _ in 0..RX_BATCH_PER_POLL {
            if self.rx_backlog.is_empty() {
                break;
            }
            let avail = queue::avail_index(memory, &queue)?;
            if self.last_seen_avail[QUEUE_RX] == avail {
                break;
            }
            let head = queue::avail_head(memory, &queue, self.last_seen_avail[QUEUE_RX])?;
            let frame = self.rx_backlog.pop_front().unwrap_or_default();
            let length = self.process_rx(memory, head, &frame)?;
            let used_index = queue::used_index(memory, &queue)?;
            queue::write_used(memory, &queue, used_index, u32::from(head), length)?;
            self.last_seen_avail[QUEUE_RX] = self.last_seen_avail[QUEUE_RX].wrapping_add(1);
            self.rx_drained = self.rx_drained.saturating_add(1);
            completed += 1;
        }
        Ok(completed > 0)
    }

    /// Writes one frame into a posted receive buffer: the 12-byte header
    /// followed by the frame, matching the pinned kernel's virtio_net (a
    /// single descriptor per buffer in the non-mergeable path, with the
    /// header at the buffer start). A buffer too small for the frame
    /// completes with length zero: the frame is dropped, never truncated.
    fn process_rx<M: GuestMemory>(
        &mut self,
        memory: &mut M,
        head: u16,
        frame: &[u8],
    ) -> Result<u32, VirtioError> {
        let queue = self.queues[QUEUE_RX];
        let mut chain = [Descriptor::default(); MAX_CHAIN_DESCRIPTORS];
        let count = queue::read_chain(memory, &queue, head, &mut chain)?;
        if count == 0 {
            return Err(VirtioError::BadQueue("receive chain is empty"));
        }
        let mut capacity: u128 = 0;
        for descriptor in &chain[..count] {
            if !descriptor.device_writable() {
                return Err(VirtioError::BadQueue(
                    "receive descriptor is not device-writable",
                ));
            }
            capacity += u128::from(descriptor.length);
        }
        if capacity < VIRTIO_NET_HDR_BYTES as u128 {
            return Err(VirtioError::BadQueue(
                "receive buffer is smaller than the virtio_net_hdr",
            ));
        }
        let payload_capacity = capacity - VIRTIO_NET_HDR_BYTES as u128;
        if frame.len() as u128 > payload_capacity {
            self.rx_dropped = self.rx_dropped.saturating_add(1);
            return Ok(0);
        }
        // Zero header: no offloads, no GSO, no csum, no mergeable buffers.
        let header_bytes = [0_u8; VIRTIO_NET_HDR_BYTES];
        if chain[0].length >= VIRTIO_NET_HDR_BYTES as u32 {
            memory.write(chain[0].address, &header_bytes)?;
        } else {
            memory.write(chain[0].address, &header_bytes[..chain[0].length as usize])?;
            let mut rest = chain[0].length as usize;
            for descriptor in &chain[1..count] {
                let step = (VIRTIO_NET_HDR_BYTES - rest).min(descriptor.length as usize);
                memory.write(descriptor.address, &header_bytes[rest..rest + step])?;
                rest += step;
                if rest == VIRTIO_NET_HDR_BYTES {
                    break;
                }
            }
        }
        // Spread the frame across the descriptors after the 12 header
        // bytes at the chain start.
        let mut written = 0_usize;
        for (index, descriptor) in chain[..count].iter().enumerate() {
            let start = if index == 0 {
                VIRTIO_NET_HDR_BYTES.min(descriptor.length as usize)
            } else {
                0
            };
            let step = (descriptor.length as usize)
                .saturating_sub(start)
                .min(frame.len() - written);
            if step > 0 {
                memory.write(
                    descriptor.address + start as u64,
                    &frame[written..written + step],
                )?;
                written += step;
            }
        }
        Ok((VIRTIO_NET_HDR_BYTES + frame.len()) as u32)
    }
}

fn merge_low(current: u64, value: u32) -> u64 {
    (current & !0xFFFF_FFFF) | u64::from(value)
}

fn merge_high(current: u64, value: u32) -> u64 {
    (current & 0xFFFF_FFFF) | (u64::from(value) << 32)
}

#[cfg(test)]
#[path = "net_tests.rs"]
mod tests;
