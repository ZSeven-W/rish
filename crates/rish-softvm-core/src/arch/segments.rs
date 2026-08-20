//! Segment selectors and their hidden descriptor caches.

/// Raw 16-bit segment selector.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub struct SegmentSelector(pub u16);

impl SegmentSelector {
    #[inline]
    #[must_use]
    pub fn index(self) -> u16 {
        self.0 >> 3
    }

    #[inline]
    #[must_use]
    pub fn table(self) -> u8 {
        u8::from(self.0 & 0b100 != 0)
    }

    #[inline]
    #[must_use]
    pub fn rpl(self) -> u8 {
        (self.0 & 0b11) as u8
    }
}

/// Segment descriptor attribute byte (access byte of an 8-byte descriptor).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SegmentAttributes {
    pub present: bool,
    pub dpl: u8,
    pub system: bool,
    pub descriptor_type: u8,
    pub accessed: bool,
}

/// A loaded segment: the visible selector plus the cached descriptor fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentRegister {
    pub selector: SegmentSelector,
    pub base: u64,
    pub limit: u32,
    pub attributes: SegmentAttributes,
    pub granularity: bool,
    /// D/B bit: 32-bit default size for code/data segments.
    pub default_32: bool,
    /// L bit: 64-bit code segment.
    pub long_mode: bool,
    /// Expand-down data segment.
    pub expand_down: bool,
    /// Writable data segment / readable code segment.
    pub writable_or_readable: bool,
    /// Code vs data segment.
    pub code: bool,
    pub conforming: bool,
}

impl Default for SegmentRegister {
    fn default() -> Self {
        Self {
            selector: SegmentSelector(0),
            base: 0,
            limit: u32::MAX,
            attributes: SegmentAttributes {
                present: true,
                dpl: 0,
                system: false,
                descriptor_type: 0,
                accessed: false,
            },
            granularity: true,
            default_32: false,
            long_mode: false,
            expand_down: false,
            writable_or_readable: true,
            code: false,
            conforming: false,
        }
    }
}

impl SegmentRegister {
    /// The real-mode reset / compatibility segment shape: 16-bit, no paging
    /// constraints, byte-granular full-address-space limit.
    #[must_use]
    pub fn real_mode(selector: SegmentSelector) -> Self {
        Self {
            selector,
            base: u64::from(selector.0) << 4,
            ..Self::default()
        }
    }

    #[inline]
    #[must_use]
    pub fn is_32_bit_code(&self) -> bool {
        self.code && self.default_32
    }

    #[inline]
    #[must_use]
    pub fn effective_limit(&self) -> u64 {
        if self.granularity {
            (u64::from(self.limit) << 12) | 0xFFF
        } else {
            u64::from(self.limit)
        }
    }
}

/// One 8-byte GDT/LDT descriptor entry, decoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Descriptor {
    pub base: u64,
    pub limit: u32,
    pub granularity: bool,
    pub default_32: bool,
    pub long_mode: bool,
    pub present: bool,
    pub dpl: u8,
    pub system: bool,
    pub descriptor_type: u8,
    pub code: bool,
    pub conforming: bool,
    pub expand_down: bool,
    pub writable_or_readable: bool,
    pub accessed: bool,
}

impl Descriptor {
    /// Decodes a legacy (non-system) 8-byte descriptor.
    #[must_use]
    pub fn decode(entry: u64) -> Self {
        Self {
            base: (entry & 0xFF00_0000_0000_0000) >> 32
                | (entry & 0x0000_00FF_0000_0000) >> 16
                | (entry >> 16) & 0xFFFF,
            limit: (((entry >> 48) & 0xF) as u32 * 0x10000) | (entry & 0xFFFF) as u32,
            granularity: entry & (1 << 55) != 0,
            default_32: entry & (1 << 54) != 0,
            long_mode: entry & (1 << 53) != 0,
            present: entry & (1 << 47) != 0,
            dpl: ((entry >> 45) & 0b11) as u8,
            system: entry & (1 << 44) == 0,
            descriptor_type: ((entry >> 40) & 0xF) as u8,
            code: entry & (1 << 43) != 0,
            conforming: entry & (1 << 42) != 0,
            expand_down: entry & (1 << 42) != 0,
            writable_or_readable: entry & (1 << 41) != 0,
            accessed: entry & (1 << 40) != 0,
        }
    }

    /// Loads this descriptor into a segment register cache.
    #[must_use]
    pub fn load(&self, selector: SegmentSelector) -> SegmentRegister {
        SegmentRegister {
            selector,
            base: self.base,
            limit: self.limit,
            attributes: SegmentAttributes {
                present: self.present,
                dpl: self.dpl,
                system: self.system,
                descriptor_type: self.descriptor_type,
                accessed: self.accessed,
            },
            granularity: self.granularity,
            default_32: self.default_32,
            long_mode: self.long_mode,
            expand_down: self.expand_down,
            writable_or_readable: self.writable_or_readable,
            code: self.code,
            conforming: self.conforming,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_64_bit_code_descriptor() {
        // base 0, limit 0, G=1, D=0, L=1, P=1, type 0xA (code, readable)
        let entry: u64 = (1 << 55) | (1 << 53) | (1 << 47) | (0xA << 40);
        let descriptor = Descriptor::decode(entry);
        assert!(descriptor.granularity);
        assert!(descriptor.long_mode);
        assert!(!descriptor.default_32);
        assert!(descriptor.present);
        assert!(descriptor.code);
        assert!(descriptor.writable_or_readable);
    }

    #[test]
    fn decodes_base_and_limit_split_fields() {
        // base 0x12345678, limit 0x9ABCD (granularity off)
        let base: u64 = 0x1234_5678;
        let entry = (base & 0xFF00_0000) << 32
            | (base & 0x00FF_0000) << 16
            | (base & 0xFFFF) << 16
            | 0x9ABC;
        let descriptor = Descriptor::decode(entry);
        assert_eq!(descriptor.base, base);
        assert_eq!(descriptor.limit, 0x9ABC);
        assert!(!descriptor.granularity);
    }

    #[test]
    fn real_mode_segment_base_shifts_selector() {
        let segment = SegmentRegister::real_mode(SegmentSelector(0x1234));
        assert_eq!(segment.base, 0x12340);
    }
}
