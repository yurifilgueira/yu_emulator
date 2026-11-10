//! Sound Processing Unit — so far only its register file and sound RAM
//! transfers. No sound is produced (that is Phase 7).
//!
//! psx-spx: "Sound Processing Unit (SPU)".
//!
//! What exists: registers remember what is written to them, the status
//! register reflects the transfer mode, and data can be moved in and out of
//! the 512 KB sound RAM by manual writes and by DMA. That is what software
//! needs to get past its sound initialisation (the BIOS shell uploads its
//! samples before it draws anything).
//!
//! What does not exist: voices, ADSR, reverb, capture buffers, CD audio,
//! the SPU interrupt, and any transfer timing ("Data Transfer Busy" never
//! reads as busy).

use crate::bus::Width;

/// psx-spx: "Additional Memory" — "512K Sound RAM".
pub const RAM_SIZE: usize = 512 * 1024;
/// psx-spx: "SPU I/O Port Summary" — 1F801C00h..1F801FFFh.
const REGISTER_BYTES: usize = 0x400;

// Register offsets from 1F801C00h.
/// psx-spx: "0x1f801da6: TSA (transfer start address)".
const TSA: usize = 0x1A6;
/// psx-spx: "0x1f801da8: DATA? (data write FIFO)".
const FIFO: usize = 0x1A8;
/// psx-spx: "0x1f801daa: ATTR (common attribute register)".
const ATTR: usize = 0x1AA;
/// psx-spx: "0x1f801dac: RAM_CTRL?".
const RAM_CTRL: usize = 0x1AC;
/// psx-spx: "0x1f801dae: STATX (status register, read-only)".
const STATX: usize = 0x1AE;

/// ATTR bits 5-4: "Sound RAM Transfer Mode (0=Stop, 1=ManualWrite,
/// 2=DMAwrite, 3=DMAread)".
const MODE_MANUAL_WRITE: u16 = 1;
const MODE_DMA_WRITE: u16 = 2;
const MODE_DMA_READ: u16 = 3;
/// "Data (max 32 halfwords)".
const FIFO_CAPACITY: usize = 32;

pub struct Spu {
    /// Register file as plain little-endian memory.
    regs: Box<[u8]>,
    ram: Box<[u8]>,
    /// The internal "current address" register, in bytes.
    address: usize,
    /// Halfwords written to the FIFO, waiting for "Manual Write".
    fifo: Vec<u16>,
}

impl Default for Spu {
    fn default() -> Self {
        Self {
            regs: vec![0; REGISTER_BYTES].into_boxed_slice(),
            ram: vec![0; RAM_SIZE].into_boxed_slice(),
            address: 0,
            fifo: Vec::with_capacity(FIFO_CAPACITY),
        }
    }
}

impl Spu {
    /// Registers whose behaviour is implemented; accesses to the others are
    /// reported by the bus as unhandled.
    pub fn is_implemented(offset: u32) -> bool {
        matches!(offset as usize & !1, TSA | FIFO | ATTR | RAM_CTRL | STATX)
    }

    pub fn ram(&self) -> &[u8] {
        &self.ram
    }

    fn reg16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.regs[offset], self.regs[offset + 1]])
    }

    fn transfer_mode(&self) -> u16 {
        (self.reg16(ATTR) >> 4) & 3
    }

    /// Reads registers; `offset` is relative to 1F801C00h.
    pub fn load(&mut self, offset: u32, width: Width) -> u32 {
        // STATX: bits 5-0 "Current SPU Mode (same as ATTR.Bit5-0, but,
        // applied a bit delayed)" — applied at once here; bit 7 "seems to be
        // same as ATTR.Bit5"; bits 8/9 are the DMA write/read requests.
        // Busy (bit 10), IRQ (bit 6) and capture half (bit 11) read zero.
        let attr = self.reg16(ATTR);
        let mut status = attr & 0x3F;
        status |= ((attr >> 5) & 1) << 7;
        status |= ((self.transfer_mode() == MODE_DMA_WRITE) as u16) << 8;
        status |= ((self.transfer_mode() == MODE_DMA_READ) as u16) << 9;
        self.regs[STATX..STATX + 2].copy_from_slice(&status.to_le_bytes());

        let offset = offset as usize;
        (0..width.bytes()).fold(0, |value, i| value | ((self.regs[offset + i] as u32) << (8 * i)))
    }

    /// Writes registers; `offset` is relative to 1F801C00h and `value` is
    /// the whole register being stored by the CPU.
    pub fn store(&mut self, offset: u32, width: Width, value: u32) {
        // The registers are 16 bits wide and a byte store writes the whole
        // halfword (hardware log of ps1-tests cpu/io-access-bitwidth: an
        // 8-bit store of 12345678h to SPUCNT reads back 5678h).
        let offset = offset as usize & if width == Width::Byte { !1 } else { !0 };
        let bytes = width.bytes().max(2);
        for i in 0..bytes {
            self.regs[offset + i] = (value >> (8 * i)) as u8;
        }
        // A 32-bit store covers two registers.
        for register in (offset..offset + bytes).step_by(2) {
            self.register_written(register);
        }
    }

    fn register_written(&mut self, register: usize) {
        match register {
            // "does additionally store the value (multiplied by 8) in
            // another internal 'current address' register".
            TSA => self.address = self.reg16(TSA) as usize * 8 % RAM_SIZE,
            FIFO => {
                if self.fifo.len() < FIFO_CAPACITY {
                    self.fifo.push(self.reg16(FIFO));
                }
            }
            // "SPU RAM Manual Write": the FIFO contents go to sound RAM
            // when the transfer mode becomes "Manual Write".
            ATTR if self.transfer_mode() == MODE_MANUAL_WRITE => {
                for halfword in std::mem::take(&mut self.fifo) {
                    self.write_ram(halfword);
                }
            }
            _ => {}
        }
    }

    fn write_ram(&mut self, halfword: u16) {
        self.ram[self.address..self.address + 2].copy_from_slice(&halfword.to_le_bytes());
        self.address = (self.address + 2) % RAM_SIZE;
    }

    fn read_ram(&mut self) -> u16 {
        let halfword = u16::from_le_bytes([self.ram[self.address], self.ram[self.address + 1]]);
        self.address = (self.address + 2) % RAM_SIZE;
        halfword
    }

    /// DMA4 data request: the SPU takes or gives data only in the DMA
    /// transfer modes. psx-spx: "SPU RAM DMA-Write" / "SPU RAM DMA-Read".
    pub fn dma_request(&self) -> bool {
        matches!(self.transfer_mode(), MODE_DMA_WRITE | MODE_DMA_READ)
    }

    /// One word arriving by DMA: two halfwords, low one first.
    pub fn dma_write(&mut self, word: u32) {
        self.write_ram(word as u16);
        self.write_ram((word >> 16) as u16);
    }

    /// One word leaving by DMA. Deviation: the FFFFh glitch of "unstable
    /// reading" (DEV4_CTRL bits 24-27 zero) is not reproduced.
    pub fn dma_read(&mut self) -> u32 {
        let low = self.read_ram() as u32;
        low | ((self.read_ram() as u32) << 16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: u32 = 0; // offsets below are relative to 1F801C00h

    fn store16(spu: &mut Spu, offset: usize, value: u16) {
        spu.store(R + offset as u32, Width::Half, value as u32);
    }

    #[test]
    fn registers_remember_writes() {
        let mut spu = Spu::default();
        store16(&mut spu, 0x000, 0x1234);
        store16(&mut spu, 0x002, 0x5678);
        assert_eq!(spu.load(0, Width::Word), 0x5678_1234);
        // A byte store writes the whole 16-bit register.
        spu.store(ATTR as u32, Width::Byte, 0x1234_5600);
        assert_eq!(spu.load(ATTR as u32, Width::Half), 0x5600);
    }

    #[test]
    fn status_mirrors_mode_and_is_read_only() {
        // psx-spx: STATX bits 5-0 follow ATTR bits 5-0.
        let mut spu = Spu::default();
        store16(&mut spu, ATTR, 0xC005);
        assert_eq!(spu.load(STATX as u32, Width::Half), 0x0005);
        store16(&mut spu, STATX, 0xFFFF);
        assert_eq!(spu.load(STATX as u32, Width::Half), 0x0005);
        // DMA write mode: bits 5-4 = 2, plus bit 7 and the write request.
        store16(&mut spu, ATTR, 0x0020);
        assert_eq!(spu.load(STATX as u32, Width::Half), 0x0020 | 0x80 | 0x100);
        store16(&mut spu, ATTR, 0x0030);
        assert_eq!(spu.load(STATX as u32, Width::Half), 0x0030 | 0x80 | 0x200);
    }

    #[test]
    fn manual_write_moves_the_fifo_to_ram() {
        // psx-spx: "SPU RAM Manual Write".
        let mut spu = Spu::default();
        store16(&mut spu, TSA, 0x0200); // address 0x1000
        store16(&mut spu, FIFO, 0xAABB);
        store16(&mut spu, FIFO, 0xCCDD);
        assert_eq!(spu.ram()[0x1000], 0, "nothing until Manual Write is selected");
        store16(&mut spu, ATTR, 0x0010);
        assert_eq!(&spu.ram()[0x1000..0x1004], &[0xBB, 0xAA, 0xDD, 0xCC]);
        assert_eq!(spu.load(TSA as u32, Width::Half), 0x0200, "TSA itself does not increment");
        // The internal address did: the next block continues after it.
        store16(&mut spu, ATTR, 0x0000);
        store16(&mut spu, FIFO, 0x1122);
        store16(&mut spu, ATTR, 0x0010);
        assert_eq!(&spu.ram()[0x1004..0x1006], &[0x22, 0x11]);
    }

    #[test]
    fn dma_write_and_read_back() {
        let mut spu = Spu::default();
        assert!(!spu.dma_request());
        store16(&mut spu, TSA, 0x0001);
        store16(&mut spu, ATTR, 0x0020);
        assert!(spu.dma_request());
        spu.dma_write(0x4433_2211);
        spu.dma_write(0x8877_6655);
        assert_eq!(&spu.ram()[8..16], &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);

        store16(&mut spu, ATTR, 0x0000);
        assert!(!spu.dma_request());
        store16(&mut spu, TSA, 0x0001);
        store16(&mut spu, ATTR, 0x0030);
        assert_eq!(spu.dma_read(), 0x4433_2211);
        assert_eq!(spu.dma_read(), 0x8877_6655);
    }

    #[test]
    fn transfers_wrap_at_the_end_of_ram() {
        let mut spu = Spu::default();
        store16(&mut spu, TSA, 0xFFFF); // 0x7FFF8
        store16(&mut spu, ATTR, 0x0020);
        spu.dma_write(0x2222_1111);
        spu.dma_write(0x4444_3333);
        spu.dma_write(0x6666_5555);
        assert_eq!(&spu.ram()[RAM_SIZE - 2..], &[0x44, 0x44]);
        assert_eq!(&spu.ram()[0..2], &[0x55, 0x55]);
    }
}
