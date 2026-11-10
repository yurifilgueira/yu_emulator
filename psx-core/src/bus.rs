//! System bus: routes CPU loads/stores to the right device.
//!
//! psx-spx: "Memory Map"
//!
//! RAM, BIOS, scratchpad and expansion regions are real, and so are the
//! memory control registers, the interrupt controller, the timers and the
//! DMA controller. Every other I/O port is stubbed: reads return 0, writes
//! are dropped, and the access is recorded in `Bus::unhandled` so the
//! harness can show exactly which device needs implementing next.
//!
//! psx-spx: "Memory Map" / "Memory Exceptions" — accesses to unused memory
//! regions fail with [`BusFault`], which the CPU turns into a bus error
//! exception (IBE/DBE). They are still recorded in `Bus::unhandled`.
//!
//! Deviation: psx-spx says gaps *inside* the I/O region also raise bus
//! errors. Until the individual devices exist (Phase 2+) the whole I/O window
//! 0x1F80_1000-0x1F80_2FFF and expansion 2 are treated as mapped stubs.

use crate::dma::{self, Dma, SyncMode};
use crate::irq::{Interrupt, Irq};
use crate::map::{self, mask_region};
use crate::timers::Timers;
use crate::video::Video;
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
    /// psx-spx: "Memory Control" — 1F801000h..1F801020h. Stored only; the
    /// access timings and region sizes they configure are not modelled.
    mem_ctrl: [u32; 9],
    /// psx-spx: "Memory Control" — 1F801060h RAM_SIZE. Stored only.
    ram_size: u32,
    pub irq: Irq,
    timers: Timers,
    dma: Dma,
    video: Video,
    /// A GPU linked-list DMA is in progress (one node per `tick`).
    dma_list_active: bool,
    /// CPU cycles consumed by DMA transfers since the last `take_stall`.
    stall: u32,
    /// SPU stub (Phase 7): the register file as plain memory.
    spu_regs: Box<[u8]>,
    /// Words sent to GP0 while the GPU is still a stub (Phase 3).
    gp0_words: u64,
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
            mem_ctrl: [0; 9],
            ram_size: 0,
            irq: Irq::default(),
            timers: Timers::default(),
            dma: Dma::default(),
            video: Video::default(),
            dma_list_active: false,
            stall: 0,
            spu_regs: vec![0; map::SPU.length as usize].into_boxed_slice(),
            gp0_words: 0,
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

    /// Words written to GP0 (directly or by DMA) and dropped by the GPU stub.
    pub fn gp0_words(&self) -> u64 {
        self.gp0_words
    }

    /// Advances the hardware clocked by the system clock by `cycles`.
    pub fn tick(&mut self, cycles: u32) {
        let events = self.video.tick(cycles);
        self.timers.tick(cycles, &events, &mut self.irq);
        // psx-spx: "Interrupts" — IRQ0 VBLANK.
        if events.vblank_started {
            self.irq.request(Interrupt::Vblank);
        }
        if self.dma_list_active {
            self.dma_list_node();
        }
    }

    /// Cycles during which DMA held the bus since the last call. The CPU is
    /// stopped for that long (psx-spx: "CPU Operation during DMA"), so the
    /// caller advances the clock without executing instructions.
    pub fn take_stall(&mut self) -> u32 {
        std::mem::take(&mut self.stall)
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
        if map::EXPANSION_1.contains(addr).is_some() || map::EXPANSION_3.contains(addr).is_some() {
            // Nothing plugged into the parallel port: open bus reads all ones.
            // Expansion 3 is mapped too (it does not bus-error on hardware,
            // see the log of ps1-tests cpu/io-access-bitwidth). What it
            // reads there depends on what was last on the bus; all ones is
            // an approximation.
            return Ok(match width {
                Width::Byte => 0xFF,
                Width::Half => 0xFFFF,
                Width::Word => 0xFFFF_FFFF,
            });
        }
        if map::IO_PORTS.contains(addr).is_some() || map::EXPANSION_2.contains(addr).is_some() {
            return Ok(self.io_load(addr, width));
        }
        if map::CACHE_CONTROL.contains(addr).is_some() {
            return Ok(self.cache_control);
        }
        Err(self.fault(AccessKind::Load, width, addr))
    }

    /// Stores the low `width` bytes of `val`.
    ///
    /// `val` should be the whole 32-bit register being stored even for byte
    /// and halfword stores: psx-spx "Caution - 8/16-bit writes to certain IO
    /// registers" — "all 32 bits of the GPR are placed on the bus", and some
    /// I/O registers latch all of them.
    pub fn store(&mut self, vaddr: u32, width: Width, val: u32) -> Result<(), BusFault> {
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
        if map::EXPANSION_1.contains(addr).is_some() || map::EXPANSION_3.contains(addr).is_some() {
            return Ok(());
        }
        if map::IO_PORTS.contains(addr).is_some() || map::EXPANSION_2.contains(addr).is_some() {
            self.io_store(addr, width, val);
            return Ok(());
        }
        if map::CACHE_CONTROL.contains(addr).is_some() {
            self.cache_control = val;
            return Ok(());
        }
        Err(self.fault(AccessKind::Store, width, addr))
    }

    // ---- I/O ports -------------------------------------------------------

    /// psx-spx: "I/O Map".
    fn io_load(&mut self, addr: u32, width: Width) -> u32 {
        let reg = addr & !3;
        let value = match reg {
            0x1F80_1000..=0x1F80_1020 => self.mem_ctrl[((reg - 0x1F80_1000) / 4) as usize],
            0x1F80_1060 => self.ram_size,
            0x1F80_1070 => self.irq.stat(),
            0x1F80_1074 => self.irq.mask(),
            0x1F80_1080..=0x1F80_10FF => self.dma.read(reg - 0x1F80_1080),
            0x1F80_1100..=0x1F80_112F => self.timers.read(reg - 0x1F80_1100),
            0x1F80_1814 => {
                // GPU stub (Phase 3). psx-spx: "1F801814h - GPUSTAT": report
                // the reset value seen on hardware (14802000h: display
                // disabled, ready for commands, FIFO empty) so software
                // waiting for the GPU to become idle does not hang.
                self.record(Region::Io, AccessKind::Load, width, addr);
                0x1480_2000
            }
            _ => {
                self.record(Region::Io, AccessKind::Load, width, addr);
                if let Some(off) = map::SPU.contains(addr) {
                    return self.spu_load(off, width);
                }
                // Expansion 2 with nothing attached is open bus and reads
                // all ones, like expansion 1 (hardware log of ps1-tests
                // cpu/io-access-bitwidth: EXPANSION2 reads FFh).
                if map::EXPANSION_2.contains(addr).is_some() {
                    return 0xFFFF_FFFF >> (32 - 8 * width.bytes() as u32);
                }
                return 0;
            }
        };
        // Narrow reads see the addressed bytes of the 32-bit register.
        let value = value >> (8 * (addr & 3));
        match width {
            Width::Byte => value & 0xFF,
            Width::Half => value & 0xFFFF,
            Width::Word => value,
        }
    }

    fn io_store(&mut self, addr: u32, width: Width, val: u32) {
        let reg = addr & !3;
        // psx-spx: "Caution - 8/16-bit writes to certain IO registers": a
        // narrow store to these registers "will behave like a 32-bit store,
        // using the register's full value". psx-spx does not say what a
        // narrow store to the middle of a register (addr & 3 != 0) does;
        // there, only the addressed bytes are written and the rest are zero.
        let value = if addr & 3 == 0 {
            val
        } else {
            let mask = match width {
                Width::Byte => 0xFF,
                Width::Half => 0xFFFF,
                Width::Word => 0xFFFF_FFFF,
            };
            (val & mask) << (8 * (addr & 3))
        };
        match reg {
            0x1F80_1000..=0x1F80_1020 => self.mem_ctrl[((reg - 0x1F80_1000) / 4) as usize] = value,
            0x1F80_1060 => self.ram_size = value,
            0x1F80_1070 => self.irq.write_stat(value),
            0x1F80_1074 => self.irq.write_mask(value),
            0x1F80_1080..=0x1F80_10FF => {
                self.dma.write(reg - 0x1F80_1080, value, &mut self.irq);
                self.dma_start_ready_channels();
            }
            0x1F80_1100..=0x1F80_112F => self.timers.write(reg - 0x1F80_1100, value),
            0x1F80_1810 => self.gp0(value),
            0x1F80_1814 => self.gp1(value),
            _ => {
                self.record(Region::Io, AccessKind::Store, width, addr);
                if let Some(off) = map::SPU.contains(addr) {
                    self.spu_store(off, width, val);
                }
            }
        }
    }

    /// GPU stub (Phase 3): only the commands that change the dot clock seen
    /// by timer 0 are interpreted.
    /// psx-spx: "GP1(00h) - Reset GPU", "GP1(08h) - Display mode".
    fn gp1(&mut self, word: u32) {
        match word >> 24 {
            0x00 => self.video.set_display_mode(0),
            0x08 => self.video.set_display_mode(word),
            _ => {}
        }
        self.record(Region::Io, AccessKind::Store, Width::Word, 0x1F80_1814);
    }

    // SPU stub (Phase 7). psx-spx: "SPU I/O Port Summary". The registers
    // only remember what was written, which is enough for software that
    // configures the SPU and reads its settings back; no sound is produced
    // and no SPU RAM transfer or interrupt ever happens.

    /// psx-spx: "0x1f801dae: STATX".
    const SPU_STATX: usize = 0x1AE;
    /// psx-spx: "0x1f801daa: ATTR".
    const SPU_ATTR: usize = 0x1AA;

    fn spu_load(&mut self, off: u32, width: Width) -> u32 {
        let off = off as usize;
        // STATX bits 5-0: "Current SPU Mode (same as ATTR.Bit5-0, but,
        // applied a bit delayed)". Applied at once here. Software waits for
        // this after every ATTR write. All other status bits read zero.
        let mode = self.spu_regs[Self::SPU_ATTR] & 0x3F;
        self.spu_regs[Self::SPU_STATX] = mode;
        self.spu_regs[Self::SPU_STATX + 1] = 0;
        read_le(&self.spu_regs, off, width)
    }

    fn spu_store(&mut self, off: u32, width: Width, val: u32) {
        // The registers are 16 bits wide and a byte store writes the whole
        // halfword (hardware log of ps1-tests cpu/io-access-bitwidth: an
        // 8-bit store of 12345678h to SPUCNT reads back 5678h).
        let (off, width) = match width {
            Width::Byte => (off as usize & !1, Width::Half),
            _ => (off as usize, width),
        };
        write_le(&mut self.spu_regs, off, width, val);
    }

    /// GPU stub (Phase 3): command words are counted and dropped.
    fn gp0(&mut self, _word: u32) {
        self.gp0_words += 1;
        self.record(Region::Io, AccessKind::Store, Width::Word, 0x1F80_1810);
    }

    // ---- DMA transfers ---------------------------------------------------
    //
    // psx-spx: "DMA Channels". Burst and slice transfers complete at once
    // and their duration (1 clk/word, "DMA Transfer Rates") is charged to
    // `stall`; linked lists advance one node per `tick` so that the CPU keeps
    // running between nodes ("CPU operation resumes ... after SyncMode 2
    // list entries") and an endless list cannot hang the emulator.
    // Deviation: chopping (CHCR bit 8) is ignored.

    /// Starts every channel that is enabled, has its start bit set and has
    /// a data request (or is forced with CHCR bit 28).
    fn dma_start_ready_channels(&mut self) {
        for n in 0..7 {
            let channel = self.dma.channels[n];
            if !channel.busy() || !self.dma.enabled(n) {
                continue;
            }
            // DREQ: the OTC channel has none, so it only starts when forced.
            // The GPU stub always requests data. No other device exists yet.
            let dreq = n == dma::CHANNEL_GPU;
            if !(channel.forced() || dreq) {
                continue;
            }
            match (n, channel.sync_mode()) {
                (dma::CHANNEL_OTC, _) => self.dma_otc(),
                (dma::CHANNEL_GPU, SyncMode::LinkedList) => {
                    if !self.dma_list_active {
                        self.dma.begin(n);
                        self.dma_list_active = true;
                    }
                }
                (dma::CHANNEL_GPU, SyncMode::Burst | SyncMode::Slice) => self.dma_gpu_block(),
                // Device not implemented (or reserved sync mode): the
                // transfer stays pending, as if DREQ never came.
                _ => self.record(Region::Io, AccessKind::Store, Width::Word, 0x1F80_1088 + 0x10 * n as u32),
            }
        }
    }

    /// psx-spx: D#_MADR — addresses past the 8 MB RAM window are not
    /// transferred; DICR bit 15 flags "transferring to/from an address
    /// outside of RAM".
    fn dma_read(&mut self, addr: u32) -> Option<u32> {
        let off = map::RAM.contains(addr & 0x00FF_FFFC)?;
        Some(read_le(&self.ram, off as usize % map::RAM_SIZE, Width::Word))
    }

    fn dma_write(&mut self, addr: u32, value: u32) -> bool {
        match map::RAM.contains(addr & 0x00FF_FFFC) {
            Some(off) => {
                write_le(&mut self.ram, off as usize % map::RAM_SIZE, Width::Word, value);
                true
            }
            None => false,
        }
    }

    /// Number of words for a BCR field: "0001h..FFFFh (or 0=10000h)".
    fn dma_count(field: u32) -> u32 {
        match field & 0xFFFF {
            0 => 0x1_0000,
            n => n,
        }
    }

    /// DMA6: "OTC (reverse clear OT)". Writes a linked list of empty nodes
    /// from MADR downwards; the last word is the FFFFFFh end marker.
    fn dma_otc(&mut self) {
        let n = dma::CHANNEL_OTC;
        self.dma.begin(n);
        let channel = self.dma.channels[n];
        let words = Self::dma_count(channel.bcr);
        let mut addr = channel.madr & 0x00FF_FFFC;
        let mut in_ram = true;
        for i in 0..words {
            let next = addr.wrapping_sub(4) & 0x00FF_FFFC;
            let value = if i == words - 1 { 0x00FF_FFFF } else { next };
            in_ram &= self.dma_write(addr, value);
            addr = next;
        }
        // SyncMode 0: "the hardware doesn't update the MADR registers".
        self.stall += words;
        if !in_ram {
            self.dma.bus_error(&mut self.irq);
        }
        self.dma.complete(n, &mut self.irq);
    }

    /// DMA2 in burst or slice mode: image data to/from the GPU.
    fn dma_gpu_block(&mut self) {
        let n = dma::CHANNEL_GPU;
        self.dma.begin(n);
        let channel = self.dma.channels[n];
        let slice = channel.sync_mode() == SyncMode::Slice;
        let words = if slice {
            Self::dma_count(channel.bcr) * Self::dma_count(channel.bcr >> 16)
        } else {
            Self::dma_count(channel.bcr)
        };
        let mut addr = channel.madr & 0x00FF_FFFC;
        let mut in_ram = true;
        for _ in 0..words {
            if channel.chcr & dma::CHCR_FROM_RAM != 0 {
                match self.dma_read(addr) {
                    Some(word) => self.gp0(word),
                    None => in_ram = false,
                }
            } else {
                // GPUREAD: the GPU stub returns zero.
                in_ram &= self.dma_write(addr, 0);
            }
            addr = addr.wrapping_add(channel.step()) & 0x00FF_FFFC;
        }
        if slice {
            // "SyncMode=1 ... the hardware does update MADR (... at transfer
            // end, it'll hold the end-address)" and "decrements BA to zero".
            self.dma.channels[n].madr = addr;
            self.dma.channels[n].bcr &= 0xFFFF;
        }
        self.stall += words;
        if !in_ram {
            self.dma.bus_error(&mut self.irq);
        }
        self.dma.complete(n, &mut self.irq);
    }

    /// DMA2 in linked-list mode: transfers one node.
    /// psx-spx: "Linked List DMA".
    fn dma_list_node(&mut self) {
        let n = dma::CHANNEL_GPU;
        let channel = self.dma.channels[n];
        // Software may stop the transfer by rewriting CHCR or DPCR.
        if !channel.busy() || channel.sync_mode() != SyncMode::LinkedList || !self.dma.enabled(n) {
            self.dma_list_active = false;
            return;
        }
        let addr = channel.madr & 0x00FF_FFFC;
        let Some(header) = self.dma_read(addr) else {
            // "any address above 8MB is invalid ... This will trigger a DMA
            // error, reflected into the DICR register."
            self.dma_list_active = false;
            self.dma.bus_error(&mut self.irq);
            self.dma.complete(n, &mut self.irq);
            return;
        };
        let count = header >> 24;
        for i in 1..=count {
            if let Some(word) = self.dma_read(addr.wrapping_add(4 * i)) {
                self.gp0(word);
            }
        }
        self.stall += count + 1;
        // "at transfer end, [MADR will] hold ... the end marker in SyncMode=2".
        let next = header & 0x00FF_FFFF;
        self.dma.channels[n].madr = next;
        if next == 0x00FF_FFFF {
            self.dma_list_active = false;
            self.dma.complete(n, &mut self.irq);
        }
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
        // Devices that are still stubs: controller port and SPU.
        b.store32(0x1F80_1040, 0xB88).unwrap();
        b.store32(0x1F80_1040, 0xB88).unwrap();
        b.load16(0x1F80_1C00).unwrap(); // SPU
        assert_eq!(b.unhandled().len(), 2);
        assert_eq!(b.unhandled()[0].addr, 0x1F80_1040);
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
        for addr in [0x0080_0000, 0x1FC8_0000, 0x1F90_0000, 0x2000_0000, 0x7FFF_FFFC, 0xC000_0000, 0xFFFE_0134] {
            assert_eq!(b.load32(addr), Err(BusFault), "load 0x{addr:08X}");
            assert_eq!(b.store32(addr, 1), Err(BusFault), "store 0x{addr:08X}");
            assert_eq!(b.load8(addr), Err(BusFault), "load8 0x{addr:08X}");
        }
        assert!(b.unhandled().iter().all(|a| a.region == Region::Unmapped));
    }

    // ---- I/O registers ---------------------------------------------------

    #[test]
    fn memory_control_registers_hold_their_value() {
        let mut b = bus();
        b.store32(0x1F80_1010, 0x0013_243F).unwrap();
        b.store32(0x1F80_1060, 0x0000_0B88).unwrap();
        assert_eq!(b.load32(0x1F80_1010).unwrap(), 0x0013_243F);
        assert_eq!(b.load32(0x1F80_1060).unwrap(), 0x0000_0B88);
        assert!(b.unhandled().is_empty());
    }

    #[test]
    fn interrupt_registers() {
        // psx-spx: "Interrupts".
        let mut b = bus();
        b.store32(0x1F80_1074, 0xFFFF_FFFF).unwrap();
        assert_eq!(b.load32(0x1F80_1074).unwrap(), 0x7FF);
        b.irq.request(Interrupt::Dma);
        assert_eq!(b.load32(0x1F80_1070).unwrap(), 0x8);
        assert!(b.irq.pending());
        b.store32(0x1F80_1070, 0).unwrap();
        assert!(!b.irq.pending());
    }

    #[test]
    fn narrow_io_stores_latch_the_whole_register_value() {
        // psx-spx: "Caution - 8/16-bit writes to certain IO registers";
        // values from the hardware log of ps1-tests cpu/io-access-bitwidth.
        let mut b = bus();
        b.store(0x1F80_10F0, Width::Byte, 0x1234_5678).unwrap();
        assert_eq!(b.load32(0x1F80_10F0).unwrap(), 0x1234_5678);
        b.store(0x1F80_1074, Width::Half, 0x1234_5678).unwrap();
        assert_eq!(b.load16(0x1F80_1074).unwrap(), 0x0678);
        b.store(0x1F80_1108, Width::Byte, 0x1234_5678).unwrap();
        assert_eq!(b.load16(0x1F80_1108).unwrap(), 0x5678);
        // Narrow reads pick the addressed bytes.
        assert_eq!(b.load8(0x1F80_10F1).unwrap(), 0x56);
        assert_eq!(b.load16(0x1F80_10F2).unwrap(), 0x1234);
        // RAM is unaffected by the upper bits.
        b.store(0x100, Width::Byte, 0x1234_5678).unwrap();
        assert_eq!(b.load32(0x100).unwrap(), 0x78);
    }

    #[test]
    fn timers_count_when_the_bus_is_ticked() {
        let mut b = bus();
        b.store32(0x1F80_1108, 50).unwrap();
        b.store32(0x1F80_1104, 0x58).unwrap(); // reset at target, IRQ, repeat
        b.tick(20);
        assert_eq!(b.load32(0x1F80_1100).unwrap(), 20);
        b.tick(30);
        assert_eq!(b.irq.stat(), 1 << 4);
    }

    #[test]
    fn vblank_interrupt_once_per_frame() {
        let mut b = bus();
        let frame = 263 * 3413 * 7 / 11;
        b.tick(frame);
        assert_eq!(b.irq.stat() & 1, 1);
        b.store32(0x1F80_1070, 0).unwrap();
        b.tick(frame / 2);
        assert_eq!(b.irq.stat() & 1, 0);
        b.tick(frame / 2 + 2);
        assert_eq!(b.irq.stat() & 1, 1);
    }

    // ---- DMA -------------------------------------------------------------

    const DPCR: u32 = 0x1F80_10F0;
    const DICR: u32 = 0x1F80_10F4;

    fn otc(b: &mut Bus, madr: u32, words: u32, chcr: u32) {
        b.store32(0x1F80_10E0, madr).unwrap();
        b.store32(0x1F80_10E4, words).unwrap();
        b.store32(0x1F80_10E8, chcr).unwrap();
    }

    #[test]
    fn otc_clears_an_ordering_table_backwards() {
        // psx-spx: "DMA Channels" — DMA6 OTC, started with 11000002h.
        let mut b = bus();
        b.store32(DPCR, 0x0F65_4321).unwrap();
        otc(&mut b, 0x0000_1010, 4, 0x1100_0002);
        assert_eq!(b.load32(0x1010).unwrap(), 0x0000_100C);
        assert_eq!(b.load32(0x100C).unwrap(), 0x0000_1008);
        assert_eq!(b.load32(0x1008).unwrap(), 0x0000_1004);
        assert_eq!(b.load32(0x1004).unwrap(), 0x00FF_FFFF);
        assert_eq!(b.load32(0x1000).unwrap(), 0, "only 4 words written");
        assert_eq!(b.load32(0x1F80_10E8).unwrap(), 0x0000_0002, "start bits cleared");
        assert_eq!(b.load32(0x1F80_10E0).unwrap(), 0x0000_1010, "MADR unchanged");
        assert_eq!(b.take_stall(), 4);
    }

    #[test]
    fn otc_needs_master_enable_start_and_trigger() {
        let mut b = bus();
        b.store32(0x1010, 0xAAAA_AAAA).unwrap();
        // Master enable off (reset DPCR).
        otc(&mut b, 0x1010, 4, 0x1100_0002);
        assert_eq!(b.load32(0x1010).unwrap(), 0xAAAA_AAAA);
        assert_eq!(b.load32(0x1F80_10E8).unwrap(), 0x1100_0002, "still pending");
        // ...enabling it afterwards starts the pending transfer.
        b.store32(DPCR, 0x0F65_4321).unwrap();
        assert_eq!(b.load32(0x1010).unwrap(), 0x0000_100C);

        // Start bit without the trigger bit: OTC has no DREQ, nothing moves.
        b.store32(0x1010, 0xAAAA_AAAA).unwrap();
        otc(&mut b, 0x1010, 4, 0x0100_0002);
        assert_eq!(b.load32(0x1010).unwrap(), 0xAAAA_AAAA);
        // Trigger without the start bit.
        otc(&mut b, 0x1010, 4, 0x1000_0002);
        assert_eq!(b.load32(0x1010).unwrap(), 0xAAAA_AAAA);
    }

    #[test]
    fn dma_completion_interrupt() {
        let mut b = bus();
        b.store32(DPCR, 0x0F65_4321).unwrap();
        b.store32(DICR, (1 << 23) | (1 << 22)).unwrap();
        otc(&mut b, 0x1010, 1, 0x1100_0002);
        assert_eq!(b.load32(DICR).unwrap(), (1 << 31) | (1 << 30) | (1 << 23) | (1 << 22));
        assert_eq!(b.irq.stat(), 1 << 3);
    }

    #[test]
    fn gpu_linked_list_runs_one_node_per_tick() {
        // psx-spx: "Linked List DMA".
        let mut b = bus();
        b.store32(0x2000, 0x0200_3000).unwrap(); // 2 words, next = 0x3000
        b.store32(0x2004, 0x1111_1111).unwrap();
        b.store32(0x2008, 0x2222_2222).unwrap();
        b.store32(0x3000, 0x00FF_FFFF).unwrap(); // empty node, end marker
        b.store32(DPCR, 0x0765_4B21).unwrap();
        b.store32(0x1F80_10A0, 0x2000).unwrap();
        b.store32(0x1F80_10A4, 0).unwrap();
        b.store32(0x1F80_10A8, 0x0100_0401).unwrap();
        assert_eq!(b.gp0_words(), 0, "nothing until the bus is ticked");

        b.tick(1);
        assert_eq!(b.gp0_words(), 2);
        assert_eq!(b.load32(0x1F80_10A0).unwrap(), 0x3000);
        assert_ne!(b.load32(0x1F80_10A8).unwrap() & (1 << 24), 0, "still busy");
        assert_eq!(b.take_stall(), 3);

        b.tick(1);
        assert_eq!(b.load32(0x1F80_10A0).unwrap(), 0x00FF_FFFF);
        assert_eq!(b.load32(0x1F80_10A8).unwrap(), 0x0000_0401, "complete");
    }

    #[test]
    fn self_referencing_list_never_completes_and_can_be_stopped() {
        // ps1-tests dma/chain-looping: the CPU must keep running.
        let mut b = bus();
        b.store32(0x2000, 0x0000_2000).unwrap();
        b.store32(DPCR, 0x0765_4B21).unwrap();
        b.store32(0x1F80_10A0, 0x2000).unwrap();
        b.store32(0x1F80_10A8, 0x0100_0401).unwrap();
        for _ in 0..1000 {
            b.tick(1);
        }
        assert_ne!(b.load32(0x1F80_10A8).unwrap() & (1 << 24), 0);
        b.store32(0x1F80_10A8, 0).unwrap();
        b.tick(1);
        assert_eq!(b.load32(0x1F80_10A8).unwrap(), 0);
    }

    #[test]
    fn list_pointing_outside_ram_raises_dma_bus_error() {
        let mut b = bus();
        b.store32(0x2000, 0x0080_0000).unwrap();
        b.store32(DPCR, 0x0765_4B21).unwrap();
        b.store32(0x1F80_10A0, 0x2000).unwrap();
        b.store32(0x1F80_10A8, 0x0100_0401).unwrap();
        b.tick(1);
        b.tick(1);
        assert_ne!(b.load32(DICR).unwrap() & (1 << 15), 0);
        assert_eq!(b.load32(0x1F80_10A8).unwrap() & (1 << 24), 0);
    }

    #[test]
    fn gpu_slice_transfer_updates_madr_and_block_count() {
        let mut b = bus();
        b.store32(DPCR, 0x0765_4B21).unwrap();
        b.store32(0x1F80_10A0, 0x4000).unwrap();
        b.store32(0x1F80_10A4, (3 << 16) | 4).unwrap(); // 3 blocks of 4 words
        b.store32(0x1F80_10A8, 0x0100_0201).unwrap();
        assert_eq!(b.gp0_words(), 12);
        assert_eq!(b.load32(0x1F80_10A0).unwrap(), 0x4030);
        assert_eq!(b.load32(0x1F80_10A4).unwrap(), 4);
        assert_eq!(b.load32(0x1F80_10A8).unwrap(), 0x0000_0201);
    }

    #[test]
    fn dma_to_missing_device_stays_pending_and_is_reported() {
        let mut b = bus();
        b.store32(DPCR, 0x0765_C321).unwrap(); // enable channel 3 (CD-ROM)
        b.store32(0x1F80_10B8, 0x1100_0000).unwrap();
        assert_eq!(b.load32(0x1F80_10B8).unwrap(), 0x1100_0000);
        assert!(b.unhandled().iter().any(|a| a.addr == 0x1F80_10B8));
    }

    // ---- stubs for later phases ------------------------------------------

    #[test]
    fn spu_registers_remember_writes_and_status_mirrors_mode() {
        let mut b = bus();
        b.store16(0x1F80_1C00, 0x1234).unwrap();
        b.store16(0x1F80_1C02, 0x5678).unwrap();
        assert_eq!(b.load32(0x1F80_1C00).unwrap(), 0x5678_1234);
        // A byte store writes the whole 16-bit register.
        b.store(0x1F80_1DAA, Width::Byte, 0x1234_5678).unwrap();
        assert_eq!(b.load16(0x1F80_1DAA).unwrap(), 0x5678);
        // psx-spx: STATX bits 5-0 follow ATTR bits 5-0.
        b.store16(0x1F80_1DAA, 0xC0A5).unwrap();
        assert_eq!(b.load16(0x1F80_1DAE).unwrap(), 0x0025);
        b.store16(0x1F80_1DAE, 0xFFFF).unwrap();
        assert_eq!(b.load16(0x1F80_1DAE).unwrap(), 0x0025, "status is read-only");
        // Still reported as unimplemented hardware.
        assert!(b.unhandled().iter().any(|a| a.addr == 0x1F80_1DAA));
    }

    #[test]
    fn empty_expansion_regions_read_open_bus() {
        let mut b = bus();
        assert_eq!(b.load8(0x1F80_2000).unwrap(), 0xFF);
        assert_eq!(b.load32(0x1F80_2000).unwrap(), 0xFFFF_FFFF);
        assert_eq!(b.load16(0x1FA0_0000).unwrap(), 0xFFFF);
        b.store32(0x1FA0_0000, 1).unwrap();
        b.store8(0x1F80_2041, 1).unwrap(); // POST
    }

    #[test]
    fn gp1_display_mode_changes_the_dot_clock() {
        let mut b = bus();
        b.store32(0x1F80_1104, 0x100).unwrap(); // timer 0 on dotclock
        b.tick(700); // exactly 1100 video cycles
        assert_eq!(b.load32(0x1F80_1100).unwrap(), 110);
        b.store32(0x1F80_1814, 0x0800_0001).unwrap(); // 320 pixels: /8
        b.store32(0x1F80_1104, 0x100).unwrap();
        b.tick(700);
        assert_eq!(b.load32(0x1F80_1100).unwrap(), 137);
    }
}
