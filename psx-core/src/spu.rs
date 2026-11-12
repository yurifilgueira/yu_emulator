//! Sound Processing Unit.
//!
//! psx-spx: "Sound Processing Unit (SPU)".
//!
//! Emulated: the register file, sound RAM transfers (manual and DMA), the
//! 24 voices (ADPCM, pitch and pitch modulation, Gaussian interpolation,
//! ADSR, noise), voice and main volumes with sweeps, CD audio input, the
//! capture buffers and the SPU interrupt. One stereo sample is produced
//! every 768 system clock cycles (44.1 kHz).
//!
//! Not emulated: reverb, the external (PIO) input, the delay with which
//! hardware applies register writes ("it may take 1/44100 seconds ... until
//! it has actually realized the new value" — key on/off wait for the next
//! sample, everything else is immediate) and transfer timing ("Data
//! Transfer Busy" never reads as busy).

mod envelope;
mod gauss;
mod voice;

use crate::bus::Width;
use crate::irq::{Interrupt, Irq};
use envelope::{Envelope, Rate};
use voice::{Settings, Voice};

/// psx-spx: "Additional Memory" — "512K Sound RAM".
pub const RAM_SIZE: usize = 512 * 1024;
/// psx-spx: "SPU I/O Port Summary" — 1F801C00h..1F801FFFh.
const REGISTER_BYTES: usize = 0x400;
/// System clock cycles per sample: 33868800 Hz / 44100 Hz.
pub const CYCLES_PER_SAMPLE: u32 = 768;
pub const SAMPLE_RATE: u32 = 44_100;
const VOICES: usize = 24;
/// Samples kept for the frontend before old ones are dropped (one second).
const OUTPUT_CAPACITY: usize = SAMPLE_RATE as usize;

// Register offsets from 1F801C00h. Voice N has eight registers at N*10h.
const VOICE_VOLUME_LEFT: usize = 0x0;
const VOICE_VOLUME_RIGHT: usize = 0x2;
const VOICE_PITCH: usize = 0x4;
const VOICE_START: usize = 0x6;
const VOICE_ADSR1: usize = 0x8;
const VOICE_ADSR2: usize = 0xA;
/// psx-spx: "ENVX (voice envelope volume)".
const VOICE_ENVX: usize = 0xC;
/// psx-spx: "LSAX (loop start address)".
const VOICE_REPEAT: usize = 0xE;
const VOICE_REGISTERS_END: usize = 0x180;
/// psx-spx: "MVOLL (main volume left)", right at +2.
const MAIN_VOLUME: usize = 0x180;
const REVERB_VOLUME: usize = 0x184;
const KEY_ON: usize = 0x188;
const KEY_OFF: usize = 0x18C;
/// psx-spx: "PMON0 (voice 1..15 pitch modulation enable)".
const PITCH_MODULATION: usize = 0x190;
/// psx-spx: "NON0 (voice 0..15 noise enable)".
const NOISE_ON: usize = 0x194;
const REVERB_ON: usize = 0x198;
/// psx-spx: "ENDX0 (voice 0..15 end reached, read-only)".
const ENDX: usize = 0x19C;
const REVERB_BASE: usize = 0x1A2;
/// psx-spx: "0x1f801da4: IRQA (IRQ address)".
const IRQ_ADDRESS: usize = 0x1A4;
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
/// psx-spx: "AVOLL (I2SA/CD-ROM volume left)", right at +2.
const CD_VOLUME: usize = 0x1B0;
/// psx-spx: "MVOLXL (current main volume left)", right at +2.
const MAIN_VOLUME_CURRENT: usize = 0x1B8;
const REVERB_REGISTERS: usize = 0x1C0;
/// psx-spx: "VOLXL (current voice volume left)" at N*4, right at +2.
const VOICE_VOLUME_CURRENT: usize = 0x200;
const VOICE_VOLUME_CURRENT_END: usize = 0x260;

// ATTR bits.
const ATTR_ENABLE: u16 = 1 << 15;
const ATTR_UNMUTE: u16 = 1 << 14;
const ATTR_IRQ_ENABLE: u16 = 1 << 6;
const ATTR_CD_ENABLE: u16 = 1 << 0;

/// ATTR bits 5-4: "Sound RAM Transfer Mode (0=Stop, 1=ManualWrite,
/// 2=DMAwrite, 3=DMAread)".
const MODE_MANUAL_WRITE: u16 = 1;
const MODE_DMA_WRITE: u16 = 2;
const MODE_DMA_READ: u16 = 3;
/// "Data (max 32 halfwords)".
const FIFO_CAPACITY: usize = 32;

// psx-spx: "SPU Memory layout" — the four 1 KB capture buffers.
const CAPTURE_CD_LEFT: usize = 0x000;
const CAPTURE_CD_RIGHT: usize = 0x400;
const CAPTURE_VOICE_1: usize = 0x800;
const CAPTURE_VOICE_3: usize = 0xC00;

/// A voice or main volume. psx-spx: "Direct Volume Mode (when Bit15=0)",
/// "Sweep Volume Mode (when Bit15=1)".
#[derive(Debug, Clone, Copy, Default)]
struct Volume {
    envelope: Envelope,
    sweep: Option<Rate>,
}

impl Volume {
    fn write(&mut self, value: u16) {
        if value & 0x8000 == 0 {
            // "Voice volume/2 (-4000h..+3FFFh = Volume -8000h..+7FFEh)".
            self.envelope.level = (value << 1) as i16;
            self.sweep = None;
        } else {
            // Bit 14 mode, 13 direction, 12 phase, 6-0 shift and step.
            // "Sweep starts at the current volume".
            self.sweep = Some(Rate::from_bits(value, value & 0x4000 != 0, value & 0x2000 != 0, value & 0x1000 != 0));
            self.envelope.restart();
        }
    }

    fn tick(&mut self) {
        if let Some(rate) = self.sweep {
            self.envelope.tick(rate);
        }
    }

    fn apply(&self, sample: i32) -> i32 {
        (sample * self.envelope.level as i32) >> 15
    }
}

pub struct Spu {
    /// Register file as plain little-endian memory.
    regs: Box<[u8]>,
    ram: Box<[u8]>,
    /// The internal "current address" register, in bytes.
    address: usize,
    /// Halfwords written to the FIFO, waiting for "Manual Write".
    fifo: Vec<u16>,

    voices: [Voice; VOICES],
    /// Left and right volume of each voice; entry 24 is the main volume.
    volumes: [[Volume; 2]; VOICES + 1],
    /// Key on / key off bits written since the last sample.
    key_on: u32,
    key_off: u32,
    endx: u32,
    /// psx-spx: "SPU Noise Generator".
    noise_timer: i32,
    noise_level: u16,
    /// Write position inside the capture buffers (0..3FEh).
    capture: usize,
    /// STATX bit 6, and a request not yet passed to the interrupt
    /// controller.
    irq_flag: bool,
    irq_request: bool,
    /// Cycles since the last sample.
    cycles: u32,
    output: Vec<[i16; 2]>,
}

impl Default for Spu {
    fn default() -> Self {
        Self {
            regs: vec![0; REGISTER_BYTES].into_boxed_slice(),
            ram: vec![0; RAM_SIZE].into_boxed_slice(),
            address: 0,
            fifo: Vec::with_capacity(FIFO_CAPACITY),
            voices: std::array::from_fn(|_| Voice::default()),
            volumes: [[Volume::default(); 2]; VOICES + 1],
            key_on: 0,
            key_off: 0,
            endx: 0,
            noise_timer: 0,
            noise_level: 0,
            capture: 0,
            irq_flag: false,
            irq_request: false,
            cycles: 0,
            output: Vec::new(),
        }
    }
}

impl Spu {
    /// Registers whose behaviour is implemented; accesses to the others
    /// (reverb, unknown ones) are reported by the bus as unhandled.
    pub fn is_implemented(offset: u32) -> bool {
        let offset = offset as usize & !1;
        match offset {
            REVERB_VOLUME..KEY_ON | REVERB_ON..ENDX | REVERB_BASE => false,
            _ => offset < REVERB_REGISTERS || (VOICE_VOLUME_CURRENT..VOICE_VOLUME_CURRENT_END).contains(&offset),
        }
    }

    pub fn ram(&self) -> &[u8] {
        &self.ram
    }

    /// Takes the stereo samples (left, right) produced since the last call,
    /// at 44.1 kHz. If nobody takes them, only the last second is kept.
    pub fn take_samples(&mut self) -> Vec<[i16; 2]> {
        std::mem::take(&mut self.output)
    }

    fn reg16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.regs[offset], self.regs[offset + 1]])
    }

    fn reg32(&self, offset: usize) -> u32 {
        self.reg16(offset) as u32 | (self.reg16(offset + 2) as u32) << 16
    }

    fn set_reg16(&mut self, offset: usize, value: u16) {
        self.regs[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn transfer_mode(&self) -> u16 {
        (self.reg16(ATTR) >> 4) & 3
    }

    /// Reads registers; `offset` is relative to 1F801C00h.
    pub fn load(&mut self, offset: u32, width: Width) -> u32 {
        self.refresh_read_only_registers();
        let offset = offset as usize;
        (0..width.bytes()).fold(0, |value, i| value | ((self.regs[offset + i] as u32) << (8 * i)))
    }

    /// Puts the values the hardware maintains into the register file.
    fn refresh_read_only_registers(&mut self) {
        // STATX: bits 5-0 "Current SPU Mode (same as ATTR.Bit5-0, but,
        // applied a bit delayed)" — applied at once here; bit 6 the IRQ
        // flag; bit 7 "seems to be same as ATTR.Bit5"; bits 8/9 the DMA
        // write/read requests; bit 11 "Writing to First/Second half of
        // Capture Buffers". Busy (bit 10) reads zero.
        let attr = self.reg16(ATTR);
        let mut status = attr & 0x3F;
        status |= (self.irq_flag as u16) << 6;
        status |= ((attr >> 5) & 1) << 7;
        status |= ((self.transfer_mode() == MODE_DMA_WRITE) as u16) << 8;
        status |= ((self.transfer_mode() == MODE_DMA_READ) as u16) << 9;
        status |= ((self.capture >= 0x200) as u16) << 11;
        self.set_reg16(STATX, status);
        self.set_reg16(ENDX, self.endx as u16);
        self.set_reg16(ENDX + 2, (self.endx >> 16) as u16);
        for n in 0..VOICES {
            self.set_reg16(n * 0x10 + VOICE_ENVX, self.voices[n].envelope.level as u16);
            for side in 0..2 {
                self.set_reg16(VOICE_VOLUME_CURRENT + n * 4 + side * 2, self.volumes[n][side].envelope.level as u16);
            }
        }
        for side in 0..2 {
            self.set_reg16(MAIN_VOLUME_CURRENT + side * 2, self.volumes[VOICES][side].envelope.level as u16);
        }
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
        let value = self.reg16(register);
        match register {
            0..VOICE_REGISTERS_END => {
                let n = register / 0x10;
                match register % 0x10 {
                    VOICE_VOLUME_LEFT => self.volumes[n][0].write(value),
                    VOICE_VOLUME_RIGHT => self.volumes[n][1].write(value),
                    VOICE_ENVX => self.voices[n].set_level(value as i16),
                    _ => {}
                }
            }
            MAIN_VOLUME => self.volumes[VOICES][0].write(value),
            r if r == MAIN_VOLUME + 2 => self.volumes[VOICES][1].write(value),
            // Key on/off take effect at the next 44.1 kHz cycle.
            KEY_ON => self.key_on |= value as u32,
            r if r == KEY_ON + 2 => self.key_on |= (value as u32 & 0xFF) << 16,
            KEY_OFF => self.key_off |= value as u32,
            r if r == KEY_OFF + 2 => self.key_off |= (value as u32 & 0xFF) << 16,
            // "does additionally store the value (multiplied by 8) in
            // another internal 'current address' register".
            TSA => self.address = value as usize * 8 % RAM_SIZE,
            FIFO => {
                if self.fifo.len() < FIFO_CAPACITY {
                    self.fifo.push(value);
                }
            }
            ATTR => {
                // "IRQ9 Enable (0=Disabled/Acknowledge, 1=Enabled; only
                // when Bit15=1)".
                if value & ATTR_IRQ_ENABLE == 0 {
                    self.irq_flag = false;
                }
                // "SPU RAM Manual Write": the FIFO contents go to sound RAM
                // when the transfer mode becomes "Manual Write".
                if self.transfer_mode() == MODE_MANUAL_WRITE {
                    for halfword in std::mem::take(&mut self.fifo) {
                        self.write_ram(halfword);
                    }
                }
            }
            _ => {}
        }
    }

    // ---- sound RAM transfers ---------------------------------------------

    fn write_ram(&mut self, halfword: u16) {
        self.ram[self.address..self.address + 2].copy_from_slice(&halfword.to_le_bytes());
        // psx-spx: "Data Transfers (usually via DMA4) to/from SPU-RAM do
        // also trap SPU interrupts".
        self.check_irq(self.address);
        self.address = (self.address + 2) % RAM_SIZE;
    }

    fn read_ram(&mut self) -> u16 {
        let halfword = u16::from_le_bytes([self.ram[self.address], self.ram[self.address + 1]]);
        self.check_irq(self.address);
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

    // ---- interrupt -----------------------------------------------------------

    /// An access to sound RAM at `address`. psx-spx: "SPU Interrupt" — the
    /// IRQ address is in 8-byte units; IRQ9 needs ATTR bits 15 and 6.
    fn check_irq(&mut self, address: usize) {
        let attr = self.reg16(ATTR);
        let enabled = attr & (ATTR_ENABLE | ATTR_IRQ_ENABLE) == ATTR_ENABLE | ATTR_IRQ_ENABLE;
        if enabled && !self.irq_flag && address / 8 == self.reg16(IRQ_ADDRESS) as usize {
            self.irq_flag = true;
            self.irq_request = true;
        }
    }

    // ---- sound -----------------------------------------------------------------

    /// Advances the SPU by `cycles` system clock cycles. `cd` is asked for
    /// one stereo sample of CD audio per output sample.
    pub fn tick(&mut self, cycles: u32, irq: &mut Irq, mut cd: impl FnMut() -> [i16; 2]) {
        self.cycles += cycles;
        while self.cycles >= CYCLES_PER_SAMPLE {
            self.cycles -= CYCLES_PER_SAMPLE;
            let sample = self.sample(cd());
            if self.output.len() >= OUTPUT_CAPACITY {
                self.output.drain(..OUTPUT_CAPACITY / 2);
            }
            self.output.push(sample);
        }
        // psx-spx: "Interrupts" — IRQ9 SPU. Also delivers requests raised
        // by transfers since the last call.
        if std::mem::take(&mut self.irq_request) {
            irq.request(Interrupt::Spu);
        }
    }

    fn settings(&self, n: usize) -> Settings {
        let base = n * 0x10;
        Settings {
            pitch: self.reg16(base + VOICE_PITCH),
            start: self.reg16(base + VOICE_START),
            adsr1: self.reg16(base + VOICE_ADSR1),
            adsr2: self.reg16(base + VOICE_ADSR2),
        }
    }

    /// psx-spx: "SPU Noise Generator".
    fn tick_noise(&mut self, attr: u16) {
        let step = 4 + ((attr >> 8) & 3) as i32;
        let shift = (attr >> 10) & 0xF;
        self.noise_timer -= step;
        let level = self.noise_level;
        let parity = ((level >> 15) ^ (level >> 12) ^ (level >> 11) ^ (level >> 10) ^ 1) & 1;
        if self.noise_timer < 0 {
            self.noise_level = (level << 1) | parity;
            self.noise_timer += 0x2_0000 >> shift;
            if self.noise_timer < 0 {
                self.noise_timer += 0x2_0000 >> shift;
            }
        }
    }

    /// One 44.1 kHz cycle: returns the left and right output.
    fn sample(&mut self, cd: [i16; 2]) -> [i16; 2] {
        let attr = self.reg16(ATTR);
        self.tick_noise(attr);
        let (key_on, key_off) = (std::mem::take(&mut self.key_on), std::mem::take(&mut self.key_off));
        let pitch_modulation = self.reg32(PITCH_MODULATION);
        let noise_on = self.reg32(NOISE_ON);

        let mut mix = [0i32; 2];
        let mut previous = 0i16;
        for n in 0..VOICES {
            let bit = 1u32 << n;
            let settings = self.settings(n);
            if key_off & bit != 0 {
                self.voices[n].key_off();
            }
            if key_on & bit != 0 {
                self.voices[n].key_on(&settings);
                // ENDX: "The bits get CLEARED when setting the corresponding
                // KEY ON bits". Deviation: the repeat address is set to the
                // start address at key on (psx-spx does not say; without
                // it a sample with no Loop Start flag would loop to a
                // stale address).
                self.endx &= !bit;
                self.set_reg16(n * 0x10 + VOICE_REPEAT, settings.start);
            }
            let repeat = self.reg16(n * 0x10 + VOICE_REPEAT);
            // PMON: "Flags for Voice 1..15 (0=Normal, 1=Modulate by Voice
            // 0..14)"; bit 0 is unused.
            let modulator = (n > 0 && pitch_modulation & bit != 0).then_some(previous);
            let noise = (noise_on & bit != 0).then_some(self.noise_level as i16);
            let events = self.voices[n].tick(&self.ram, &settings, repeat, modulator, noise);
            if let Some(address) = events.read_block {
                // "Triggers an IRQ when a voice reads ADPCM data from the
                // IRQ address": a block covers two 8-byte units.
                self.check_irq(address as usize);
                self.check_irq(address as usize + 8);
            }
            if let Some(address) = events.loop_start {
                self.set_reg16(n * 0x10 + VOICE_REPEAT, address);
            }
            if events.end {
                self.endx |= bit;
            }
            previous = self.voices[n].output;
            for (side, total) in mix.iter_mut().enumerate() {
                self.volumes[n][side].tick();
                *total += self.volumes[n][side].apply(previous as i32);
            }
        }

        // psx-spx: "SPU Memory layout" — CD audio "before Volume
        // processing" and voices 1 and 3 "after ADSR processing" are
        // written to the capture buffers, always.
        let captures = [
            (CAPTURE_CD_LEFT, cd[0]),
            (CAPTURE_CD_RIGHT, cd[1]),
            (CAPTURE_VOICE_1, self.voices[1].output),
            (CAPTURE_VOICE_3, self.voices[3].output),
        ];
        for (buffer, value) in captures {
            let address = buffer + self.capture;
            self.ram[address..address + 2].copy_from_slice(&value.to_le_bytes());
            // "Capture IRQs do NOT occur if 1F801DACh.bit3-2 are both zero".
            if self.reg16(RAM_CTRL) & 0xC != 0 {
                self.check_irq(address);
            }
        }
        self.capture = (self.capture + 2) & 0x3FF;

        // ATTR bits 15 and 14 silence the voices but "Don't care for CD
        // Audio"; bit 0 lets the CD in, scaled by AVOLL/AVOLR.
        let voices_audible = attr & (ATTR_ENABLE | ATTR_UNMUTE) == ATTR_ENABLE | ATTR_UNMUTE;
        std::array::from_fn(|side| {
            let mut total = if voices_audible { mix[side].clamp(-0x8000, 0x7FFF) } else { 0 };
            if attr & ATTR_CD_ENABLE != 0 {
                total += (cd[side] as i32 * self.reg16(CD_VOLUME + side * 2) as i16 as i32) >> 15;
            }
            self.volumes[VOICES][side].tick();
            self.volumes[VOICES][side].apply(total.clamp(-0x8000, 0x7FFF)).clamp(-0x8000, 0x7FFF) as i16
        })
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

    // ---- sound -------------------------------------------------------------

    use super::voice::tests::block;

    fn load16(spu: &mut Spu, offset: usize) -> u16 {
        spu.load(offset as u32, Width::Half) as u16
    }

    /// An SPU that is on and unmuted, with `blocks` in sound RAM from 1000h
    /// and voice `n` set up to play them at 44.1 kHz with an envelope that
    /// reaches full level in the first cycles and stays there.
    fn playing(n: usize, blocks: &[[u8; 16]]) -> Spu {
        let mut spu = Spu::default();
        for (i, block) in blocks.iter().enumerate() {
            spu.ram[0x1000 + i * 16..][..16].copy_from_slice(block);
        }
        store16(&mut spu, ATTR, 0xC000);
        store16(&mut spu, MAIN_VOLUME, 0x3FFF);
        store16(&mut spu, MAIN_VOLUME + 2, 0x3FFF);
        let voice = n * 0x10;
        store16(&mut spu, voice + VOICE_VOLUME_LEFT, 0x3FFF);
        store16(&mut spu, voice + VOICE_VOLUME_RIGHT, 0x1000);
        store16(&mut spu, voice + VOICE_PITCH, 0x1000);
        store16(&mut spu, voice + VOICE_START, 0x200);
        // Attack shift 0 step 0; the slowest decay, which ends at once
        // because the sustain level Fh = 8000h is already reached; sustain
        // rate all ones.
        store16(&mut spu, voice + VOICE_ADSR1, 0x00FF);
        store16(&mut spu, voice + VOICE_ADSR2, 0x1FC0);
        spu
    }

    fn key_on(spu: &mut Spu, n: usize) {
        store16(spu, KEY_ON + (n / 16) * 2, 1 << (n % 16));
    }

    fn run(spu: &mut Spu, samples: u32) -> Vec<[i16; 2]> {
        let mut irq = Irq::default();
        spu.tick(samples * CYCLES_PER_SAMPLE, &mut irq, || [0, 0]);
        spu.take_samples()
    }

    #[test]
    fn one_sample_every_768_cycles() {
        let mut spu = Spu::default();
        let mut irq = Irq::default();
        spu.tick(767, &mut irq, || [0, 0]);
        assert!(spu.take_samples().is_empty());
        spu.tick(1, &mut irq, || [0, 0]);
        assert_eq!(spu.take_samples(), [[0, 0]]);
        spu.tick(768 * 10 + 5, &mut irq, || [0, 0]);
        assert_eq!(spu.take_samples().len(), 10);
        // Unread output is limited to the last second.
        spu.tick(768 * 3 * SAMPLE_RATE, &mut irq, || [0, 0]);
        assert!(spu.take_samples().len() <= OUTPUT_CAPACITY);
    }

    #[test]
    fn a_keyed_on_voice_is_heard_with_its_volumes() {
        // A loop of constant 7000h samples.
        let mut spu = playing(0, &[block(0x00, 7, 0x7)]);
        assert_eq!(run(&mut spu, 4), [[0, 0]; 4], "silent before key on");
        key_on(&mut spu, 0);
        let out = run(&mut spu, 40);
        let [left, right] = out[39];
        // Left volume 7FFEh, right 2000h, main 7FFEh: left is close to the
        // interpolated 6F9xh, right a quarter of it.
        assert!((0x6E00..0x7000).contains(&left), "{left:04X}");
        assert!((left as i32 / 4 - right as i32).abs() <= 2, "{left:04X} {right:04X}");
        // ENVX shows the envelope; key off releases it (linear, shift 0).
        assert_eq!(load16(&mut spu, VOICE_ENVX), 0x7FFF);
        store16(&mut spu, KEY_OFF, 1);
        assert_eq!(run(&mut spu, 3)[2], [0, 0]);
        assert_eq!(load16(&mut spu, VOICE_ENVX), 0);
    }

    #[test]
    fn mute_and_enable_bits_silence_the_voices() {
        for attr in [0x8000, 0x4000, 0x0000] {
            let mut spu = playing(0, &[block(0x00, 7, 0x7)]);
            store16(&mut spu, ATTR, attr);
            key_on(&mut spu, 0);
            assert_eq!(run(&mut spu, 40)[39], [0, 0], "ATTR {attr:04X}");
        }
    }

    #[test]
    fn voices_are_summed_and_the_sum_saturates() {
        let mut spu = playing(0, &[block(0x00, 7, 0x7)]);
        let one = {
            key_on(&mut spu, 0);
            run(&mut spu, 40)[39][0] as i32
        };
        // A second voice with the same settings doubles it: saturated.
        let mut spu = playing(0, &[block(0x00, 7, 0x7)]);
        for register in [VOICE_VOLUME_LEFT, VOICE_PITCH, VOICE_START, VOICE_ADSR1, VOICE_ADSR2] {
            let value = spu.reg16(register);
            store16(&mut spu, 0x50 + register, value);
        }
        spu.store(KEY_ON as u32, Width::Word, 0x21);
        let two = run(&mut spu, 40)[39][0] as i32;
        assert!(one * 2 > 0x7FFF);
        assert_eq!(two, (0x7FFF * 0x7FFE) >> 15);
    }

    #[test]
    fn endx_is_set_at_loop_end_and_cleared_by_key_on() {
        // psx-spx: ENDX — "The bits get SET when reaching an LOOP-END flag",
        // "CLEARED when setting the corresponding KEY ON bits".
        let mut spu = playing(17, &[block(0x00, 3, 0x1)]);
        key_on(&mut spu, 17);
        run(&mut spu, 27);
        assert_eq!(spu.load(ENDX as u32, Width::Word), 0);
        run(&mut spu, 1);
        assert_eq!(spu.load(ENDX as u32, Width::Word), 1 << 17);
        key_on(&mut spu, 17);
        run(&mut spu, 1);
        assert_eq!(spu.load(ENDX as u32, Width::Word), 0);
        // Writing the register does not stick.
        store16(&mut spu, ENDX, 0xFFFF);
        assert_eq!(load16(&mut spu, ENDX), 0);
    }

    #[test]
    fn repeat_address_follows_loop_start_and_can_be_redirected() {
        // psx-spx: LSAX.
        let silent_loop = block(0x00, 7, 0);
        let mut spu = playing(0, &[block(0x00, 0, 0x1), block(0x00, 4, 0x1), block(0x00, 3, 0x1), silent_loop]);
        key_on(&mut spu, 0);
        run(&mut spu, 1);
        assert_eq!(load16(&mut spu, VOICE_REPEAT), 0x200, "start address at key on");
        run(&mut spu, 28);
        assert_eq!(load16(&mut spu, VOICE_REPEAT), 0x202, "the Loop Start block");
        // Software redirects the loop to the silent block.
        store16(&mut spu, VOICE_REPEAT, 0x206);
        run(&mut spu, 28 * 2 + 10);
        assert_eq!(run(&mut spu, 1), [[0, 0]]);
        assert_eq!(load16(&mut spu, ENDX), 1);
    }

    #[test]
    fn irq_when_a_voice_reads_the_irq_address() {
        // psx-spx: "Voice Interrupt"; ATTR bit 6 enables, clearing it
        // acknowledges; STATX bit 6 is the flag.
        let mut spu = playing(0, &[block(0x00, 0, 0), block(0x00, 0, 0), block(0x00, 3, 0)]);
        store16(&mut spu, IRQ_ADDRESS, 0x204); // third block
        store16(&mut spu, ATTR, 0xC040);
        key_on(&mut spu, 0);
        let mut irq = Irq::default();
        spu.tick(56 * CYCLES_PER_SAMPLE, &mut irq, || [0, 0]);
        assert_eq!((irq.stat(), load16(&mut spu, STATX) & 0x40), (0, 0));
        spu.tick(CYCLES_PER_SAMPLE, &mut irq, || [0, 0]);
        assert_eq!((irq.stat(), load16(&mut spu, STATX) & 0x40), (1 << 9, 0x40));
        // Acknowledge, then re-enable: the voice loops over its three
        // blocks and reads the address again 84 samples later.
        store16(&mut spu, ATTR, 0xC000);
        assert_eq!(load16(&mut spu, STATX) & 0x40, 0);
        irq.write_stat(0);
        store16(&mut spu, ATTR, 0xC040);
        spu.tick(83 * CYCLES_PER_SAMPLE, &mut irq, || [0, 0]);
        assert_eq!(irq.stat(), 0);
        spu.tick(CYCLES_PER_SAMPLE, &mut irq, || [0, 0]);
        assert_eq!(irq.stat(), 1 << 9);
    }

    #[test]
    fn irq_on_transfers_and_not_when_disabled() {
        // psx-spx: "Data Transfers ... do also trap SPU interrupts".
        let mut spu = Spu::default();
        let mut irq = Irq::default();
        store16(&mut spu, IRQ_ADDRESS, 0x300);
        store16(&mut spu, TSA, 0x300);
        store16(&mut spu, ATTR, 0x8020); // enabled, DMA write, IRQ off
        spu.dma_write(0);
        spu.tick(1, &mut irq, || [0, 0]);
        assert_eq!(irq.stat(), 0);
        store16(&mut spu, TSA, 0x300);
        store16(&mut spu, ATTR, 0x8060);
        spu.dma_write(0);
        spu.tick(1, &mut irq, || [0, 0]);
        assert_eq!(irq.stat(), 1 << 9);
    }

    #[test]
    fn cd_audio_is_mixed_with_its_volume_and_captured() {
        // psx-spx: AVOLL/AVOLR, ATTR bit 0; capture buffers at 000h/400h.
        let mut spu = Spu::default();
        let mut irq = Irq::default();
        store16(&mut spu, MAIN_VOLUME, 0x3FFF);
        store16(&mut spu, MAIN_VOLUME + 2, 0x3FFF);
        store16(&mut spu, CD_VOLUME, 0x4000);
        store16(&mut spu, CD_VOLUME + 2, 0x8000); // -8000h: inverted
        // CD input off: silence, but captured all the same.
        spu.tick(CYCLES_PER_SAMPLE, &mut irq, || [1000, -2000]);
        assert_eq!(spu.take_samples(), [[0, 0]]);
        assert_eq!(&spu.ram()[0..2], &1000i16.to_le_bytes());
        assert_eq!(&spu.ram()[0x400..0x402], &(-2000i16).to_le_bytes());
        // On, even with the SPU itself disabled and muted.
        store16(&mut spu, ATTR, 0x0001);
        spu.tick(CYCLES_PER_SAMPLE, &mut irq, || [1000, -2000]);
        let expected = [(500 * 0x7FFE) >> 15, (2000 * 0x7FFE) >> 15];
        assert_eq!(spu.take_samples(), [[expected[0] as i16, expected[1] as i16]]);
        assert_eq!(&spu.ram()[2..4], &1000i16.to_le_bytes(), "the write position advanced");
    }

    #[test]
    fn voices_1_and_3_are_captured_and_the_status_shows_the_half() {
        let mut spu = playing(1, &[block(0x00, 7, 0x7)]);
        key_on(&mut spu, 1);
        // Each 400h-byte buffer holds 200h samples; a half is 100h.
        run(&mut spu, 0xFF);
        let captured = i16::from_le_bytes([spu.ram()[0x800 + 0x1FC], spu.ram()[0x800 + 0x1FD]]);
        assert!((0x6E00..0x7000).contains(&captured), "{captured:04X}");
        assert_eq!(spu.ram()[0xC00 + 0x1FC], 0, "voice 3 is silent");
        assert_eq!(load16(&mut spu, STATX) & 0x800, 0, "first half");
        run(&mut spu, 1);
        assert_eq!(load16(&mut spu, STATX) & 0x800, 0x800, "second half");
        run(&mut spu, 0x100);
        assert_eq!(load16(&mut spu, STATX) & 0x800, 0);
    }

    #[test]
    fn volume_sweep_and_current_volume_registers() {
        // psx-spx: "Sweep Volume Mode" — linear increase, shift 0 step 0:
        // +3800h per cycle from the current level, up to 7FFFh.
        let mut spu = Spu::default();
        store16(&mut spu, 0x30 + VOICE_VOLUME_LEFT, 0x0800); // direct: 1000h
        assert_eq!(load16(&mut spu, VOICE_VOLUME_CURRENT + 3 * 4), 0x1000);
        store16(&mut spu, 0x30 + VOICE_VOLUME_LEFT, 0x8000);
        run(&mut spu, 1);
        assert_eq!(load16(&mut spu, VOICE_VOLUME_CURRENT + 3 * 4), 0x4800);
        run(&mut spu, 2);
        assert_eq!(load16(&mut spu, VOICE_VOLUME_CURRENT + 3 * 4), 0x7FFF);
        // Negative direct volume: bit 14 is the sign.
        store16(&mut spu, MAIN_VOLUME + 2, 0x7000);
        assert_eq!(load16(&mut spu, MAIN_VOLUME_CURRENT + 2), 0xE000);
        assert_eq!(load16(&mut spu, MAIN_VOLUME + 2), 0x7000, "the register keeps what was written");
    }

    #[test]
    fn noise_generator_follows_the_documented_sequence() {
        // psx-spx: "SPU Noise Generator". Shift Fh, step 4: the timer
        // reloads with 20000h >> 15 = 4, so the level shifts every cycle;
        // from zero the parity bit is 1 until bit 10 becomes set.
        let mut spu = Spu::default();
        store16(&mut spu, ATTR, 0x3C00);
        let levels: Vec<u16> = (0..13)
            .map(|_| {
                run(&mut spu, 1);
                spu.noise_level
            })
            .collect();
        assert_eq!(levels[..11], [1, 3, 7, 0xF, 0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF]);
        // Bit 10 set: parity = 1 ^ 1 = 0; then bits 10 and 11: parity 1.
        assert_eq!(levels[11..], [0xFFE, 0x1FFD]);
        // Shift 0: reload 20000h, one shift every 8000h cycles at step 4.
        let mut spu = Spu::default();
        run(&mut spu, 1);
        assert_eq!(spu.noise_level, 1);
        run(&mut spu, 0x7FFF);
        assert_eq!(spu.noise_level, 1);
        run(&mut spu, 1);
        assert_eq!(spu.noise_level, 3);
    }

    #[test]
    fn noise_voice_outputs_the_noise_level() {
        let mut spu = playing(2, &[block(0x00, 7, 0)]);
        store16(&mut spu, ATTR, 0xFC00);
        store16(&mut spu, NOISE_ON, 1 << 2);
        key_on(&mut spu, 2);
        let out = run(&mut spu, 30);
        assert!(out[10..].iter().any(|s| s[0] > 0x1000) && out[10..].iter().any(|s| s[0] < -0x1000), "{out:?}");
    }
}
