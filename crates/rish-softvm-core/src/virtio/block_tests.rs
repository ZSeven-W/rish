use super::*;
use crate::virtio::queue::{DESC_FLAG_NEXT, DESC_FLAG_WRITE};
use crate::virtio::{backend::VecBlockBackend, queue::SliceMemory};

const RAM_BYTES: usize = 0x4000;
const DESC_BASE: u64 = 0x1000;
const AVAIL_BASE: u64 = 0x1100;
const USED_BASE: u64 = 0x1200;
const HEADER_ADDR: u64 = 0x1500;
const DATA_ADDR: u64 = 0x1600;
const STATUS_ADDR: u64 = 0x1900;
const DRIVER_OK: u32 = STATUS_DRIVER_OK;

/// A configured device plus the guest memory holding its rings.
struct Harness {
    device: VirtioMmioBlk,
    bytes: Vec<u8>,
    backend_bytes: Vec<u8>,
}

impl Harness {
    fn new() -> Self {
        let mut backend_bytes = vec![0_u8; 4096];
        for (index, byte) in backend_bytes.iter_mut().enumerate() {
            *byte = (index & 0xFF) as u8;
        }
        let device = VirtioMmioBlk::new(Box::new(VecBlockBackend {
            bytes: backend_bytes.clone(),
        }))
        .unwrap();
        Self {
            device,
            bytes: vec![0_u8; RAM_BYTES],
            backend_bytes,
        }
    }

    fn configure_queue(&mut self, size: u16) {
        self.device.mmio_write(QUEUE_SEL, 0);
        self.device.mmio_write(QUEUE_NUM, u32::from(size));
        self.device.mmio_write(QUEUE_DESC_LOW, DESC_BASE as u32);
        self.device.mmio_write(QUEUE_AVAIL_LOW, AVAIL_BASE as u32);
        self.device.mmio_write(QUEUE_USED_LOW, USED_BASE as u32);
        self.device.mmio_write(QUEUE_READY, 1);
        assert!(self.device.queue_ready);
    }

    /// Performs the driver-side feature negotiation and status dance the
    /// virtio spec requires before the first queue kick: the driver writes
    /// the offered features back, then sets FEATURES_OK plus DRIVER_OK.
    fn negotiate(&mut self) {
        self.device.mmio_write(DEVICE_FEATURES_SEL, 0);
        let word0 = self.device.mmio_read(DEVICE_FEATURES);
        self.device.mmio_write(DEVICE_FEATURES_SEL, 1);
        let word1 = self.device.mmio_read(DEVICE_FEATURES);
        self.device.mmio_write(DRIVER_FEATURES_SEL, 0);
        self.device.mmio_write(DRIVER_FEATURES, word0);
        self.device.mmio_write(DRIVER_FEATURES_SEL, 1);
        self.device.mmio_write(DRIVER_FEATURES, word1);
        self.device
            .mmio_write(STATUS, STATUS_FEATURES_OK | DRIVER_OK);
    }

    /// Publishes header/data/status descriptors and one available entry
    /// (the seq-th submission) describing them, then notifies the
    /// device.
    fn submit_read(&mut self, seq: u16, sector: u64, data_len: u32, kind: u32) {
        // OUT data descriptors are device-readable; IN and GET_ID demand
        // device-writable ones.
        let data_flags = if kind == REQ_OUT {
            DESC_FLAG_NEXT
        } else {
            DESC_FLAG_NEXT | DESC_FLAG_WRITE
        };
        self.write_desc(0, HEADER_ADDR, HEADER_BYTES as u32, DESC_FLAG_NEXT, 1);
        self.write_desc(1, DATA_ADDR, data_len, data_flags, 2);
        self.write_desc(2, STATUS_ADDR, 1, DESC_FLAG_WRITE, 0);
        let mut header = [0_u8; HEADER_BYTES];
        header[0..4].copy_from_slice(&kind.to_le_bytes());
        header[8..16].copy_from_slice(&sector.to_le_bytes());
        self.bytes[HEADER_ADDR as usize..HEADER_ADDR as usize + HEADER_BYTES]
            .copy_from_slice(&header);
        self.bytes[AVAIL_BASE as usize + 2..AVAIL_BASE as usize + 4]
            .copy_from_slice(&(seq + 1).to_le_bytes());
        self.bytes[AVAIL_BASE as usize + 4..AVAIL_BASE as usize + 6]
            .copy_from_slice(&0_u16.to_le_bytes());
        self.device.mmio_write(QUEUE_NOTIFY, 0);
        assert!(self.device.kick_pending);
    }

    /// Publishes a chain whose data descriptor addresses outside RAM.
    fn submit_bad_chain(&mut self) {
        self.write_desc(0, HEADER_ADDR, HEADER_BYTES as u32, DESC_FLAG_NEXT, 1);
        self.write_desc(
            1,
            RAM_BYTES as u64 + 0x1000,
            512,
            DESC_FLAG_NEXT | DESC_FLAG_WRITE,
            2,
        );
        self.write_desc(2, STATUS_ADDR, 1, DESC_FLAG_WRITE, 0);
        let mut header = [0_u8; HEADER_BYTES];
        header[0..4].copy_from_slice(&REQ_IN.to_le_bytes());
        self.bytes[HEADER_ADDR as usize..HEADER_ADDR as usize + HEADER_BYTES]
            .copy_from_slice(&header);
        self.bytes[AVAIL_BASE as usize + 2..AVAIL_BASE as usize + 4]
            .copy_from_slice(&1_u16.to_le_bytes());
        self.bytes[AVAIL_BASE as usize + 4..AVAIL_BASE as usize + 6]
            .copy_from_slice(&0_u16.to_le_bytes());
        self.device.mmio_write(QUEUE_NOTIFY, 0);
    }

    fn write_desc(&mut self, index: u16, address: u64, length: u32, flags: u16, next: u16) {
        let offset = DESC_BASE + u64::from(index) * 16;
        self.bytes[offset as usize..offset as usize + 8].copy_from_slice(&address.to_le_bytes());
        self.bytes[offset as usize + 8..offset as usize + 12]
            .copy_from_slice(&length.to_le_bytes());
        self.bytes[offset as usize + 12..offset as usize + 14]
            .copy_from_slice(&flags.to_le_bytes());
        self.bytes[offset as usize + 14..offset as usize + 16].copy_from_slice(&next.to_le_bytes());
    }

    fn used_entry(&self) -> (u32, u32) {
        self.used_entry_at(0)
    }

    fn used_entry_at(&self, slot: usize) -> (u32, u32) {
        let offset = USED_BASE as usize + 4 + slot * 8;
        let id = u32::from_le_bytes(self.bytes[offset..offset + 4].try_into().unwrap());
        let len = u32::from_le_bytes(self.bytes[offset + 4..offset + 8].try_into().unwrap());
        (id, len)
    }
}

#[test]
fn register_file_identifies_the_device() {
    let mut harness = Harness::new();
    assert_eq!(harness.device.mmio_read(MAGIC_VALUE), 0x7472_6976);
    assert_eq!(harness.device.mmio_read(VERSION), 2);
    assert_eq!(harness.device.mmio_read(DEVICE_ID), VIRTIO_BLK_DEVICE_ID);
    assert_eq!(harness.device.mmio_read(VENDOR_ID), VENDOR_ID_RISH);
    assert_eq!(harness.device.mmio_read(QUEUE_NUM_MAX), 128);
}

#[test]
fn config_space_reports_capacity_and_block_size() {
    let mut harness = Harness::new();
    assert_eq!(harness.device.capacity_sectors(), 8);
    assert_eq!(harness.device.mmio_read(0x100), 8);
    assert_eq!(harness.device.mmio_read(0x104), 0);
    assert_eq!(harness.device.mmio_read(0x10C), SEG_MAX);
    assert_eq!(harness.device.mmio_read(0x114), BLK_SIZE);
}

#[test]
fn device_features_select_their_halves() {
    let mut harness = Harness::new();
    assert_eq!(
        harness.device.mmio_read(DEVICE_FEATURES),
        OFFERED_FEATURES as u32
    );
    harness.device.mmio_write(DEVICE_FEATURES_SEL, 1);
    assert_eq!(
        harness.device.mmio_read(DEVICE_FEATURES),
        (OFFERED_FEATURES >> 32) as u32
    );
    harness.device.mmio_write(DEVICE_FEATURES_SEL, 2);
    assert_eq!(harness.device.mmio_read(DEVICE_FEATURES), 0);
}

#[test]
fn a_read_request_moves_backend_bytes_into_guest_memory() {
    let mut harness = Harness::new();
    harness.configure_queue(4);
    harness.negotiate();
    harness.submit_read(0, 0, 512, REQ_IN);
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(harness.device.poll_kick(&mut memory).unwrap());
    // The used ring published one completion for head 0 with the 512 data
    // bytes plus the status byte.
    assert_eq!(harness.used_entry(), (0, 513));
    // Guest memory holds the backend bytes; status is OK.
    let expected = &harness.backend_bytes[0..512];
    assert_eq!(
        &harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 512],
        expected
    );
    assert_eq!(harness.bytes[STATUS_ADDR as usize], STATUS_BYTE_OK);
    // The used-ring interrupt bit is set, and nothing latched a fault.
    assert_eq!(harness.device.irq_status & INTERRUPT_USED_RING, 1);
    assert!(harness.device.fault.is_none());
}

#[test]
fn a_write_request_moves_guest_bytes_into_the_backend() {
    let mut harness = Harness::new();
    harness.configure_queue(4);
    harness.negotiate();
    let payload = vec![0xAB_u8; 512];
    harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 512].copy_from_slice(&payload);
    harness.submit_read(0, 1, 512, REQ_OUT);
    {
        let mut memory = SliceMemory {
            bytes: &mut harness.bytes,
        };
        assert!(harness.device.poll_kick(&mut memory).unwrap());
    }
    // OUT wrote only the status byte into device-writable descriptors.
    assert_eq!(harness.used_entry(), (0, 1));
    assert_eq!(harness.bytes[STATUS_ADDR as usize], STATUS_BYTE_OK);
    // Read sector 1 back from the device and compare: the write reached
    // the backend byte for byte.
    harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 512].fill(0);
    harness.submit_read(1, 1, 512, REQ_IN);
    {
        let mut memory = SliceMemory {
            bytes: &mut harness.bytes,
        };
        assert!(harness.device.poll_kick(&mut memory).unwrap());
    }
    assert_eq!(harness.bytes[STATUS_ADDR as usize], STATUS_BYTE_OK);
    assert_eq!(
        &harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 512],
        &payload[..]
    );
}

#[test]
fn a_request_past_capacity_completes_with_ioerr_and_no_data_movement() {
    let mut harness = Harness::new();
    harness.configure_queue(4);
    harness.negotiate();
    // sector 8 is one past the 8-sector backend.
    harness.submit_read(0, 8, 512, REQ_IN);
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(harness.device.poll_kick(&mut memory).unwrap());
    // Only the status byte was written.
    assert_eq!(harness.used_entry(), (0, 1));
    assert_eq!(harness.bytes[STATUS_ADDR as usize], STATUS_BYTE_IOERR);
    assert!(
        harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 512]
            .iter()
            .all(|byte| *byte == 0)
    );
    // The device stays alive: it is a request-level error, not a fault.
    assert!(harness.device.fault.is_none());
    assert_eq!(harness.device.irq_status & INTERRUPT_USED_RING, 1);
}

#[test]
fn an_out_of_ram_descriptor_latches_a_fault_and_stops_servicing() {
    let mut harness = Harness::new();
    harness.configure_queue(4);
    harness.negotiate();
    harness.submit_bad_chain();
    {
        let mut memory = SliceMemory {
            bytes: &mut harness.bytes,
        };
        assert!(!harness.device.poll_kick(&mut memory).unwrap());
    }
    assert!(harness.device.fault.is_some());
    // A later, well-formed kick is ignored: fail closed, not guessing.
    harness.submit_read(0, 0, 512, REQ_IN);
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(!harness.device.poll_kick(&mut memory).unwrap());
}

#[test]
fn get_id_returns_the_fixed_serial() {
    let mut harness = Harness::new();
    harness.configure_queue(4);
    harness.negotiate();
    harness.submit_read(0, 0, 32, REQ_GET_ID);
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(harness.device.poll_kick(&mut memory).unwrap());
    assert_eq!(harness.bytes[STATUS_ADDR as usize], STATUS_BYTE_OK);
    let serial = &harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 32];
    assert_eq!(&serial[..SERIAL.len()], SERIAL);
    assert!(serial[SERIAL.len()..].iter().all(|byte| *byte == 0x20));
}

#[test]
fn flush_completes_unsupported_without_data_movement() {
    let mut harness = Harness::new();
    harness.configure_queue(4);
    harness.negotiate();
    harness.submit_read(0, 0, 512, REQ_FLUSH);
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(harness.device.poll_kick(&mut memory).unwrap());
    assert_eq!(harness.bytes[STATUS_ADDR as usize], STATUS_BYTE_UNSUPP);
    assert!(
        harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 512]
            .iter()
            .all(|byte| *byte == 0)
    );
}

#[test]
fn notify_on_an_unready_queue_is_ignored() {
    let mut harness = Harness::new();
    harness.device.mmio_write(QUEUE_NOTIFY, 0);
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(!harness.device.poll_kick(&mut memory).unwrap());
    assert_eq!(harness.device.irq_status, 0);
}

#[test]
fn an_oversized_queue_num_is_rejected() {
    let mut harness = Harness::new();
    harness.device.mmio_write(QUEUE_SEL, 0);
    harness
        .device
        .mmio_write(QUEUE_NUM, u32::from(QUEUE_NUM_MAX_VALUE) + 1);
    assert_eq!(harness.device.queue.size, 0);
    harness.device.mmio_write(QUEUE_READY, 1);
    assert!(!harness.device.queue_ready);
}

#[test]
fn a_status_write_of_zero_resets_the_device() {
    let mut harness = Harness::new();
    harness.configure_queue(4);
    harness.device.mmio_write(DRIVER_FEATURES_SEL, 0);
    harness.device.mmio_write(DRIVER_FEATURES, 0xFFFF_FFFF);
    harness.device.mmio_write(STATUS, STATUS_FEATURES_OK);
    assert_ne!(harness.device.negotiated_features, 0);
    harness.device.mmio_write(STATUS, 0);
    assert_eq!(harness.device.status, 0);
    assert_eq!(harness.device.negotiated_features, 0);
    assert!(!harness.device.queue_ready);
    assert_eq!(harness.device.queue.size, 0);
    assert_eq!(harness.device.irq_status, 0);
}

#[test]
fn a_read_may_span_multiple_data_segments() {
    let mut harness = Harness::new();
    harness.configure_queue(8);
    harness.negotiate();
    // Header -> two 256-byte data segments -> status.
    harness.write_desc(0, HEADER_ADDR, HEADER_BYTES as u32, DESC_FLAG_NEXT, 1);
    harness.write_desc(1, DATA_ADDR, 256, DESC_FLAG_NEXT | DESC_FLAG_WRITE, 2);
    harness.write_desc(2, DATA_ADDR + 256, 256, DESC_FLAG_NEXT | DESC_FLAG_WRITE, 3);
    harness.write_desc(3, STATUS_ADDR, 1, DESC_FLAG_WRITE, 0);
    let mut header = [0_u8; HEADER_BYTES];
    header[0..4].copy_from_slice(&REQ_IN.to_le_bytes());
    harness.bytes[HEADER_ADDR as usize..HEADER_ADDR as usize + HEADER_BYTES]
        .copy_from_slice(&header);
    harness.bytes[AVAIL_BASE as usize + 2..AVAIL_BASE as usize + 4]
        .copy_from_slice(&1_u16.to_le_bytes());
    harness.bytes[AVAIL_BASE as usize + 4..AVAIL_BASE as usize + 6]
        .copy_from_slice(&0_u16.to_le_bytes());
    harness.device.mmio_write(QUEUE_NOTIFY, 0);
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(harness.device.poll_kick(&mut memory).unwrap());
    // 512 data bytes plus the status byte.
    assert_eq!(harness.used_entry(), (0, 513));
    assert_eq!(
        &harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 512],
        &harness.backend_bytes[0..512]
    );
    assert_eq!(harness.bytes[STATUS_ADDR as usize], STATUS_BYTE_OK);
}

/// Publishes a raw 3-descriptor request (header + one data segment + status)
/// with explicit descriptor flags and addresses, plus one avail entry.
fn publish_request(
    harness: &mut Harness,
    data_address: u64,
    data_len: u32,
    data_flags: u16,
    status_address: u64,
    kind: u32,
) {
    harness.write_desc(0, HEADER_ADDR, HEADER_BYTES as u32, DESC_FLAG_NEXT, 1);
    harness.write_desc(1, data_address, data_len, data_flags, 2);
    harness.write_desc(2, status_address, 1, DESC_FLAG_WRITE, 0);
    let mut header = [0_u8; HEADER_BYTES];
    header[0..4].copy_from_slice(&kind.to_le_bytes());
    harness.bytes[HEADER_ADDR as usize..HEADER_ADDR as usize + HEADER_BYTES]
        .copy_from_slice(&header);
    harness.bytes[AVAIL_BASE as usize + 2..AVAIL_BASE as usize + 4]
        .copy_from_slice(&1_u16.to_le_bytes());
    harness.bytes[AVAIL_BASE as usize + 4..AVAIL_BASE as usize + 6]
        .copy_from_slice(&0_u16.to_le_bytes());
    harness.device.mmio_write(QUEUE_NOTIFY, 0);
    assert!(harness.device.kick_pending);
}

#[test]
fn an_avail_index_leap_beyond_the_queue_size_fails_closed() {
    let mut harness = Harness::new();
    harness.configure_queue(4);
    harness.negotiate();
    // One well-formed read request at head 0, published in every avail slot,
    // while avail.idx claims 65535 entries were added to a 4-entry ring.
    harness.write_desc(0, HEADER_ADDR, HEADER_BYTES as u32, DESC_FLAG_NEXT, 1);
    harness.write_desc(1, DATA_ADDR, 512, DESC_FLAG_NEXT | DESC_FLAG_WRITE, 2);
    harness.write_desc(2, STATUS_ADDR, 1, DESC_FLAG_WRITE, 0);
    let mut header = [0_u8; HEADER_BYTES];
    header[0..4].copy_from_slice(&REQ_IN.to_le_bytes());
    harness.bytes[HEADER_ADDR as usize..HEADER_ADDR as usize + HEADER_BYTES]
        .copy_from_slice(&header);
    harness.bytes[AVAIL_BASE as usize + 2..AVAIL_BASE as usize + 4]
        .copy_from_slice(&0xFFFF_u16.to_le_bytes());
    for slot in 0..4 {
        harness.bytes[AVAIL_BASE as usize + 4 + slot * 2..AVAIL_BASE as usize + 6 + slot * 2]
            .copy_from_slice(&0_u16.to_le_bytes());
    }
    harness.device.mmio_write(QUEUE_NOTIFY, 0);
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    // The device must refuse to walk 65535 slots of a 4-entry ring instead
    // of replaying the same request over and over.
    assert!(!harness.device.poll_kick(&mut memory).unwrap());
    assert!(harness.device.fault.is_some());
}

/// Backend that records every write the device issued in a shared log the
/// test can inspect after the poll.
#[derive(Clone)]
struct RecordingBackend {
    bytes: Vec<u8>,
    writes: std::rc::Rc<std::cell::RefCell<Vec<(u64, usize)>>>,
}

impl RecordingBackend {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            writes: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
        }
    }
}

impl BlockBackend for RecordingBackend {
    fn length(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<(), VirtioError> {
        let end = offset + output.len() as u64;
        if end > self.bytes.len() as u64 {
            return Err(VirtioError::Backend("read past end".to_owned()));
        }
        output.copy_from_slice(&self.bytes[offset as usize..end as usize]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, input: &[u8]) -> Result<(), VirtioError> {
        self.writes.borrow_mut().push((offset, input.len()));
        let end = offset + input.len() as u64;
        if end > self.bytes.len() as u64 {
            return Err(VirtioError::Backend("write past end".to_owned()));
        }
        self.bytes[offset as usize..end as usize].copy_from_slice(input);
        Ok(())
    }
}

#[test]
fn an_out_request_reaches_no_backend_byte_when_a_later_descriptor_is_out_of_ram() {
    let backend = RecordingBackend::new(vec![0_u8; 4096]);
    let writes = backend.writes.clone();
    let mut harness = Harness {
        device: VirtioMmioBlk::new(Box::new(backend)).unwrap(),
        bytes: vec![0_u8; RAM_BYTES],
        backend_bytes: Vec::new(),
    };
    harness.configure_queue(4);
    harness.negotiate();
    // Valid header and data segment, but the status descriptor address is
    // exactly one past the end of guest RAM.
    harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 512].fill(0xAB);
    publish_request(
        &mut harness,
        DATA_ADDR,
        512,
        DESC_FLAG_NEXT,
        RAM_BYTES as u64,
        REQ_OUT,
    );
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(!harness.device.poll_kick(&mut memory).unwrap());
    assert!(harness.device.fault.is_some());
    // The valid prefix of the chain never reached the host disk: the whole
    // chain is validated before the first backend byte moves.
    assert!(writes.borrow().is_empty());
}

#[test]
fn a_read_request_leaves_guest_memory_untouched_when_a_later_descriptor_is_out_of_ram() {
    let mut harness = Harness::new();
    harness.configure_queue(8);
    harness.negotiate();
    // Header -> valid 256-byte writable segment -> out-of-RAM 256-byte
    // segment -> status.
    harness.write_desc(0, HEADER_ADDR, HEADER_BYTES as u32, DESC_FLAG_NEXT, 1);
    harness.write_desc(1, DATA_ADDR, 256, DESC_FLAG_NEXT | DESC_FLAG_WRITE, 2);
    harness.write_desc(
        2,
        RAM_BYTES as u64 + 0x1000,
        256,
        DESC_FLAG_NEXT | DESC_FLAG_WRITE,
        3,
    );
    harness.write_desc(3, STATUS_ADDR, 1, DESC_FLAG_WRITE, 0);
    let mut header = [0_u8; HEADER_BYTES];
    header[0..4].copy_from_slice(&REQ_IN.to_le_bytes());
    harness.bytes[HEADER_ADDR as usize..HEADER_ADDR as usize + HEADER_BYTES]
        .copy_from_slice(&header);
    harness.bytes[AVAIL_BASE as usize + 2..AVAIL_BASE as usize + 4]
        .copy_from_slice(&1_u16.to_le_bytes());
    harness.bytes[AVAIL_BASE as usize + 4..AVAIL_BASE as usize + 6]
        .copy_from_slice(&0_u16.to_le_bytes());
    harness.device.mmio_write(QUEUE_NOTIFY, 0);
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(!harness.device.poll_kick(&mut memory).unwrap());
    assert!(harness.device.fault.is_some());
    // No partial copy reached the guest: the first segment stays zeroed.
    assert!(
        harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 256]
            .iter()
            .all(|byte| *byte == 0)
    );
}

#[test]
fn a_read_request_rejects_a_device_readable_data_descriptor() {
    let mut harness = Harness::new();
    harness.configure_queue(4);
    harness.negotiate();
    // The IN data descriptor is marked device-readable (no WRITE flag): the
    // guest asked the device to read a buffer it may also read itself.
    publish_request(
        &mut harness,
        DATA_ADDR,
        512,
        DESC_FLAG_NEXT,
        STATUS_ADDR,
        REQ_IN,
    );
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(!harness.device.poll_kick(&mut memory).unwrap());
    assert!(harness.device.fault.is_some());
    assert!(
        harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 512]
            .iter()
            .all(|byte| *byte == 0)
    );
}

#[test]
fn a_write_request_rejects_a_device_writable_data_descriptor() {
    let backend = RecordingBackend::new(vec![0_u8; 4096]);
    let writes = backend.writes.clone();
    let mut harness = Harness {
        device: VirtioMmioBlk::new(Box::new(backend)).unwrap(),
        bytes: vec![0_u8; RAM_BYTES],
        backend_bytes: Vec::new(),
    };
    harness.configure_queue(4);
    harness.negotiate();
    harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 512].fill(0xCD);
    // The OUT data descriptor is marked device-writable: the guest asked
    // the device to write a buffer it may also write itself.
    publish_request(
        &mut harness,
        DATA_ADDR,
        512,
        DESC_FLAG_NEXT | DESC_FLAG_WRITE,
        STATUS_ADDR,
        REQ_OUT,
    );
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(!harness.device.poll_kick(&mut memory).unwrap());
    assert!(harness.device.fault.is_some());
    assert!(writes.borrow().is_empty());
}

#[test]
fn an_in_request_length_that_is_not_a_sector_multiple_fails_closed() {
    let mut harness = Harness::new();
    harness.configure_queue(4);
    harness.negotiate();
    publish_request(
        &mut harness,
        DATA_ADDR,
        513,
        DESC_FLAG_NEXT | DESC_FLAG_WRITE,
        STATUS_ADDR,
        REQ_IN,
    );
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(!harness.device.poll_kick(&mut memory).unwrap());
    assert!(harness.device.fault.is_some());
}

#[test]
fn an_out_request_length_that_is_not_a_sector_multiple_fails_closed() {
    let mut harness = Harness::new();
    harness.configure_queue(4);
    harness.negotiate();
    harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 513].fill(0xCD);
    publish_request(
        &mut harness,
        DATA_ADDR,
        513,
        DESC_FLAG_NEXT,
        STATUS_ADDR,
        REQ_OUT,
    );
    let before = harness.backend_bytes.clone();
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(!harness.device.poll_kick(&mut memory).unwrap());
    assert!(harness.device.fault.is_some());
    assert_eq!(harness.backend_bytes, before);
}

#[test]
fn used_lengths_count_only_device_writable_bytes() {
    let mut harness = Harness::new();
    harness.configure_queue(4);
    harness.negotiate();
    // IN: 512 data bytes plus the 1 status byte the device wrote.
    harness.submit_read(0, 0, 512, REQ_IN);
    {
        let mut memory = SliceMemory {
            bytes: &mut harness.bytes,
        };
        assert!(harness.device.poll_kick(&mut memory).unwrap());
    }
    assert_eq!(harness.used_entry_at(0), (0, 513));
    assert_eq!(harness.bytes[STATUS_ADDR as usize], STATUS_BYTE_OK);
    // OUT: only the status byte is device-writable.
    harness.submit_read(1, 1, 512, REQ_OUT);
    {
        let mut memory = SliceMemory {
            bytes: &mut harness.bytes,
        };
        assert!(harness.device.poll_kick(&mut memory).unwrap());
    }
    assert_eq!(harness.used_entry_at(1), (0, 1));
    assert_eq!(harness.bytes[STATUS_ADDR as usize], STATUS_BYTE_OK);
}

#[test]
fn a_kick_before_driver_ok_is_ignored() {
    let mut harness = Harness::new();
    harness.configure_queue(4);
    // No FEATURES_OK / DRIVER_OK: the kick must be dropped, not serviced.
    harness.submit_read(0, 0, 512, REQ_IN);
    {
        let mut memory = SliceMemory {
            bytes: &mut harness.bytes,
        };
        assert!(!harness.device.poll_kick(&mut memory).unwrap());
    }
    assert_eq!(harness.device.irq_status, 0);
    assert!(harness.device.fault.is_none());
    // After the negotiation dance the same request is serviced normally.
    harness.negotiate();
    harness.submit_read(0, 0, 512, REQ_IN);
    {
        let mut memory = SliceMemory {
            bytes: &mut harness.bytes,
        };
        assert!(harness.device.poll_kick(&mut memory).unwrap());
    }
    assert_eq!(harness.used_entry_at(0), (0, 513));
}

#[test]
fn queue_configuration_through_a_nonexistent_selection_is_ignored() {
    let mut harness = Harness::new();
    // The device has exactly one queue (selection 0). Programming it through
    // selection 1 must not reach queue 0.
    harness.device.mmio_write(QUEUE_SEL, 1);
    harness.device.mmio_write(QUEUE_NUM, 4);
    harness.device.mmio_write(QUEUE_DESC_LOW, DESC_BASE as u32);
    harness
        .device
        .mmio_write(QUEUE_AVAIL_LOW, AVAIL_BASE as u32);
    harness.device.mmio_write(QUEUE_USED_LOW, USED_BASE as u32);
    harness.device.mmio_write(QUEUE_READY, 1);
    assert!(!harness.device.queue_ready);
    assert_eq!(harness.device.queue.size, 0);
    // Selecting the real queue afterwards must still show an untouched queue.
    harness.device.mmio_write(QUEUE_SEL, 0);
    assert_eq!(harness.device.queue.size, 0);
    harness.device.mmio_write(QUEUE_READY, 1);
    assert!(!harness.device.queue_ready);
}

#[test]
fn a_queue_too_small_for_a_request_never_becomes_ready() {
    let mut harness = Harness::new();
    harness.device.mmio_write(QUEUE_SEL, 0);
    harness.device.mmio_write(QUEUE_NUM, 2);
    harness.device.mmio_write(QUEUE_DESC_LOW, DESC_BASE as u32);
    harness
        .device
        .mmio_write(QUEUE_AVAIL_LOW, AVAIL_BASE as u32);
    harness.device.mmio_write(QUEUE_USED_LOW, USED_BASE as u32);
    harness.device.mmio_write(QUEUE_READY, 1);
    // A 2-entry ring cannot hold a request chain; it must be rejected at
    // configuration time instead of bricking the device on the first kick.
    assert!(!harness.device.queue_ready);
}

#[test]
fn the_advertised_segment_limit_fits_the_chain_descriptor_budget() {
    // A chain is header + up to SEG_MAX data segments + status, and the
    // device walks at most MAX_CHAIN_DESCRIPTORS entries. The advertised
    // value must never let a conforming driver submit a chain the device
    // would have to fault on.
    assert!(SEG_MAX as usize + 2 <= MAX_CHAIN_DESCRIPTORS);
}

#[test]
fn get_id_rejects_an_out_of_ram_descriptor_before_writing_anything() {
    let mut harness = Harness::new();
    harness.configure_queue(8);
    harness.negotiate();
    // Header -> valid 32-byte writable segment -> out-of-RAM 32-byte
    // segment -> status.
    harness.write_desc(0, HEADER_ADDR, HEADER_BYTES as u32, DESC_FLAG_NEXT, 1);
    harness.write_desc(1, DATA_ADDR, 32, DESC_FLAG_NEXT | DESC_FLAG_WRITE, 2);
    harness.write_desc(
        2,
        RAM_BYTES as u64 + 0x100,
        32,
        DESC_FLAG_NEXT | DESC_FLAG_WRITE,
        3,
    );
    harness.write_desc(3, STATUS_ADDR, 1, DESC_FLAG_WRITE, 0);
    let mut header = [0_u8; HEADER_BYTES];
    header[0..4].copy_from_slice(&REQ_GET_ID.to_le_bytes());
    harness.bytes[HEADER_ADDR as usize..HEADER_ADDR as usize + HEADER_BYTES]
        .copy_from_slice(&header);
    harness.bytes[AVAIL_BASE as usize + 2..AVAIL_BASE as usize + 4]
        .copy_from_slice(&1_u16.to_le_bytes());
    harness.bytes[AVAIL_BASE as usize + 4..AVAIL_BASE as usize + 6]
        .copy_from_slice(&0_u16.to_le_bytes());
    harness.device.mmio_write(QUEUE_NOTIFY, 0);
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(!harness.device.poll_kick(&mut memory).unwrap());
    assert!(harness.device.fault.is_some());
    // No serial bytes may have reached the valid first segment.
    assert!(
        harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 32]
            .iter()
            .all(|byte| *byte == 0)
    );
}

/// Guest memory that records the largest single write the device issued.
struct RecorderMemory {
    bytes: Vec<u8>,
    largest_write: usize,
}

impl GuestMemory for RecorderMemory {
    fn ram_bytes(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn read(&self, address: u64, output: &mut [u8]) -> Result<(), VirtioError> {
        self.check_range(address, output.len() as u64)?;
        output.copy_from_slice(&self.bytes[address as usize..address as usize + output.len()]);
        Ok(())
    }

    fn write(&mut self, address: u64, input: &[u8]) -> Result<(), VirtioError> {
        self.check_range(address, input.len() as u64)?;
        self.largest_write = self.largest_write.max(input.len());
        self.bytes[address as usize..address as usize + input.len()].copy_from_slice(input);
        Ok(())
    }
}

#[test]
fn get_id_moves_large_buffers_in_bounded_chunks() {
    const BIG_RAM: usize = 0x80000;
    const BIG_DATA: u64 = 0x10000;
    const BIG_LEN: u32 = 0x40000; // 256 KiB in one descriptor.
    let mut device = VirtioMmioBlk::new(Box::new(VecBlockBackend {
        bytes: vec![0_u8; 4096],
    }))
    .unwrap();
    device.mmio_write(QUEUE_SEL, 0);
    device.mmio_write(QUEUE_NUM, 4);
    device.mmio_write(QUEUE_DESC_LOW, DESC_BASE as u32);
    device.mmio_write(QUEUE_AVAIL_LOW, AVAIL_BASE as u32);
    device.mmio_write(QUEUE_USED_LOW, USED_BASE as u32);
    device.mmio_write(DEVICE_FEATURES_SEL, 0);
    let word0 = device.mmio_read(DEVICE_FEATURES);
    device.mmio_write(DEVICE_FEATURES_SEL, 1);
    let word1 = device.mmio_read(DEVICE_FEATURES);
    device.mmio_write(DRIVER_FEATURES_SEL, 0);
    device.mmio_write(DRIVER_FEATURES, word0);
    device.mmio_write(DRIVER_FEATURES_SEL, 1);
    device.mmio_write(DRIVER_FEATURES, word1);
    device.mmio_write(STATUS, STATUS_FEATURES_OK | DRIVER_OK);
    device.mmio_write(QUEUE_READY, 1);
    let mut bytes = vec![0_u8; BIG_RAM];
    let write_desc =
        |bytes: &mut [u8], index: u16, address: u64, length: u32, flags: u16, next: u16| {
            let offset = DESC_BASE + u64::from(index) * 16;
            bytes[offset as usize..offset as usize + 8].copy_from_slice(&address.to_le_bytes());
            bytes[offset as usize + 8..offset as usize + 12].copy_from_slice(&length.to_le_bytes());
            bytes[offset as usize + 12..offset as usize + 14].copy_from_slice(&flags.to_le_bytes());
            bytes[offset as usize + 14..offset as usize + 16].copy_from_slice(&next.to_le_bytes());
        };
    write_desc(
        &mut bytes,
        0,
        HEADER_ADDR,
        HEADER_BYTES as u32,
        DESC_FLAG_NEXT,
        1,
    );
    write_desc(
        &mut bytes,
        1,
        BIG_DATA,
        BIG_LEN,
        DESC_FLAG_NEXT | DESC_FLAG_WRITE,
        2,
    );
    write_desc(&mut bytes, 2, STATUS_ADDR, 1, DESC_FLAG_WRITE, 0);
    let mut header = [0_u8; HEADER_BYTES];
    header[0..4].copy_from_slice(&REQ_GET_ID.to_le_bytes());
    bytes[HEADER_ADDR as usize..HEADER_ADDR as usize + HEADER_BYTES].copy_from_slice(&header);
    bytes[AVAIL_BASE as usize + 2..AVAIL_BASE as usize + 4].copy_from_slice(&1_u16.to_le_bytes());
    bytes[AVAIL_BASE as usize + 4..AVAIL_BASE as usize + 6].copy_from_slice(&0_u16.to_le_bytes());
    device.mmio_write(QUEUE_NOTIFY, 0);
    let mut memory = RecorderMemory {
        bytes,
        largest_write: 0,
    };
    assert!(device.poll_kick(&mut memory).unwrap());
    assert!(device.fault.is_none());
    assert_eq!(memory.bytes[STATUS_ADDR as usize], STATUS_BYTE_OK);
    assert_eq!(
        &memory.bytes[BIG_DATA as usize..BIG_DATA as usize + SERIAL.len()],
        SERIAL
    );
    assert!(
        memory.bytes[BIG_DATA as usize + SERIAL.len()..BIG_DATA as usize + 64]
            .iter()
            .all(|byte| *byte == 0x20)
    );
    // The device never asked for a multi-hundred-KiB host buffer: a
    // hostile segment length stays bounded by the transfer chunk size.
    assert!(memory.largest_write > 0);
    assert!(memory.largest_write <= TRANSFER_CHUNK_BYTES);
}

/// Backend that fails host-side after a fixed number of write calls.
struct FailingBackend {
    bytes: Vec<u8>,
    write_calls: usize,
    fail_on: usize,
}

impl BlockBackend for FailingBackend {
    fn length(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<(), VirtioError> {
        let end = offset + output.len() as u64;
        if end > self.bytes.len() as u64 {
            return Err(VirtioError::Backend("read past end".to_owned()));
        }
        output.copy_from_slice(&self.bytes[offset as usize..end as usize]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, input: &[u8]) -> Result<(), VirtioError> {
        self.write_calls += 1;
        if self.write_calls > self.fail_on {
            return Err(VirtioError::Backend(
                "injected host write failure".to_owned(),
            ));
        }
        let end = offset + input.len() as u64;
        if end > self.bytes.len() as u64 {
            return Err(VirtioError::Backend("write past end".to_owned()));
        }
        self.bytes[offset as usize..end as usize].copy_from_slice(input);
        Ok(())
    }
}

#[test]
fn a_host_backend_error_completes_ioerr_and_keeps_the_device_alive() {
    let mut harness = Harness {
        device: VirtioMmioBlk::new(Box::new(FailingBackend {
            bytes: vec![0_u8; 4096],
            write_calls: 0,
            fail_on: 0,
        }))
        .unwrap(),
        bytes: vec![0_u8; RAM_BYTES],
        backend_bytes: Vec::new(),
    };
    harness.device.mmio_write(QUEUE_SEL, 0);
    harness.device.mmio_write(QUEUE_NUM, 4);
    harness.device.mmio_write(QUEUE_DESC_LOW, DESC_BASE as u32);
    harness
        .device
        .mmio_write(QUEUE_AVAIL_LOW, AVAIL_BASE as u32);
    harness.device.mmio_write(QUEUE_USED_LOW, USED_BASE as u32);
    harness
        .device
        .mmio_write(STATUS, STATUS_FEATURES_OK | DRIVER_OK);
    harness.device.mmio_write(QUEUE_READY, 1);
    harness.write_desc(0, HEADER_ADDR, HEADER_BYTES as u32, DESC_FLAG_NEXT, 1);
    harness.write_desc(1, DATA_ADDR, 512, DESC_FLAG_NEXT, 2);
    harness.write_desc(2, STATUS_ADDR, 1, DESC_FLAG_WRITE, 0);
    let mut header = [0_u8; HEADER_BYTES];
    header[0..4].copy_from_slice(&REQ_OUT.to_le_bytes());
    harness.bytes[HEADER_ADDR as usize..HEADER_ADDR as usize + HEADER_BYTES]
        .copy_from_slice(&header);
    harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 512].fill(0x5A);
    harness.bytes[AVAIL_BASE as usize + 2..AVAIL_BASE as usize + 4]
        .copy_from_slice(&1_u16.to_le_bytes());
    harness.bytes[AVAIL_BASE as usize + 4..AVAIL_BASE as usize + 6]
        .copy_from_slice(&0_u16.to_le_bytes());
    harness.device.mmio_write(QUEUE_NOTIFY, 0);
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(harness.device.poll_kick(&mut memory).unwrap());
    assert_eq!(harness.used_entry(), (0, 1));
    assert_eq!(harness.bytes[STATUS_ADDR as usize], STATUS_BYTE_IOERR);
    assert!(harness.device.fault.is_none());
}
