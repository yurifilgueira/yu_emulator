//! Macroblock decoder (MDEC): decompresses the pictures of videos.
//!
//! psx-spx: "Macroblock Decoder (MDEC)".
//!
//! Deviations, all for lack of better information in psx-spx:
//! - Decoding takes no time: a macroblock is in the output queue as soon as
//!   its last halfword has been written.
//! - The output queue holds whole macroblocks in picture order (16 pixels
//!   per row for coloured ones). psx-spx says the chip outputs 8x8 blocks
//!   and that DMA1 "is doing the re-ordering automatically"; here DMA1 is a
//!   plain copy and a program reading 1F801820h by hand gets the reordered
//!   data too.
//! - The queues have no size limit, so "Data-In Fifo Full" never shows.
//! - The IDCT is psx-spx's "real_idct_core", which it describes as "roughly"
//!   what the hardware does: pixels may differ from a console by a unit.

use std::collections::VecDeque;

/// psx-spx: "zigzag[0..63]".
#[rustfmt::skip]
const ZIGZAG: [u8; 64] = [
    0,  1,  5,  6,  14, 15, 27, 28,
    2,  4,  7,  13, 16, 26, 29, 42,
    3,  8,  12, 17, 25, 30, 41, 43,
    9,  11, 18, 24, 31, 40, 44, 53,
    10, 19, 23, 32, 39, 45, 52, 54,
    20, 22, 33, 38, 46, 51, 55, 60,
    21, 34, 37, 47, 50, 56, 59, 61,
    35, 36, 48, 49, 57, 58, 62, 63,
];

/// "zagzig[0..63] (reversed zigzag table)": zagzig[zigzag[i]] = i.
const ZAGZIG: [u8; 64] = {
    let mut table = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        table[ZIGZAG[i] as usize] = i as u8;
        i += 1;
    }
    table
};

/// End of block, and padding between blocks.
const END_OF_BLOCK: u16 = 0xFE00;

/// Block numbers as the status register shows them: "0..3=Y1..Y4, 4=Cr,
/// 5=Cb (or for mono: always 4=Y)".
const BLOCK_CR: u8 = 4;
const BLOCK_CB: u8 = 5;

/// psx-spx: MDEC(1) bits 28-27 "Data Output Depth".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Depth {
    Mono4,
    Mono8,
    Rgb24,
    Rgb15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    /// MDEC(1): compressed data is arriving.
    Decode,
    /// MDEC(2): `left` words of quant tables to come, `at` bytes stored.
    QuantTable { left: u32, at: usize },
    /// MDEC(3): scale table, `at` halfwords stored.
    ScaleTable { at: usize },
}

pub struct Mdec {
    state: State,
    /// Status bits 23-26, copied from bits 25-28 of the last command.
    output_bits: u32,
    /// Status bits 0-15: "Number of Parameter Words remaining minus 1".
    remaining: u16,
    enable_in: bool,
    enable_out: bool,

    /// Luminance and colour quant tables (MDEC(2)).
    quant: [[u8; 64]; 2],
    /// IDCT matrix (MDEC(3)).
    scale: [i16; 64],

    /// Block being received: 4 = Cr, 5 = Cb, 0..3 = Y1..Y4.
    block: u8,
    /// Coefficients of the block being received, and the run-length state:
    /// `position` is None before the block's first halfword.
    coefficients: [i32; 64],
    position: Option<usize>,
    quant_scale: i32,
    /// Decoded colour blocks of the macroblock being received.
    cr: [i16; 64],
    cb: [i16; 64],
    /// The 16x16 macroblock being assembled, as signed R, G, B.
    pixels: [[i16; 3]; 256],

    output: VecDeque<u8>,
}

impl Default for Mdec {
    fn default() -> Self {
        Self {
            state: State::Idle,
            output_bits: 0,
            remaining: 0,
            enable_in: false,
            enable_out: false,
            quant: [[0; 64]; 2],
            scale: [0; 64],
            block: BLOCK_CR,
            coefficients: [0; 64],
            position: None,
            quant_scale: 0,
            cr: [0; 64],
            cb: [0; 64],
            pixels: [[0; 3]; 256],
            output: VecDeque::new(),
        }
    }
}

fn signed10(value: u16) -> i32 {
    ((value as i32) << 22) >> 22
}

impl Mdec {
    fn depth(&self) -> Depth {
        match (self.output_bits >> 2) & 3 {
            0 => Depth::Mono4,
            1 => Depth::Mono8,
            2 => Depth::Rgb24,
            _ => Depth::Rgb15,
        }
    }

    fn coloured(&self) -> bool {
        matches!(self.depth(), Depth::Rgb24 | Depth::Rgb15)
    }

    // ---- registers ---------------------------------------------------------

    /// 1F801824h read. psx-spx: "MDEC1 - MDEC Status Register (R)".
    pub fn status(&self) -> u32 {
        let mut status = self.remaining as u32;
        status |= (self.block as u32) << 16;
        status |= self.output_bits << 23;
        status |= ((self.enable_out && !self.output.is_empty()) as u32) << 27;
        status |= (self.dma_in_request() as u32) << 28;
        status |= ((self.state != State::Idle) as u32) << 29;
        status |= (self.output.is_empty() as u32) << 31;
        status
    }

    /// 1F801824h write. psx-spx: "MDEC1 - MDEC Control/Reset Register (W)".
    pub fn write_control(&mut self, value: u32) {
        if value & (1 << 31) != 0 {
            // "Abort any command, and set status=80040000h". The quant and
            // scale tables are kept: psx-spx does not say they are cleared.
            let (quant, scale) = (self.quant, self.scale);
            *self = Self { quant, scale, ..Self::default() };
        }
        self.enable_in = value & (1 << 30) != 0;
        self.enable_out = value & (1 << 29) != 0;
    }

    /// DMA0 data request: "set when DMA0 enabled and ready to receive data".
    pub fn dma_in_request(&self) -> bool {
        self.enable_in
    }

    /// DMA1 data request for a transfer of `words`: output is requested
    /// while there is data, and the transfer starts once all it asks for
    /// is there or no more is coming.
    pub fn dma_out_request(&self, words: usize) -> bool {
        self.enable_out && !self.output.is_empty() && (self.output.len() >= words * 4 || self.state != State::Decode)
    }

    /// 1F801820h read: "Macroblock Data (or Garbage if there's no data
    /// available)". Zero stands in for the garbage.
    pub fn read(&mut self) -> u32 {
        u32::from_le_bytes(std::array::from_fn(|_| self.output.pop_front().unwrap_or(0)))
    }

    /// 1F801820h write: a command word or a parameter word.
    pub fn write(&mut self, word: u32) {
        match self.state {
            State::Idle => self.command(word),
            State::Decode => {
                self.halfword(word as u16);
                self.halfword((word >> 16) as u16);
                self.parameter_done();
            }
            // psx-spx: "64 unsigned parameter bytes for the Luminance Quant
            // Table", and with command bit 0 another 64 for colour.
            State::QuantTable { left, at } => {
                for (i, byte) in word.to_le_bytes().into_iter().enumerate() {
                    self.quant[(at + i) / 64][(at + i) % 64] = byte;
                }
                self.state = State::QuantTable { left: left - 1, at: at + 4 };
                self.parameter_done();
            }
            // "64 signed halfwords with 14bit fractional part".
            State::ScaleTable { at } => {
                self.scale[at] = word as i16;
                self.scale[at + 1] = (word >> 16) as i16;
                self.state = State::ScaleTable { at: at + 2 };
                self.parameter_done();
            }
        }
    }

    fn parameter_done(&mut self) {
        self.remaining = self.remaining.wrapping_sub(1);
        if self.remaining == 0xFFFF {
            self.state = State::Idle;
        }
    }

    /// psx-spx: "MDEC Commands".
    fn command(&mut self, word: u32) {
        // "Bit25-28 are copied to STAT.23-26", for every command.
        self.output_bits = (word >> 25) & 0xF;
        match word >> 29 {
            // MDEC(1) - Decode Macroblock(s); bits 0-15 = parameter words.
            1 => {
                self.remaining = (word as u16).wrapping_sub(1);
                self.block = if self.coloured() { BLOCK_CR } else { 4 };
                self.position = None;
                // A count of zero expects nothing (deviation: psx-spx does
                // not describe it).
                if word & 0xFFFF != 0 {
                    self.state = State::Decode;
                }
            }
            // MDEC(2) - Set Quant Table(s): 16 words, or 32 with colour.
            2 => {
                let words = if word & 1 != 0 { 32 } else { 16 };
                self.remaining = words as u16 - 1;
                self.state = State::QuantTable { left: words, at: 0 };
            }
            // MDEC(3) - Set Scale Table: 32 words.
            3 => {
                self.remaining = 31;
                self.state = State::ScaleTable { at: 0 };
            }
            // "MDEC(0) - No function ... Command bits 0-15 are reflected to
            // Status bits 0-15"; "MDEC(4..7) ... act identical as MDEC(0)".
            _ => self.remaining = word as u16,
        }
    }

    // ---- decompression -----------------------------------------------------

    /// One halfword of compressed data. psx-spx: "rl_decode_block".
    fn halfword(&mut self, n: u16) {
        let quant = &self.quant[(self.coloured() && self.block >= BLOCK_CR) as usize];
        let Some(position) = self.position else {
            // "ignore padding (FE00h as first halfword)".
            if n == END_OF_BLOCK {
                return;
            }
            self.coefficients = [0; 64];
            self.quant_scale = ((n >> 10) & 0x3F) as i32;
            // First value: "signed10bit(n AND 3FFh)*qt[k]", or times 2 in
            // the "special mode without qt[k]" (q_scale = 0).
            let value = if self.quant_scale == 0 { signed10(n) * 2 } else { signed10(n) * quant[0] as i32 };
            self.store(0, value);
            self.position = Some(0);
            return;
        };
        // "k=k+((n SHR 10) AND 3Fh)+1 ;skip zerofilled entries".
        let position = position + ((n >> 10) & 0x3F) as usize + 1;
        if position > 63 {
            // "should end with n=FE00h (that sets k>63)".
            self.position = None;
            self.block_done();
            return;
        }
        let value = if self.quant_scale == 0 {
            signed10(n) * 2
        } else {
            (signed10(n) * quant[position] as i32 * self.quant_scale + 4) / 8
        };
        self.store(position, value);
        self.position = Some(position);
    }

    fn store(&mut self, position: usize, value: i32) {
        // "saturate to signed 11bit range".
        let value = value.clamp(-0x400, 0x3FF);
        // "if q_scale>0 then blk[zagzig[k]]=val", "if q_scale=0 then
        // blk[k]=val (special, no zigzag)".
        let index = if self.quant_scale == 0 { position } else { ZAGZIG[position] as usize };
        self.coefficients[index] = value;
    }

    /// psx-spx: "real_idct_core(blk)".
    fn idct(&self) -> [i16; 64] {
        let mut source = self.coefficients;
        let mut target = [0i32; 64];
        for _pass in 0..2 {
            for x in 0..8 {
                for y in 0..8 {
                    let mut sum = 0i32;
                    for z in 0..8 {
                        // "the hardware does actually use only the upper
                        // 13bit of those 16bit values".
                        sum += source[y + z * 8] * (self.scale[x + z * 8] as i32 >> 3);
                    }
                    target[x + y * 8] = (sum + 0xFFF) >> 13;
                }
            }
            std::mem::swap(&mut source, &mut target);
        }
        source.map(|v| v.clamp(i16::MIN as i32, i16::MAX as i32) as i16)
    }

    /// A block has been received. psx-spx: "decode_colored_macroblock" and
    /// "decode_monochrome_macroblock".
    fn block_done(&mut self) {
        let block = self.idct();
        if !self.coloured() {
            self.emit_mono(&block);
            return;
        }
        match self.block {
            BLOCK_CR => {
                self.cr = block;
                self.block = BLOCK_CB;
            }
            BLOCK_CB => {
                self.cb = block;
                self.block = 0;
            }
            n => {
                self.yuv_to_rgb(&block, (n as usize % 2) * 8, (n as usize / 2) * 8);
                if n == 3 {
                    self.emit_colour();
                    self.block = BLOCK_CR;
                } else {
                    self.block = n + 1;
                }
            }
        }
    }

    /// psx-spx: "yuv_to_rgb(xx,yy)". Deviation: "The exact fixed point
    /// resolution for yuv_to_rgb is unknown"; 8 fractional bits with
    /// rounding are used.
    fn yuv_to_rgb(&mut self, luma: &[i16; 64], xx: usize, yy: usize) {
        for y in 0..8 {
            for x in 0..8 {
                let chroma = (x + xx) / 2 + (y + yy) / 2 * 8;
                let (cr, cb) = (self.cr[chroma] as i32, self.cb[chroma] as i32);
                // "G=(-0.3437*B)+(-0.7143*R), R=(1.402*R), B=(1.772*B)".
                let red = (359 * cr + 128) >> 8;
                let green = (-88 * cb - 183 * cr + 128) >> 8;
                let blue = (454 * cb + 128) >> 8;
                let luma = luma[x + y * 8] as i32;
                let clamp = |v: i32| v.clamp(-128, 127) as i16;
                self.pixels[(x + xx) + (y + yy) * 16] = [clamp(luma + red), clamp(luma + green), clamp(luma + blue)];
            }
        }
    }

    /// "if unsigned then ... xor 80h ;aka add 128".
    fn byte(&self, value: i16) -> u8 {
        let signed = self.output_bits & 2 != 0;
        if signed {
            value as u8
        } else {
            value as u8 ^ 0x80
        }
    }

    fn emit_colour(&mut self) {
        let bit15 = (self.output_bits & 1) as u16;
        for i in 0..256 {
            let [r, g, b] = self.pixels[i].map(|v| self.byte(v));
            if self.depth() == Depth::Rgb24 {
                self.output.extend([r, g, b]);
            } else {
                let pixel = (r as u16 >> 3) | ((g as u16 >> 3) << 5) | ((b as u16 >> 3) << 10) | (bit15 << 15);
                self.output.extend(pixel.to_le_bytes());
            }
        }
    }

    /// psx-spx: "y_to_mono".
    fn emit_mono(&mut self, block: &[i16; 64]) {
        // "clip to signed 9bit range", "saturate from 9bit to signed 8bit".
        let bytes = block.map(|y| self.byte((((y as i32) << 23) >> 23).clamp(-128, 127) as i16));
        if self.depth() == Depth::Mono8 {
            self.output.extend(bytes);
        } else {
            // 4 bits per pixel: the upper bits of each value, first pixel
            // in the low nibble (the packing order is not in psx-spx).
            for pair in bytes.chunks(2) {
                self.output.push_back((pair[0] >> 4) | (pair[1] & 0xF0));
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// psx-spx: "set_scale_table" — the matrix every game sends.
    #[rustfmt::skip]
    pub(crate) const SCALE: [u16; 64] = [
        0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x5A82,
        0x7D8A, 0x6A6D, 0x471C, 0x18F8, 0xE707, 0xB8E3, 0x9592, 0x8275,
        0x7641, 0x30FB, 0xCF04, 0x89BE, 0x89BE, 0xCF04, 0x30FB, 0x7641,
        0x6A6D, 0xE707, 0x8275, 0xB8E3, 0x471C, 0x7D8A, 0x18F8, 0x9592,
        0x5A82, 0xA57D, 0xA57D, 0x5A82, 0x5A82, 0xA57D, 0xA57D, 0x5A82,
        0x471C, 0x8275, 0x18F8, 0x6A6D, 0x9592, 0xE707, 0x7D8A, 0xB8E3,
        0x30FB, 0x89BE, 0x7641, 0xCF04, 0xCF04, 0x7641, 0x89BE, 0x30FB,
        0x18F8, 0xB8E3, 0x6A6D, 0x8275, 0x7D8A, 0x9592, 0x471C, 0xE707,
    ];

    /// The words that set quant tables of `luma` and `colour` everywhere and
    /// the standard scale table.
    pub(crate) fn setup_words(luma: u8, colour: u8) -> Vec<u32> {
        let mut words = vec![0x4000_0001];
        words.extend([u32::from_le_bytes([luma; 4]); 16]);
        words.extend([u32::from_le_bytes([colour; 4]); 16]);
        words.push(0x6000_0000);
        words.extend(SCALE.chunks(2).map(|pair| pair[0] as u32 | (pair[1] as u32) << 16));
        words
    }

    fn mdec() -> Mdec {
        let mut mdec = Mdec::default();
        for word in setup_words(2, 2) {
            mdec.write(word);
        }
        mdec
    }

    /// Sends a decode command with the given halfwords, padded to words.
    fn decode(mdec: &mut Mdec, command: u32, halfwords: &[u16]) {
        let mut halfwords = halfwords.to_vec();
        if halfwords.len() % 2 == 1 {
            halfwords.push(END_OF_BLOCK);
        }
        mdec.write(command | (halfwords.len() / 2) as u32);
        for pair in halfwords.chunks(2) {
            mdec.write(pair[0] as u32 | (pair[1] as u32) << 16);
        }
    }

    fn output(mdec: &mut Mdec) -> Vec<u8> {
        mdec.output.drain(..).collect()
    }

    /// A block with only a DC value: quant scale 1, DC `dc`.
    fn dc(dc: i16) -> [u16; 2] {
        [(1 << 10) | (dc as u16 & 0x3FF), END_OF_BLOCK]
    }

    const MONO8: u32 = 0x2800_0000;
    const RGB24: u32 = 0x3000_0000;
    const RGB15: u32 = 0x3800_0000;

    #[test]
    fn reset_and_control() {
        // psx-spx: "Reset MDEC (... set status=80040000h)".
        let mut mdec = Mdec::default();
        mdec.write(0x3800_0010);
        mdec.write_control(0x8000_0000);
        assert_eq!(mdec.status(), 0x8004_0000);
        // Bit 30 enables the data-in request (status bit 28).
        mdec.write_control(0x6000_0000);
        assert_eq!(mdec.status(), 0x9004_0000);
        assert!(mdec.dma_in_request());
        assert!(!mdec.dma_out_request(1), "nothing to send");
    }

    #[test]
    fn commands_show_in_the_status_register() {
        let mut mdec = Mdec::default();
        // MDEC(0): bits 25-28 -> status 23-26, bits 0-15 -> status 0-15.
        mdec.write(0x1E00_1234);
        assert_eq!(mdec.status(), 0x8000_0000 | 0x0780_0000 | 0x0004_0000 | 0x1234);
        // MDEC(1) with 2 parameter words: busy, remaining-1 counts down to
        // FFFFh.
        mdec.write(RGB15 | 2);
        assert_eq!(mdec.status() & 0x2000_FFFF, 0x2000_0001);
        assert_eq!((mdec.status() >> 25) & 3, 3, "depth 15 bit");
        mdec.write(0xFE00_FE00);
        assert_eq!(mdec.status() & 0x2000_FFFF, 0x2000_0000);
        mdec.write(0xFE00_FE00);
        assert_eq!(mdec.status() & 0x2000_FFFF, 0x0000_FFFF, "done");
    }

    #[test]
    fn tables_are_loaded_by_commands_2_and_3() {
        let mut mdec = Mdec::default();
        mdec.write(0x4000_0001);
        assert_eq!(mdec.status() & 0x2000_FFFF, 0x2000_001F);
        for i in 0..32u32 {
            mdec.write(u32::from_le_bytes([4 * i as u8, 4 * i as u8 + 1, 4 * i as u8 + 2, 4 * i as u8 + 3]));
        }
        assert_eq!(mdec.status() & 0x2000_0000, 0);
        assert_eq!(mdec.quant[0][5], 5);
        assert_eq!(mdec.quant[1][5], 69);
        // Luminance only: 16 words.
        mdec.write(0x4000_0000);
        for _ in 0..16 {
            mdec.write(0x0909_0909);
        }
        assert_eq!(mdec.status() & 0x2000_0000, 0);
        assert_eq!((mdec.quant[0][63], mdec.quant[1][5]), (9, 69));

        let mdec = self::mdec();
        assert_eq!(mdec.scale[0], 0x5A82);
        assert_eq!(mdec.scale[63], 0xE707u16 as i16);
    }

    #[test]
    fn run_length_decoding_places_coefficients_in_zigzag_order() {
        // psx-spx: "rl_decode_block".
        let mut mdec = mdec();
        mdec.write(MONO8 | 8);
        // Quant scale 4, DC 10: 10 * qt[0] = 20.
        mdec.halfword((4 << 10) | 10);
        // No skip, AC -3: (-3 * 2 * 4 + 4) / 8 = -2 (rounded toward zero),
        // zigzag position 1 -> index 1.
        mdec.halfword(0x3FD);
        // Skip 1, AC 100: (100 * 2 * 4 + 4) / 8 = 100, position 3 -> index 16.
        mdec.halfword((1 << 10) | 100);
        // Skip 0, AC 511 with saturation: 511 -> 511; position 4 -> index 9.
        mdec.halfword(511);
        assert_eq!(mdec.coefficients[0], 20);
        assert_eq!(mdec.coefficients[1], -2);
        assert_eq!(mdec.coefficients[8], 0, "position 2 skipped");
        assert_eq!(mdec.coefficients[16], 100);
        assert_eq!(mdec.coefficients[9], 511);
        assert_eq!(mdec.position, Some(4));
        // The end code finishes the block.
        mdec.halfword(END_OF_BLOCK);
        assert_eq!(mdec.position, None);
        assert_eq!(mdec.output.len(), 64);
    }

    #[test]
    fn values_saturate_to_11_bits_and_quant_scale_zero_skips_the_tables() {
        let mut mdec = Mdec::default();
        for word in setup_words(255, 255) {
            mdec.write(word);
        }
        mdec.write(MONO8 | 8);
        mdec.halfword((63 << 10) | 0x1FF); // DC 511 * 255
        mdec.halfword(0x200); // AC -512 * 255 * 63 / 8
        assert_eq!((mdec.coefficients[0], mdec.coefficients[1]), (0x3FF, -0x400));
        mdec.halfword(END_OF_BLOCK);
        // "special mode without qt[k]": value * 2, and no zigzag.
        mdec.halfword(5);
        mdec.halfword((1 << 10) | 7);
        assert_eq!((mdec.coefficients[0], mdec.coefficients[2]), (10, 14));
    }

    #[test]
    fn monochrome_block_with_only_dc_is_flat() {
        // DC 100 * qt 2 = 200; the IDCT turns a lone DC into DC/8 = 25;
        // unsigned output adds 128.
        let mut mdec = mdec();
        decode(&mut mdec, MONO8, &dc(100));
        assert_eq!(output(&mut mdec), vec![153; 64]);
        // Signed output (command bit 26): no offset.
        decode(&mut mdec, MONO8 | 0x0400_0000, &dc(-100));
        assert_eq!(output(&mut mdec), vec![(-25i8) as u8; 64]);
        // Saturation: DC 511 * 2 = 1022 -> 127 -> FFh.
        decode(&mut mdec, MONO8, &dc(511));
        assert_eq!(output(&mut mdec), vec![255; 64]);
    }

    #[test]
    fn padding_before_a_block_is_ignored() {
        let mut mdec = mdec();
        decode(&mut mdec, MONO8, &[END_OF_BLOCK, END_OF_BLOCK, dc(100)[0], END_OF_BLOCK]);
        assert_eq!(output(&mut mdec), vec![153; 64]);
    }

    #[test]
    fn an_ac_coefficient_makes_a_gradient() {
        // The first horizontal frequency: brighter on the left, darker on
        // the right, symmetric around the DC level, the same on every row.
        let mut mdec = mdec();
        decode(&mut mdec, MONO8 | 0x0400_0000, &[(4 << 10) | 100, 100, END_OF_BLOCK]);
        let block = output(&mut mdec);
        let row: Vec<i8> = block[..8].iter().map(|b| *b as i8).collect();
        assert!(row.windows(2).all(|w| w[0] > w[1]), "{row:?}");
        for (left, right) in row[..4].iter().zip(row[4..].iter().rev()) {
            assert!(((*left as i32 - 25) + (*right as i32 - 25)).abs() <= 1, "{row:?}");
        }
        assert!(block.chunks(8).all(|r| r == &block[..8]));
    }

    fn colour_macroblock(cr: i16, cb: i16, y: [i16; 4]) -> Vec<u16> {
        [cr, cb, y[0], y[1], y[2], y[3]].iter().flat_map(|v| dc(*v)).collect()
    }

    #[test]
    fn grey_colour_macroblock_at_15_and_24_bits() {
        // psx-spx: "decode_colored_macroblock" — Cr, Cb, Y1..Y4.
        let mut mdec = mdec();
        decode(&mut mdec, RGB24, &colour_macroblock(0, 0, [100; 4]));
        assert_eq!(output(&mut mdec), vec![153; 768]);
        // 15 bit: 153 >> 3 = 19 per channel; command bit 25 sets bit 15.
        decode(&mut mdec, RGB15, &colour_macroblock(0, 0, [100; 4]));
        let pixel = 19u16 | 19 << 5 | 19 << 10;
        assert_eq!(output(&mut mdec), pixel.to_le_bytes().repeat(256));
        decode(&mut mdec, RGB15 | 0x0200_0000, &colour_macroblock(0, 0, [100; 4]));
        assert_eq!(output(&mut mdec)[..2], (pixel | 0x8000).to_le_bytes());
    }

    #[test]
    fn luminance_blocks_fill_the_four_quarters() {
        let mut mdec = mdec();
        decode(&mut mdec, RGB24, &colour_macroblock(0, 0, [0, 40, 80, 120]));
        let picture = output(&mut mdec);
        let red = |x: usize, y: usize| picture[(x + y * 16) * 3];
        assert_eq!([red(0, 0), red(15, 0), red(0, 15), red(15, 15)], [128, 138, 148, 158]);
        assert_eq!([red(7, 7), red(8, 7), red(7, 8), red(8, 8)], [128, 138, 148, 158]);
    }

    #[test]
    fn colour_conversion() {
        // psx-spx: "yuv_to_rgb" — R = Y + 1.402*Cr, G = Y - 0.3437*Cb -
        // 0.7143*Cr, B = Y + 1.772*Cb. Cr = 20 (DC 80), Cb = 0, Y = 0.
        let mut mdec = mdec();
        decode(&mut mdec, RGB24, &colour_macroblock(80, 0, [0; 4]));
        assert_eq!(output(&mut mdec)[..3], [128 + 28, 128 - 14, 128]);
        // Cb = -20: G = +7, B = -35.
        decode(&mut mdec, RGB24, &colour_macroblock(0, -80, [0; 4]));
        assert_eq!(output(&mut mdec)[..3], [128, 128 + 7, 128 - 35]);
        // Saturation of the sum.
        decode(&mut mdec, RGB24, &colour_macroblock(400, 0, [400; 4]));
        assert_eq!(output(&mut mdec)[..3], [255, 128 + 100 - 71, 128 + 100]);
    }

    #[test]
    fn several_macroblocks_in_one_command_and_reading_the_data_register() {
        let mut mdec = mdec();
        let mut data = colour_macroblock(0, 0, [100; 4]);
        data.extend(colour_macroblock(0, 0, [-100; 4]));
        mdec.write_control(0x2000_0000);
        decode(&mut mdec, RGB15, &data);
        assert_eq!(mdec.output.len(), 2 * 512);
        assert_eq!(mdec.status() & 0x8800_0000, 0x0800_0000, "data out request, not empty");
        assert!(mdec.dma_out_request(256));
        assert_eq!(mdec.read(), 0x4E73_4E73);
        for _ in 0..127 {
            mdec.read();
        }
        let dark = 12u32 | 12 << 5 | 12 << 10;
        assert_eq!(mdec.read(), dark | dark << 16);
    }

    #[test]
    fn data_out_request_waits_for_the_whole_transfer() {
        let mut mdec = mdec();
        mdec.write_control(0x2000_0000);
        let data = colour_macroblock(0, 0, [100; 4]);
        // The command announces more data than has arrived.
        mdec.write(RGB15 | 12);
        for pair in data.chunks(2) {
            mdec.write(pair[0] as u32 | (pair[1] as u32) << 16);
        }
        assert!(mdec.dma_out_request(128), "one macroblock is there");
        assert!(!mdec.dma_out_request(256), "the second is still to come");
        for _ in 0..6 {
            mdec.write(0xFE00_FE00);
        }
        assert!(mdec.dma_out_request(256), "nothing more is coming");
    }
}
