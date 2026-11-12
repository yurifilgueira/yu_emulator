//! DMA controller registers and interrupt logic.
//!
//! psx-spx: "DMA Channels". This module owns the register file; the bus
//! performs the actual transfers because it owns RAM and the devices.

use crate::irq::{Interrupt, Irq};

pub const CHANNEL_MDEC_IN: usize = 0;
pub const CHANNEL_MDEC_OUT: usize = 1;
pub const CHANNEL_GPU: usize = 2;
pub const CHANNEL_CDROM: usize = 3;
pub const CHANNEL_SPU: usize = 4;
pub const CHANNEL_OTC: usize = 6;

// psx-spx: "1F801088h+N*10h - D#_CHCR".
pub const CHCR_FROM_RAM: u32 = 1 << 0;
pub const CHCR_DECREMENT: u32 = 1 << 1;
const CHCR_BUSY: u32 = 1 << 24;
const CHCR_FORCE: u32 = 1 << 28;
/// Bits that exist: 0, 1, 8, 9-10, 16-18, 20-22, 24, 28, 29, 30.
const CHCR_WRITABLE: u32 = 0x7177_0703;
/// "D6_CHCR has only three read/write-able bits: 24,28,30. All other bits
/// are read-only: bit 1 is always 1 (increment=-4), and the other bits are
/// always 0."
const CHCR_OTC_WRITABLE: u32 = 0x5100_0000;

// psx-spx: "1F8010F4h - DICR".
const DICR_WRITABLE: u32 = 0x00FF_803F;
const DICR_BUS_ERROR: u32 = 1 << 15;
const DICR_MASTER_ENABLE: u32 = 1 << 23;
const DICR_FLAGS: u32 = 0x7F00_0000;
const DICR_MASTER_FLAG: u32 = 1 << 31;

/// Transfer mode, CHCR bits 9-10.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    Burst,
    Slice,
    LinkedList,
    Reserved,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Channel {
    pub madr: u32,
    pub bcr: u32,
    pub chcr: u32,
}

impl Channel {
    pub fn sync_mode(&self) -> SyncMode {
        match (self.chcr >> 9) & 3 {
            0 => SyncMode::Burst,
            1 => SyncMode::Slice,
            2 => SyncMode::LinkedList,
            _ => SyncMode::Reserved,
        }
    }

    pub fn busy(&self) -> bool {
        self.chcr & CHCR_BUSY != 0
    }

    /// Bit 28: "Force transfer start without waiting for DREQ".
    pub fn forced(&self) -> bool {
        self.chcr & CHCR_FORCE != 0
    }

    /// MADR step per word: "0=+4, 1=-4".
    pub fn step(&self) -> u32 {
        if self.chcr & CHCR_DECREMENT != 0 {
            4u32.wrapping_neg()
        } else {
            4
        }
    }
}

#[derive(Debug)]
pub struct Dma {
    pub channels: [Channel; 7],
    dpcr: u32,
    dicr: u32,
}

impl Default for Dma {
    fn default() -> Self {
        let mut channels = [Channel::default(); 7];
        channels[CHANNEL_OTC].chcr = CHCR_DECREMENT;
        // psx-spx: DPCR "Initial value on reset is 07654321h".
        Self { channels, dpcr: 0x0765_4321, dicr: 0 }
    }
}

impl Dma {
    /// psx-spx: DPCR bit 3+4n is the channel's master enable.
    pub fn enabled(&self, channel: usize) -> bool {
        self.dpcr & (8 << (channel * 4)) != 0
    }

    /// Reads a register; `offset` is relative to 1F801080h, word-aligned.
    pub fn read(&self, offset: u32) -> u32 {
        let channel = (offset >> 4) as usize;
        match (channel, offset & 0xC) {
            (0..=6, 0x0) => self.channels[channel].madr,
            (0..=6, 0x4) => self.channels[channel].bcr,
            // psx-spx: "Memory Mirrors" — the control registers at 1F8010x8h
            // are mirrored to 1F8010xCh.
            (0..=6, _) => self.channels[channel].chcr,
            (7, 0x0) => self.dpcr,
            (7, 0x4) => self.dicr,
            // 1F8010F8h/FCh: "strange read-only values"; not emulated.
            _ => 0,
        }
    }

    /// Writes a register; `offset` is relative to 1F801080h, word-aligned.
    pub fn write(&mut self, offset: u32, value: u32, irq: &mut Irq) {
        let channel = (offset >> 4) as usize;
        match (channel, offset & 0xC) {
            (0..=6, 0x0) => self.channels[channel].madr = value & 0x00FF_FFFF,
            (0..=6, 0x4) => self.channels[channel].bcr = value,
            (CHANNEL_OTC, _) => {
                self.channels[channel].chcr = (value & CHCR_OTC_WRITABLE) | CHCR_DECREMENT;
            }
            (0..=6, _) => self.channels[channel].chcr = value & CHCR_WRITABLE,
            (7, 0x0) => self.dpcr = value,
            (7, 0x4) => {
                // Bits 24-30 are acknowledged by writing 1; bit 31 is
                // "recalculated on every write to DICR".
                let flags = self.dicr & DICR_FLAGS & !value;
                self.dicr = (self.dicr & DICR_MASTER_FLAG) | (value & DICR_WRITABLE) | flags;
                self.update_master_flag(irq);
            }
            _ => {}
        }
    }

    /// Marks a transfer as started: "Bit 28 is automatically cleared upon
    /// BEGIN of the transfer".
    pub fn begin(&mut self, channel: usize) {
        self.channels[channel].chcr &= !CHCR_FORCE;
    }

    /// Marks a transfer as complete: "Bit 24 is automatically cleared upon
    /// COMPLETION of the transfer", and the completion flag is set "ONLY if
    /// BOTH bit (16+n) AND bit 23 are enabled".
    ///
    /// Deviation: DICR bits 0-6 (interrupt on every slice / list node) are
    /// stored but ignored; only whole-transfer completion raises a flag.
    pub fn complete(&mut self, channel: usize, irq: &mut Irq) {
        self.channels[channel].chcr &= !(CHCR_BUSY | CHCR_FORCE);
        if self.dicr & DICR_MASTER_ENABLE != 0 && self.dicr & (1 << (16 + channel)) != 0 {
            self.dicr |= 1 << (24 + channel);
        }
        self.update_master_flag(irq);
    }

    /// psx-spx: DICR bit 15, "Raised when transferring to/from an address
    /// outside of RAM. Forces bit 31."
    pub fn bus_error(&mut self, irq: &mut Irq) {
        self.dicr |= DICR_BUS_ERROR;
        self.update_master_flag(irq);
    }

    /// "IF b15=1 OR (b23=1 AND b(24-30)>0) THEN b31=1 ELSE b31=0", and
    /// "Upon 0-to-1 transition of Bit 31, the IRQ3 flag in I_STAT gets set".
    fn update_master_flag(&mut self, irq: &mut Irq) {
        let was_set = self.dicr & DICR_MASTER_FLAG != 0;
        let set = self.dicr & DICR_BUS_ERROR != 0
            || (self.dicr & DICR_MASTER_ENABLE != 0 && self.dicr & DICR_FLAGS != 0);
        if set {
            self.dicr |= DICR_MASTER_FLAG;
            if !was_set {
                irq.request(Interrupt::Dma);
            }
        } else {
            self.dicr &= !DICR_MASTER_FLAG;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DPCR: u32 = 0x70;
    const DICR: u32 = 0x74;

    #[test]
    fn reset_values() {
        let dma = Dma::default();
        assert_eq!(dma.read(DPCR), 0x0765_4321);
        assert_eq!(dma.read(0x68), 0x0000_0002, "OTC CHCR bit 1 is always 1");
        assert!(!dma.enabled(6));
    }

    #[test]
    fn register_masks() {
        // Values from the hardware log of ps1-tests cpu/io-access-bitwidth.
        let (mut dma, mut irq) = (Dma::default(), Irq::default());
        dma.write(0x00, 0x1234_5678, &mut irq);
        assert_eq!(dma.read(0x00), 0x0034_5678);
        dma.write(DPCR, 0x1234_5678, &mut irq);
        assert_eq!(dma.read(DPCR), 0x1234_5678);
        dma.write(DICR, 0x1234_5678, &mut irq);
        assert_eq!(dma.read(DICR), 0x0034_0038);
        // OTC control: only bits 24, 28 and 30 are writable.
        dma.write(0x68, 0xFFFF_FFFF, &mut irq);
        assert_eq!(dma.read(0x68), 0x5100_0002);
        dma.write(0x68, 0, &mut irq);
        assert_eq!(dma.read(0x68), 0x0000_0002);
        // Other channels: unused bits read zero; CHCR is mirrored at +0xC.
        dma.write(0x28, 0xFFFF_FFFF, &mut irq);
        assert_eq!(dma.read(0x28), 0x7177_0703);
        assert_eq!(dma.read(0x2C), 0x7177_0703);
    }

    #[test]
    fn completion_flag_needs_channel_and_master_enable() {
        let (mut dma, mut irq) = (Dma::default(), Irq::default());
        dma.channels[6].chcr = 0x1100_0002;
        dma.complete(6, &mut irq);
        assert_eq!(dma.channels[6].chcr, 0x0000_0002, "busy and force cleared");
        assert_eq!(dma.read(DICR), 0);
        assert_eq!(irq.stat(), 0);

        dma.write(DICR, (1 << 23) | (1 << 22), &mut irq);
        dma.complete(6, &mut irq);
        assert_eq!(dma.read(DICR), (1 << 31) | (1 << 30) | (1 << 23) | (1 << 22));
        assert_eq!(irq.stat(), 1 << 3);

        // Acknowledge by writing 1 to the flag; bit 31 follows.
        irq.write_stat(0);
        dma.write(DICR, (1 << 30) | (1 << 23) | (1 << 22), &mut irq);
        assert_eq!(dma.read(DICR), (1 << 23) | (1 << 22));
        assert_eq!(irq.stat(), 0);
    }

    #[test]
    fn master_flag_edge_requests_irq_once() {
        let (mut dma, mut irq) = (Dma::default(), Irq::default());
        dma.write(DICR, (1 << 23) | (1 << 22) | (1 << 18), &mut irq);
        dma.complete(6, &mut irq);
        irq.write_stat(0);
        dma.complete(2, &mut irq);
        assert_eq!(irq.stat(), 0, "bit 31 was already set: no new edge");
    }

    #[test]
    fn bus_error_forces_master_flag() {
        let (mut dma, mut irq) = (Dma::default(), Irq::default());
        dma.bus_error(&mut irq);
        assert_eq!(dma.read(DICR), (1 << 31) | (1 << 15));
        assert_eq!(irq.stat(), 1 << 3);
    }
}
