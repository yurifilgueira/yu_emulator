//! System bus: routes CPU loads/stores to the right device.
//!
//! psx-spx: "Memory Map"
//!
//! RAM, BIOS, scratchpad and expansion regions are real. I/O ports are
//! stubbed: reads return 0, writes are dropped, and every unhandled access is
//! recorded in `Bus::unhandled` so the harness can show exactly which device
//! needs implementing next.
//!
//! psx-spx: "Memory Map" / "Memory Exceptions" — accesses to unused memory
//! regions fail with [`BusFault`], which the CPU turns into a bus error
//! exception (IBE/DBE). They are still recorded in `Bus::unhandled`.
//!
//! Deviation: psx-spx says gaps *inside* the I/O region also raise bus
//! errors. Until the individual devices exist (Phase 2+) the whole I/O window
//! 0x1F80_1000-0x1F80_2FFF and expansion 2 are treated as mapped stubs.

use crate::map::{self, mask_region};
use std::collections::HashSet;
use std::fmt;

/// Width of a memory access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Width {
    Byte,
    Half,
    Word,
}

impl Width {
    pub const fn bytes(self) -> usize {
        match self {
            Width::Byte => 1,
            Width::Half => 2,
            Width::Word => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessKind {
    Load,
    Store,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Region {
    /// Hardware register not emulated yet.
    Io,
    /// Nothing mapped at this address (bus error on real hardware).
    Unmapped,
}

/// An access the bus could not service faithfully.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnhandledAccess {
    pub region: Region,
    pub kind: AccessKind,
    pub width: Width,
    /// Physical address.
    pub addr: u32,
}

impl fmt::Display for UnhandledAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} {:?} {:?} @ 0x{:08X}",
            self.region, self.kind, self.width, self.addr
        )
    }
}

/// Nothing answered on the bus at the requested address.
/// psx-spx: "Memory Map" / "Memory Exceptions" (Bus Error -> unused regions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusFault;

#[derive(Debug, PartialEq, Eq)]
pub enum BiosError {
    WrongSize { expected: usize, got: usize },
}

impl fmt::Display for BiosError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BiosError::WrongSize { expected, got } => {
                write!(f, "BIOS must be {expected} bytes, got {got}")
            }
        }
    }
}

impl std::error::Error for BiosError {}

pub struct Bus {
    ram: Box<[u8]>,
    bios: Box<[u8]>,
    scratchpad: Box<[u8]>,
    cache_control: u32,
    /// Distinct unhandled accesses, in first-seen order.
    unhandled: Vec<UnhandledAccess>,
    unhandled_seen: HashSet<UnhandledAccess>,
}

impl Bus {
    /// Creates a bus with the given BIOS image (must be exactly 512 KB).
    pub fn new(bios: Vec<u8>) -> Result<Self, BiosError> {
        if bios.len() != map::BIOS_SIZE {
            return Err(BiosError::WrongSize {
                expected: map::BIOS_SIZE,
                got: bios.len(),
            });
        }
        Ok(Self {
            // Real RAM powers up with garbage; zero is deterministic for tests.
            ram: vec![0; map::RAM_SIZE].into_boxed_slice(),
            bios: bios.into_boxed_slice(),
            scratchpad: vec![0; map::SCRATCHPAD_SIZE].into_boxed_slice(),
            cache_control: 0,
            unhandled: Vec::new(),
            unhandled_seen: HashSet::new(),
        })
    }

    /// Distinct accesses that hit unimplemented hardware, first-seen order.
    pub fn unhandled(&self) -> &[UnhandledAccess] {
        &self.unhandled
    }

    pub fn cache_control(&self) -> u32 {
        self.cache_control
    }

    // ---- public CPU-facing API -------------------------------------------

    pub fn load8(&mut self, addr: u32) -> Result<u8, BusFault> {
        self.load(addr, Width::Byte).map(|v| v as u8)
    }

    pub fn load16(&mut self, addr: u32) -> Result<u16, BusFault> {
        debug_assert!(addr.is_multiple_of(2), "unaligned load16 @ 0x{addr:08X}");
        self.load(addr, Width::Half).map(|v| v as u16)
    }

    pub fn load32(&mut self, addr: u32) -> Result<u32, BusFault> {
        debug_assert!(addr.is_multiple_of(4), "unaligned load32 @ 0x{addr:08X}");
        self.load(addr, Width::Word)
    }

    pub fn store8(&mut self, addr: u32, val: u8) -> Result<(), BusFault> {
        self.store(addr, Width::Byte, val as u32)
    }

    pub fn store16(&mut self, addr: u32, val: u16) -> Result<(), BusFault> {
        debug_assert!(addr.is_multiple_of(2), "unaligned store16 @ 0x{addr:08X}");
        self.store(addr, Width::Half, val as u32)
    }

    pub fn store32(&mut self, addr: u32, val: u32) -> Result<(), BusFault> {
        debug_assert!(addr.is_multiple_of(4), "unaligned store32 @ 0x{addr:08X}");
        self.store(addr, Width::Word, val)
    }

    /// Copies bytes straight into RAM, bypassing the CPU.
    /// Used by the harness to sideload PS-EXE files (Phase 2).
    pub fn write_ram(&mut self, phys_offset: u32, data: &[u8]) {
        for (i, b) in data.iter().enumerate() {
            let off = (phys_offset as usize + i) % map::RAM_SIZE;
            self.ram[off] = *b;
        }
    }

    // ---- routing ----------------------------------------------------------

    fn load(&mut self, vaddr: u32, width: Width) -> Result<u32, BusFault> {
        let addr = mask_region(vaddr);

        if let Some(off) = map::RAM.contains(addr) {
            // 2 MB mirrored across 8 MB.
            return Ok(read_le(&self.ram, off as usize % map::RAM_SIZE, width));
        }
        if let Some(off) = map::BIOS.contains(addr) {
            return Ok(read_le(&self.bios, off as usize, width));
        }
        if let Some(off) = map::SCRATCHPAD.contains(addr) {
            if is_kseg1(vaddr) {
                return Err(self.fault(AccessKind::Load, width, addr));
            }
            return Ok(read_le(&self.scratchpad, off as usize, width));
        }
        if map::EXPANSION_1.contains(addr).is_some() {
            // Nothing plugged into the parallel port: open bus reads all ones.
            return Ok(match width {
                Width::Byte => 0xFF,
                Width::Half => 0xFFFF,
                Width::Word => 0xFFFF_FFFF,
            });
        }
        if map::IO_PORTS.contains(addr).is_some() || map::EXPANSION_2.contains(addr).is_some() {
            self.record(Region::Io, AccessKind::Load, width, addr);
            return Ok(0);
        }
        if map::CACHE_CONTROL.contains(addr).is_some() {
            return Ok(self.cache_control);
        }
        Err(self.fault(AccessKind::Load, width, addr))
    }

    fn store(&mut self, vaddr: u32, width: Width, val: u32) -> Result<(), BusFault> {
        let addr = mask_region(vaddr);

        if let Some(off) = map::RAM.contains(addr) {
            write_le(&mut self.ram, off as usize % map::RAM_SIZE, width, val);
            return Ok(());
        }
        if map::BIOS.contains(addr).is_some() {
            // ROM: writes are silently ignored.
            return Ok(());
        }
        if let Some(off) = map::SCRATCHPAD.contains(addr) {
            if is_kseg1(vaddr) {
                return Err(self.fault(AccessKind::Store, width, addr));
            }
            write_le(&mut self.scratchpad, off as usize, width, val);
            return Ok(());
        }
        if map::EXPANSION_1.contains(addr).is_some() {
            return Ok(());
        }
        if map::IO_PORTS.contains(addr).is_some() || map::EXPANSION_2.contains(addr).is_some() {
            self.record(Region::Io, AccessKind::Store, width, addr);
            return Ok(());
        }
        if map::CACHE_CONTROL.contains(addr).is_some() {
            self.cache_control = val;
            return Ok(());
        }
        Err(self.fault(AccessKind::Store, width, addr))
    }

    fn record(&mut self, region: Region, kind: AccessKind, width: Width, addr: u32) {
        let a = UnhandledAccess { region, kind, width, addr };
        if self.unhandled_seen.insert(a) {
            self.unhandled.push(a);
        }
    }

    /// Records an access to an unused region and returns the fault for it.
    fn fault(&mut self, kind: AccessKind, width: Width, addr: u32) -> BusFault {
        self.record(Region::Unmapped, kind, width, addr);
        BusFault
    }
}

#[inline(always)]
fn is_kseg1(vaddr: u32) -> bool {
    vaddr >> 29 == 0b101
}

/// The R3000A on the PS1 is little-endian.
#[inline(always)]
fn read_le(mem: &[u8], off: usize, width: Width) -> u32 {
    let mut v = 0u32;
    for i in 0..width.bytes() {
        v |= (mem[off + i] as u32) << (8 * i);
    }
    v
}

#[inline(always)]
fn write_le(mem: &mut [u8], off: usize, width: Width, val: u32) {
    for i in 0..width.bytes() {
        mem[off + i] = (val >> (8 * i)) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_bios() -> Vec<u8> {
        let mut b = vec![0u8; map::BIOS_SIZE];
        // First instruction of real BIOSes: lui $t0, 0x0013 -> 0x3C080013
        b[0..4].copy_from_slice(&0x3C08_0013u32.to_le_bytes());
        b[map::BIOS_SIZE - 4..].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        b
    }

    fn bus() -> Bus {
        Bus::new(fake_bios()).unwrap()
    }

    #[test]
    fn rejects_wrong_bios_size() {
        assert_eq!(
            Bus::new(vec![0; 1000]).err(),
            Some(BiosError::WrongSize { expected: map::BIOS_SIZE, got: 1000 })
        );
    }

    #[test]
    fn reset_vector_reads_bios() {
        let mut b = bus();
        assert_eq!(b.load32(map::RESET_VECTOR).unwrap(), 0x3C08_0013);
        assert_eq!(b.load32(0xBFC7_FFFC).unwrap(), 0xDEAD_BEEF);
    }

    #[test]
    fn bios_visible_in_all_segments() {
        let mut b = bus();
        for base in [0x1FC0_0000, 0x9FC0_0000, 0xBFC0_0000] {
            assert_eq!(b.load32(base).unwrap(), 0x3C08_0013, "base 0x{base:08X}");
        }
    }

    #[test]
    fn bios_is_read_only() {
        let mut b = bus();
        b.store32(0xBFC0_0000, 0).unwrap();
        assert_eq!(b.load32(0xBFC0_0000).unwrap(), 0x3C08_0013);
    }

    #[test]
    fn ram_is_little_endian() {
        let mut b = bus();
        b.store32(0x100, 0x1122_3344).unwrap();
        assert_eq!(b.load8(0x100).unwrap(), 0x44);
        assert_eq!(b.load8(0x103).unwrap(), 0x11);
        assert_eq!(b.load16(0x100).unwrap(), 0x3344);
        assert_eq!(b.load16(0x102).unwrap(), 0x1122);
    }

    #[test]
    fn ram_segments_alias() {
        let mut b = bus();
        b.store32(0x8000_1000, 0xCAFE_BABE).unwrap();
        assert_eq!(b.load32(0x0000_1000).unwrap(), 0xCAFE_BABE);
        assert_eq!(b.load32(0xA000_1000).unwrap(), 0xCAFE_BABE);
    }

    #[test]
    fn ram_mirrors_every_2mb() {
        let mut b = bus();
        b.store32(0x10, 0x1234_5678).unwrap();
        for mirror in 0..4u32 {
            assert_eq!(b.load32(mirror * 0x20_0000 + 0x10).unwrap(), 0x1234_5678);
        }
    }

    #[test]
    fn scratchpad_works_but_not_through_kseg1() {
        let mut b = bus();
        b.store32(0x1F80_0000, 42).unwrap();
        assert_eq!(b.load32(0x9F80_0000).unwrap(), 42);
        // psx-spx: "Memory Map" — scratchpad is not mirrored in KSEG1.
        assert_eq!(b.load32(0xBF80_0000), Err(BusFault));
        assert_eq!(b.store32(0xBF80_0000, 1), Err(BusFault));
        assert!(b.unhandled().iter().any(|a| a.region == Region::Unmapped));
    }

    #[test]
    fn expansion1_reads_open_bus() {
        let mut b = bus();
        assert_eq!(b.load8(0x1F00_0084).unwrap(), 0xFF);
        assert_eq!(b.load32(0x1F00_0000).unwrap(), 0xFFFF_FFFF);
    }

    #[test]
    fn io_accesses_are_recorded_once() {
        let mut b = bus();
        b.store32(0x1F80_1060, 0xB88).unwrap(); // RAM_SIZE register
        b.store32(0x1F80_1060, 0xB88).unwrap();
        b.load16(0x1F80_1C00).unwrap(); // SPU
        assert_eq!(b.unhandled().len(), 2);
        assert_eq!(b.unhandled()[0].addr, 0x1F80_1060);
        assert_eq!(b.unhandled()[0].kind, AccessKind::Store);
        assert_eq!(b.unhandled()[1].addr, 0x1F80_1C00);
    }

    #[test]
    fn cache_control_roundtrip() {
        let mut b = bus();
        b.store32(0xFFFE_0130, 0x0001_E988).unwrap();
        assert_eq!(b.load32(0xFFFE_0130).unwrap(), 0x0001_E988);
    }

    #[test]
    fn sideload_writes_ram() {
        let mut b = bus();
        b.write_ram(0x1_0000, &[1, 2, 3, 4]);
        assert_eq!(b.load32(0x8001_0000).unwrap(), 0x0403_0201);
    }

    #[test]
    fn unused_regions_raise_bus_faults() {
        // psx-spx: "Memory Map" / "Memory Exceptions".
        let mut b = bus();
        for addr in [0x0080_0000, 0x1FC8_0000, 0x2000_0000, 0x7FFF_FFFC, 0xC000_0000, 0xFFFE_0134] {
            assert_eq!(b.load32(addr), Err(BusFault), "load 0x{addr:08X}");
            assert_eq!(b.store32(addr, 1), Err(BusFault), "store 0x{addr:08X}");
            assert_eq!(b.load8(addr), Err(BusFault), "load8 0x{addr:08X}");
        }
        assert!(b.unhandled().iter().all(|a| a.region == Region::Unmapped));
    }
}
