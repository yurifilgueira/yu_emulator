//! One of the 24 SPU voices: ADPCM decoding, pitch, interpolation, ADSR.
//!
//! psx-spx: "SPU ADPCM Samples", "SPU ADPCM Pitch", "SPU Volume and ADSR
//! Generator".

use super::envelope::{Envelope, Rate};
use super::gauss::GAUSS;

/// Samples in a 16-byte ADPCM block.
pub const BLOCK_SAMPLES: usize = 28;

// psx-spx: "Flag Bits (in 2nd byte of ADPCM Header)".
const FLAG_LOOP_END: u8 = 1;
const FLAG_LOOP_REPEAT: u8 = 2;
const FLAG_LOOP_START: u8 = 4;

/// psx-spx: "Pos/neg Tables", with the fifth filter that only SPU-ADPCM
/// has ("unlike SPU-ADPCM which supports five filters (0..4)").
const FILTER_POS: [i32; 5] = [0, 60, 115, 98, 122];
const FILTER_NEG: [i32; 5] = [0, 0, -52, -55, -60];

/// Decodes the 28 samples of a 16-byte block. `history` holds the two
/// previous samples (old, older) and is updated.
/// psx-spx: "decode_28_nibbles" (the SPU uses the same algorithm, with the
/// nibbles in order).
pub fn decode_block(block: &[u8], history: &mut [i32; 2], out: &mut [i16; BLOCK_SAMPLES]) {
    // Shift values 13-15 act like 9 (psx-spx, "XA-ADPCM Header Bytes").
    let shift = match block[0] & 0x0F {
        n @ 0..=12 => n,
        _ => 9,
    };
    // Deviation: filters 5-7 do not exist; they are treated as filter 0
    // here (psx-spx does not say what the hardware does).
    let filter = ((block[0] >> 4) & 7) as usize;
    let (f0, f1) = if filter < 5 { (FILTER_POS[filter], FILTER_NEG[filter]) } else { (0, 0) };
    for (i, sample) in out.iter_mut().enumerate() {
        let nibble = (block[2 + i / 2] >> (4 * (i % 2))) & 0x0F;
        // "t = signed4bit(...)"; "s = (t SHL shift) + ((old*f0 +
        // older*f1+32)/64)", where shift = 12 - header nibble.
        let t = ((nibble as i16) << 12) >> shift;
        let s = (t as i32 + ((history[0] * f0 + history[1] * f1 + 32) >> 6)).clamp(-0x8000, 0x7FFF);
        history[1] = history[0];
        history[0] = s;
        *sample = s as i16;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Attack,
    Decay,
    Sustain,
    Release,
    /// Released down to zero (or never started).
    Off,
}

/// The registers a voice works from, read from the register file.
#[derive(Debug, Clone, Copy, Default)]
pub struct Settings {
    pub pitch: u16,
    /// Sample start address in 8-byte units.
    pub start: u16,
    pub adsr1: u16,
    pub adsr2: u16,
}

/// What a voice did in one 44.1 kHz cycle that the SPU has to act on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Events {
    /// A block was read at this byte address (for the SPU interrupt).
    pub read_block: Option<u32>,
    /// A block with the Loop Start flag was read: its address in 8-byte
    /// units goes to the repeat address register.
    pub loop_start: Option<u16>,
    /// A block with the Loop End flag has been played: set ENDX.
    pub end: bool,
}

pub struct Voice {
    /// Byte address of the block being played.
    address: u32,
    /// Pitch counter: "Bit12 and up indicates the current sample".
    counter: u32,
    /// The last three samples of the previous block, then this block.
    samples: [i16; 3 + BLOCK_SAMPLES],
    history: [i32; 2],
    flags: u8,
    /// The block at `address` still has to be read.
    need_block: bool,
    pub envelope: Envelope,
    pub phase: Phase,
    /// The voice's output after ADSR ("VxOUTX"), for pitch modulation of
    /// the next voice and for the capture buffers.
    pub output: i16,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            address: 0,
            counter: 0,
            samples: [0; 3 + BLOCK_SAMPLES],
            history: [0; 2],
            flags: 0,
            need_block: true,
            envelope: Envelope::default(),
            phase: Phase::Off,
            output: 0,
        }
    }
}

impl Voice {
    /// psx-spx: KON — "Starts the ADSR Envelope, and automatically
    /// initializes ADSR Volume to zero"; SSA — "The start address is copied
    /// to the current address upon Key On".
    pub fn key_on(&mut self, settings: &Settings) {
        self.address = settings.start as u32 * 8;
        self.counter = 0;
        self.samples = [0; 3 + BLOCK_SAMPLES];
        self.history = [0; 2];
        self.need_block = true;
        self.envelope.level = 0;
        self.envelope.restart();
        self.phase = Phase::Attack;
    }

    /// psx-spx: KOF — "Start Release".
    pub fn key_off(&mut self) {
        if self.phase != Phase::Off {
            self.phase = Phase::Release;
            self.envelope.restart();
        }
    }

    /// ENVX written by software: "writing allows to let the ADSR generator
    /// to jump to a specific volume level".
    pub fn set_level(&mut self, level: i16) {
        self.envelope.level = level;
    }

    /// One 44.1 kHz cycle. `ram` is the sound RAM, `repeat` the repeat
    /// address register (8-byte units), `modulator` the previous voice's
    /// output when pitch modulation is on for this voice, `noise` the noise
    /// generator's level when this voice is in noise mode.
    pub fn tick(
        &mut self,
        ram: &[u8],
        settings: &Settings,
        repeat: u16,
        modulator: Option<i16>,
        noise: Option<i16>,
    ) -> Events {
        let mut events = Events::default();
        if self.need_block {
            self.need_block = false;
            let address = self.address as usize % ram.len();
            // The block may straddle the end of RAM only if misaligned;
            // wrap byte by byte to be safe.
            let block: [u8; 16] = std::array::from_fn(|i| ram[(address + i) % ram.len()]);
            self.samples.copy_within(BLOCK_SAMPLES.., 0);
            let mut decoded = [0i16; BLOCK_SAMPLES];
            decode_block(&block, &mut self.history, &mut decoded);
            self.samples[3..].copy_from_slice(&decoded);
            self.flags = block[1];
            events.read_block = Some(self.address);
            // "If the hardware finds an ADPCM header with Loop-Start-Bit,
            // then it copies the current address to the repeat address".
            if self.flags & FLAG_LOOP_START != 0 {
                events.loop_start = Some((self.address / 8) as u16);
            }
        }

        // psx-spx: "4-Point Gaussian Interpolation" on "the 4 most recent
        // 16bit ADPCM samples (new,old,older,oldest)".
        let index = (self.counter >> 12) as usize;
        let i = ((self.counter >> 4) & 0xFF) as usize;
        let tap = |gauss: usize, sample: usize| (GAUSS[gauss] as i32 * self.samples[index + sample] as i32) >> 15;
        let interpolated = tap(0xFF - i, 0) + tap(0x1FF - i, 1) + tap(0x100 + i, 2) + tap(i, 3);
        // "(0=ADPCM, 1=Noise)": the ADPCM data is still read in noise mode.
        let sample = noise.map_or(interpolated, |level| level as i32);

        self.tick_envelope(settings);
        self.output = ((sample * self.envelope.level as i32) >> 15) as i16;

        // psx-spx: "Pitch Counter".
        let mut step = settings.pitch as u32;
        if let Some(modulator) = modulator {
            let factor = modulator as i32 + 0x8000;
            step = (((settings.pitch as i16 as i32) * factor) >> 15) as u32 & 0xFFFF;
        }
        self.counter += step.min(0x4000);
        if self.counter >> 12 >= BLOCK_SAMPLES as u32 {
            self.counter -= (BLOCK_SAMPLES as u32) << 12;
            self.need_block = true;
            self.address = (self.address + 16) % ram.len() as u32;
            if self.flags & FLAG_LOOP_END != 0 {
                // "Set ENDX flag and Jump to [1F801C0Eh+N*10h]".
                events.end = true;
                self.address = repeat as u32 * 8;
                // "Code 1 = End+Mute (jump to Loop-address, set ENDX flag,
                // Release, Env=0000h)". Deviation: applied in noise mode
                // too.
                if self.flags & FLAG_LOOP_REPEAT == 0 {
                    self.envelope.level = 0;
                    self.phase = Phase::Off;
                }
            }
        }
        events
    }

    /// psx-spx: ADSR1 / ADSR2.
    fn tick_envelope(&mut self, settings: &Settings) {
        let (adsr1, adsr2) = (settings.adsr1, settings.adsr2);
        let rate = match self.phase {
            // "Attack Direction (Fixed, always Increase)"; shift in bits
            // 14-10, step in bits 9-8, mode in bit 15.
            Phase::Attack => Rate::from_bits(adsr1 >> 8, adsr1 & 0x8000 != 0, false, false),
            // "Decay Mode (Fixed, always Exponential)", "Decay Step (Fixed,
            // always -8)", shift in bits 7-4.
            Phase::Decay => {
                Rate { exponential: true, decreasing: true, shift: ((adsr1 >> 4) & 0xF) as u8, ..Default::default() }
            }
            // ADSR2 bits 15/14 = mode/direction, 12-8 shift, 7-6 step.
            Phase::Sustain => Rate::from_bits(adsr2 >> 6, adsr2 & 0x8000 != 0, adsr2 & 0x4000 != 0, false),
            // Bit 5 mode, bits 4-0 shift, "Release Step (Fixed, always
            // -8)"; a shift of all ones never steps.
            Phase::Release => {
                let shift = (adsr2 & 0x1F) as u8;
                Rate { exponential: adsr2 & 0x20 != 0, decreasing: true, shift, stuck: shift == 0x1F, ..Default::default() }
            }
            Phase::Off => return,
        };
        self.envelope.tick(rate);
        let level = self.envelope.level as i32;
        match self.phase {
            // "(until Level 7FFFh)".
            Phase::Attack if level >= 0x7FFF => self.next_phase(Phase::Decay),
            // "(until Sustain Level)" — "Level=(N+1)*800h".
            Phase::Decay if level <= ((adsr1 & 0xF) as i32 + 1) * 0x800 => self.next_phase(Phase::Sustain),
            // "(until Level 0000h)".
            Phase::Release if level <= 0 => {
                self.envelope.level = 0;
                self.phase = Phase::Off;
            }
            _ => {}
        }
    }

    fn next_phase(&mut self, phase: Phase) {
        self.phase = phase;
        self.envelope.restart();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A 16-byte block: shift/filter byte, flags, and one nibble value for
    /// all 28 samples.
    pub(crate) fn block(shift_filter: u8, flags: u8, nibble: u8) -> [u8; 16] {
        let mut block = [nibble | (nibble << 4); 16];
        block[0] = shift_filter;
        block[1] = flags;
        block
    }

    #[test]
    fn decoding_without_filter_shifts_the_nibbles() {
        // Header shift 12 - n: nibble 7 with shift value 0 is 7000h, with
        // shift value 12 it is 7; nibble Fh is -1.
        let mut out = [0; BLOCK_SAMPLES];
        decode_block(&block(0x00, 0, 0x7), &mut [0; 2], &mut out);
        assert_eq!(out, [0x7000; 28]);
        decode_block(&block(0x0C, 0, 0x7), &mut [0; 2], &mut out);
        assert_eq!(out, [7; 28]);
        decode_block(&block(0x08, 0, 0xF), &mut [0; 2], &mut out);
        assert_eq!(out, [-16; 28]);
        // Nibble order: low nibble first.
        let mut data = block(0x0C, 0, 0);
        data[2] = 0x21;
        decode_block(&data, &mut [0; 2], &mut out);
        assert_eq!(out[..3], [1, 2, 0]);
        // Shift values 13-15 act like 9.
        decode_block(&block(0x0F, 0, 0x1), &mut [0; 2], &mut out);
        assert_eq!(out[0], 8);
    }

    #[test]
    fn decoding_with_filters_uses_the_previous_samples() {
        // Filter 1: s = t + (old*60 + 32) / 64.
        let mut history = [0; 2];
        let mut out = [0; BLOCK_SAMPLES];
        let mut data = block(0x1C, 0, 0);
        data[2] = 0x01; // first sample 1, then zeros
        history[0] = 640;
        decode_block(&data, &mut history, &mut out);
        assert_eq!(out[0], 1 + 600);
        assert_eq!(out[1], ((601i32 * 60 + 32) >> 6) as i16);
        // Filter 2: old*115 - older*52.
        let mut history = [100, 200];
        decode_block(&block(0x2C, 0, 0), &mut history, &mut out);
        assert_eq!(out[0], ((100 * 115 - 200 * 52 + 32) >> 6) as i16);
        // Filter 4 exists for SPU-ADPCM: old*122 - older*60.
        let mut history = [1000, 0];
        decode_block(&block(0x4C, 0, 0), &mut history, &mut out);
        assert_eq!(out[0], ((1000 * 122 + 32) >> 6) as i16);
        // Saturation.
        let mut history = [0x7FFF, 0];
        decode_block(&block(0x40, 0, 0x7), &mut history, &mut out);
        assert_eq!(out[0], 0x7FFF);
    }

    /// Sound RAM with `blocks` placed from address 1000h.
    pub(crate) fn ram(blocks: &[[u8; 16]]) -> Vec<u8> {
        let mut ram = vec![0; 512 * 1024];
        for (i, block) in blocks.iter().enumerate() {
            ram[0x1000 + i * 16..][..16].copy_from_slice(block);
        }
        ram
    }

    /// Full volume at once: attack shift 0 would take 5 cycles, so tests
    /// that look at samples set the level by hand and hold it with a
    /// sustain rate of all ones.
    fn held(pitch: u16) -> Settings {
        Settings { pitch, start: 0x200, adsr1: 0x000F, adsr2: 0x1FC0 }
    }

    fn start(voice: &mut Voice, settings: &Settings) {
        voice.key_on(settings);
        voice.phase = Phase::Sustain;
        voice.set_level(0x7FFF);
    }

    #[test]
    fn constant_sample_comes_out_through_the_interpolation() {
        // A block of 7000h: once the four taps are all 7000h the output is
        // the sum of the four table products, each rounded down.
        let ram = ram(&[block(0x00, 0, 0x7), block(0x00, 0, 0x7)]);
        let settings = held(0x1000);
        let mut voice = Voice::default();
        start(&mut voice, &settings);
        let outputs: Vec<i16> = (0..6).map(|_| {
            voice.tick(&ram, &settings, 0, None, None);
            voice.output
        }).collect();
        // First cycle: only the newest tap has data, weighted by gauss[0].
        assert_eq!(outputs[0] as i32, (((GAUSS[0] as i32 * 0x7000) >> 15) * 0x7FFF) >> 15);
        let full: i32 = [0xFF, 0x1FF, 0x100, 0x000].iter().map(|g| (GAUSS[*g] as i32 * 0x7000) >> 15).sum();
        assert_eq!(outputs[3] as i32, (full * 0x7FFF) >> 15);
        assert_eq!(outputs[4], outputs[3]);
        assert!((0x6F00..0x7000).contains(&outputs[3]), "{:04X}", outputs[3]);
    }

    #[test]
    fn pitch_sets_how_fast_blocks_are_consumed() {
        // 1000h = one sample per cycle: a block lasts 28 cycles.
        let ram = ram(&[block(0x0C, 0, 1), block(0x0C, 0, 2), block(0x0C, 0, 3)]);
        let mut voice = Voice::default();
        let settings = held(0x1000);
        start(&mut voice, &settings);
        let reads = |voice: &mut Voice, settings: &Settings, cycles: usize| -> Vec<(usize, u32)> {
            (0..cycles).filter_map(|i| voice.tick(&ram, settings, 0, None, None).read_block.map(|a| (i, a))).collect()
        };
        assert_eq!(reads(&mut voice, &settings, 60), [(0, 0x1000), (28, 0x1010), (56, 0x1020)]);
        // 0800h = half speed; values above 4000h are clipped to 4000h.
        let settings = held(0x0800);
        start(&mut voice, &settings);
        assert_eq!(reads(&mut voice, &settings, 60), [(0, 0x1000), (56, 0x1010)]);
        let settings = held(0xFFFF);
        start(&mut voice, &settings);
        assert_eq!(reads(&mut voice, &settings, 15), [(0, 0x1000), (7, 0x1010), (14, 0x1020)]);
    }

    #[test]
    fn pitch_modulation_scales_the_step_by_the_previous_voice() {
        // psx-spx: "Pitch Counter" — factor = VxOUTX(x-1) + 8000h, so an
        // output of 0 leaves the pitch alone, -4000h halves it and +4000h
        // gives 1.5 times.
        let ram = ram(&[block(0x0C, 0, 1); 8]);
        let settings = held(0x1000);
        let count = |modulator: i16| {
            let mut voice = Voice::default();
            start(&mut voice, &settings);
            (0..56).filter(|_| voice.tick(&ram, &settings, 0, Some(modulator), None).read_block.is_some()).count()
        };
        assert_eq!(count(0), 2);
        assert_eq!(count(-0x4000), 1);
        assert_eq!(count(0x4000), 3);
    }

    #[test]
    fn loop_flags() {
        // Block 0: loop start. Block 1: loop end + repeat -> back to block
        // 0, ENDX reported.
        let ram = ram(&[block(0x0C, FLAG_LOOP_START, 1), block(0x0C, FLAG_LOOP_END | FLAG_LOOP_REPEAT, 2)]);
        let settings = held(0x4000);
        let mut voice = Voice::default();
        start(&mut voice, &settings);
        let mut repeat = 0;
        let mut log = Vec::new();
        for _ in 0..21 {
            let events = voice.tick(&ram, &settings, repeat, None, None);
            if let Some(address) = events.loop_start {
                repeat = address;
            }
            if let Some(address) = events.read_block {
                log.push((address, events.end));
            }
            if events.end {
                log.push((0, true));
            }
        }
        assert_eq!(log, [(0x1000, false), (0x1010, false), (0, true), (0x1000, false)]);
        assert_eq!(repeat, 0x200);
        assert_eq!(voice.phase, Phase::Sustain, "still playing");
    }

    #[test]
    fn loop_end_without_repeat_mutes_the_voice() {
        // "Code 1 = End+Mute (jump to Loop-address, set ENDX flag, Release,
        // Env=0000h)".
        let ram = ram(&[block(0x00, FLAG_LOOP_END, 7)]);
        let settings = held(0x4000);
        let mut voice = Voice::default();
        start(&mut voice, &settings);
        let ended: Vec<bool> = (0..7).map(|_| voice.tick(&ram, &settings, 0x300, None, None).end).collect();
        assert_eq!(ended, [false, false, false, false, false, false, true]);
        assert_eq!((voice.envelope.level, voice.phase), (0, Phase::Off));
        // It keeps reading, from the repeat address.
        assert_eq!(voice.tick(&ram, &settings, 0x300, None, None).read_block, Some(0x1800));
        assert_eq!(voice.output, 0);
    }

    #[test]
    fn noise_replaces_the_sample_but_the_data_is_still_read() {
        let ram = ram(&[block(0x00, 0, 7)]);
        let settings = held(0x1000);
        let mut voice = Voice::default();
        start(&mut voice, &settings);
        let events = voice.tick(&ram, &settings, 0, None, Some(-0x4000));
        assert_eq!(events.read_block, Some(0x1000));
        assert_eq!(voice.output as i32, (-0x4000 * 0x7FFF) >> 15);
    }

    #[test]
    fn adsr_runs_through_its_phases() {
        // ADSR1: linear attack, shift 0 step 0 (+3800h per cycle); decay
        // shift 0; sustain level 7 (4000h). ADSR2: sustain linear decrease
        // shift 11 step 0 (-8 per cycle); release linear shift 0.
        let ram = ram(&[block(0x00, 0, 0); 4]);
        let settings = Settings { pitch: 0x400, start: 0x200, adsr1: 0x0007, adsr2: 0x4B00 };
        let mut voice = Voice::default();
        assert_eq!(voice.phase, Phase::Off);
        voice.key_on(&settings);
        let step = |voice: &mut Voice| {
            voice.tick(&ram, &settings, 0, None, None);
            (voice.phase, voice.envelope.level)
        };
        assert_eq!(step(&mut voice), (Phase::Attack, 0x3800));
        assert_eq!(step(&mut voice), (Phase::Attack, 0x7000));
        assert_eq!(step(&mut voice), (Phase::Decay, 0x7FFF));
        // Decay: -8 << 11 = -4000h, times level / 8000h: halves.
        // (-4000h * 7FFFh) >> 15 rounds down to -4000h.
        assert_eq!(step(&mut voice), (Phase::Sustain, 0x3FFF));
        assert_eq!(step(&mut voice), (Phase::Sustain, 0x3FFF - 8));
        // Release: -4000h per cycle, stopping at zero.
        voice.key_off();
        assert_eq!(voice.phase, Phase::Release);
        assert_eq!(step(&mut voice), (Phase::Off, 0));
    }

    #[test]
    fn release_ends_at_zero_and_key_on_restarts() {
        let ram = ram(&[block(0x00, 0, 0); 4]);
        let settings = Settings { pitch: 0x400, start: 0x200, adsr1: 0x0007, adsr2: 0x4B00 };
        let mut voice = Voice::default();
        voice.key_on(&settings);
        voice.tick(&ram, &settings, 0, None, None);
        voice.key_off();
        voice.tick(&ram, &settings, 0, None, None);
        assert_eq!((voice.phase, voice.envelope.level), (Phase::Off, 0));
        // Key off on a silent voice changes nothing.
        voice.key_off();
        assert_eq!(voice.phase, Phase::Off);
        voice.key_on(&settings);
        assert_eq!((voice.phase, voice.envelope.level), (Phase::Attack, 0));
    }
}
