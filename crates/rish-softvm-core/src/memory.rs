//! Physical guest memory with bounds-checked access.

use std::cell::RefCell;

use crate::CpuError;
use crate::devices::ioapic::{IOAPIC_BASE, IOAPIC_SIZE, IoApic};
use crate::devices::lapic::{LAPIC_BASE, LAPIC_SIZE, LocalApic};
use crate::virtio::{
    GuestMemory, VIRTIO_MMIO_BASE, VIRTIO_MMIO_REGISTER_BYTES, VIRTIO_MMIO_WINDOW_BYTES,
    VIRTIO_NET_MMIO_BASE, VirtioError, VirtioMmioBlk, VirtioMmioNet,
};

pub struct Memory {
    ram: Box<[u8]>,
    lapic: Option<RefCell<LocalApic>>,
    ioapic: RefCell<IoApic>,
    /// The virtio-mmio block device, when a backend is attached.
    virtio_blk: Option<RefCell<VirtioMmioBlk>>,
    /// The virtio-mmio network device, when a backend is attached.
    virtio_net: Option<RefCell<VirtioMmioNet>>,
    /// Host timestamp of the last RISH_DBG_NET diagnostics line.
    net_dbg_last: Option<std::time::Instant>,
    /// Bumped on every write so translation caches can invalidate cheaply.
    generation: u64,
    /// Per-4KiB-page write counter. A decode-cache entry records the page's
    /// counter at decode time and re-validates against it on a hit, so a hit
    /// costs a single load instead of re-reading and comparing the instruction
    /// bytes. Any write (including code patched through a writable alias, since
    /// this is keyed on the physical page) bumps the counter and invalidates
    /// the cached decode without an explicit flush.
    code_gen: Box<[u32]>,
}

/// Guest physical page size.
const PAGE_SIZE: u64 = 4096;

impl Memory {
    pub fn new(megabytes: usize) -> Result<Self, CpuError> {
        if megabytes == 0 {
            return Err(CpuError::InvalidConfig(
                "guest memory must be at least 1 MiB".to_owned(),
            ));
        }
        let bytes = megabytes
            .checked_mul(1024 * 1024)
            .ok_or_else(|| CpuError::InvalidConfig("guest memory size overflow".to_owned()))?;
        let pages = bytes.div_ceil(PAGE_SIZE as usize);
        Ok(Self {
            ram: vec![0; bytes].into_boxed_slice(),
            lapic: None,
            ioapic: RefCell::new(IoApic::new()),
            virtio_blk: None,
            virtio_net: None,
            net_dbg_last: None,
            generation: 0,
            code_gen: vec![0_u32; pages].into_boxed_slice(),
        })
    }

    /// Write counter for the 4KiB page containing `physical`, used by the
    /// decode cache to validate a hit without re-reading the bytes. Addresses
    /// outside RAM (memory-mapped devices) never hold cached code, so they map
    /// to a stable zero.
    #[inline]
    #[must_use]
    pub fn page_generation(&self, physical: u64) -> u32 {
        let page = (physical / PAGE_SIZE) as usize;
        self.code_gen.get(page).copied().unwrap_or(0)
    }

    /// Bumps the per-page write counters for every page the range touches.
    #[inline]
    fn bump_code_gen(&mut self, start: usize, len: usize) {
        bump_page_counters(&mut self.code_gen, start, len);
    }

    /// Write generation, for translation-cache invalidation.
    #[inline]
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.ram.len()
    }

    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn attach_lapic(&mut self) {
        self.lapic = Some(RefCell::new(LocalApic::new()));
    }

    /// Removes the next LAPIC vector ready for delivery to the CPU.
    pub fn lapic_pop_interrupt(&mut self) -> Option<u8> {
        self.lapic
            .as_mut()
            .and_then(|lapic| lapic.get_mut().pop_interrupt())
    }

    #[cfg(test)]
    pub fn lapic_enqueue_interrupt(&self, vector: u8) {
        if let Some(lapic) = &self.lapic {
            lapic.borrow_mut().enqueue_interrupt(vector);
        }
    }

    /// Advances the local APIC timer by a batch of guest instructions.
    pub fn lapic_tick(&mut self, instructions: u32) {
        if let Some(lapic) = &mut self.lapic {
            lapic.get_mut().tick(instructions);
        }
    }

    fn in_lapic(address: u64) -> bool {
        (LAPIC_BASE..LAPIC_BASE + LAPIC_SIZE).contains(&address)
    }

    fn in_ioapic(address: u64) -> bool {
        (IOAPIC_BASE..IOAPIC_BASE + IOAPIC_SIZE).contains(&address)
    }

    fn in_virtio(address: u64) -> bool {
        (VIRTIO_MMIO_BASE..VIRTIO_MMIO_BASE + VIRTIO_MMIO_WINDOW_BYTES).contains(&address)
    }

    fn in_virtio_net(address: u64) -> bool {
        (VIRTIO_NET_MMIO_BASE..VIRTIO_NET_MMIO_BASE + VIRTIO_MMIO_WINDOW_BYTES).contains(&address)
    }

    /// Attaches the virtio-mmio block device. A second device is refused so
    /// a caller can never silently replace a running backend.
    pub fn attach_virtio_blk(&mut self, device: VirtioMmioBlk) -> Result<(), CpuError> {
        if self.virtio_blk.is_some() {
            return Err(CpuError::InvalidConfig(
                "a virtio block device is already attached".to_owned(),
            ));
        }
        self.virtio_blk = Some(RefCell::new(device));
        Ok(())
    }

    /// Attaches the virtio-mmio network device. A second device is refused
    /// so a caller can never silently replace a running backend.
    pub fn attach_virtio_net(&mut self, device: VirtioMmioNet) -> Result<(), CpuError> {
        if self.virtio_net.is_some() {
            return Err(CpuError::InvalidConfig(
                "a virtio network device is already attached".to_owned(),
            ));
        }
        self.virtio_net = Some(RefCell::new(device));
        Ok(())
    }

    /// The sticky fail-closed fault the block device latched, if any. The
    /// provider surfaces it for diagnostics; the device itself stops
    /// servicing after it latches.
    #[must_use]
    pub fn virtio_blk_fault(&self) -> Option<String> {
        self.virtio_blk
            .as_ref()
            .and_then(|device| device.borrow().fault().map(str::to_owned))
    }

    /// The sticky fail-closed fault the network device latched, if any.
    #[must_use]
    pub fn virtio_net_fault(&self) -> Option<String> {
        self.virtio_net
            .as_ref()
            .and_then(|device| device.borrow().fault().map(str::to_owned))
    }

    /// Drains block requests the guest kicked since the last device tick.
    /// Returns true when at least one request completed, so the CPU can
    /// raise the used-ring interrupt edge on the device's IRQ line.
    pub fn poll_virtio_irq(&mut self) -> bool {
        let Some(device) = &self.virtio_blk else {
            return false;
        };
        let mut device = device.borrow_mut();
        let mut guest = DeviceMemory {
            ram: &mut self.ram,
            code_gen: &mut self.code_gen,
            wrote: false,
        };
        match device.poll_kick(&mut guest) {
            Ok(completed) => {
                if guest.wrote {
                    // Device writes into guest RAM can reach page-table or
                    // code pages; invalidate both caches like any RAM write.
                    self.generation = self.generation.wrapping_add(1);
                }
                completed
            }
            // poll_kick latches the fault itself; no edge is raised.
            Err(_) => false,
        }
    }

    /// Services the network device: transmit kicks, the host backend poll
    /// (wall-clock throttled inside the device), and receive-buffer fills.
    /// Returns true when at least one buffer completed, so the CPU can raise
    /// the used-ring interrupt edge on the device's IRQ line.
    pub fn poll_virtio_net(&mut self) -> bool {
        let Some(device) = &self.virtio_net else {
            return false;
        };
        let mut device = device.borrow_mut();
        // Env-gated diagnostics: fault, counters, and drop tallies, at most
        // once every two seconds of host time. The device itself stops
        // servicing after it latches a fault, so this is the only place the
        // host can see why.
        if std::env::var_os("RISH_DBG_NET").is_some()
            && self
                .net_dbg_last
                .is_none_or(|last| last.elapsed() >= std::time::Duration::from_secs(2))
        {
            self.net_dbg_last = Some(std::time::Instant::now());
            eprintln!(
                "[rish-softvm net] fault={:?} {}",
                device.fault(),
                device.debug_state(),
            );
        }
        let mut guest = DeviceMemory {
            ram: &mut self.ram,
            code_gen: &mut self.code_gen,
            wrote: false,
        };
        match device.poll(&mut guest) {
            Ok(completed) => {
                if guest.wrote {
                    self.generation = self.generation.wrapping_add(1);
                }
                completed
            }
            // poll latches the fault itself; no edge is raised.
            Err(_) => false,
        }
    }

    /// Applies interrupt line levels to the I/O APIC and forwards any fired
    /// vectors to the local APIC.
    pub fn ioapic_set_lines(&self, lines: u32) {
        let fired = self.ioapic.borrow_mut().set_lines(lines);
        self.forward_to_lapic(&fired);
    }

    /// Delivers a one-shot edge on an I/O APIC pin (rising then falling).
    pub fn ioapic_pulse(&self, pin: u8, levels: u32) {
        let bit = 1_u32 << pin;
        let fired = {
            let mut ioapic = self.ioapic.borrow_mut();
            let fired = ioapic.set_lines(levels | bit);
            ioapic.set_lines(levels & !bit);
            fired
        };
        self.forward_to_lapic(&fired);
    }

    fn forward_to_lapic(&self, vectors: &[u8]) {
        if vectors.is_empty() {
            return;
        }
        if let Some(lapic) = &self.lapic {
            let mut lapic = lapic.borrow_mut();
            for vector in vectors {
                lapic.request(*vector);
            }
        }
    }

    #[inline]
    pub fn read(&self, address: u64, output: &mut [u8]) -> Result<(), CpuError> {
        if Self::in_virtio_net(address) {
            let offset = address - VIRTIO_NET_MMIO_BASE;
            if offset >= VIRTIO_MMIO_REGISTER_BYTES {
                for slot in output.iter_mut() {
                    *slot = 0;
                }
                return Ok(());
            }
            if let Some(device) = &self.virtio_net {
                let value = device.borrow_mut().mmio_read(offset);
                let bytes = value.to_le_bytes();
                let count = output.len().min(4);
                output[..count].copy_from_slice(&bytes[..count]);
                if output.len() > 4 {
                    let value2 = device.borrow_mut().mmio_read(offset + 4);
                    let bytes2 = value2.to_le_bytes();
                    let rest = output.len() - 4;
                    output[4..].copy_from_slice(&bytes2[..rest]);
                }
            } else {
                for slot in output.iter_mut() {
                    *slot = 0;
                }
            }
            return Ok(());
        }
        if Self::in_virtio(address) {
            let offset = address - VIRTIO_MMIO_BASE;
            if offset >= VIRTIO_MMIO_REGISTER_BYTES {
                // Inside the advertised 1 KiB window but past the register
                // file: reads as zero.
                for slot in output.iter_mut() {
                    *slot = 0;
                }
                return Ok(());
            }
            if let Some(device) = &self.virtio_blk {
                let value = device.borrow_mut().mmio_read(offset);
                let bytes = value.to_le_bytes();
                let count = output.len().min(4);
                output[..count].copy_from_slice(&bytes[..count]);
                if output.len() > 4 {
                    let value2 = device.borrow_mut().mmio_read(offset + 4);
                    let bytes2 = value2.to_le_bytes();
                    let rest = output.len() - 4;
                    output[4..].copy_from_slice(&bytes2[..rest]);
                }
            } else {
                for slot in output.iter_mut() {
                    *slot = 0;
                }
            }
            return Ok(());
        }
        if Self::in_ioapic(address) {
            let offset = address - IOAPIC_BASE;
            let value = self.ioapic.borrow_mut().read(offset, output.len() as u8);
            let bytes = value.to_le_bytes();
            let count = output.len().min(4);
            output[..count].copy_from_slice(&bytes[..count]);
            for slot in output.iter_mut().skip(4) {
                *slot = 0;
            }
            return Ok(());
        }
        if Self::in_lapic(address) {
            if let Some(lapic) = &self.lapic {
                let offset = address - LAPIC_BASE;
                let value = lapic.borrow_mut().read(offset, output.len() as u8);
                let bytes = value.to_le_bytes();
                let first = output.len().min(4);
                output[..first].copy_from_slice(&bytes[..first]);
                if output.len() > 4 {
                    let value2 = lapic
                        .borrow_mut()
                        .read(offset + 4, (output.len() - 4) as u8);
                    let bytes2 = value2.to_le_bytes();
                    let rest = output.len() - 4;
                    output[4..].copy_from_slice(&bytes2[..rest]);
                }
                return Ok(());
            }
        }
        let end = address.checked_add(output.len() as u64).ok_or_else(|| {
            CpuError::GuestFault(format!("memory read address overflow at {address:#x}"))
        })?;
        if end > self.ram.len() as u64 {
            return Err(CpuError::GuestFault(format!(
                "memory read {address:#x}..{end:#x} exceeds {} bytes",
                self.ram.len()
            )));
        }
        let start = address as usize;
        output.copy_from_slice(&self.ram[start..start + output.len()]);
        Ok(())
    }

    #[inline]
    pub fn write(&mut self, address: u64, input: &[u8]) -> Result<(), CpuError> {
        if Self::in_virtio_net(address) {
            let offset = address - VIRTIO_NET_MMIO_BASE;
            if offset < VIRTIO_MMIO_REGISTER_BYTES {
                if std::env::var_os("RISH_DBG_NET").is_some() {
                    let mut buffer = [0_u8; 4];
                    let count = input.len().min(4);
                    buffer[..count].copy_from_slice(&input[..count]);
                    eprintln!(
                        "[rish-softvm net wr] offset={offset:#x} value={:#010x}",
                        u32::from_le_bytes(buffer),
                    );
                }
                if let Some(device) = &self.virtio_net {
                    let mut buffer = [0_u8; 4];
                    let count = input.len().min(4);
                    buffer[..count].copy_from_slice(&input[..count]);
                    device
                        .borrow_mut()
                        .mmio_write(offset, u32::from_le_bytes(buffer));
                }
            }
            return Ok(());
        }
        if Self::in_virtio(address) {
            let offset = address - VIRTIO_MMIO_BASE;
            if offset < VIRTIO_MMIO_REGISTER_BYTES {
                if let Some(device) = &self.virtio_blk {
                    let mut buffer = [0_u8; 4];
                    let count = input.len().min(4);
                    buffer[..count].copy_from_slice(&input[..count]);
                    device
                        .borrow_mut()
                        .mmio_write(offset, u32::from_le_bytes(buffer));
                }
            }
            return Ok(());
        }
        if Self::in_ioapic(address) {
            let offset = address - IOAPIC_BASE;
            let mut buffer = [0_u8; 4];
            let count = input.len().min(4);
            buffer[..count].copy_from_slice(&input[..count]);
            self.ioapic
                .borrow_mut()
                .write(offset, input.len() as u8, u32::from_le_bytes(buffer));
            return Ok(());
        }
        if Self::in_lapic(address) {
            if let Some(lapic) = &self.lapic {
                let offset = address - LAPIC_BASE;
                let mut buffer = [0_u8; 4];
                let count = input.len().min(4);
                buffer[..count].copy_from_slice(&input[..count]);
                let value = u32::from_le_bytes(buffer);
                let retired = lapic.borrow_mut().write(offset, input.len() as u8, value);
                // A retiring level-triggered vector may refire immediately if
                // its line is still asserted at the I/O APIC.
                if let Some(vector) = retired {
                    let refired = self.ioapic.borrow_mut().end_of_interrupt(vector);
                    self.forward_to_lapic(&refired);
                }
                return Ok(());
            }
        }
        let end = address.checked_add(input.len() as u64).ok_or_else(|| {
            CpuError::GuestFault(format!("memory write address overflow at {address:#x}"))
        })?;
        if end > self.ram.len() as u64 {
            return Err(CpuError::GuestFault(format!(
                "memory write {address:#x}..{end:#x} exceeds {} bytes",
                self.ram.len()
            )));
        }
        let start = address as usize;
        self.ram[start..start + input.len()].copy_from_slice(input);
        self.generation = self.generation.wrapping_add(1);
        self.bump_code_gen(start, input.len());
        Ok(())
    }

    #[inline]
    pub fn read_u8(&self, address: u64) -> Result<u8, CpuError> {
        self.read_u64_at(address, 1).map(|value| value as u8)
    }

    #[inline]
    pub fn read_u16(&self, address: u64) -> Result<u16, CpuError> {
        self.read_u64_at(address, 2).map(|value| value as u16)
    }

    #[inline]
    pub fn read_u32(&self, address: u64) -> Result<u32, CpuError> {
        self.read_u64_at(address, 4).map(|value| value as u32)
    }

    #[inline]
    pub fn read_u64(&self, address: u64) -> Result<u64, CpuError> {
        self.read_u64_at(address, 8)
    }

    #[inline]
    fn read_u64_at(&self, address: u64, size: usize) -> Result<u64, CpuError> {
        let mut buffer = [0_u8; 8];
        self.read(address, &mut buffer[..size])?;
        Ok(u64::from_le_bytes(buffer))
    }

    #[inline]
    pub fn write_u8(&mut self, address: u64, value: u8) -> Result<(), CpuError> {
        self.write(address, &value.to_le_bytes())
    }

    #[inline]
    pub fn write_u16(&mut self, address: u64, value: u16) -> Result<(), CpuError> {
        self.write(address, &value.to_le_bytes())
    }

    #[inline]
    pub fn write_u32(&mut self, address: u64, value: u32) -> Result<(), CpuError> {
        self.write(address, &value.to_le_bytes())
    }

    #[inline]
    pub fn write_u64(&mut self, address: u64, value: u64) -> Result<(), CpuError> {
        self.write(address, &value.to_le_bytes())
    }
}

/// Guest-memory adapter handed to the virtio block device while it drains
/// its queue: direct, bounds-checked access to the RAM slice plus the
/// decode-cache write counters, so device writes invalidate cached decodes
/// exactly like any other RAM write.
struct DeviceMemory<'a> {
    ram: &'a mut [u8],
    code_gen: &'a mut [u32],
    wrote: bool,
}

impl GuestMemory for DeviceMemory<'_> {
    fn ram_bytes(&self) -> u64 {
        self.ram.len() as u64
    }

    fn read(&self, address: u64, output: &mut [u8]) -> Result<(), VirtioError> {
        let end = address
            .checked_add(output.len() as u64)
            .ok_or(VirtioError::OutOfBounds {
                address,
                bytes: output.len() as u64,
            })?;
        if end > self.ram.len() as u64 {
            return Err(VirtioError::OutOfBounds {
                address,
                bytes: output.len() as u64,
            });
        }
        output.copy_from_slice(&self.ram[address as usize..end as usize]);
        Ok(())
    }

    fn write(&mut self, address: u64, input: &[u8]) -> Result<(), VirtioError> {
        let end = address
            .checked_add(input.len() as u64)
            .ok_or(VirtioError::OutOfBounds {
                address,
                bytes: input.len() as u64,
            })?;
        if end > self.ram.len() as u64 {
            return Err(VirtioError::OutOfBounds {
                address,
                bytes: input.len() as u64,
            });
        }
        let start = address as usize;
        self.ram[start..end as usize].copy_from_slice(input);
        bump_page_counters(self.code_gen, start, input.len());
        self.wrote = true;
        Ok(())
    }
}

/// Bumps the per-page write counters for every page the range touches.
fn bump_page_counters(code_gen: &mut [u32], start: usize, len: usize) {
    let first = start / PAGE_SIZE as usize;
    let last = (start + len - 1) / PAGE_SIZE as usize;
    for page in first..=last {
        if let Some(counter) = code_gen.get_mut(page) {
            *counter = counter.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_scalar_widths() {
        let mut memory = Memory::new(1).unwrap();
        memory.write_u32(0x1000, 0xDEAD_BEEF).unwrap();
        assert_eq!(memory.read_u32(0x1000).unwrap(), 0xDEAD_BEEF);
        assert_eq!(memory.read_u16(0x1000).unwrap(), 0xBEEF);
        memory.write_u8(0x1002, 0x42).unwrap();
        assert_eq!(memory.read_u32(0x1000).unwrap(), 0xDE42_BEEF);
        memory.write_u64(0x2000, 0x0102_0304_0506_0708).unwrap();
        assert_eq!(memory.read_u64(0x2000).unwrap(), 0x0102_0304_0506_0708);
    }

    #[test]
    fn out_of_bounds_reads_fail() {
        let memory = Memory::new(1).unwrap();
        let end = memory.len() as u64;
        assert!(memory.read_u32(end - 2).is_err());
        assert!(memory.read_u8(end).is_err());
    }

    #[test]
    fn zero_memory_is_rejected() {
        assert!(Memory::new(0).is_err());
    }

    #[test]
    fn virtio_mmio_window_dispatches_reads_and_writes() {
        use crate::virtio::backend::VecBlockBackend;
        use crate::virtio::{VIRTIO_MMIO_BASE, VIRTIO_MMIO_REGISTER_BYTES, VirtioMmioBlk};

        let mut memory = Memory::new(1).unwrap();
        // Without a device the window reads as zero and writes are dropped.
        let mut bytes = [0xFF_u8; 4];
        memory.read(VIRTIO_MMIO_BASE, &mut bytes).unwrap();
        assert_eq!(bytes, [0; 4]);
        memory.write(VIRTIO_MMIO_BASE, &[0x12, 0, 0, 0]).unwrap();

        let device = VirtioMmioBlk::new(Box::new(VecBlockBackend {
            bytes: vec![0_u8; 4096],
        }))
        .unwrap();
        memory.attach_virtio_blk(device).unwrap();
        // A second device is refused, never silently replaced.
        let second = VirtioMmioBlk::new(Box::new(VecBlockBackend {
            bytes: vec![0_u8; 512],
        }))
        .unwrap();
        assert!(memory.attach_virtio_blk(second).is_err());

        // The magic value reads back through the MMIO dispatch.
        let mut magic = [0_u8; 4];
        memory.read(VIRTIO_MMIO_BASE, &mut magic).unwrap();
        assert_eq!(magic, [0x76, 0x69, 0x72, 0x74]); // "virt"
        // Reads past the register file (inside the 1 KiB window) are zero.
        let mut past = [0xAB_u8; 4];
        memory
            .read(VIRTIO_MMIO_BASE + VIRTIO_MMIO_REGISTER_BYTES, &mut past)
            .unwrap();
        assert_eq!(past, [0; 4]);
        // An 8-byte config read composes the two capacity words.
        let mut capacity = [0_u8; 8];
        memory
            .read(VIRTIO_MMIO_BASE + 0x100, &mut capacity)
            .unwrap();
        assert_eq!(capacity, 8_u64.to_le_bytes());
    }
}
