//! Interrupt controller.
//!
//! psx-spx: "Interrupts" — I_STAT (1F801070h) and I_MASK (1F801074h).

/// Interrupt sources, numbered as their I_STAT/I_MASK bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupt {
    Vblank = 0,
    Gpu = 1,
    Cdrom = 2,
    Dma = 3,
    Timer0 = 4,
    Timer1 = 5,
    Timer2 = 6,
    Controller = 7,
    Sio = 8,
    Spu = 9,
    Lightpen = 10,
}

/// Bits 0-10 exist; psx-spx: "11-15 Not used (always zero)". Bits 16-31 are
/// "Garbage" on hardware and read as zero here.
const IMPLEMENTED: u32 = 0x07FF;

#[derive(Debug, Default)]
pub struct Irq {
    stat: u32,
    mask: u32,
}

impl Irq {
    /// Latches an interrupt request. Callers invoke this on the false-to-true
    /// edge of their source (psx-spx: "the interrupt request bits in I_STAT
    /// are edge-triggered").
    pub fn request(&mut self, source: Interrupt) {
        self.stat |= 1 << source as u32;
    }

    pub fn stat(&self) -> u32 {
        self.stat
    }

    pub fn mask(&self) -> u32 {
        self.mask
    }

    /// psx-spx: "Acknowledge: Write I_STAT (0=Clear Bit, 1=No change)".
    pub fn write_stat(&mut self, value: u32) {
        self.stat &= value;
    }

    pub fn write_mask(&mut self, value: u32) {
        self.mask = value & IMPLEMENTED;
    }

    /// State of the line into cop0r13.bit10.
    /// psx-spx: "PSX specific COP0 Notes" — not a latch: it follows
    /// "(I_STAT AND I_MASK) != 0".
    pub fn pending(&self) -> bool {
        self.stat & self.mask != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_mask_acknowledge() {
        let mut irq = Irq::default();
        irq.request(Interrupt::Timer0);
        assert_eq!(irq.stat(), 0x10);
        assert!(!irq.pending(), "masked");
        irq.write_mask(0xFFFF_FFFF);
        assert_eq!(irq.mask(), 0x7FF);
        assert!(irq.pending());
        // Writing 1 leaves a bit alone, writing 0 clears it.
        irq.request(Interrupt::Vblank);
        irq.write_stat(!0x10);
        assert_eq!(irq.stat(), 0x01);
        irq.write_stat(0);
        assert!(!irq.pending());
    }
}
