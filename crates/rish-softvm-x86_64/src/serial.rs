use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
};

pub(crate) struct HostSerial {
    pub input: SyncSender<u8>,
    output: Mutex<Receiver<u8>>,
    dropped_output: Arc<AtomicU64>,
}

/// I/O object owned by one provider instance on its VM worker thread.
pub struct ProviderIo {
    input: Receiver<u8>,
    output: SyncSender<u8>,
    dropped_output: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}

pub(crate) fn serial_pair(capacity: usize, cancel: Arc<AtomicBool>) -> (HostSerial, ProviderIo) {
    let (input_tx, input_rx) = mpsc::sync_channel(capacity);
    let (output_tx, output_rx) = mpsc::sync_channel(capacity);
    let dropped_output = Arc::new(AtomicU64::new(0));
    (
        HostSerial {
            input: input_tx,
            output: Mutex::new(output_rx),
            dropped_output: Arc::clone(&dropped_output),
        },
        ProviderIo {
            input: input_rx,
            output: output_tx,
            dropped_output,
            cancel,
        },
    )
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

impl ProviderIo {
    /// Sends as many guest UART bytes as fit in the bounded host queue.
    pub fn write_serial(&self, bytes: &[u8]) -> usize {
        let mut written = 0;
        for byte in bytes {
            match self.output.try_send(*byte) {
                Ok(()) => written += 1,
                Err(TrySendError::Full(_)) => {
                    self.dropped_output.fetch_add(
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

    /// Reads currently queued host-to-guest UART bytes without blocking.
    pub fn read_serial(&self, output: &mut [u8]) -> usize {
        let mut read = 0;
        while read < output.len() {
            match self.input.try_recv() {
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
