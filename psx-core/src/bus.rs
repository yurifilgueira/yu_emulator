//! System bus: routes CPU loads/stores to the right device.
//!
//! psx-spx: "Memory Map"
//!
//! RAM, BIOS, scratchpad and expansion regions are real, and so are the
//! memory control registers, the interrupt controller, the timers, the DMA
//! controller and the GPU. Every other I/O port is stubbed: reads return 0, writes
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

use crate::cdrom::Cdrom;
use crate::dma::{self, Dma, SyncMode};
use crate::gpu::Gpu;
use crate::irq::{Interrupt, Irq};
use crate::map::{self, mask_region};
use crate::mdec::Mdec;
use crate::sio::Sio0;
use crate::spu::Spu;
use crate::timers::Timers;
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

/// Linked-list nodes transferred between two CPU instructions.
///
/// psx-spx: "CPU Operation during DMA" — during DMA the CPU runs only while
/// it touches nothing but cache, scratchpad and coprocessors; "any read
/// access from RAM or I/O registers ... will stall the CPU until the DMA is
/// finished", and it "resumes during periods when DMA gets interrupted
/// (ie. ... after SyncMode 2 list entries)". Ordinary code reads RAM all
/// the time, so in practice it stands still while a list goes out. That is
/// not modelled access by access; instead the list advances many nodes per
/// instruction, which keeps the CPU from racing a list it has just started.
/// The Sony BIOS shell depends on this: it reuses primitive memory for the
/// next frame right after sending a list, and with one node per instruction
/// (the Phase 2 model) the CPU overtook the list and rewrote packets under
/// it, sending it off to address 0. The CPU still gets one instruction per
/// slice, so an endless list (ps1-tests dma/chain-looping) cannot hang it.
/// Deviation: on hardware that test measures the CPU running at about 60%
/// of its speed, far more than one instruction per 64 nodes.
const LIST_NODES_PER_SLICE: u32 = 64;

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
    pub gpu: Gpu,
    /// A GPU linked-list DMA is in progress (one node per `tick`).
    dma_list_active: bool,
    /// CPU cycles consumed by DMA transfers since the last `take_stall`.
    stall: u32,
    pub spu: Spu,
    /// Controller and memory card port.
    pub sio0: Sio0,
    pub cdrom: Cdrom,
    pub mdec: Mdec,
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
            gpu: Gpu::new(),
            dma_list_active: false,
            stall: 0,
            spu: Spu::default(),
            sio0: Sio0::default(),
            cdrom: Cdrom::default(),
            mdec: Mdec::default(),
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

    /// Words written to GP0, directly or by DMA.
    pub fn gp0_words(&self) -> u64 {
        self.gpu.gp0_words()
    }

    /// Advances the hardware clocked by the system clock by `cycles`.
    pub fn tick(&mut self, cycles: u32) {
        let events = self.gpu.video.tick(cycles);
        self.timers.tick(cycles, &events, &mut self.irq);
        // psx-spx: "Interrupts" — IRQ0 VBLANK.
        if events.vblank_started {
            self.irq.request(Interrupt::Vblank);
        }
        self.sio0.tick(cycles, &mut self.irq);
        self.cdrom.tick(cycles, &mut self.irq);
        // A linked-list transfer in progress takes the bus for a slice of
        // nodes after every instruction (see `LIST_NODES_PER_SLICE`).
        let mut nodes = 0;
        while self.dma_list_active && nodes < LIST_NODES_PER_SLICE {
            self.dma_list_node();
            nodes += 1;
        }
    }

    /// Cycles since the last call during which the CPU was waiting: for DMA
    /// holding the bus (psx-spx: "CPU Operation during DMA") or for its own
    /// memory reads (`read_delay`). The caller advances the clock by that
    /// much without executing instructions.
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

    /// Cycles a CPU read at `vaddr` takes beyond the instruction's own one.
    ///
    /// psx-spx "Memory Control" explains that every region has its own
    /// access time, set by the delay/size registers, with a formula it
    /// calls "fairly complex overall, and difficult to properly describe".
    /// Instead of that formula, these are the times measured on a console
    /// by ps1-tests cpu/access-time (its psx.log, rounded, minus the one
    /// cycle of the instruction), which are the values for the settings
    /// the BIOS programs. Software that reprograms the delays is not
    /// followed. Writes go through the write queue and cost nothing extra
    /// (psx-spx: "Store operations are passed to the write-queue, so they
    /// can execute within a single clock cycle").
    pub fn read_delay(vaddr: u32, width: Width) -> u32 {
        let addr = mask_region(vaddr);
        let by_width = |byte: u32, half: u32, word: u32| match width {
            Width::Byte => byte,
            Width::Half => half,
            Width::Word => word,
        };
        if map::RAM.contains(addr).is_some() {
            4
        } else if map::BIOS.contains(addr).is_some() {
            by_width(7, 12, 24)
        } else if map::SCRATCHPAD.contains(addr).is_some() || map::CACHE_CONTROL.contains(addr).is_some() {
            0
        } else if map::EXPANSION_1.contains(addr).is_some() {
            by_width(6, 13, 25)
        } else if map::EXPANSION_3.contains(addr).is_some() {
            by_width(6, 5, 9)
        } else if map::EXPANSION_2.contains(addr).is_some() {
            by_width(10, 25, 55)
        } else if map::SPU.contains(addr).is_some() {
            by_width(17, 17, 38)
        } else if (0x1F80_1800..0x1F80_1804).contains(&addr) {
            // CD-ROM controller.
            by_width(7, 13, 25)
        } else {
            // The other I/O ports: about 3 cycles in total.
            2
        }
    }

    /// Charges the time of a CPU read to the clock (see `take_stall`).
    pub fn charge_read(&mut self, vaddr: u32, width: Width) {
        self.stall += Self::read_delay(vaddr, width);
    }

    /// Tells whether anything answers at `vaddr`, without accessing it
    /// (no register side effects).
    pub fn probe(&mut self, vaddr: u32) -> Result<(), BusFault> {
        let addr = mask_region(vaddr);
        if map::IO_PORTS.contains(addr).is_some() || map::EXPANSION_2.contains(addr).is_some() {
            return Ok(());
        }
        // Everything else has no read side effects.
        self.load(vaddr, Width::Byte).map(|_| ())
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
        // psx-spx: "Serial Interfaces (SIO)" — SIO0 has 8- and 16-bit
        // registers at 1F801040h..1F80104Fh.
        if let Some(off) = map::SIO0.contains(addr) {
            let value = self.sio0.read(off, width.bytes());
            return value & (0xFFFF_FFFF >> (32 - 8 * width.bytes() as u32));
        }
        // psx-spx: "CDROM Controller I/O Ports" — four byte-wide registers.
        // Deviation: a 16- or 32-bit read returns only the addressed one.
        if (0x1F80_1800..0x1F80_1804).contains(&addr) {
            return self.cdrom.read(addr & 3) as u32;
        }
        let reg = addr & !3;
        let value = match reg {
            0x1F80_1000..=0x1F80_1020 => self.mem_ctrl[((reg - 0x1F80_1000) / 4) as usize],
            0x1F80_1060 => self.ram_size,
            0x1F80_1070 => self.irq.stat(),
            0x1F80_1074 => self.irq.mask(),
            0x1F80_1080..=0x1F80_10FF => self.dma.read(reg - 0x1F80_1080),
            0x1F80_1100..=0x1F80_112F => self.timers.read(reg - 0x1F80_1100),
            // psx-spx: "GPU I/O Ports" — GPUREAD and GPUSTAT.
            0x1F80_1810 => self.gpu.read(),
            0x1F80_1814 => self.gpu.status(),
            // psx-spx: "MDEC I/O Ports".
            0x1F80_1820 => self.mdec.read(),
            0x1F80_1824 => self.mdec.status(),
            _ => {
                if let Some(off) = map::SPU.contains(addr) {
                    if !Spu::is_implemented(off) {
                        self.record(Region::Io, AccessKind::Load, width, addr);
                    }
                    return self.spu.load(off, width);
                }
                self.record(Region::Io, AccessKind::Load, width, addr);
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
        if let Some(off) = map::SIO0.contains(addr) {
            self.sio0.write(off, val);
            // A 32-bit store at 1F801048h covers the mode and the control
            // register.
            if width == Width::Word && off == 0x8 {
                self.sio0.write(0xA, val >> 16);
            }
            return;
        }
        if (0x1F80_1800..0x1F80_1804).contains(&addr) {
            self.cdrom.write(addr & 3, val as u8, &mut self.irq);
            // Requesting the sector buffer raises the DMA3 data request.
            self.dma_start_ready_channels();
            return;
        }
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
            0x1F80_1814 => self.gpu.gp1(value),
            0x1F80_1820 | 0x1F80_1824 => {
                if reg == 0x1F80_1820 {
                    self.mdec.write(value);
                } else {
                    self.mdec.write_control(value);
                }
                // Decoded data or an enabled request may start a waiting
                // DMA0 or DMA1 transfer.
                self.dma_start_ready_channels();
            }
            _ => {
                if let Some(off) = map::SPU.contains(addr) {
                    if !Spu::is_implemented(off) {
                        self.record(Region::Io, AccessKind::Store, width, addr);
                    }
                    self.spu.store(off, width, val);
                    // Selecting a DMA transfer mode raises the SPU's data
                    // request, which may start a waiting DMA4 transfer.
                    self.dma_start_ready_channels();
                    return;
                }
                self.record(Region::Io, AccessKind::Store, width, addr);
            }
        }
    }

    /// Sends a word to GP0 and forwards a GP0(1Fh) interrupt request.
    /// psx-spx: "Interrupts" — IRQ1 GPU.
    fn gp0(&mut self, word: u32) {
        self.gpu.gp0(word);
        if self.gpu.take_irq() {
            self.irq.request(Interrupt::Gpu);
        }
    }

    // ---- DMA transfers ---------------------------------------------------
    //
    // psx-spx: "DMA Channels". Burst and slice transfers complete at once
    // and their duration (1 clk/word, "DMA Transfer Rates") is charged to
    // `stall`; linked lists advance a slice of nodes per `tick`
    // (`LIST_NODES_PER_SLICE`), each charged the same way.
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
            // The GPU executes commands instantly, so it is always ready
            // for data (deviation: GP1(04h) "DMA Direction" is not checked).
            // The SPU requests data while a DMA transfer mode is selected.
            // The CD-ROM requests while its sector buffer has bytes, the
            // MDEC as its control register and output queue say.
            let dreq = match n {
                dma::CHANNEL_MDEC_IN => self.mdec.dma_in_request(),
                dma::CHANNEL_MDEC_OUT => self.mdec.dma_out_request(Self::dma_words(&channel) as usize),
                dma::CHANNEL_GPU => true,
                dma::CHANNEL_CDROM => self.cdrom.data_ready(),
                dma::CHANNEL_SPU => self.spu.dma_request(),
                _ => false,
            };
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
                (
                    dma::CHANNEL_MDEC_IN | dma::CHANNEL_MDEC_OUT | dma::CHANNEL_GPU | dma::CHANNEL_SPU,
                    SyncMode::Burst | SyncMode::Slice,
                ) => self.dma_block(n),
                (dma::CHANNEL_CDROM, SyncMode::Burst) => self.dma_block(n),
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

    /// Words a burst or slice transfer moves: BCR's word count, or in
    /// slice mode block size times number of blocks.
    fn dma_words(channel: &dma::Channel) -> u32 {
        if channel.sync_mode() == SyncMode::Slice {
            Self::dma_count(channel.bcr) * Self::dma_count(channel.bcr >> 16)
        } else {
            Self::dma_count(channel.bcr)
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

    /// Burst or slice transfer between RAM and a device: image data to/from
    /// the GPU (DMA2) or sound RAM data to/from the SPU (DMA4).
    fn dma_block(&mut self, n: usize) {
        self.dma.begin(n);
        let channel = self.dma.channels[n];
        let slice = channel.sync_mode() == SyncMode::Slice;
        let words = Self::dma_words(&channel);
        let mut addr = channel.madr & 0x00FF_FFFC;
        let mut in_ram = true;
        for _ in 0..words {
            if channel.chcr & dma::CHCR_FROM_RAM != 0 {
                match (self.dma_read(addr), n) {
                    (Some(word), dma::CHANNEL_GPU) => self.gp0(word),
                    (Some(word), dma::CHANNEL_SPU) => self.spu.dma_write(word),
                    (Some(word), dma::CHANNEL_MDEC_IN) => self.mdec.write(word),
                    // The CD-ROM channel only transfers to RAM.
                    (Some(_), _) => {}
                    (None, _) => in_ram = false,
                }
            } else {
                // "DMA2 is equivalent to accessing Port 1F801810h".
                let word = match n {
                    dma::CHANNEL_GPU => self.gpu.read(),
                    dma::CHANNEL_SPU => self.spu.dma_read(),
                    dma::CHANNEL_MDEC_OUT => self.mdec.read(),
                    // DMA3 reads the sector buffer, as RDDATA does.
                    _ => u32::from_le_bytes(std::array::from_fn(|_| self.cdrom.read_data())),
                };
                in_ram &= self.dma_write(addr, word);
            }
            addr = addr.wrapping_add(channel.step()) & 0x00FF_FFFC;
        }
        if slice {
            // "SyncMode=1 ... the hardware does update MADR (... at transfer
            // end, it'll hold the end-address)" and "decrements BA to zero".
            self.dma.channels[n].madr = addr;
            self.dma.channels[n].bcr &= 0xFFFF;
        }
        // psx-spx: "DMA Transfer Rates" — GPU 1 clk/word, SPU 4 clks/word,
        // CDROM 24 clks/word with the BIOS's setting (40 with the one most
        // games use; the Memory Control register that selects it is not
        // modelled, so 24 is used for all).
        self.stall += words
            * match n {
                dma::CHANNEL_SPU => 4,
                dma::CHANNEL_CDROM => 24,
                _ => 1,
            };
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
        // Devices that are still missing or stubs: SIO1 and SPU voices.
        b.store32(0x1F80_1050, 0xB88).unwrap();
        b.store32(0x1F80_1050, 0xB88).unwrap();
        b.load16(0x1F80_1C00).unwrap(); // SPU
        assert_eq!(b.unhandled().len(), 2);
        assert_eq!(b.unhandled()[0].addr, 0x1F80_1050);
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
    fn gpu_linked_list_is_sent_when_the_bus_is_ticked() {
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
        assert_ne!(b.load32(0x1F80_10A8).unwrap() & (1 << 24), 0, "busy");

        b.tick(1);
        assert_eq!(b.gp0_words(), 2);
        assert_eq!(b.load32(0x1F80_10A0).unwrap(), 0x00FF_FFFF, "MADR holds the end marker");
        assert_eq!(b.load32(0x1F80_10A8).unwrap(), 0x0000_0401, "complete");
        assert_eq!(b.take_stall(), 4, "one cycle per word, headers included");
    }

    #[test]
    fn long_lists_go_out_in_slices_between_instructions() {
        // A chain of 100 empty nodes: 64 on the first tick, the rest on the
        // second (LIST_NODES_PER_SLICE).
        let mut b = bus();
        for i in 0..100u32 {
            let next = if i == 99 { 0x00FF_FFFF } else { 0x2000 + 4 * (i + 1) };
            b.store32(0x2000 + 4 * i, next).unwrap();
        }
        b.store32(DPCR, 0x0765_4B21).unwrap();
        b.store32(0x1F80_10A0, 0x2000).unwrap();
        b.store32(0x1F80_10A8, 0x0100_0401).unwrap();
        b.tick(1);
        assert_eq!(b.load32(0x1F80_10A0).unwrap(), 0x2000 + 4 * 64);
        assert_ne!(b.load32(0x1F80_10A8).unwrap() & (1 << 24), 0, "still busy");
        b.tick(1);
        assert_eq!(b.load32(0x1F80_10A8).unwrap() & (1 << 24), 0);
        assert_eq!(b.take_stall(), 100);
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
        b.store32(DPCR, 0x07E5_4321).unwrap(); // enable channel 5 (PIO)
        b.store32(0x1F80_10D8, 0x1100_0000).unwrap();
        assert_eq!(b.load32(0x1F80_10D8).unwrap(), 0x1100_0000);
        assert!(b.unhandled().iter().any(|a| a.addr == 0x1F80_10D8));
    }

    #[test]
    fn dma3_copies_a_cd_sector_to_ram() {
        // psx-spx: "DMA Channels" — DMA3 CDROM, CHCR 11000000h, BCR = words;
        // "CDROM Controller I/O Ports" — the sector is requested with BFRD.
        use crate::disc::{tests::data_track, Disc};
        let mut b = bus();
        b.cdrom.insert(Some(Disc::from_data_track(data_track(40)).unwrap()));
        let command = |b: &mut Bus, command: u8, parameters: &[u8]| {
            b.store8(0x1F80_1800, 0).unwrap();
            for p in parameters {
                b.store8(0x1F80_1802, *p).unwrap();
            }
            b.store8(0x1F80_1801, command).unwrap();
            b.store8(0x1F80_1800, 1).unwrap();
            while b.load8(0x1F80_1803).unwrap() & 7 == 0 {
                b.tick(1000);
            }
            b.store8(0x1F80_1803, 0x07).unwrap();
        };
        b.store8(0x1F80_1800, 1).unwrap();
        b.store8(0x1F80_1802, 0x1F).unwrap();
        command(&mut b, 0x0E, &[0x80]); // Setmode: double speed, 800h bytes
        command(&mut b, 0x02, &[0x00, 0x02, 0x33]); // Setloc 00:02:33 = sector 33
        command(&mut b, 0x06, &[]); // ReadN
        assert_eq!(b.irq.stat() & 4, 4, "IRQ2 was requested");
        // Wait for INT1.
        b.store8(0x1F80_1800, 1).unwrap();
        while b.load8(0x1F80_1803).unwrap() & 7 != 1 {
            b.tick(1000);
        }
        b.store8(0x1F80_1800, 0).unwrap();
        b.store8(0x1F80_1803, 0x80).unwrap();
        assert_eq!(b.load8(0x1F80_1800).unwrap() & 0x40, 0x40, "DRQSTS");

        b.store32(DPCR, 0x0765_C321).unwrap();
        b.store32(0x1F80_10B0, 0x0001_0000).unwrap();
        b.store32(0x1F80_10B4, 0x200).unwrap();
        b.take_stall();
        b.store32(0x1F80_10B8, 0x1100_0000).unwrap();
        assert_eq!(b.load32(0x8001_0000).unwrap(), 0x2121_2121);
        assert_eq!(b.load32(0x8001_07FC).unwrap(), 0x2121_2121);
        assert_eq!(b.load32(0x8001_0800).unwrap(), 0, "only 200h words");
        assert_eq!(b.load32(0x1F80_10B8).unwrap() & 0x0100_0000, 0, "finished");
        assert_eq!(b.take_stall(), 0x200 * 24, "24 clks/word");
        assert_eq!(b.load8(0x1F80_1800).unwrap() & 0x40, 0, "buffer empty");
    }

    #[test]
    fn mdec_decodes_through_dma0_and_dma1() {
        // psx-spx "MDEC I/O Ports" — "DMA0 (MDEC.In) to send compressed
        // data from RAM to MDEC", "DMA1 (MDEC.Out) to send uncompressed
        // macroblocks from MDEC to RAM".
        let mut b = bus();
        b.store32(0x1F80_1824, 0x8000_0000).unwrap();
        b.store32(0x1F80_1824, 0x6000_0000).unwrap();
        assert_eq!(b.load32(0x1F80_1824).unwrap(), 0x9004_0000);
        for word in crate::mdec::tests::setup_words(2, 2) {
            b.store32(0x1F80_1820, word).unwrap();
        }
        // One grey 15-bit macroblock: Cr and Cb with DC 0, four Y blocks
        // with DC 100 (scale 1), each followed by the end code. Padded with
        // FE00h to 20h words, as psx-spx recommends.
        let mut words = vec![0xFE00_0400u32, 0xFE00_0400, 0xFE00_0464, 0xFE00_0464, 0xFE00_0464, 0xFE00_0464];
        words.resize(0x20, 0xFE00_FE00);
        for (i, word) in words.iter().enumerate() {
            b.store32(0x8001_0000 + 4 * i as u32, *word).unwrap();
        }
        b.store32(DPCR, 0x0765_43BB).unwrap(); // enable channels 0 and 1

        // The output transfer is started first and waits for data.
        b.store32(0x1F80_1090, 0x0002_0000).unwrap();
        b.store32(0x1F80_1094, 0x0004_0020).unwrap(); // 4 blocks of 20h words
        b.store32(0x1F80_1098, 0x0100_0200).unwrap();
        assert_ne!(b.load32(0x1F80_1098).unwrap() & 0x0100_0000, 0, "waiting");

        b.store32(0x1F80_1820, 0x3800_0020).unwrap(); // decode, 20h words
        b.store32(0x1F80_1080, 0x0001_0000).unwrap();
        b.store32(0x1F80_1084, 0x0001_0020).unwrap();
        b.store32(0x1F80_1088, 0x0100_0201).unwrap();
        assert_eq!(b.load32(0x1F80_1088).unwrap() & 0x0100_0000, 0, "input done");
        assert_eq!(b.load32(0x1F80_1098).unwrap() & 0x0100_0000, 0, "output done");
        assert_eq!(b.load32(0x8002_0000).unwrap(), 0x4E73_4E73);
        assert_eq!(b.load32(0x8002_01FC).unwrap(), 0x4E73_4E73);
        assert_eq!(b.load32(0x8002_0200).unwrap(), 0);
        assert_eq!(b.load32(0x1F80_1824).unwrap() & 0xA000_0000, 0x8000_0000, "idle, output empty");
    }

    // ---- stubs for later phases ------------------------------------------

    #[test]
    fn spu_registers_are_reachable_and_unimplemented_ones_reported() {
        let mut b = bus();
        b.store16(0x1F80_1C00, 0x1234).unwrap(); // voice 0 volume: no voices yet
        assert_eq!(b.load16(0x1F80_1C00).unwrap(), 0x1234);
        assert!(b.unhandled().iter().any(|a| a.addr == 0x1F80_1C00));
        // The transfer registers are implemented and not reported.
        b.store(0x1F80_1DAA, Width::Byte, 0x1234_5625).unwrap();
        assert_eq!(b.load16(0x1F80_1DAA).unwrap(), 0x5625);
        assert_eq!(b.load16(0x1F80_1DAE).unwrap() & 0x3F, 0x25);
        assert!(!b.unhandled().iter().any(|a| a.addr == 0x1F80_1DAA || a.addr == 0x1F80_1DAE));
    }

    #[test]
    fn dma4_uploads_to_sound_ram_once_the_spu_requests_data() {
        // psx-spx: "SPU RAM DMA-Write".
        let mut b = bus();
        b.store32(0x6000, 0x4433_2211).unwrap();
        b.store32(0x6004, 0x8877_6655).unwrap();
        b.store32(DPCR, 0x076D_4321).unwrap(); // enable channel 4
        b.store16(0x1F80_1DA6, 0x0002).unwrap(); // sound RAM address 0x10
        b.store32(0x1F80_10C0, 0x6000).unwrap();
        b.store32(0x1F80_10C4, (2 << 16) | 1).unwrap(); // 2 blocks of 1 word
        b.store32(0x1F80_10C8, 0x0100_0201).unwrap();
        assert_ne!(b.load32(0x1F80_10C8).unwrap() & (1 << 24), 0, "waits: SPU transfer mode is Stop");
        b.store16(0x1F80_1DAA, 0x0020).unwrap(); // DMA write
        assert_eq!(b.load32(0x1F80_10C8).unwrap() & (1 << 24), 0, "completed");
        assert_eq!(&b.spu.ram()[0x10..0x18], &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
        assert_eq!(b.take_stall(), 8, "4 clks per word");
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
    fn gpu_ports_reach_the_gpu() {
        let mut b = bus();
        assert_eq!(b.load32(0x1F80_1814).unwrap() & 0x7FFF_FFFF, 0x1480_2000);
        // Fill 16x1 at (0,0) with white, then read it back through GPUREAD.
        for word in [0x02FF_FFFF, 0, (1 << 16) | 16, 0xC000_0000, 0, (1 << 16) | 2] {
            b.store32(0x1F80_1810, word).unwrap();
        }
        assert_eq!(b.load32(0x1F80_1810).unwrap(), 0x7FFF_7FFF);
        assert!(b.unhandled().is_empty(), "the GPU is no longer a stub");
        // GP0(1Fh) raises IRQ1.
        b.store32(0x1F80_1810, 0x1F00_0000).unwrap();
        assert_eq!(b.irq.stat(), 1 << 1);
    }

    #[test]
    fn dma_reads_vram_through_gpuread() {
        let mut b = bus();
        for word in [0x02FF_FFFF, 0, (1 << 16) | 16, 0xC000_0000, 0, (1 << 16) | 4] {
            b.store32(0x1F80_1810, word).unwrap();
        }
        b.store32(DPCR, 0x0765_4B21).unwrap();
        b.store32(0x1F80_10A0, 0x5000).unwrap();
        b.store32(0x1F80_10A4, (1 << 16) | 2).unwrap();
        b.store32(0x1F80_10A8, 0x0100_0200).unwrap(); // device to RAM
        assert_eq!(b.load32(0x5000).unwrap(), 0x7FFF_7FFF);
        assert_eq!(b.load32(0x5004).unwrap(), 0x7FFF_7FFF);
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

    #[test]
    fn controller_port_is_reachable_through_the_bus() {
        use crate::sio::{button, DigitalPad};
        let mut b = bus();
        b.sio0.ports[0].pad = Some(DigitalPad { pressed: button::UP });
        b.store16(0x1F80_1048, 0x000D).unwrap(); // mode
        b.store16(0x1F80_104E, 0x0088).unwrap(); // baud
        b.store16(0x1F80_104A, 0x1003).unwrap(); // select port 1, TX, /ACK IRQ
        let mut replies = Vec::new();
        for byte in [0x01u8, 0x42, 0x00, 0x00, 0x00] {
            b.store8(0x1F80_1040, byte).unwrap();
            b.tick(2000);
            assert_ne!(b.load16(0x1F80_1044).unwrap() & 2, 0, "RX ready");
            replies.push(b.load8(0x1F80_1040).unwrap());
        }
        assert_eq!(replies, [0xFF, 0x41, 0x5A, 0xEF, 0xFF]);
        assert_ne!(b.irq.stat() & (1 << 7), 0, "IRQ7 from /ACK");
        assert!(b.unhandled().is_empty());
    }

    #[test]
    fn read_delays_follow_the_console_measurements() {
        // ps1-tests cpu/access-time psx.log: RAM 5.2, BIOS 7.6/12.9/24.9,
        // scratchpad ~1, I/O ~3, expansion 2 11/26/56, SPUCNT 18/18/39,
        // CD-ROM 8/14/26 cycles per load instruction.
        use Width::{Byte, Half, Word};
        assert_eq!(Bus::read_delay(0x8000_0000, Word), 4);
        assert_eq!(Bus::read_delay(0x0000_1000, Byte), 4);
        assert_eq!([Bus::read_delay(0xBFC0_0000, Byte), Bus::read_delay(0xBFC0_0000, Half), Bus::read_delay(0xBFC0_0000, Word)], [7, 12, 24]);
        assert_eq!(Bus::read_delay(0x1F80_0000, Word), 0);
        assert_eq!(Bus::read_delay(0xFFFE_0130, Word), 0);
        assert_eq!(Bus::read_delay(0x1F80_1070, Word), 2);
        assert_eq!(Bus::read_delay(0x1F80_1814, Word), 2);
        assert_eq!(Bus::read_delay(0x1F80_2000, Word), 55);
        assert_eq!(Bus::read_delay(0x1F80_1DAA, Half), 17);
        assert_eq!(Bus::read_delay(0x1F80_1800, Byte), 7);
        let mut b = bus();
        b.charge_read(0x8000_0000, Word);
        b.charge_read(0xBFC0_0000, Word);
        assert_eq!(b.take_stall(), 28);
    }
}
