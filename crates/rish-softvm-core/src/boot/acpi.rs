//! Minimal ACPI tables for the software machine.
//!
//! The kernel needs a MADT to discover the local APIC and the I/O APIC and to
//! switch into symmetric I/O mode; without one it stays on the legacy 8259
//! path, which modern kernels no longer bring up fully. The set published
//! here is the smallest coherent one: an ACPI 2.0 RSDP pointing at an XSDT
//! that lists a normal (non-hardware-reduced) FADT, a minimal FACS, an empty
//! DSDT, and the MADT.
//!
//! The FADT is deliberately *not* hardware-reduced: a hardware-reduced FADT
//! makes the kernel run `acpi_generic_reduced_hw_init`, which swaps in the
//! null legacy PIC and never preallocates the legacy IRQ descriptors, so the
//! 8250 serial console can never register its IRQ 4 and every console write
//! fails with EIO. A normal FADT keeps the 8259/PIT alive. To avoid also
//! having to emulate the SMI/SCI enable handshake, `SMI_CMD` is left zero,
//! which makes ACPICA treat the machine as already in ACPI mode.
//!
//! The tables live in the reserved window at the top of RAM and are announced
//! through `boot_params.acpi_rsdp_addr` (boot protocol 2.14+). A copy of the
//! RSDP is also placed in the legacy BIOS search area at 0xF0000 for
//! completeness.

use crate::devices::ioapic::IOAPIC_BASE;
use crate::devices::lapic::LAPIC_BASE;

const OEM_ID: &[u8; 6] = b"RISHVM";
const OEM_TABLE_ID: &[u8; 8] = b"RISHSOFT";
const CREATOR_ID: &[u8; 4] = b"RISH";

/// Legacy BIOS area address where a second RSDP copy is placed; the kernel
/// scans 0xE0000..0xFFFFF on 16-byte boundaries.
pub const RSDP_BIOS_AREA: u64 = 0xF0000;

/// One built table blob and where it must be written in guest memory.
pub struct AcpiTables {
    pub rsdp_address: u64,
    pub blob_address: u64,
    pub blob: Vec<u8>,
    pub rsdp_copy: Vec<u8>,
}

/// Builds the table set for a machine with one CPU, placing everything at
/// `base` (page aligned, inside RAM the E820 map reports as ACPI data).
#[must_use]
pub fn build(base: u64) -> AcpiTables {
    let mut blob = Vec::new();

    // The DSDT is a valid empty definition block: nothing but its header.
    let dsdt_offset = blob.len();
    blob.extend_from_slice(&sdt(*b"DSDT", 2, &[]));
    let dsdt_address = base + dsdt_offset as u64;

    // The FACS must be 64-byte aligned per the ACPI spec.
    let facs_offset = (blob.len() + 63) & !63;
    blob.resize(facs_offset, 0);
    blob.extend_from_slice(&facs());
    let facs_address = base + facs_offset as u64;

    let fadt_offset = align_up(blob.len());
    blob.resize(fadt_offset, 0);
    blob.extend_from_slice(&fadt(facs_address, dsdt_address));
    let fadt_address = base + fadt_offset as u64;

    let madt_offset = align_up(blob.len());
    blob.resize(madt_offset, 0);
    blob.extend_from_slice(&madt());
    let madt_address = base + madt_offset as u64;

    let xsdt_offset = align_up(blob.len());
    blob.resize(xsdt_offset, 0);
    let mut entries = Vec::new();
    entries.extend_from_slice(&fadt_address.to_le_bytes());
    entries.extend_from_slice(&madt_address.to_le_bytes());
    blob.extend_from_slice(&sdt(*b"XSDT", 1, &entries));
    let xsdt_address = base + xsdt_offset as u64;

    let rsdp_offset = align_up(blob.len());
    blob.resize(rsdp_offset, 0);
    let rsdp = rsdp(xsdt_address);
    blob.extend_from_slice(&rsdp);
    let rsdp_address = base + rsdp_offset as u64;

    AcpiTables {
        rsdp_address,
        blob_address: base,
        blob,
        rsdp_copy: rsdp,
    }
}

fn align_up(offset: usize) -> usize {
    (offset + 15) & !15
}

/// An SDT: standard 36-byte header plus payload, with the checksum fixed up.
fn sdt(signature: [u8; 4], revision: u8, payload: &[u8]) -> Vec<u8> {
    let length = 36 + payload.len();
    let mut table = Vec::with_capacity(length);
    table.extend_from_slice(&signature);
    table.extend_from_slice(&(length as u32).to_le_bytes());
    table.push(revision);
    table.push(0); // checksum, patched below
    table.extend_from_slice(OEM_ID);
    table.extend_from_slice(OEM_TABLE_ID);
    table.extend_from_slice(&1_u32.to_le_bytes()); // OEM revision
    table.extend_from_slice(CREATOR_ID);
    table.extend_from_slice(&1_u32.to_le_bytes()); // creator revision
    table.extend_from_slice(payload);
    table[9] = checksum(&table);
    table
}

fn checksum(bytes: &[u8]) -> u8 {
    let sum: u8 = bytes.iter().fold(0_u8, |acc, b| acc.wrapping_add(*b));
    sum.wrapping_neg()
}

/// ACPI 2.0 RSDP: 36 bytes with both checksums valid. The RSDT address is
/// zero, steering the kernel to the XSDT.
fn rsdp(xsdt_address: u64) -> Vec<u8> {
    let mut table = Vec::with_capacity(36);
    table.extend_from_slice(b"RSD PTR ");
    table.push(0); // checksum over the first 20 bytes, patched below
    table.extend_from_slice(OEM_ID);
    table.push(2); // revision: ACPI 2.0+
    table.extend_from_slice(&0_u32.to_le_bytes()); // rsdt_address: none
    table.extend_from_slice(&36_u32.to_le_bytes()); // length
    table.extend_from_slice(&xsdt_address.to_le_bytes());
    table.push(0); // extended checksum, patched below
    table.extend_from_slice(&[0, 0, 0]); // reserved
    table[8] = checksum(&table[..20]);
    table[32] = checksum(&table);
    table
}

/// Normal (non-hardware-reduced) FADT, revision 5. It keeps the legacy 8259
/// and PIT alive and points at the FACS, DSDT, and PM register block. `SMI_CMD`
/// is zero so ACPICA treats the machine as already in ACPI mode and skips the
/// enable handshake.
fn fadt(facs_address: u64, dsdt_address: u64) -> Vec<u8> {
    use crate::devices::acpi_pm::PM_BASE;

    // FADT body after the 36-byte header is 268 - 36 = 232 bytes, which reaches
    // through the X_GPE0_BLK generic address at offset 220.
    let mut body = vec![0_u8; 232];
    // All offsets below are absolute FADT offsets minus the 36-byte header.
    let put32 = |body: &mut [u8], abs: usize, value: u32| {
        body[abs - 36..abs - 36 + 4].copy_from_slice(&value.to_le_bytes());
    };
    let put16 = |body: &mut [u8], abs: usize, value: u16| {
        body[abs - 36..abs - 36 + 2].copy_from_slice(&value.to_le_bytes());
    };

    put32(&mut body, 36, facs_address as u32); // FIRMWARE_CTRL
    put32(&mut body, 40, dsdt_address as u32); // DSDT (32-bit)
    body[45 - 36] = 0; // preferred PM profile: unspecified
    put16(&mut body, 46, 9); // SCI_INT
    put32(&mut body, 48, 0); // SMI_CMD: 0 => already in ACPI mode
    body[52 - 36] = 0; // ACPI_ENABLE
    body[53 - 36] = 0; // ACPI_DISABLE
    put32(&mut body, 56, u32::from(PM_BASE)); // PM1a_EVT_BLK
    put32(&mut body, 64, u32::from(PM_BASE) + 4); // PM1a_CNT_BLK
    put32(&mut body, 76, u32::from(PM_BASE) + 8); // PM_TMR_BLK
    body[88 - 36] = 4; // PM1_EVT_LEN
    body[89 - 36] = 2; // PM1_CNT_LEN
    body[91 - 36] = 4; // PM_TMR_LEN
    // IAPC_BOOT_ARCH (u16 at 109): legacy devices present, no VGA, no CMOS RTC.
    put16(&mut body, 109, (1 << 0) | (1 << 2) | (1 << 5));
    // FLAGS (u32 at 112): WBINVD, control-method power and sleep buttons.
    // Crucially, HW_REDUCED_ACPI (bit 20) is clear.
    put32(&mut body, 112, (1 << 0) | (1 << 4) | (1 << 5));
    body[131 - 36] = 1; // FADT minor version
    // X_DSDT (64-bit) at offset 140, matching the 32-bit DSDT above.
    body[140 - 36..148 - 36].copy_from_slice(&dsdt_address.to_le_bytes());
    sdt(*b"FACP", 5, &body)
}

/// Minimal 64-byte FACS. It carries no waking vector or global lock; ACPICA
/// only needs it to exist for a non-hardware-reduced FADT. The FACS has no
/// SDT header and no checksum.
fn facs() -> Vec<u8> {
    let mut table = vec![0_u8; 64];
    table[0..4].copy_from_slice(b"FACS");
    table[4..8].copy_from_slice(&64_u32.to_le_bytes()); // length
    table[32] = 2; // version
    table
}

/// MADT with the local APIC, one enabled CPU, and the I/O APIC at GSI 0.
fn madt() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&(LAPIC_BASE as u32).to_le_bytes());
    body.extend_from_slice(&1_u32.to_le_bytes()); // flags: PCAT_COMPAT

    // Processor local APIC: type 0, length 8.
    body.extend_from_slice(&[0, 8, 0 /* acpi id */, 0 /* apic id */]);
    body.extend_from_slice(&1_u32.to_le_bytes()); // flags: enabled

    // I/O APIC: type 1, length 12.
    body.extend_from_slice(&[1, 12, 0 /* ioapic id */, 0 /* reserved */]);
    body.extend_from_slice(&(IOAPIC_BASE as u32).to_le_bytes());
    body.extend_from_slice(&0_u32.to_le_bytes()); // global system interrupt base

    sdt(*b"APIC", 3, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_at(blob: &[u8], base: u64, address: u64) -> &[u8] {
        let offset = (address - base) as usize;
        let length = u32::from_le_bytes([
            blob[offset + 4],
            blob[offset + 5],
            blob[offset + 6],
            blob[offset + 7],
        ]) as usize;
        &blob[offset..offset + length]
    }

    #[test]
    fn every_table_checksums_to_zero() {
        let tables = build(0x3FFE_0000);
        let rsdp_offset = (tables.rsdp_address - tables.blob_address) as usize;
        let rsdp = &tables.blob[rsdp_offset..rsdp_offset + 36];
        assert_eq!(&rsdp[..8], b"RSD PTR ");
        assert_eq!(rsdp[..20].iter().fold(0_u8, |a, b| a.wrapping_add(*b)), 0);
        assert_eq!(rsdp.iter().fold(0_u8, |a, b| a.wrapping_add(*b)), 0);
        let xsdt_address = u64::from_le_bytes(rsdp[24..32].try_into().unwrap());
        let xsdt = table_at(&tables.blob, tables.blob_address, xsdt_address);
        assert_eq!(&xsdt[..4], b"XSDT");
        assert_eq!(xsdt.iter().fold(0_u8, |a, b| a.wrapping_add(*b)), 0);
        // Two XSDT entries: FADT then MADT.
        assert_eq!(xsdt.len(), 36 + 16);
        let fadt_address = u64::from_le_bytes(xsdt[36..44].try_into().unwrap());
        let madt_address = u64::from_le_bytes(xsdt[44..52].try_into().unwrap());
        let fadt = table_at(&tables.blob, tables.blob_address, fadt_address);
        assert_eq!(&fadt[..4], b"FACP");
        assert_eq!(fadt.len(), 268);
        assert_eq!(fadt.iter().fold(0_u8, |a, b| a.wrapping_add(*b)), 0);
        let madt = table_at(&tables.blob, tables.blob_address, madt_address);
        assert_eq!(&madt[..4], b"APIC");
        assert_eq!(madt.iter().fold(0_u8, |a, b| a.wrapping_add(*b)), 0);
    }

    #[test]
    fn the_fadt_is_not_hardware_reduced_and_names_the_dsdt() {
        use crate::devices::acpi_pm::PM_BASE;
        let tables = build(0x1000);
        let rsdp_offset = (tables.rsdp_address - tables.blob_address) as usize;
        let rsdp = &tables.blob[rsdp_offset..rsdp_offset + 36];
        let xsdt_address = u64::from_le_bytes(rsdp[24..32].try_into().unwrap());
        let xsdt = table_at(&tables.blob, tables.blob_address, xsdt_address);
        let fadt_address = u64::from_le_bytes(xsdt[36..44].try_into().unwrap());
        let fadt = table_at(&tables.blob, tables.blob_address, fadt_address);
        // HW_REDUCED_ACPI must be clear so the kernel keeps the legacy 8259.
        let flags = u32::from_le_bytes(fadt[112..116].try_into().unwrap());
        assert_eq!(flags & (1 << 20), 0, "HW_REDUCED_ACPI must be clear");
        // SMI_CMD is zero so ACPICA treats the machine as already in ACPI mode.
        assert_eq!(u32::from_le_bytes(fadt[48..52].try_into().unwrap()), 0);
        // The PM register block is published at PM_BASE.
        assert_eq!(
            u32::from_le_bytes(fadt[56..60].try_into().unwrap()),
            u32::from(PM_BASE)
        );
        // Both the 32-bit DSDT and X_DSDT point at the DSDT table.
        let dsdt32 = u32::from_le_bytes(fadt[40..44].try_into().unwrap());
        let x_dsdt = u64::from_le_bytes(fadt[140..148].try_into().unwrap());
        assert_eq!(u64::from(dsdt32), x_dsdt);
        let dsdt = table_at(&tables.blob, tables.blob_address, x_dsdt);
        assert_eq!(&dsdt[..4], b"DSDT");
        assert_eq!(dsdt.len(), 36);
        // The FIRMWARE_CTRL pointer names a valid FACS.
        let facs_address = u32::from_le_bytes(fadt[36..40].try_into().unwrap());
        let facs = table_at(&tables.blob, tables.blob_address, u64::from(facs_address));
        assert_eq!(&facs[..4], b"FACS");
    }

    #[test]
    fn the_madt_names_the_lapic_and_ioapic() {
        let tables = build(0x1000);
        let rsdp_offset = (tables.rsdp_address - tables.blob_address) as usize;
        let rsdp = &tables.blob[rsdp_offset..rsdp_offset + 36];
        let xsdt_address = u64::from_le_bytes(rsdp[24..32].try_into().unwrap());
        let xsdt = table_at(&tables.blob, tables.blob_address, xsdt_address);
        let madt_address = u64::from_le_bytes(xsdt[44..52].try_into().unwrap());
        let madt = table_at(&tables.blob, tables.blob_address, madt_address);
        let lapic = u32::from_le_bytes(madt[36..40].try_into().unwrap());
        assert_eq!(u64::from(lapic), LAPIC_BASE);
        // First subtable: enabled CPU. Second: the I/O APIC.
        assert_eq!(madt[44], 0);
        assert_eq!(madt[52], 1);
        let ioapic = u32::from_le_bytes(madt[56..60].try_into().unwrap());
        assert_eq!(u64::from(ioapic), IOAPIC_BASE);
    }
}
