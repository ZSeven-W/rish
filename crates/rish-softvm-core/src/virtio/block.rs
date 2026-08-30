//! The virtio-mmio block device: register file, feature negotiation, the
//! one split virtqueue, and virtio-blk request processing.
//!
//! This module follows the modern (v2) virtio-mmio register layout. Queue
//! servicing is driven by the memory dispatch: a QueueNotify write sets a
//! pending flag, and the CPU periodic device tick drains the queue through
//! Memory::poll_virtio_irq, which then raises the used-ring interrupt edge.
//!
//! Every guest-physical access goes through the GuestMemory bounds checks;
//! anything outside guest RAM fails closed and latches a sticky device
//! fault instead of panicking or touching memory the guest does not own.

use crate::virtio::{
    GuestMemory, VirtioError,
    backend::BlockBackend,
    queue::{self, Descriptor, MAX_CHAIN_DESCRIPTORS, QueueLayout},
};
/// virtio-mmio register offsets.
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
/// Block config space starts right after the spec register block.
const CONFIG_SPACE: u64 = 0x100;

/// virtio-blk device id.
pub const VIRTIO_BLK_DEVICE_ID: u32 = 2;

/// Block size reported in the config space, and the sector unit of every
/// request.
pub const VIRTIO_BLK_SECTOR_SIZE: u64 = 512;

/// Vendor id, spelling "RISH" little-endian.
const VENDOR_ID_RISH: u32 = 0x5249_5348;

/// Device status bit the device watches: the driver finished feature
/// negotiation (virtio spec section 2.1).
const STATUS_FEATURES_OK: u32 = 8;
/// Device status bit gating queue use: the driver is ready to drive the
/// device (virtio spec section 2.1). Kicks before this bit is set are
/// dropped instead of serviced.
const STATUS_DRIVER_OK: u32 = 4;

/// Feature bits the device offers.
const VIRTIO_F_VERSION_1: u64 = 1 << 32;
const VIRTIO_BLK_F_SEG_MAX: u64 = 1 << 1;
const VIRTIO_BLK_F_BLK_SIZE: u64 = 1 << 4;

/// Offered features: modern transport, a bounded segment count, and an
/// explicit block size. Deliberately absent: FLUSH, DISCARD, WRITE_ZEROES,
/// MQ, geometry/topology, size-max -- see virtio/mod.rs.
pub const OFFERED_FEATURES: u64 = VIRTIO_F_VERSION_1 | VIRTIO_BLK_F_SEG_MAX | VIRTIO_BLK_F_BLK_SIZE;

/// Largest queue the device accepts.
pub const QUEUE_NUM_MAX_VALUE: u16 = 128;

/// Smallest queue the device lets the driver mark ready: a request chain
/// needs a header, at least one data descriptor, and a status descriptor,
/// so anything smaller can only ever fault on the first kick.
const MIN_SERVICEABLE_QUEUE_SIZE: u16 = 3;

/// Max segments per request, reported when VIRTIO_BLK_F_SEG_MAX is
/// negotiated. A chain is header + data segments + status, and the device
/// walks at most MAX_CHAIN_DESCRIPTORS entries, so the advertised value
/// must never let a conforming driver submit a chain the device would have
/// to fault on: SEG_MAX + 2 == MAX_CHAIN_DESCRIPTORS.
pub const SEG_MAX: u32 = 62;

/// Reported block size (bytes).
pub const BLK_SIZE: u32 = 512;

/// virtio-blk request types.
const REQ_IN: u32 = 0;
const REQ_OUT: u32 = 1;
const REQ_FLUSH: u32 = 4;
const REQ_GET_ID: u32 = 8;
const REQ_DISCARD: u32 = 10;
const REQ_WRITE_ZEROES: u32 = 11;

/// Per-request status bytes written into the final chain descriptor.
const STATUS_BYTE_OK: u8 = 0;
const STATUS_BYTE_IOERR: u8 = 1;
const STATUS_BYTE_UNSUPP: u8 = 2;

/// Used-ring interrupt bit in the ISR register.
const INTERRUPT_USED_RING: u32 = 0x1;

/// Fixed device serial returned for VIRTIO_BLK_T_GET_ID.
const SERIAL: &[u8] = b"RISH-VIRTIO-BLK-01";

/// Chunk size for data movement; keeps per-request host allocations bounded
/// even when a broken guest submits a multi-gigabyte segment.
const TRANSFER_CHUNK_BYTES: usize = 64 * 1024;

/// Header layout of one virtio-blk request: type, reserved, sector.
const HEADER_BYTES: usize = 16;
/// The virtio-mmio block device.
pub struct VirtioMmioBlk {
    backend: Box<dyn BlockBackend>,
    capacity_sectors: u64,
    status: u32,
    device_feature_sel: u32,
    driver_feature_sel: u32,
    driver_features: [u32; 2],
    negotiated_features: u64,
    queue_sel: u32,
    queue: QueueLayout,
    queue_ready: bool,
    irq_status: u32,
    last_seen_avail: u16,
    kick_pending: bool,
    /// Sticky fail-closed latch: once the guest handed the device an
    /// address outside its RAM or a structurally malformed chain, further
    /// kicks are ignored until the machine restarts.
    fault: Option<String>,
}

impl VirtioMmioBlk {
    pub fn new(backend: Box<dyn BlockBackend>) -> Result<Self, VirtioError> {
        let length = backend.length();
        if length == 0 {
            return Err(VirtioError::Backend("block backend is empty".to_owned()));
        }
        if length % VIRTIO_BLK_SECTOR_SIZE != 0 {
            return Err(VirtioError::Backend(format!(
                "block backend size {length} is not a multiple of {VIRTIO_BLK_SECTOR_SIZE} bytes"
            )));
        }
        Ok(Self {
            backend,
            capacity_sectors: length / VIRTIO_BLK_SECTOR_SIZE,
            status: 0,
            device_feature_sel: 0,
            driver_feature_sel: 0,
            driver_features: [0; 2],
            negotiated_features: 0,
            queue_sel: 0,
            queue: QueueLayout::default(),
            queue_ready: false,
            irq_status: 0,
            last_seen_avail: 0,
            kick_pending: false,
            fault: None,
        })
    }

    /// Capacity in 512-byte sectors, as reported in the config space.
    #[must_use]
    pub fn capacity_sectors(&self) -> u64 {
        self.capacity_sectors
    }

    /// The sticky fail-closed fault, when the device latched one.
    #[must_use]
    pub fn fault(&self) -> Option<&str> {
        self.fault.as_deref()
    }

    /// Reads one 32-bit word of the register file. offset may be any byte
    /// offset inside the register block; the word containing it is returned.
    pub fn mmio_read(&mut self, offset: u64) -> u32 {
        let word = offset & !3;
        match word {
            MAGIC_VALUE => 0x7472_6976, // "virt", little-endian
            VERSION => 2,
            DEVICE_ID => VIRTIO_BLK_DEVICE_ID,
            VENDOR_ID => VENDOR_ID_RISH,
            DEVICE_FEATURES => self.device_feature_word(),
            QUEUE_NUM_MAX => u32::from(QUEUE_NUM_MAX_VALUE),
            QUEUE_NUM => u32::from(self.queue.size),
            QUEUE_READY => u32::from(self.queue_ready),
            INTERRUPT_STATUS => self.irq_status,
            STATUS => self.status,
            CONFIG_GENERATION => 0,
            offset if offset >= CONFIG_SPACE => self.config_word(offset),
            _ => 0,
        }
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
            QUEUE_SEL => self.queue_sel = value,
            QUEUE_NUM => {
                // The device has exactly one queue; configuration written
                // through any other selection must not reach it.
                if self.queue_sel == 0 && !self.queue_ready {
                    self.queue.size = if value > 0 && value <= u32::from(QUEUE_NUM_MAX_VALUE) {
                        value as u16
                    } else {
                        0
                    };
                }
            }
            QUEUE_READY => {
                if self.queue_sel != 0 {
                    // No such queue: reject instead of programming queue 0.
                } else if value == 0 {
                    self.queue_ready = false;
                    self.last_seen_avail = 0;
                } else if value == 1 && self.queue.size >= MIN_SERVICEABLE_QUEUE_SIZE {
                    self.queue_ready = true;
                }
            }
            QUEUE_NOTIFY => {
                if value == self.queue_sel && self.queue_ready {
                    self.kick_pending = true;
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
            QUEUE_DESC_LOW if self.queue_sel == 0 => {
                self.queue.desc = merge_low(self.queue.desc, value)
            }
            QUEUE_DESC_HIGH if self.queue_sel == 0 => {
                self.queue.desc = merge_high(self.queue.desc, value)
            }
            QUEUE_AVAIL_LOW if self.queue_sel == 0 => {
                self.queue.avail = merge_low(self.queue.avail, value)
            }
            QUEUE_AVAIL_HIGH if self.queue_sel == 0 => {
                self.queue.avail = merge_high(self.queue.avail, value)
            }
            QUEUE_USED_LOW if self.queue_sel == 0 => {
                self.queue.used = merge_low(self.queue.used, value)
            }
            QUEUE_USED_HIGH if self.queue_sel == 0 => {
                self.queue.used = merge_high(self.queue.used, value)
            }
            // Read-only, unimplemented (shared-memory), and reserved
            // registers: discarded.
            _ => {}
        }
    }

    /// Drains pending requests from the available ring. Returns true when at
    /// least one request completed and the used-ring interrupt edge should
    /// be raised. A malformed queue or an out-of-RAM access latches the
    /// device fault and stops servicing instead of guessing.
    pub fn poll_kick<M: GuestMemory>(&mut self, memory: &mut M) -> Result<bool, VirtioError> {
        if !self.kick_pending {
            return Ok(false);
        }
        self.kick_pending = false;
        // The virtio lifecycle gates queue use on DRIVER_OK: a kick before
        // the driver finished negotiation is dropped, never serviced.
        if self.fault.is_some()
            || !self.queue_ready
            || self.queue.size == 0
            || self.status & STATUS_DRIVER_OK == 0
        {
            return Ok(false);
        }
        let drained = (|| {
            // The shared drain skeleton: validate the rings, bound the
            // avail delta by the queue size, walk every head through the
            // request processor (which validates each whole chain before
            // moving data), and publish one used entry per completion.
            let queue_layout = self.queue;
            let mut last_seen = self.last_seen_avail;
            let completed =
                queue::drain_available(memory, &queue_layout, &mut last_seen, |memory, head| {
                    self.process_request(memory, head)
                })?;
            self.last_seen_avail = last_seen;
            Ok::<u32, VirtioError>(completed)
        })();
        match drained {
            Ok(completed) => {
                if completed > 0 {
                    self.irq_status |= INTERRUPT_USED_RING;
                }
                Ok(completed > 0)
            }
            Err(error) => {
                // Any queue-level failure (a malformed chain or an address
                // outside guest RAM) latches the device: further kicks are
                // ignored instead of the device guessing.
                self.fault = Some(format!("virtio-blk stopped servicing: {error}"));
                Ok(false)
            }
        }
    }
    fn reset(&mut self) {
        self.status = 0;
        self.driver_features = [0; 2];
        self.driver_feature_sel = 0;
        self.negotiated_features = 0;
        self.queue = QueueLayout::default();
        self.queue_ready = false;
        self.irq_status = 0;
        self.last_seen_avail = 0;
        self.kick_pending = false;
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
            0x100 => self.capacity_sectors as u32,
            0x104 => (self.capacity_sectors >> 32) as u32,
            0x108 => 0, // size_max: not offered
            0x10C => SEG_MAX,
            0x110 => 0, // legacy geometry
            0x114 => BLK_SIZE,
            _ => 0,
        }
    }

    /// Services one request: validates the whole chain first — every
    /// descriptor inside guest RAM, the permission bits each request type
    /// demands, and the sector-aligned length semantics — and only then
    /// moves a single byte between the guest and the backend. A malformed
    /// chain fails closed before any data movement or disk write, so a
    /// hostile request can never leave a partial copy in guest RAM or a
    /// partial write on the host disk. Returns the byte length published in
    /// the used ring: the bytes written into device-writable descriptors
    /// (data plus the status byte for IN/GET_ID, the status byte alone for
    /// OUT and error completions).
    fn process_request<M: GuestMemory>(
        &mut self,
        memory: &mut M,
        head: u16,
    ) -> Result<u32, VirtioError> {
        let mut chain = [Descriptor::default(); MAX_CHAIN_DESCRIPTORS];
        // Phase 1: the whole chain is walked and proven inside guest RAM
        // before anything else (the shared two-phase gate).
        let validated = queue::validated_chain(memory, &self.queue, head, &mut chain)?;
        if validated.count() < 3 {
            return Err(VirtioError::BadQueue(
                "request chain has fewer than three descriptors",
            ));
        }
        let descriptors = validated.descriptors();
        let header = descriptors[0];
        let status = descriptors[validated.count() - 1];
        if header.length < HEADER_BYTES as u32 || header.device_writable() {
            return Err(VirtioError::BadQueue(
                "request header is missing or device-writable",
            ));
        }
        if status.length < 1 || !status.device_writable() {
            return Err(VirtioError::BadQueue(
                "request status descriptor is missing or not device-writable",
            ));
        }
        // Reading the header is a pure guest read and part of validation:
        // it moves nothing and touches no host state.
        let mut header_bytes = [0_u8; HEADER_BYTES];
        memory.read(header.address, &mut header_bytes)?;
        let request_type = u32::from_le_bytes(header_bytes[0..4].try_into().unwrap());
        let sector = u64::from_le_bytes(header_bytes[8..16].try_into().unwrap());
        let data = &descriptors[1..validated.count() - 1];
        // Data descriptors must carry the direction each request type
        // demands. Violations are structurally malformed chains, not
        // request-level errors.
        match request_type {
            REQ_IN | REQ_GET_ID => {
                for descriptor in data {
                    if !descriptor.device_writable() {
                        return Err(VirtioError::BadQueue(
                            "read request data descriptor is not device-writable",
                        ));
                    }
                }
            }
            REQ_OUT => {
                for descriptor in data {
                    if descriptor.device_writable() {
                        return Err(VirtioError::BadQueue(
                            "write request data descriptor is device-writable",
                        ));
                    }
                }
            }
            _ => {}
        }
        let mut total: u128 = 0;
        for descriptor in data {
            total += u128::from(descriptor.length);
        }
        // IN/OUT payloads are whole sectors; anything else is a driver
        // contract violation and fails closed.
        if matches!(request_type, REQ_IN | REQ_OUT) && total % VIRTIO_BLK_SECTOR_SIZE as u128 != 0 {
            return Err(VirtioError::BadQueue(
                "request length is not a multiple of the block size",
            ));
        }
        match request_type {
            REQ_IN => match self.checked_range(sector, total)? {
                None => {
                    self.write_status(memory, &status, STATUS_BYTE_IOERR)?;
                    return Ok(1);
                }
                Some(end) => self.copy_from_backend(memory, data, sector * 512, end)?,
            },
            REQ_OUT => match self.checked_range(sector, total)? {
                None => {
                    self.write_status(memory, &status, STATUS_BYTE_IOERR)?;
                    return Ok(1);
                }
                Some(end) => match self.copy_to_backend(memory, data, sector * 512, end) {
                    Ok(()) => {}
                    // Host backend failures complete the request with IOERR.
                    // The device cannot roll back chunks already committed
                    // without staging the whole request (which would break
                    // the bounded-allocation rule); the guest must treat
                    // IOERR as "request failed, data state undefined", the
                    // same contract real disks offer on host I/O errors.
                    Err(VirtioError::Backend(_)) => {
                        self.write_status(memory, &status, STATUS_BYTE_IOERR)?;
                        return Ok(1);
                    }
                    Err(fatal) => return Err(fatal),
                },
            },
            REQ_GET_ID => self.write_serial(memory, data)?,
            REQ_FLUSH | REQ_DISCARD | REQ_WRITE_ZEROES => {
                // Not offered in the feature set; a non-conforming driver
                // gets an explicit unsupported status, never data movement.
                self.write_status(memory, &status, STATUS_BYTE_UNSUPP)?;
                return Ok(1);
            }
            _ => {
                self.write_status(memory, &status, STATUS_BYTE_UNSUPP)?;
                return Ok(1);
            }
        };
        self.write_status(memory, &status, STATUS_BYTE_OK)?;
        // IN and GET_ID wrote the data plus the status byte; OUT wrote only
        // the status byte.
        let device_written = if request_type == REQ_OUT {
            1
        } else {
            total.saturating_add(1)
        };
        Ok(u32::try_from(device_written).unwrap_or(u32::MAX))
    }

    /// Validates the sector range against the backend capacity. Ok(None)
    /// means the range is out of bounds (the request then completes with
    /// IOERR); Ok(Some(end)) is the byte offset one past the last data byte.
    fn checked_range(&self, sector: u64, total: u128) -> Result<Option<u64>, VirtioError> {
        let start = u128::from(sector) * VIRTIO_BLK_SECTOR_SIZE as u128;
        let end = start
            .checked_add(total)
            .ok_or_else(|| VirtioError::Backend("request byte range overflowed".to_owned()))?;
        let capacity = u128::from(self.capacity_sectors) * VIRTIO_BLK_SECTOR_SIZE as u128;
        if end > capacity {
            return Ok(None);
        }
        // start + total <= capacity fits in u64, so this cast is exact.
        Ok(Some(end as u64))
    }

    /// Copies guest data segments out of the backend (read request), in
    /// bounded chunks so a hostile segment length cannot force a huge
    /// allocation.
    fn copy_from_backend<M: GuestMemory>(
        &mut self,
        memory: &mut M,
        data: &[Descriptor],
        start: u64,
        end: u64,
    ) -> Result<(), VirtioError> {
        let mut offset = start;
        for descriptor in data {
            let mut done = 0_u32;
            while done < descriptor.length {
                let step = descriptor
                    .length
                    .saturating_sub(done)
                    .min(TRANSFER_CHUNK_BYTES as u32) as usize;
                let mut buffer = vec![0_u8; step];
                self.backend.read_at(offset, &mut buffer)?;
                memory.write(descriptor.address + u64::from(done), &buffer)?;
                offset += step as u64;
                done += step as u32;
            }
        }
        debug_assert_eq!(offset, end);
        Ok(())
    }

    /// Copies guest data segments into the backend (write request), in
    /// bounded chunks.
    fn copy_to_backend<M: GuestMemory>(
        &mut self,
        memory: &mut M,
        data: &[Descriptor],
        start: u64,
        end: u64,
    ) -> Result<(), VirtioError> {
        let mut offset = start;
        for descriptor in data {
            let mut done = 0_u32;
            while done < descriptor.length {
                let step = descriptor
                    .length
                    .saturating_sub(done)
                    .min(TRANSFER_CHUNK_BYTES as u32) as usize;
                let mut buffer = vec![0_u8; step];
                memory.read(descriptor.address + u64::from(done), &mut buffer)?;
                self.backend.write_at(offset, &buffer)?;
                offset += step as u64;
                done += step as u32;
            }
        }
        debug_assert_eq!(offset, end);
        Ok(())
    }

    /// Writes the fixed device serial across the data segments, padding the
    /// remainder of the buffer with spaces (the virtio-blk GET_ID
    /// convention). Like the data paths it moves bytes in bounded chunks, so
    /// a hostile segment length (up to u32::MAX) can never force a
    /// multi-gigabyte host allocation.
    fn write_serial<M: GuestMemory>(
        &self,
        memory: &mut M,
        data: &[Descriptor],
    ) -> Result<(), VirtioError> {
        let mut written: u128 = 0;
        for descriptor in data {
            let mut done = 0_u32;
            while done < descriptor.length {
                let step = descriptor
                    .length
                    .saturating_sub(done)
                    .min(TRANSFER_CHUNK_BYTES as u32) as usize;
                let mut buffer = vec![0x20_u8; step]; // space padding
                let position = written + u128::from(done);
                if position < u128::from(SERIAL.len() as u32) {
                    let remaining = u128::from(SERIAL.len() as u32) - position;
                    let overlap = remaining.min(step as u128) as usize;
                    buffer[..overlap]
                        .copy_from_slice(&SERIAL[position as usize..position as usize + overlap]);
                }
                memory.write(descriptor.address + u64::from(done), &buffer)?;
                written += step as u128;
                done += step as u32;
            }
        }
        Ok(())
    }

    fn write_status<M: GuestMemory>(
        &self,
        memory: &mut M,
        status: &Descriptor,
        byte: u8,
    ) -> Result<(), VirtioError> {
        memory.write(status.address, &[byte])
    }
}

fn merge_low(current: u64, value: u32) -> u64 {
    (current & !0xFFFF_FFFF) | u64::from(value)
}

fn merge_high(current: u64, value: u32) -> u64 {
    (current & 0xFFFF_FFFF) | (u64::from(value) << 32)
}

#[cfg(test)]
#[path = "block_tests.rs"]
mod tests;
