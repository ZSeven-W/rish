//! Bounded 16550 UART byte channels between the host adapter and the provider.
//!
//! The console serial carries human-readable boot diagnostics. The control
//! serial carries versioned rish guest protocol frames; drops in either
//! direction are counted and surfaced so the transport can fail closed.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
};

pub(crate) struct HostSerial {
    pub input: SyncSender<u8>,
    output: Mutex<Receiver<u8>>,
    dropped_output: Arc<AtomicU64>,
}

impl HostSerial {
    pub fn drain(&self, limit: usize) -> Vec<u8> {
        let receiver = self
            .output
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut bytes = Vec::new();
        while bytes.len() < limit {
            match receiver.try_recv() {
                Ok(byte) => bytes.push(byte),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        bytes
    }

    pub fn dropped_output(&self) -> u64 {
        self.dropped_output.load(Ordering::Relaxed)
    }
}

/// Byte queue with a hard capacity. Excess bytes are dropped and counted.
struct ControlBuffer {
    queue: VecDeque<u8>,
    capacity: usize,
    dropped: Arc<AtomicU64>,
}

impl ControlBuffer {
    fn new(capacity: usize, dropped: Arc<AtomicU64>) -> Self {
        Self {
            queue: VecDeque::with_capacity(capacity.min(4096)),
            capacity,
            dropped,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> usize {
        let mut written = 0;
        for byte in bytes {
            if self.queue.len() >= self.capacity {
                break;
            }
            self.queue.push_back(*byte);
            written += 1;
        }
        let dropped = bytes.len() - written;
        if dropped != 0 {
            self.dropped.fetch_add(dropped as u64, Ordering::Relaxed);
        }
        written
    }

    fn drain(&mut self, output: &mut [u8]) -> usize {
        let count = output.len().min(self.queue.len());
        for slot in &mut output[..count] {
            *slot = self
                .queue
                .pop_front()
                .expect("count is bounded by the queue length");
        }
        count
    }
}

/// Versioned guest control channel shared by the host adapter and the provider.
///
/// The host writes request frames with write_input and drains guest frames
/// with drain_output. The provider reads host frames with read_input and
/// emits guest frames with write_output. Both directions are bounded; drops
/// are counted, never silent.
pub struct ControlChannel {
    input: Mutex<ControlBuffer>,
    output: Mutex<ControlBuffer>,
    dropped_input: AtomicU64,
    dropped_output: AtomicU64,
}

impl ControlChannel {
    pub fn new(capacity: usize) -> Self {
        Self {
            input: Mutex::new(ControlBuffer::new(capacity, Arc::new(AtomicU64::new(0)))),
            output: Mutex::new(ControlBuffer::new(capacity, Arc::new(AtomicU64::new(0)))),
            dropped_input: AtomicU64::new(0),
            dropped_output: AtomicU64::new(0),
        }
    }

    // Host side.

    pub fn write_input(&self, bytes: &[u8]) -> usize {
        self.input
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(bytes)
    }

    pub fn drain_output(&self, limit: usize) -> Vec<u8> {
        let mut bytes = vec![0_u8; limit];
        let read = self
            .output
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain(&mut bytes);
        bytes.truncate(read);
        bytes
    }

    pub fn dropped_input(&self) -> u64 {
        self.dropped_input.load(Ordering::Relaxed)
    }

    pub fn dropped_output(&self) -> u64 {
        self.dropped_output.load(Ordering::Relaxed)
    }

    // Provider side.

    pub fn read_input(&self, output: &mut [u8]) -> usize {
        self.input
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain(output)
    }

    pub fn write_output(&self, bytes: &[u8]) -> usize {
        self.output
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(bytes)
    }
}

/// I/O objects owned by one provider instance on its VM worker thread.
pub struct ProviderIo {
    console_input: Receiver<u8>,
    console_output: SyncSender<u8>,
    console_dropped_output: Arc<AtomicU64>,
    pub control: Arc<ControlChannel>,
    cancel: Arc<AtomicBool>,
}

pub(crate) fn io_pair(
    console_capacity: usize,
    control_capacity: usize,
    cancel: Arc<AtomicBool>,
) -> (HostSerial, Arc<ControlChannel>, ProviderIo) {
    let (input_tx, input_rx) = mpsc::sync_channel(console_capacity);
    let (output_tx, output_rx) = mpsc::sync_channel(console_capacity);
    let dropped_output = Arc::new(AtomicU64::new(0));
    let control = Arc::new(ControlChannel::new(control_capacity));
    (
        HostSerial {
            input: input_tx,
            output: Mutex::new(output_rx),
            dropped_output: Arc::clone(&dropped_output),
        },
        Arc::clone(&control),
        ProviderIo {
            console_input: input_rx,
            console_output: output_tx,
            console_dropped_output: Arc::clone(&dropped_output),
            control,
            cancel,
        },
    )
}

impl ProviderIo {
    /// Sends as many guest console bytes as fit in the bounded host queue.
    pub fn write_console(&self, bytes: &[u8]) -> usize {
        let mut written = 0;
        for byte in bytes {
            match self.console_output.try_send(*byte) {
                Ok(()) => written += 1,
                Err(TrySendError::Full(_)) => {
                    self.console_dropped_output.fetch_add(
                        u64::try_from(bytes.len() - written).unwrap_or(u64::MAX),
                        Ordering::Relaxed,
                    );
                    break;
                }
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
        written
    }

    /// Reads currently queued host-to-guest console bytes without blocking.
    pub fn read_console(&self, output: &mut [u8]) -> usize {
        let mut read = 0;
        while read < output.len() {
            match self.console_input.try_recv() {
                Ok(byte) => {
                    output[read] = byte;
                    read += 1;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        read
    }

    #[must_use]
    pub fn should_cancel(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}
