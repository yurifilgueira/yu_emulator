//! Memory map.
//!
//! psx-spx: "Memory Map"
//!
//! The CPU sees a 4 GB virtual address space split into segments:
//!
//! | Segment | Virtual range           | Notes                         |
//! |---------|-------------------------|-------------------------------|
//! | KUSEG   | 0x0000_0000-0x7FFF_FFFF | cached, user                  |
//! | KSEG0   | 0x8000_0000-0x9FFF_FFFF | cached mirror of KUSEG low 512 MB |
//! | KSEG1   | 0xA000_0000-0xBFFF_FFFF | uncached mirror of KUSEG low 512 MB |
//! | KSEG2   | 0xC000_0000-0xFFFF_FFFF | only cache control lives here |
//!
//! The PS1 has no MMU, so KSEG0/KSEG1 are just the low 512 MB with the top
//! bits stripped.

/// Mask indexed by the top 3 bits of the address (`addr >> 29`).
const REGION_MASK: [u32; 8] = [
    0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF, // KUSEG: 2048 MB
    0x7FFF_FFFF,                                        // KSEG0: 512 MB
    0x1FFF_FFFF,                                        // KSEG1: 512 MB
    0xFFFF_FFFF, 0xFFFF_FFFF,                           // KSEG2: 1024 MB
];

/// Converts a CPU virtual address into a physical one.
#[inline(always)]
pub fn mask_region(addr: u32) -> u32 {
    addr & REGION_MASK[(addr >> 29) as usize]
}

/// A half-open physical address range.
#[derive(Debug, Clone, Copy)]
pub struct Range {
    pub start: u32,
    pub length: u32,
}

impl Range {
    pub const fn new(start: u32, length: u32) -> Self {
        Self { start, length }
    }

    /// Returns the offset inside the range, if `addr` falls in it.
    #[inline(always)]
    pub fn contains(self, addr: u32) -> Option<u32> {
        if addr >= self.start && addr - self.start < self.length {
            Some(addr - self.start)
        } else {
            None
        }
    }
}

/// Size of main RAM actually installed.
pub const RAM_SIZE: usize = 2 * 1024 * 1024;
/// Main RAM. 2 MB physically, mirrored four times across the first 8 MB.
/// psx-spx: "Memory Map" / "Memory Control" (RAM_SIZE register).
pub const RAM: Range = Range::new(0x0000_0000, 8 * 1024 * 1024);

/// Expansion region 1 (parallel port). Nothing connected: reads 0xFF.
pub const EXPANSION_1: Range = Range::new(0x1F00_0000, 8 * 1024 * 1024);

/// Scratchpad: 1 KB of fast data-cache RAM.
/// psx-spx: "Memory Map" — NOT accessible through KSEG1.
pub const SCRATCHPAD_SIZE: usize = 1024;
pub const SCRATCHPAD: Range = Range::new(0x1F80_0000, SCRATCHPAD_SIZE as u32);

/// Hardware I/O ports (memory control, peripherals, interrupts, DMA,
/// timers, CD-ROM, GPU, MDEC, SPU) plus expansion region 2.
pub const IO_PORTS: Range = Range::new(0x1F80_1000, 0x2000);
pub const EXPANSION_2: Range = Range::new(0x1F80_2000, 0x2000);

/// Expansion region 3 ("DEV1 expansion (SRAM BIOS region for DTL cards)").
/// psx-spx: "Memory Map". Nothing connected on a retail console.
pub const EXPANSION_3: Range = Range::new(0x1FA0_0000, 2 * 1024 * 1024);

/// SIO0, the controller and memory card port.
/// psx-spx: "Serial Interfaces (SIO)" (1F801040h..1F80104Fh).
pub const SIO0: Range = Range::new(0x1F80_1040, 0x10);

/// SPU registers. psx-spx: "SPU I/O Port Summary" (1F801C00h..1F801FFFh).
pub const SPU: Range = Range::new(0x1F80_1C00, 0x400);

/// BIOS ROM: 512 KB.
pub const BIOS_SIZE: usize = 512 * 1024;
pub const BIOS: Range = Range::new(0x1FC0_0000, BIOS_SIZE as u32);

/// Cache control register (KSEG2). psx-spx: "Cache Control".
pub const CACHE_CONTROL: Range = Range::new(0xFFFE_0130, 4);

/// Address the CPU starts executing from after reset (KSEG1 BIOS).
pub const RESET_VECTOR: u32 = 0xBFC0_0000;
