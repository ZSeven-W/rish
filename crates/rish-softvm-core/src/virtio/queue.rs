//! Split-virtqueue parsing with fail-closed bounds checks.
//!
//! The device owns the queue layout the guest wrote into the virtio-mmio
//! registers; this module only walks the descriptor, available, and used
//! rings in guest memory. Every ring address is validated against the guest
//! memory implementation before use, so an out-of-range ring or a malformed
//! chain fails closed with VirtioError instead of reading or writing
//! past the end of RAM.

use crate::virtio::VirtioError;

/// Guest-memory view used while servicing a queue. Implementations must
/// reject any access that escapes the guest's RAM.
pub trait GuestMemory {
    /// Total guest RAM available to the queue, in bytes.
    fn ram_bytes(&self) -> u64;
    fn read(&self, address: u64, output: &mut [u8]) -> Result<(), VirtioError>;
    fn write(&mut self, address: u64, input: &[u8]) -> Result<(), VirtioError>;
}

/// Maximum descriptors accepted in one request chain (header + data segments
/// + status). A longer chain — including one that cycles — fails closed.
pub const MAX_CHAIN_DESCRIPTORS: usize = 64;

/// Layout of the one split virtqueue the block device exposes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueueLayout {
    /// Number of descriptors the driver configured; zero means unconfigured.
    pub size: u16,
    pub desc: u64,
    pub avail: u64,
    pub used: u64,
}

impl QueueLayout {
    /// Byte lengths of the three rings for the current queue size.
    #[must_use]
    pub fn ring_bytes(&self) -> (usize, usize, usize) {
        let size = usize::from(self.size);
        (size * 16, 6 + size * 2, 6 + size * 8)
    }

    /// Fails closed unless all three rings fit entirely inside guest RAM.
    pub fn validate(&self, ram_bytes: u64) -> Result<(), VirtioError> {
        let (desc_bytes, avail_bytes, used_bytes) = self.ring_bytes();
        for (base, bytes) in [
            (self.desc, desc_bytes),
            (self.avail, avail_bytes),
            (self.used, used_bytes),
        ] {
            let end = base.checked_add(bytes as u64).ok_or(VirtioError::BadQueue(
                "queue ring address arithmetic overflowed",
            ))?;
            if end > ram_bytes {
                return Err(VirtioError::OutOfBounds {
                    address: base,
                    bytes: bytes as u64,
                });
            }
        }
        Ok(())
    }
}

/// One split-virtqueue descriptor entry, decoded from guest memory.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Descriptor {
    pub address: u64,
    pub length: u32,
    pub flags: u16,
}

impl Descriptor {
    #[must_use]
    pub fn has_next(&self) -> bool {
        self.flags & DESC_FLAG_NEXT != 0
    }

    #[must_use]
    pub fn device_writable(&self) -> bool {
        self.flags & DESC_FLAG_WRITE != 0
    }
}

pub const DESC_FLAG_NEXT: u16 = 1;
pub const DESC_FLAG_WRITE: u16 = 2;

/// The driver's position in the available ring, read at kick time.
pub fn avail_index<M: GuestMemory>(memory: &M, queue: &QueueLayout) -> Result<u16, VirtioError> {
    read_u16(memory, queue.avail + 2)
}

/// The descriptor-chain head the driver published at `slot` in the
/// available ring.
pub fn avail_head<M: GuestMemory>(
    memory: &M,
    queue: &QueueLayout,
    slot: u16,
) -> Result<u16, VirtioError> {
    let size = usize::from(queue.size);
    if size == 0 {
        return Err(VirtioError::BadQueue("available ring has zero size"));
    }
    let entry = queue.avail + 4 + (u64::from(slot % queue.size)) * 2;
    read_u16(memory, entry)
}

/// Walks the descriptor chain starting at `head`, filling `chain` and
/// returning how many entries it holds. Fails closed when an index leaves
/// the ring, when the chain exceeds the device limit (this also catches
/// cycles), or when any descriptor entry itself lies outside guest RAM.
pub fn read_chain<M: GuestMemory>(
    memory: &M,
    queue: &QueueLayout,
    head: u16,
    chain: &mut [Descriptor],
) -> Result<usize, VirtioError> {
    let size = usize::from(queue.size);
    if size == 0 {
        return Err(VirtioError::BadQueue("descriptor ring has zero size"));
    }
    if chain.is_empty() {
        return Err(VirtioError::BadQueue("chain buffer is empty"));
    }
    let mut index = usize::from(head);
    for (slot, entry) in chain.iter_mut().enumerate() {
        if index >= size {
            return Err(VirtioError::BadQueue(
                "descriptor chain index left the ring",
            ));
        }
        let offset = queue.desc + (index as u64) * 16;
        *entry = Descriptor {
            address: read_u64(memory, offset)?,
            length: read_u32(memory, offset + 8)?,
            flags: read_u16(memory, offset + 12)?,
        };
        let next = read_u16(memory, offset + 14)?;
        if entry.has_next() {
            index = usize::from(next);
            continue;
        }
        return Ok(slot + 1);
    }
    Err(VirtioError::BadQueue(
        "descriptor chain exceeds the device limit",
    ))
}

/// The driver's position in the used ring.
pub fn used_index<M: GuestMemory>(memory: &M, queue: &QueueLayout) -> Result<u16, VirtioError> {
    read_u16(memory, queue.used + 2)
}

/// Publishes one completion in the used ring and returns the new used index.
pub fn write_used<M: GuestMemory>(
    memory: &mut M,
    queue: &QueueLayout,
    used_index: u16,
    id: u32,
    length: u32,
) -> Result<u16, VirtioError> {
    let size = usize::from(queue.size);
    if size == 0 {
        return Err(VirtioError::BadQueue("used ring has zero size"));
    }
    let slot = usize::from(used_index) % size;
    let offset = queue.used + 4 + (slot as u64) * 8;
    write_u32(memory, offset, id)?;
    write_u32(memory, offset + 4, length)?;
    let next = used_index.wrapping_add(1);
    write_u16(memory, queue.used + 2, next)?;
    Ok(next)
}

fn read_u16<M: GuestMemory>(memory: &M, address: u64) -> Result<u16, VirtioError> {
    let mut bytes = [0_u8; 2];
    memory.read(address, &mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32<M: GuestMemory>(memory: &M, address: u64) -> Result<u32, VirtioError> {
    let mut bytes = [0_u8; 4];
    memory.read(address, &mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64<M: GuestMemory>(memory: &M, address: u64) -> Result<u64, VirtioError> {
    let mut bytes = [0_u8; 8];
    memory.read(address, &mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn write_u16<M: GuestMemory>(memory: &mut M, address: u64, value: u16) -> Result<(), VirtioError> {
    memory.write(address, &value.to_le_bytes())
}

fn write_u32<M: GuestMemory>(memory: &mut M, address: u64, value: u32) -> Result<(), VirtioError> {
    memory.write(address, &value.to_le_bytes())
}

/// Backs a guest-physical window with a plain byte slice for unit tests.
#[cfg(test)]
#[derive(Debug)]
pub struct SliceMemory<'a> {
    pub bytes: &'a mut [u8],
}

#[cfg(test)]
impl GuestMemory for SliceMemory<'_> {
    fn ram_bytes(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn read(&self, address: u64, output: &mut [u8]) -> Result<(), VirtioError> {
        let end = address
            .checked_add(output.len() as u64)
            .ok_or(VirtioError::OutOfBounds {
                address,
                bytes: output.len() as u64,
            })?;
        if end > self.bytes.len() as u64 {
            return Err(VirtioError::OutOfBounds {
                address,
                bytes: output.len() as u64,
            });
        }
        output.copy_from_slice(&self.bytes[address as usize..end as usize]);
        Ok(())
    }

    fn write(&mut self, address: u64, input: &[u8]) -> Result<(), VirtioError> {
        let end = address
            .checked_add(input.len() as u64)
            .ok_or(VirtioError::OutOfBounds {
                address,
                bytes: input.len() as u64,
            })?;
        if end > self.bytes.len() as u64 {
            return Err(VirtioError::OutOfBounds {
                address,
                bytes: input.len() as u64,
            });
        }
        self.bytes[address as usize..end as usize].copy_from_slice(input);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(size: u16, base: u64) -> QueueLayout {
        let desc_bytes = (size as u64) * 16;
        let avail_bytes = 6 + (size as u64) * 2;
        QueueLayout {
            size,
            desc: base,
            avail: base + desc_bytes,
            used: base + desc_bytes + avail_bytes,
        }
    }

    #[test]
    fn layout_places_the_rings_back_to_back() {
        let queue = layout(4, 0x2000);
        assert_eq!(queue.ring_bytes(), (64, 14, 38));
        assert_eq!(queue.desc, 0x2000);
        assert_eq!(queue.avail, 0x2040);
        assert_eq!(queue.used, 0x204E);
    }

    #[test]
    fn ring_validation_rejects_a_used_ring_past_ram() {
        let queue = QueueLayout {
            size: 4,
            desc: 0x0,
            avail: 0x40,
            used: 0x1000 - 30,
        };
        assert!(matches!(
            queue.validate(0x1000),
            Err(VirtioError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn chain_walk_reads_a_multi_segment_chain() {
        let mut bytes = vec![0_u8; 0x1000];
        let queue = layout(8, 0x100);
        // desc 3 -> desc 5 (NEXT), desc 5 final.
        write_desc(&mut bytes, &queue, 3, 0xA000, 16, DESC_FLAG_NEXT, 5);
        write_desc(&mut bytes, &queue, 5, 0xB000, 4096, 0, 0);
        let memory = SliceMemory { bytes: &mut bytes };
        let mut chain = [Descriptor::default(); MAX_CHAIN_DESCRIPTORS];
        let count = read_chain(&memory, &queue, 3, &mut chain).unwrap();
        assert_eq!(count, 2);
        assert_eq!(chain[0].address, 0xA000);
        assert!(chain[0].has_next());
        assert_eq!(chain[1].address, 0xB000);
        assert!(!chain[1].has_next());
        assert_eq!(chain[1].length, 4096);
    }

    #[test]
    fn chain_walk_fails_closed_on_an_out_of_range_next() {
        let mut bytes = vec![0_u8; 0x1000];
        let queue = layout(4, 0x100);
        write_desc(&mut bytes, &queue, 0, 0xA000, 16, DESC_FLAG_NEXT, 9);
        let memory = SliceMemory { bytes: &mut bytes };
        let mut chain = [Descriptor::default(); MAX_CHAIN_DESCRIPTORS];
        assert!(matches!(
            read_chain(&memory, &queue, 0, &mut chain),
            Err(VirtioError::BadQueue(_))
        ));
    }

    #[test]
    fn chain_walk_fails_closed_on_a_cycle() {
        let mut bytes = vec![0_u8; 0x1000];
        let queue = layout(4, 0x100);
        write_desc(&mut bytes, &queue, 0, 0xA000, 16, DESC_FLAG_NEXT, 1);
        write_desc(&mut bytes, &queue, 1, 0xB000, 16, DESC_FLAG_NEXT, 0);
        let memory = SliceMemory { bytes: &mut bytes };
        let mut chain = [Descriptor::default(); MAX_CHAIN_DESCRIPTORS];
        assert!(matches!(
            read_chain(&memory, &queue, 0, &mut chain),
            Err(VirtioError::BadQueue(_))
        ));
    }

    #[test]
    fn used_ring_publication_advances_the_index() {
        let mut bytes = vec![0_u8; 0x1000];
        let queue = layout(4, 0x100);
        let mut memory = SliceMemory { bytes: &mut bytes };
        let next = write_used(&mut memory, &queue, 0, 7, 4096).unwrap();
        assert_eq!(next, 1);
        assert_eq!(used_index(&memory, &queue).unwrap(), 1);
        let entry = &bytes[queue.used as usize + 4..queue.used as usize + 12];
        assert_eq!(u32::from_le_bytes(entry[0..4].try_into().unwrap()), 7);
        assert_eq!(u32::from_le_bytes(entry[4..8].try_into().unwrap()), 4096);
    }

    #[test]
    fn used_ring_wraps_within_the_ring() {
        let mut bytes = vec![0_u8; 0x1000];
        let queue = layout(4, 0x100);
        let mut memory = SliceMemory { bytes: &mut bytes };
        for expected in 1..=4 {
            let next =
                write_used(&mut memory, &queue, expected - 1, u32::from(expected), 0).unwrap();
            assert_eq!(next, expected);
        }
        // The fifth entry wraps to slot 0, overwriting the first entry.
        let next = write_used(&mut memory, &queue, 4, 99, 0).unwrap();
        assert_eq!(next, 5);
        let entry = &bytes[queue.used as usize + 4..queue.used as usize + 12];
        assert_eq!(u32::from_le_bytes(entry[0..4].try_into().unwrap()), 99);
    }

    #[test]
    fn avail_slots_wrap_within_the_ring() {
        let mut bytes = vec![0_u8; 0x1000];
        let queue = layout(4, 0x100);
        for slot in 0..8_u16 {
            let entry = queue.avail + 4 + (u64::from(slot % 4)) * 2;
            bytes[entry as usize..entry as usize + 2].copy_from_slice(&(100 + slot).to_le_bytes());
        }
        let memory = SliceMemory { bytes: &mut bytes };
        // Slots wrap modulo the ring size: slot 4 overwrites slot 0.
        assert_eq!(avail_head(&memory, &queue, 0).unwrap(), 104);
        assert_eq!(avail_head(&memory, &queue, 4).unwrap(), 104);
        assert_eq!(avail_head(&memory, &queue, 7).unwrap(), 107);
    }

    fn write_desc(
        bytes: &mut [u8],
        queue: &QueueLayout,
        index: u16,
        address: u64,
        length: u32,
        flags: u16,
        next: u16,
    ) {
        let offset = queue.desc + u64::from(index) * 16;
        bytes[offset as usize..offset as usize + 8].copy_from_slice(&address.to_le_bytes());
        bytes[offset as usize + 8..offset as usize + 12].copy_from_slice(&length.to_le_bytes());
        bytes[offset as usize + 12..offset as usize + 14].copy_from_slice(&flags.to_le_bytes());
        bytes[offset as usize + 14..offset as usize + 16].copy_from_slice(&next.to_le_bytes());
    }
}
