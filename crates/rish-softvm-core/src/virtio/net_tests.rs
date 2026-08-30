use super::*;
use crate::net::{NetBackend, NetCounters};
use crate::virtio::queue::{DESC_FLAG_NEXT, DESC_FLAG_WRITE, SliceMemory};
use std::sync::{Arc, Mutex};

const RAM_BYTES: usize = 0x4000;
// RX queue rings.
const RX_DESC_BASE: u64 = 0x1000;
const RX_AVAIL_BASE: u64 = 0x1100;
const RX_USED_BASE: u64 = 0x1200;
// TX queue rings.
const TX_DESC_BASE: u64 = 0x1300;
const TX_AVAIL_BASE: u64 = 0x1400;
const TX_USED_BASE: u64 = 0x1500;
// Shared buffers.
const HEADER_ADDR: u64 = 0x1600;
const DATA_ADDR: u64 = 0x1700;

/// Shared state a test reaches through its own Rc clone while the device
/// owns the backend.
#[derive(Default)]
struct TestInner {
    enqueued: Vec<Vec<u8>>,
    deliver: std::collections::VecDeque<Vec<u8>>,
}

struct TestBackend {
    mac: [u8; 6],
    inner: Arc<Mutex<TestInner>>,
}

impl NetBackend for TestBackend {
    fn mac(&self) -> [u8; 6] {
        self.mac
    }

    fn enqueue(&mut self, frame: &[u8]) {
        self.inner.lock().unwrap().enqueued.push(frame.to_vec());
    }

    fn poll(&mut self, output: &mut Vec<Vec<u8>>) {
        let mut inner = self.inner.lock().unwrap();
        while let Some(frame) = inner.deliver.pop_front() {
            output.push(frame);
        }
    }

    fn counters(&self) -> NetCounters {
        NetCounters::default()
    }
}

const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

struct Harness {
    device: VirtioMmioNet,
    inner: Arc<Mutex<TestInner>>,
    bytes: Vec<u8>,
}

impl Harness {
    fn new() -> Self {
        let inner = Arc::new(Mutex::new(TestInner::default()));
        let backend = Box::new(TestBackend {
            mac: MAC,
            inner: Arc::clone(&inner),
        });
        Self {
            device: VirtioMmioNet::new(backend),
            inner,
            bytes: vec![0_u8; RAM_BYTES],
        }
    }

    fn configure_queue(&mut self, index: u32, desc: u64, avail: u64, used: u64, size: u16) {
        self.device.mmio_write(QUEUE_SEL, index);
        self.device.mmio_write(QUEUE_NUM, u32::from(size));
        self.device.mmio_write(QUEUE_DESC_LOW, desc as u32);
        self.device.mmio_write(QUEUE_AVAIL_LOW, avail as u32);
        self.device.mmio_write(QUEUE_USED_LOW, used as u32);
        self.device.mmio_write(QUEUE_READY, 1);
    }

    fn write_desc(
        &mut self,
        base: u64,
        index: u16,
        address: u64,
        length: u32,
        flags: u16,
        next: u16,
    ) {
        let offset = base + u64::from(index) * 16;
        self.bytes[offset as usize..offset as usize + 8].copy_from_slice(&address.to_le_bytes());
        self.bytes[offset as usize + 8..offset as usize + 12]
            .copy_from_slice(&length.to_le_bytes());
        self.bytes[offset as usize + 12..offset as usize + 14]
            .copy_from_slice(&flags.to_le_bytes());
        self.bytes[offset as usize + 14..offset as usize + 16].copy_from_slice(&next.to_le_bytes());
    }

    /// Publishes one TX buffer (header + payload) and notifies queue 1.
    /// Publishes one TX buffer the way the pinned kernel's virtio_net
    /// does: a single descriptor holding the 12-byte virtio_net_hdr
    /// pushed inline with the frame. The queue selector is deliberately
    /// left on queue 0, the way the Linux virtio-mmio driver notifies
    /// (the written value IS the queue index).
    fn submit_tx(&mut self, payload: &[u8]) {
        self.write_desc(
            TX_DESC_BASE,
            0,
            HEADER_ADDR,
            12 + payload.len() as u32,
            0,
            0,
        );
        self.bytes[HEADER_ADDR as usize..HEADER_ADDR as usize + 12].fill(0);
        self.bytes[HEADER_ADDR as usize + 12..HEADER_ADDR as usize + 12 + payload.len()]
            .copy_from_slice(payload);
        self.bytes[TX_AVAIL_BASE as usize + 2..TX_AVAIL_BASE as usize + 4]
            .copy_from_slice(&1_u16.to_le_bytes());
        self.bytes[TX_AVAIL_BASE as usize + 4..TX_AVAIL_BASE as usize + 6]
            .copy_from_slice(&0_u16.to_le_bytes());
        // Keep queue_sel on RX (0) while notifying TX (1).
        self.device.mmio_write(QUEUE_SEL, 0);
        self.device.mmio_write(QUEUE_NOTIFY, 1);
    }

    /// Publishes a TX buffer in the split shape the driver uses when it
    /// cannot push the header inline: a 12-byte header descriptor followed
    /// by a payload descriptor.
    fn submit_tx_split(&mut self, payload: &[u8]) {
        self.write_desc(TX_DESC_BASE, 0, HEADER_ADDR, 12, DESC_FLAG_NEXT, 1);
        self.write_desc(TX_DESC_BASE, 1, DATA_ADDR, payload.len() as u32, 0, 0);
        self.bytes[HEADER_ADDR as usize..HEADER_ADDR as usize + 12].fill(0);
        self.bytes[DATA_ADDR as usize..DATA_ADDR as usize + payload.len()].copy_from_slice(payload);
        self.bytes[TX_AVAIL_BASE as usize + 2..TX_AVAIL_BASE as usize + 4]
            .copy_from_slice(&1_u16.to_le_bytes());
        self.bytes[TX_AVAIL_BASE as usize + 4..TX_AVAIL_BASE as usize + 6]
            .copy_from_slice(&0_u16.to_le_bytes());
        self.device.mmio_write(QUEUE_NOTIFY, 1);
    }

    /// Publishes a TX buffer whose single descriptor points outside RAM.
    fn submit_bad_tx(&mut self) {
        self.write_desc(TX_DESC_BASE, 0, RAM_BYTES as u64 + 0x1000, 512, 0, 0);
        self.bytes[TX_AVAIL_BASE as usize + 2..TX_AVAIL_BASE as usize + 4]
            .copy_from_slice(&1_u16.to_le_bytes());
        self.bytes[TX_AVAIL_BASE as usize + 4..TX_AVAIL_BASE as usize + 6]
            .copy_from_slice(&0_u16.to_le_bytes());
        self.device.mmio_write(QUEUE_NOTIFY, 1);
    }

    /// Posts one RX buffer the way the pinned kernel's virtio_net does:
    /// a single writable descriptor holding the 12-byte header area plus
    /// the frame area.
    fn post_rx(&mut self, data_len: u32) {
        self.write_desc(
            RX_DESC_BASE,
            0,
            HEADER_ADDR,
            12 + data_len,
            DESC_FLAG_WRITE,
            0,
        );
        self.bytes[RX_AVAIL_BASE as usize + 2..RX_AVAIL_BASE as usize + 4]
            .copy_from_slice(&1_u16.to_le_bytes());
        self.bytes[RX_AVAIL_BASE as usize + 4..RX_AVAIL_BASE as usize + 6]
            .copy_from_slice(&0_u16.to_le_bytes());
    }

    fn rx_used_entry(&self) -> (u32, u32) {
        let offset = RX_USED_BASE as usize + 4;
        let id = u32::from_le_bytes(self.bytes[offset..offset + 4].try_into().unwrap());
        let len = u32::from_le_bytes(self.bytes[offset + 4..offset + 8].try_into().unwrap());
        (id, len)
    }

    fn tx_used_entry(&self) -> (u32, u32) {
        let offset = TX_USED_BASE as usize + 4;
        let id = u32::from_le_bytes(self.bytes[offset..offset + 4].try_into().unwrap());
        let len = u32::from_le_bytes(self.bytes[offset + 4..offset + 8].try_into().unwrap());
        (id, len)
    }

    fn poll(&mut self) -> Result<bool, VirtioError> {
        let mut memory = SliceMemory {
            bytes: &mut self.bytes,
        };
        self.device.poll(&mut memory)
    }
}

#[test]
fn register_file_identifies_the_net_device() {
    let mut harness = Harness::new();
    assert_eq!(harness.device.mmio_read(MAGIC_VALUE), 0x7472_6976);
    assert_eq!(harness.device.mmio_read(VERSION), 2);
    assert_eq!(harness.device.mmio_read(DEVICE_ID), VIRTIO_NET_DEVICE_ID);
    assert_eq!(harness.device.mmio_read(VENDOR_ID), VENDOR_ID_RISH);
    assert_eq!(harness.device.mmio_read(QUEUE_NUM_MAX), 128);
    assert_eq!(
        harness.device.mmio_read(DEVICE_FEATURES),
        OFFERED_FEATURES as u32,
    );
    harness.device.mmio_write(DEVICE_FEATURES_SEL, 1);
    assert_eq!(
        harness.device.mmio_read(DEVICE_FEATURES),
        (OFFERED_FEATURES >> 32) as u32,
    );
}

#[test]
fn config_space_reports_mac_and_link_status() {
    let mut harness = Harness::new();
    assert_eq!(harness.device.mac(), MAC);
    assert_eq!(
        harness.device.mmio_read(0x100),
        u32::from_le_bytes([MAC[0], MAC[1], MAC[2], MAC[3]]),
    );
    assert_eq!(
        harness.device.mmio_read(0x104),
        u32::from(MAC[4]) | (u32::from(MAC[5]) << 8) | (1_u32 << 16),
    );
}

#[test]
fn byte_offsets_shift_the_register_word() {
    // The virtio-mmio driver reads the MAC and status bytes one at a time;
    // an unshifted word would report its first byte for every offset (the
    // bug this test guards: the guest once read 52:52:52:52:34:34 and
    // carrier DOWN from a correct config).
    let mut harness = Harness::new();
    for (offset, expected) in [
        (0x100_u64, MAC[0]),
        (0x101, MAC[1]),
        (0x102, MAC[2]),
        (0x103, MAC[3]),
        (0x104, MAC[4]),
        (0x105, MAC[5]),
        (0x106, 1), // LINK_UP, low byte
        (0x107, 0), // LINK_UP, high byte
    ] {
        assert_eq!(
            harness.device.mmio_read(offset) as u8,
            expected,
            "byte at {offset:#x}",
        );
    }
}

#[test]
fn a_transmit_buffer_reaches_the_backend_and_completes() {
    let mut harness = Harness::new();
    harness.configure_queue(1, TX_DESC_BASE, TX_AVAIL_BASE, TX_USED_BASE, 4);
    harness.submit_tx(b"hello-eth-frame");
    assert!(harness.poll().unwrap());
    assert_eq!(harness.tx_used_entry(), (0, 12 + 15));
    assert_eq!(
        harness.inner.lock().unwrap().enqueued,
        vec![b"hello-eth-frame".to_vec()],
    );
    assert_eq!(harness.device.irq_status & INTERRUPT_USED_RING, 1);
    assert!(harness.device.fault().is_none());
}

#[test]
fn an_oversized_transmit_is_dropped_not_truncated() {
    let mut harness = Harness::new();
    harness.configure_queue(1, TX_DESC_BASE, TX_AVAIL_BASE, TX_USED_BASE, 4);
    let payload = vec![0xAB_u8; MAX_FRAME_BYTES + 1];
    harness.submit_tx(&payload);
    assert!(harness.poll().unwrap());
    assert_eq!(
        harness.tx_used_entry(),
        (0, (12 + MAX_FRAME_BYTES + 1) as u32),
    );
    assert_eq!(harness.device.dropped(), (0, 1));
    assert!(harness.inner.lock().unwrap().enqueued.is_empty());
    assert!(harness.device.fault().is_none());
}

#[test]
fn the_split_transmit_shape_is_accepted_too() {
    // When the driver cannot push the header inline (cloned skb, short
    // headroom), it submits a 12-byte header descriptor followed by the
    // payload descriptor.
    let mut harness = Harness::new();
    harness.configure_queue(1, TX_DESC_BASE, TX_AVAIL_BASE, TX_USED_BASE, 4);
    harness.submit_tx_split(b"split-frame");
    assert!(harness.poll().unwrap());
    assert_eq!(harness.tx_used_entry(), (0, 12 + 11));
    assert_eq!(
        harness.inner.lock().unwrap().enqueued,
        vec![b"split-frame".to_vec()],
    );
}

#[test]
fn a_transmit_chain_outside_ram_latches_the_fault() {
    let mut harness = Harness::new();
    harness.configure_queue(1, TX_DESC_BASE, TX_AVAIL_BASE, TX_USED_BASE, 4);
    harness.submit_bad_tx();
    assert!(!harness.poll().unwrap());
    assert!(harness.device.fault().is_some());
    // A later, well-formed kick is ignored: fail closed, not guessing.
    harness.submit_tx(b"later");
    assert!(!harness.poll().unwrap());
    assert!(harness.inner.lock().unwrap().enqueued.is_empty());
}

#[test]
fn a_receive_buffer_collects_a_backend_frame() {
    let mut harness = Harness::new();
    harness.configure_queue(0, RX_DESC_BASE, RX_AVAIL_BASE, RX_USED_BASE, 4);
    harness.post_rx(2048);
    let frame = vec![0x42_u8; 60];
    harness
        .inner
        .lock()
        .unwrap()
        .deliver
        .push_back(frame.clone());
    // The backend poll is throttled to 1 ms of host wall clock.
    std::thread::sleep(std::time::Duration::from_millis(2));
    assert!(harness.poll().unwrap());
    assert_eq!(harness.rx_used_entry(), (0, 12 + 60));
    assert_eq!(
        &harness.bytes[HEADER_ADDR as usize..HEADER_ADDR as usize + 12],
        &[0_u8; 12],
    );
    assert_eq!(
        &harness.bytes[HEADER_ADDR as usize + 12..HEADER_ADDR as usize + 12 + 60],
        &frame[..],
    );
    assert_eq!(harness.device.irq_status & INTERRUPT_USED_RING, 1);
}

#[test]
fn an_undersized_receive_buffer_drops_the_frame() {
    let mut harness = Harness::new();
    harness.configure_queue(0, RX_DESC_BASE, RX_AVAIL_BASE, RX_USED_BASE, 4);
    harness.post_rx(8);
    let frame = vec![0x42_u8; 60];
    harness.inner.lock().unwrap().deliver.push_back(frame);
    std::thread::sleep(std::time::Duration::from_millis(2));
    assert!(harness.poll().unwrap());
    assert_eq!(harness.rx_used_entry(), (0, 0));
    assert_eq!(harness.device.dropped(), (1, 0));
}

#[test]
fn queue_selection_isolates_the_two_queues() {
    let mut harness = Harness::new();
    harness.configure_queue(0, RX_DESC_BASE, RX_AVAIL_BASE, RX_USED_BASE, 4);
    harness.configure_queue(1, TX_DESC_BASE, TX_AVAIL_BASE, TX_USED_BASE, 4);
    // A notify for queue 1 with no published TX buffer completes nothing.
    harness.device.mmio_write(QUEUE_NOTIFY, 1);
    assert!(!harness.poll().unwrap());
    assert_eq!(harness.tx_used_entry(), (0, 0));
    // Queue 0 fills independently of queue 1.
    harness.post_rx(2048);
    let frame = vec![0x11_u8; 40];
    harness
        .inner
        .lock()
        .unwrap()
        .deliver
        .push_back(frame.clone());
    std::thread::sleep(std::time::Duration::from_millis(2));
    assert!(harness.poll().unwrap());
    assert_eq!(harness.rx_used_entry(), (0, 12 + 40));
    assert_eq!(
        &harness.bytes[HEADER_ADDR as usize + 12..HEADER_ADDR as usize + 12 + 40],
        &frame[..],
    );
}

#[test]
fn notify_on_an_unready_queue_is_ignored() {
    let mut harness = Harness::new();
    harness.device.mmio_write(QUEUE_NOTIFY, 1);
    assert!(!harness.poll().unwrap());
    assert_eq!(harness.device.irq_status, 0);
}

#[test]
fn a_status_write_of_zero_resets_the_device() {
    let mut harness = Harness::new();
    harness.configure_queue(1, TX_DESC_BASE, TX_AVAIL_BASE, TX_USED_BASE, 4);
    harness.device.mmio_write(DRIVER_FEATURES_SEL, 0);
    harness.device.mmio_write(DRIVER_FEATURES, 0xFFFF_FFFF);
    harness.device.mmio_write(STATUS, STATUS_FEATURES_OK);
    assert_ne!(harness.device.negotiated_features, 0);
    harness.device.mmio_write(STATUS, 0);
    assert_eq!(harness.device.status, 0);
    assert_eq!(harness.device.negotiated_features, 0);
    assert_eq!(harness.device.queues[QUEUE_TX].size, 0);
    assert!(!harness.device.queue_ready[QUEUE_TX]);
    assert_eq!(harness.device.irq_status, 0);
}

#[test]
fn a_tx_avail_index_leap_beyond_the_queue_size_fails_closed() {
    let mut harness = Harness::new();
    harness.configure_queue(1, TX_DESC_BASE, TX_AVAIL_BASE, TX_USED_BASE, 4);
    // One valid TX buffer, but avail.idx claims 65535 entries were added to
    // a 4-entry ring: the device must not replay the same frame thousands
    // of times inside one poll.
    harness.submit_tx(b"replay");
    harness.bytes[TX_AVAIL_BASE as usize + 2..TX_AVAIL_BASE as usize + 4]
        .copy_from_slice(&0xFFFF_u16.to_le_bytes());
    for slot in 0..4 {
        harness.bytes[TX_AVAIL_BASE as usize + 4 + slot * 2..TX_AVAIL_BASE as usize + 6 + slot * 2]
            .copy_from_slice(&0_u16.to_le_bytes());
    }
    assert!(!harness.poll().unwrap());
    assert!(harness.device.fault().is_some());
    // At most one frame may have reached the backend before the fault.
    assert!(harness.inner.lock().unwrap().enqueued.len() <= 1);
}

#[test]
fn an_rx_avail_index_leap_beyond_the_queue_size_fails_closed() {
    let mut harness = Harness::new();
    harness.configure_queue(0, RX_DESC_BASE, RX_AVAIL_BASE, RX_USED_BASE, 4);
    harness.post_rx(2048);
    harness.bytes[RX_AVAIL_BASE as usize + 2..RX_AVAIL_BASE as usize + 4]
        .copy_from_slice(&0xFFFF_u16.to_le_bytes());
    let frame = vec![0x42_u8; 60];
    harness.inner.lock().unwrap().deliver.push_back(frame);
    std::thread::sleep(std::time::Duration::from_millis(2));
    assert!(!harness.poll().unwrap());
    assert!(harness.device.fault().is_some());
}
