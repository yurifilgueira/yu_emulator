//! The SPU's reverb unit.
//!
//! psx-spx: "SPU Reverb Registers", "SPU Reverb Formula".
//!
//! Deviations: both sides are computed in the same 22050 Hz step (psx-spx:
//! the hardware "spends one 44100h cycle on left calculations, and the next
//! ... on right"); the vIIR = -8000h bug is not reproduced; reverb buffer
//! accesses do not raise the SPU interrupt (psx-spx: "Unknown which of the
//! various reverb read(s) and/or reverb write(s) are triggering").

/// psx-spx: "Reverb Buffer Resampling" — "a 39-tap FIR filter".
#[rustfmt::skip]
const FIR: [i32; 39] = [
    -0x0001,  0x0000,  0x0002,  0x0000, -0x000A,  0x0000,  0x0023,  0x0000,
    -0x0067,  0x0000,  0x010A,  0x0000, -0x0268,  0x0000,  0x0534,  0x0000,
    -0x0B90,  0x0000,  0x2806,  0x4000,  0x2806,  0x0000, -0x0B90,  0x0000,
     0x0534,  0x0000, -0x0268,  0x0000,  0x010A,  0x0000, -0x0067,  0x0000,
     0x0023,  0x0000, -0x000A,  0x0000,  0x0002,  0x0000, -0x0001,
];

// Register offsets from 1F801C00h. Left registers; the right ones are 2
// higher.
const D_APF1: usize = 0x1C0;
const D_APF2: usize = 0x1C2;
const V_IIR: usize = 0x1C4;
const V_COMB: [usize; 4] = [0x1C6, 0x1C8, 0x1CA, 0x1CC];
const V_WALL: usize = 0x1CE;
const V_APF1: usize = 0x1D0;
const V_APF2: usize = 0x1D2;
const M_SAME: usize = 0x1D4;
const M_COMB: [usize; 4] = [0x1D8, 0x1DC, 0x1E8, 0x1EC];
const D_SAME: usize = 0x1E0;
const M_DIFF: usize = 0x1E4;
const D_DIFF: usize = 0x1F0;
const M_APF1: usize = 0x1F4;
const M_APF2: usize = 0x1F8;
const V_IN: usize = 0x1FC;
/// "ESA base Reverb Work Area Start Address in Sound RAM".
pub const BASE: usize = 0x1A2;

fn clamp16(value: i32) -> i32 {
    value.clamp(-0x8000, 0x7FFF)
}

#[derive(Debug, Clone)]
pub struct Reverb {
    /// "current buffer address", in bytes.
    address: usize,
    /// The last 39 inputs and (zero-stuffed) outputs at 44.1 kHz, per side,
    /// newest at `position`.
    input: [[i32; 2]; 64],
    output: [[i32; 2]; 64],
    position: usize,
}

impl Default for Reverb {
    fn default() -> Self {
        Self { address: 0, input: [[0; 2]; 64], output: [[0; 2]; 64], position: 0 }
    }
}

impl Reverb {
    /// "Writing a value to ESA does additionally set the current buffer
    /// address to that value."
    pub fn set_base(&mut self, base: u16) {
        self.address = base as usize * 8;
    }

    /// One 22050 Hz step of the "Reverb Formula". `regs` is the SPU's
    /// register file, `input` the left and right sum of everything routed
    /// to the reverb; `write` is ATTR bit 7 ("When the Reverb Master Enable
    /// flag is cleared, the SPU stops to write any data to the Reverb
    /// buffer").
    fn step(&mut self, regs: &[u8], ram: &mut [u8], input: [i32; 2], write: bool) -> [i32; 2] {
        let reg = |offset: usize| u16::from_le_bytes([regs[offset], regs[offset + 1]]);
        let volume = |offset: usize| reg(offset) as i16 as i32;
        let base = (reg(BASE) as usize * 8).min(ram.len() - 2);
        let size = (ram.len() - base) as i64;
        let current = self.address.max(base);
        // "All memory addresses are relative to the current BufferAddress,
        // and wrapped within ESA..7FFFEh when exceeding that region."
        let address = |offset: usize, delta: i64| -> usize {
            let relative = current as i64 - base as i64 + reg(offset) as i64 * 8 + delta;
            (base + relative.rem_euclid(size) as usize) & !1
        };
        let read = |ram: &[u8], at: usize| i16::from_le_bytes([ram[at], ram[at + 1]]) as i32;
        // "The values written to memory are saturated to -8000h..+7FFFh."
        let store = |ram: &mut [u8], at: usize, value: i32| {
            if write {
                ram[at..at + 2].copy_from_slice(&(clamp16(value) as i16).to_le_bytes());
            }
        };
        // "The multiplication results are divided by +8000h".
        let mul = |a: i32, b: i32| (a * b) >> 15;

        let v_iir = volume(V_IIR);
        let v_wall = volume(V_WALL);
        let mut out = [0i32; 2];
        for side in 0..2 {
            let s = side * 2;
            let other = (1 - side) * 2;
            let rin = mul(volume(V_IN + s), input[side]);
            // "[mLSAME] = (Lin + [dLSAME]*vWALL - [mLSAME-2])*vIIR +
            // [mLSAME-2]".
            let old = read(ram, address(M_SAME + s, -2));
            let same = mul(rin + mul(read(ram, address(D_SAME + s, 0)), v_wall) - old, v_iir) + old;
            store(ram, address(M_SAME + s, 0), same);
            // "[mLDIFF] = (Lin + [dRDIFF]*vWALL - [mLDIFF-2])*vIIR +
            // [mLDIFF-2]": the other side's reflection.
            let old = read(ram, address(M_DIFF + s, -2));
            let diff = mul(rin + mul(read(ram, address(D_DIFF + other, 0)), v_wall) - old, v_iir) + old;
            store(ram, address(M_DIFF + s, 0), diff);
            // "Lout=vCOMB1*[mLCOMB1]+vCOMB2*[mLCOMB2]+vCOMB3*[mLCOMB3]+
            // vCOMB4*[mLCOMB4]".
            let mut value: i32 = (0..4).map(|i| mul(volume(V_COMB[i]), read(ram, address(M_COMB[i] + s, 0)))).sum();
            // "Lout=Lout-vAPF1*[mLAPF1-dAPF1], [mLAPF1]=Lout,
            // Lout=Lout*vAPF1+[mLAPF1-dAPF1]", then the same with APF2.
            for (m_apf, d_apf, v_apf) in [(M_APF1, D_APF1, V_APF1), (M_APF2, D_APF2, V_APF2)] {
                let delayed_at = address(m_apf + s, -(reg(d_apf) as i64 * 8));
                let v_apf = volume(v_apf);
                value = clamp16(value - mul(v_apf, read(ram, delayed_at)));
                store(ram, address(m_apf + s, 0), value);
                // Read after the store: with a displacement of zero the
                // value just written comes back.
                value = clamp16(mul(value, v_apf) + read(ram, delayed_at));
            }
            out[side] = value;
        }
        // "BufferAddress = MAX(ESA, (BufferAddress+2) AND 7FFFEh)".
        self.address = ((current + 2) & (ram.len() - 2)).max(base);
        out
    }

    /// One 44.1 kHz cycle: takes the sum routed to the reverb and returns
    /// the reverb's output (before the output volume). The unit itself runs
    /// at 22050 Hz; its input and output are resampled with the FIR filter.
    pub fn tick(&mut self, regs: &[u8], ram: &mut [u8], input: [i32; 2], write: bool) -> [i32; 2] {
        self.position = (self.position + 1) & 63;
        self.input[self.position] = input.map(clamp16);
        self.output[self.position] = [0; 2];
        let filter = |ring: &[[i32; 2]; 64], position: usize, side: usize| -> i32 {
            FIR.iter().enumerate().map(|(i, c)| c * ring[(position + 64 - i) & 63][side]).sum::<i32>() >> 15
        };
        if self.position & 1 == 0 {
            let down = [filter(&self.input, self.position, 0), filter(&self.input, self.position, 1)];
            self.output[self.position] = self.step(regs, ram, down, write);
        }
        // Every other output slot is zero, so the filter's gain is halved:
        // times two.
        std::array::from_fn(|side| clamp16(filter(&self.output, self.position, side) * 2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(regs: &mut [u8], offset: usize, value: u16) {
        regs[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    const BASE_UNITS: u16 = 0xF000;
    const BASE_BYTES: usize = 0x78000;

    /// A 32 KB work area used as a plain delay. The buffer address advances
    /// 2 bytes per step, so what is written at offset W units is read at
    /// offset R units (W - R) * 4 steps later (or, when R is the larger
    /// one, only after going round the whole buffer). Left: written at 40,
    /// read by comb 1 at 38, 8 steps later, at half volume. Right: 60 and
    /// 58. The all-pass filters only pass the value on; their buffers and
    /// the unused taps sit below the comb taps, out of reach.
    fn delay_setup() -> (Vec<u8>, Vec<u8>, Reverb) {
        let mut regs = vec![0u8; 0x400];
        let ram = vec![0u8; 512 * 1024];
        set(&mut regs, BASE, BASE_UNITS);
        set(&mut regs, V_IN, 0x7FFF);
        set(&mut regs, V_IN + 2, 0x7FFF);
        set(&mut regs, V_IIR, 0x7FFF);
        set(&mut regs, V_COMB[0], 0x4000);
        set(&mut regs, M_SAME, 40);
        set(&mut regs, M_SAME + 2, 60);
        set(&mut regs, M_COMB[0], 38);
        set(&mut regs, M_COMB[0] + 2, 58);
        for register in [M_DIFF, M_COMB[1], M_COMB[2], M_COMB[3], D_SAME, D_DIFF] {
            set(&mut regs, register, 20);
            set(&mut regs, register + 2, 21);
        }
        set(&mut regs, M_APF1, 10);
        set(&mut regs, M_APF1 + 2, 12);
        set(&mut regs, M_APF2, 14);
        set(&mut regs, M_APF2 + 2, 16);
        let mut reverb = Reverb::default();
        reverb.set_base(BASE_UNITS);
        (regs, ram, reverb)
    }

    #[test]
    fn an_impulse_comes_back_after_the_buffer_delay() {
        let (regs, mut ram, mut reverb) = delay_setup();
        let outputs: Vec<[i32; 2]> = (0..12)
            .map(|step| reverb.step(&regs, &mut ram, if step == 0 { [0x4000, 0] } else { [0, 0] }, true))
            .collect();
        // 4000h * vLIN = 3FFFh, * vIIR = 3FFEh, * vCOMB1 (0.5) = 1FFFh.
        let mut expected = [[0, 0]; 12];
        expected[8] = [0x1FFF, 0];
        assert_eq!(outputs, expected);
        // The stored reflection, where it was written: base + 40 units.
        assert_eq!(&ram[BASE_BYTES + 40 * 8..][..2], &0x3FFEu16.to_le_bytes());
    }

    #[test]
    fn right_side_is_separate_and_the_buffer_wraps() {
        let (regs, mut ram, mut reverb) = delay_setup();
        // Go round the 16384-sample buffer more than once first.
        for _ in 0..20_000 {
            reverb.step(&regs, &mut ram, [0, 0], true);
        }
        let outputs: Vec<[i32; 2]> = (0..9)
            .map(|step| reverb.step(&regs, &mut ram, if step == 0 { [0, -0x4000] } else { [0, 0] }, true))
            .collect();
        // -4000h * 7FFFh >> 15 = -4000h (rounded down), twice; then half.
        assert_eq!(outputs[8], [0, -0x2000]);
        assert!(outputs[..8].iter().all(|o| *o == [0, 0]));
        assert!(ram[..BASE_BYTES].iter().all(|b| *b == 0), "nothing below the work area is touched");
    }

    #[test]
    fn master_enable_off_stops_writes_but_not_reads() {
        // psx-spx: "Reverb Disable".
        let (regs, mut ram, mut reverb) = delay_setup();
        reverb.step(&regs, &mut ram, [0x4000, 0], false);
        assert!(ram.iter().all(|b| *b == 0));
        // Data already in the buffer is still read: put a sample where comb
        // 1 reads at the next step (the buffer address is now base + 2).
        // The value should then pass through the all-pass buffers, but
        // those are not written either, so nothing comes out.
        let at = BASE_BYTES + 2 + 38 * 8;
        ram[at..at + 2].copy_from_slice(&0x2000u16.to_le_bytes());
        assert_eq!(reverb.step(&regs, &mut ram, [0, 0], false), [0, 0]);
        // With writes on, the same sample is heard at half volume.
        let at = BASE_BYTES + 4 + 38 * 8;
        ram[at..at + 2].copy_from_slice(&0x2000u16.to_le_bytes());
        assert_eq!(reverb.step(&regs, &mut ram, [0, 0], true), [0x1000, 0]);
    }

    #[test]
    fn all_pass_filter_feeds_back() {
        // APF1 with volume 0.5 and a displacement of 1 unit (4 steps). The
        // comb delivers 1FFFh at step 8; products are rounded down.
        let (mut regs, mut ram, mut reverb) = delay_setup();
        set(&mut regs, V_APF1, 0x4000);
        set(&mut regs, D_APF1, 1);
        let outputs: Vec<i32> = (0..17)
            .map(|step| reverb.step(&regs, &mut ram, if step == 0 { [0x4000, 0] } else { [0, 0] }, true)[0])
            .collect();
        // Step 8: in = 1FFFh, delayed = 0: stored 1FFFh, out = 0FFFh.
        assert_eq!(outputs[8], 0x0FFF);
        // Step 12: in = 0, delayed = 1FFFh: stored 0 - 0FFFh = -0FFFh,
        // out = (-0FFFh * 0.5 = -800h) + 1FFFh = 17FFh.
        assert_eq!(outputs[12], 0x17FF);
        // Step 16: delayed = -0FFFh: stored 0 + 800h, out = 400h - 0FFFh.
        assert_eq!(outputs[16], 0x0400 - 0x0FFF);
        assert!(outputs.iter().enumerate().all(|(i, o)| [8, 12, 16].contains(&i) || *o == 0), "{outputs:X?}");
    }

    #[test]
    fn resampling_keeps_the_level_of_a_steady_signal() {
        // The filter's taps sum to 7FFEh (unity, nearly) for the way in;
        // on the way out the zero-stuffed signal is doubled. A steady input
        // is then a steady signal inside, and comes back from the delay at
        // half volume.
        let sum: i32 = FIR.iter().sum();
        assert_eq!(sum, 0x7FFE);
        let (regs, mut ram, mut reverb) = delay_setup();
        let mut last = [0; 2];
        for _ in 0..400 {
            last = reverb.tick(&regs, &mut ram, [0x2000, 0], true);
        }
        assert!((0x0FF0..=0x1004).contains(&last[0]), "{:04X}", last[0]);
        assert_eq!(last[1], 0);
    }
}
