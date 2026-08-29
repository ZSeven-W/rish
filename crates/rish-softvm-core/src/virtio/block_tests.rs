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

    /// Publishes header/data/status descriptors and one available entry
    /// (the seq-th submission) describing them, then notifies the
    /// device.
    fn submit_read(&mut self, seq: u16, sector: u64, data_len: u32, kind: u32) {
        self.write_desc(0, HEADER_ADDR, HEADER_BYTES as u32, DESC_FLAG_NEXT, 1);
        self.write_desc(1, DATA_ADDR, data_len, DESC_FLAG_NEXT | DESC_FLAG_WRITE, 2);
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
        let offset = USED_BASE as usize + 4;
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
    harness.submit_read(0, 0, 512, REQ_IN);
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(harness.device.poll_kick(&mut memory).unwrap());
    // The used ring published one completion for head 0 with 512 bytes.
    assert_eq!(harness.used_entry(), (0, 512));
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
    let payload = vec![0xAB_u8; 512];
    harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 512].copy_from_slice(&payload);
    harness.submit_read(0, 1, 512, REQ_OUT);
    {
        let mut memory = SliceMemory {
            bytes: &mut harness.bytes,
        };
        assert!(harness.device.poll_kick(&mut memory).unwrap());
    }
    assert_eq!(harness.used_entry(), (0, 512));
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
    // sector 8 is one past the 8-sector backend.
    harness.submit_read(0, 8, 512, REQ_IN);
    let mut memory = SliceMemory {
        bytes: &mut harness.bytes,
    };
    assert!(harness.device.poll_kick(&mut memory).unwrap());
    assert_eq!(harness.used_entry(), (0, 0));
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
    assert_eq!(harness.used_entry(), (0, 512));
    assert_eq!(
        &harness.bytes[DATA_ADDR as usize..DATA_ADDR as usize + 512],
        &harness.backend_bytes[0..512]
    );
    assert_eq!(harness.bytes[STATUS_ADDR as usize], STATUS_BYTE_OK);
}
