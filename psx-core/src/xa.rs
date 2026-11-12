//! XA-ADPCM: the compressed audio of CD-ROM XA sectors, decoded by the
//! CD-ROM controller and sent to the SPU's CD input.
//!
//! psx-spx: "CDROM XA Audio ADPCM Compression".
//!
//! Deviations: 8-bit ADPCM and emphasis are not handled (sectors with 8-bit
//! samples are skipped; psx-spx: emphasis "isn't used by any known PSX
//! games"); 18900 Hz audio is resampled by playing every sample twice
//! through the 37800 Hz path (psx-spx only guesses how the hardware does
//! it); the six-step counter starts at 6 rather than at a random value.

use std::collections::VecDeque;

/// psx-spx: "Pos/neg Tables" — "XA-ADPCM supports only four filters".
const FILTER_POS: [i32; 4] = [0, 60, 115, 98];
const FILTER_NEG: [i32; 4] = [0, 0, -52, -55];

/// psx-spx: "25-point Zigzag Interpolation" — Table1..Table7, entries for
/// i = 1..29, copied from the document by a script.
#[rustfmt::skip]
const ZIGZAG: [[i16; 29]; 7] = [
    [0x0000, 0x0000, 0x0000, 0x0000, 0x0000, -0x0002, 0x000A, -0x0022, 0x0041, -0x0054, 0x0034, 0x0009, -0x010A, 0x0400, -0x0A78, 0x234C, 0x6794, -0x1780, 0x0BCD, -0x0623, 0x0350, -0x016D, 0x006B, 0x000A, -0x0010, 0x0011, -0x0008, 0x0003, -0x0001],
    [0x0000, 0x0000, 0x0000, -0x0002, 0x0000, 0x0003, -0x0013, 0x003C, -0x004B, 0x00A2, -0x00E3, 0x0132, -0x0043, -0x0267, 0x0C9D, 0x74BB, -0x11B4, 0x09B8, -0x05BF, 0x0372, -0x01A8, 0x00A6, -0x001B, 0x0005, 0x0006, -0x0008, 0x0003, -0x0001, 0x0000],
    [0x0000, 0x0000, -0x0001, 0x0003, -0x0002, -0x0005, 0x001F, -0x004A, 0x00B3, -0x0192, 0x02B1, -0x039E, 0x04F8, -0x05A6, 0x7939, -0x05A6, 0x04F8, -0x039E, 0x02B1, -0x0192, 0x00B3, -0x004A, 0x001F, -0x0005, -0x0002, 0x0003, -0x0001, 0x0000, 0x0000],
    [0x0000, -0x0001, 0x0003, -0x0008, 0x0006, 0x0005, -0x001B, 0x00A6, -0x01A8, 0x0372, -0x05BF, 0x09B8, -0x11B4, 0x74BB, 0x0C9D, -0x0267, -0x0043, 0x0132, -0x00E3, 0x00A2, -0x004B, 0x003C, -0x0013, 0x0003, 0x0000, -0x0002, 0x0000, 0x0000, 0x0000],
    [-0x0001, 0x0003, -0x0008, 0x0011, -0x0010, 0x000A, 0x006B, -0x016D, 0x0350, -0x0623, 0x0BCD, -0x1780, 0x6794, 0x234C, -0x0A78, 0x0400, -0x010A, 0x0009, 0x0034, -0x0054, 0x0041, -0x0022, 0x000A, -0x0001, 0x0000, 0x0001, 0x0000, 0x0000, 0x0000],
    [0x0002, -0x0008, 0x0010, -0x0023, 0x002B, 0x001A, -0x00EB, 0x027B, -0x0548, 0x0AFA, -0x16FA, 0x53E0, 0x3C07, -0x1249, 0x080E, -0x0347, 0x015B, -0x0044, -0x0017, 0x0046, -0x0023, 0x0011, -0x0005, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000],
    [-0x0005, 0x0011, -0x0023, 0x0046, -0x0017, -0x0044, 0x015B, -0x0347, 0x080E, -0x1249, 0x3C07, 0x53E0, -0x16FA, 0x0AFA, -0x0548, 0x027B, -0x00EB, 0x001A, 0x002B, -0x0023, 0x0010, -0x0008, 0x0002, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000],
];

/// 37800 Hz to 44100 Hz for one channel: "to insert one extra sample after
/// each six samples".
#[derive(Debug, Clone)]
struct Resampler {
    ring: [i16; 32],
    position: usize,
    six_step: u8,
}

impl Default for Resampler {
    fn default() -> Self {
        Self { ring: [0; 32], position: 0, six_step: 6 }
    }
}

impl Resampler {
    /// "Output37800Hz(sample)": takes one sample, gives none or seven.
    fn push(&mut self, sample: i16, out: &mut Vec<i16>) {
        self.ring[self.position & 0x1F] = sample;
        self.position = self.position.wrapping_add(1);
        self.six_step -= 1;
        if self.six_step == 0 {
            self.six_step = 6;
            for table in &ZIGZAG {
                // "sum=sum+(ringbuf[(p-i) AND 1Fh]*TableX[i])/8000h".
                let sum: i32 = (1..=29)
                    .map(|i| (self.ring[self.position.wrapping_sub(i) & 0x1F] as i32 * table[i - 1] as i32) >> 15)
                    .sum();
                out.push(sum.clamp(-0x8000, 0x7FFF) as i16);
            }
        }
    }
}

/// What the subheader says about a sector's sound.
/// psx-spx: "Subheader" — "mono/stereo flag, 37800Hz/18900Hz sampling rate,
/// 4bit/8bit format, and emphasis".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coding {
    pub stereo: bool,
    pub half_rate: bool,
    pub eight_bit: bool,
}

impl Coding {
    /// From the coding info byte (the fourth subheader byte).
    pub fn from_byte(byte: u8) -> Self {
        Self { stereo: byte & 0x03 == 1, half_rate: byte & 0x0C == 0x04, eight_bit: byte & 0x30 == 0x10 }
    }
}

#[derive(Default)]
pub struct Decoder {
    /// (old, older) per channel. psx-spx: "Old/Older Values" carry over
    /// from the previous sector.
    history: [[i32; 2]; 2],
    resamplers: [Resampler; 2],
}

impl Decoder {
    /// psx-spx: "decode_28_nibbles(src,blk,nibble,dst,old,older)". `src` is
    /// a 128-byte portion.
    fn decode_28_nibbles(src: &[u8], block: usize, nibble: usize, history: &mut [i32; 2], out: &mut Vec<i16>) {
        let header = src[4 + block * 2 + nibble];
        // "reserved shift values 13..15 will act same as shift=9".
        let shift = match header & 0x0F {
            n @ 0..=12 => n,
            _ => 9,
        };
        let filter = ((header >> 4) & 3) as usize;
        let (f0, f1) = (FILTER_POS[filter], FILTER_NEG[filter]);
        for j in 0..28 {
            let t = (src[16 + block + j * 4] >> (nibble * 4)) & 0x0F;
            let t = (((t as i16) << 12) >> shift) as i32;
            let s = (t + ((history[0] * f0 + history[1] * f1 + 32) >> 6)).clamp(-0x8000, 0x7FFF);
            history[1] = history[0];
            history[0] = s;
            out.push(s as i16);
        }
    }

    /// Decodes the sound of a raw 2352-byte sector into stereo samples at
    /// 44100 Hz, appended to `output`. psx-spx: "decode_sector(src)".
    pub fn decode_sector(&mut self, raw: &[u8], output: &mut VecDeque<[i16; 2]>) {
        let coding = Coding::from_byte(raw[19]);
        if coding.eight_bit {
            return;
        }
        // "skip sync,header,subheader"; "12h 128-byte portions".
        let (mut left, mut right) = (Vec::with_capacity(4032), Vec::with_capacity(2016));
        for portion in raw[24..24 + 18 * 128].chunks(128) {
            for block in 0..4 {
                if coding.stereo {
                    Self::decode_28_nibbles(portion, block, 0, &mut self.history[0], &mut left);
                    Self::decode_28_nibbles(portion, block, 1, &mut self.history[1], &mut right);
                } else {
                    Self::decode_28_nibbles(portion, block, 0, &mut self.history[0], &mut left);
                    Self::decode_28_nibbles(portion, block, 1, &mut self.history[0], &mut left);
                }
            }
        }
        let repeat = if coding.half_rate { 2 } else { 1 };
        let resample = |samples: &[i16], resampler: &mut Resampler| {
            let mut out = Vec::with_capacity(samples.len() * repeat * 7 / 6);
            for sample in samples {
                for _ in 0..repeat {
                    resampler.push(*sample, &mut out);
                }
            }
            out
        };
        let left = resample(&left, &mut self.resamplers[0]);
        if coding.stereo {
            let right = resample(&right, &mut self.resamplers[1]);
            output.extend(left.iter().zip(&right).map(|(l, r)| [*l, *r]));
        } else {
            output.extend(left.iter().map(|s| [*s, *s]));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A raw sector of XA audio: every sound group has the given header
    /// byte for all blocks and `word` as every data word.
    pub(crate) fn sector(coding: u8, header: u8, word: u32) -> Vec<u8> {
        let mut raw = vec![0u8; 2352];
        raw[15] = 2; // mode 2
        raw[18] = 0x64; // submode: audio, form 2, real time
        raw[19] = coding;
        for portion in raw[24..24 + 18 * 128].chunks_mut(128) {
            portion[..16].fill(header);
            for data in portion[16..].chunks_mut(4) {
                data.copy_from_slice(&word.to_le_bytes());
            }
        }
        raw
    }

    #[test]
    fn coding_info_bits() {
        assert_eq!(Coding::from_byte(0x00), Coding { stereo: false, half_rate: false, eight_bit: false });
        assert_eq!(Coding::from_byte(0x01), Coding { stereo: true, half_rate: false, eight_bit: false });
        assert_eq!(Coding::from_byte(0x04), Coding { stereo: false, half_rate: true, eight_bit: false });
        assert_eq!(Coding::from_byte(0x11), Coding { stereo: true, half_rate: false, eight_bit: true });
    }

    #[test]
    fn nibbles_are_taken_per_block_from_the_data_words() {
        // Header shift 12 (values as they are), filter 0. Data word
        // 87654321h: block 0 uses byte 0 (21h): low nibble 1, high nibble 2;
        // block 3 uses byte 3 (87h): 7 and -8.
        let mut portion = [0u8; 128];
        portion[..16].fill(0x0C);
        for data in portion[16..].chunks_mut(4) {
            data.copy_from_slice(&0x8765_4321u32.to_le_bytes());
        }
        let decode = |block, nibble| {
            let mut out = Vec::new();
            Decoder::decode_28_nibbles(&portion, block, nibble, &mut [0; 2], &mut out);
            out
        };
        assert_eq!(decode(0, 0), [1; 28]);
        assert_eq!(decode(0, 1), [2; 28]);
        assert_eq!(decode(3, 0), [7; 28]);
        assert_eq!(decode(3, 1), [-8; 28]);
    }

    #[test]
    fn filter_uses_the_history_across_calls() {
        // Filter 1 (old * 60 / 64), all nibbles zero: decays from the
        // history value.
        let mut portion = [0u8; 128];
        portion[..16].fill(0x1C);
        let mut history = [6400, 0];
        let mut out = Vec::new();
        Decoder::decode_28_nibbles(&portion, 0, 0, &mut history, &mut out);
        assert_eq!(out[0], 6000);
        assert_eq!(out[1], ((6000 * 60 + 32) >> 6) as i16);
        assert_eq!(history[0], out[27] as i32);
    }

    #[test]
    fn a_sector_becomes_2352_samples_at_44100_hz() {
        // 18 portions x 4 blocks x 28 samples = 2016 per channel at
        // 37800 Hz, times 7/6.
        let mut decoder = Decoder::default();
        let mut output = VecDeque::new();
        decoder.decode_sector(&sector(0x01, 0x00, 0x7777_7777), &mut output);
        assert_eq!(output.len(), 2352);
        // A constant 7000h comes out on both sides scaled by the tables'
        // gain (about 0.906, see the last test) once the filter has filled.
        let [left, right] = output[1000];
        assert!((0x6500..=0x6600).contains(&left), "{left:04X}");
        assert_eq!(left, right);
    }

    #[test]
    fn mono_uses_all_eight_blocks_for_one_channel() {
        // Mono: 4032 samples per sector -> 4704 output samples; both sides
        // carry the same signal.
        let mut decoder = Decoder::default();
        let mut output = VecDeque::new();
        decoder.decode_sector(&sector(0x00, 0x00, 0x1111_1111), &mut output);
        assert_eq!(output.len(), 4704);
        assert!(output.iter().all(|s| s[0] == s[1]));
        assert!((0x0E60..=0x0E90).contains(&output[2000][0]), "{:04X}", output[2000][0]);
    }

    #[test]
    fn half_rate_plays_twice_as_long_and_8_bit_is_skipped() {
        let mut decoder = Decoder::default();
        let mut output = VecDeque::new();
        decoder.decode_sector(&sector(0x05, 0x00, 0x1111_1111), &mut output);
        assert_eq!(output.len(), 4704);
        output.clear();
        decoder.decode_sector(&sector(0x11, 0x00, 0x1111_1111), &mut output);
        assert!(output.is_empty());
    }

    #[test]
    fn interpolation_tables_have_the_same_gain() {
        // The seven tables of psx-spx each sum to 73E5h..741Dh: a gain of
        // about 0.906, the same for every output phase. (I expected 8000h;
        // the sums are what the document's values give, and being equal
        // across the tables they are not a copying error.)
        for table in &ZIGZAG {
            let sum: i32 = table.iter().map(|v| *v as i32).sum();
            assert!((0x73E5..=0x741D).contains(&sum), "{sum:04X}");
        }
    }
}
