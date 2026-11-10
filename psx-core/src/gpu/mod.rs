//! Graphics Processing Unit.
//!
//! psx-spx: "Graphics Processing Unit (GPU)".
//!
//! Models a v2 GPU (CXD8561, "LATE-PU-8 and up") with 1 MB of VRAM. All
//! commands execute instantly: there is no command FIFO and no drawing
//! time, so the busy bits of the status register only reflect whether a
//! command is still waiting for parameters or data.
//!
//! The rasteriser (triangles, rectangles, lines, pixel blending) lives in
//! `raster.rs`; this file holds the registers, the GP0/GP1 decoders, the
//! VRAM transfers and the display output.

mod raster;

use crate::video::Video;
use raster::Vertex;

pub const VRAM_WIDTH: usize = 1024;
pub const VRAM_HEIGHT: usize = 512;

// psx-spx: "GP0(E1h) - Draw Mode setting (aka "TPage")".
const MODE_DITHER: u32 = 1 << 9;
const MODE_DRAW_TO_DISPLAY: u32 = 1 << 10;
const MODE_FLIP_X: u32 = 1 << 12;
const MODE_FLIP_Y: u32 = 1 << 13;
/// Bits a textured polygon's tpage attribute replaces: psx-spx "TPage
/// Attribute" — bits 0-8 and 11; 9-10 and 12-13 are left alone.
const MODE_TPAGE_ATTRIBUTE: u32 = 0x09FF;
/// Bits 0-13 exist.
const MODE_WRITABLE: u32 = 0x3FFF;
/// Bit 11, "Texture page Y Base 2": only writable after GP1(09h) allowed it.
const MODE_Y_BASE_2: u32 = 1 << 11;

// psx-spx: "GP1(08h) - Display mode".
const DISPLAY_480: u32 = 1 << 2;
const DISPLAY_24BIT: u32 = 1 << 4;
const DISPLAY_INTERLACE: u32 = 1 << 5;

/// What the GP0 port expects next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gp0State {
    /// A command word, or the remaining parameters of the one in `buffer`.
    Command,
    /// Further vertices of a polyline. `gouraud` lines alternate colour and
    /// vertex words.
    Polyline { gouraud: bool, blend: bool, previous: Vertex, pending_color: Option<u32> },
    /// Pixel data of a CPU-to-VRAM transfer.
    Load(Transfer),
}

/// Rectangle being transferred to or from the CPU, and the next position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Transfer {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    col: u32,
    row: u32,
}

impl Transfer {
    fn new(position: u32, size: u32) -> Self {
        // psx-spx: "Masking for COPY Commands parameters".
        Self {
            x: position & 0x3FF,
            y: (position >> 16) & 0x1FF,
            width: ((size & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1,
            height: (((size >> 16) & 0xFFFF).wrapping_sub(1) & 0x1FF) + 1,
            col: 0,
            row: 0,
        }
    }

    /// VRAM index of the current pixel. psx-spx: "Wrapping" — transfers
    /// wrap to the opposite edge without carry between X and Y.
    fn index(&self) -> usize {
        let x = (self.x + self.col) as usize % VRAM_WIDTH;
        let y = (self.y + self.row) as usize % VRAM_HEIGHT;
        y * VRAM_WIDTH + x
    }

    /// Moves to the next pixel; returns false when the rectangle is done.
    fn advance(&mut self) -> bool {
        self.col += 1;
        if self.col == self.width {
            self.col = 0;
            self.row += 1;
        }
        self.row < self.height
    }
}

/// The picture the GPU is sending to the television.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    /// `width * height` pixels, 3 bytes each (R, G, B).
    pub rgb: Vec<u8>,
}

pub struct Gpu {
    vram: Box<[u16]>,
    pub video: Video,

    // ---- rendering attributes (GP0 E1h..E6h) ----
    /// GP0(E1h) bits 0-13.
    draw_mode: u32,
    /// GP0(E2h) bits 0-19.
    texture_window: u32,
    /// GP0(E3h)/(E4h) raw values and decoded corners (inclusive).
    area_top_left: u32,
    area_bottom_right: u32,
    clip_x1: i32,
    clip_y1: i32,
    clip_x2: i32,
    clip_y2: i32,
    /// GP0(E5h) raw value and decoded offsets.
    draw_offset: u32,
    offset_x: i32,
    offset_y: i32,
    /// GP0(E6h): force bit 15 on written pixels / protect pixels with bit 15.
    mask_set: bool,
    mask_check: bool,

    // ---- display control (GP1) ----
    display_disabled: bool,
    dma_direction: u32,
    display_x: u32,
    display_y: u32,
    display_mode: u32,
    /// GP1(09h) bit 0: GP0(E1h) bit 11 may be set.
    allow_y_base_2: bool,
    irq: bool,
    /// An IRQ1 edge the bus has not collected yet.
    irq_edge: bool,

    // ---- GP0 port ----
    state: Gp0State,
    /// Command word and parameters received so far.
    buffer: Vec<u32>,
    /// VRAM-to-CPU transfer in progress.
    store: Option<Transfer>,
    /// psx-spx: "GPU Texture Caching" — "a CLUT cache that is preserved
    /// between GPU drawing commands". Holds the palette last used, and which
    /// one that was: its CLUT attribute and whether all 256 entries (8-bit
    /// textures) or only the first 16 (4-bit) were loaded.
    clut_cache: [u16; 256],
    clut_cache_key: Option<(u32, bool)>,
    /// Value returned by GPUREAD outside of a transfer (GP1(10h) latch).
    read_latch: u32,
    /// GP0 words received since power-on (statistics for the harness).
    gp0_words: u64,
}

impl Default for Gpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Gpu {
    pub fn new() -> Self {
        let mut gpu = Self {
            // Real VRAM powers up with garbage; zero is deterministic.
            vram: vec![0; VRAM_WIDTH * VRAM_HEIGHT].into_boxed_slice(),
            video: Video::default(),
            draw_mode: 0,
            texture_window: 0,
            area_top_left: 0,
            area_bottom_right: 0,
            clip_x1: 0,
            clip_y1: 0,
            clip_x2: 0,
            clip_y2: 0,
            draw_offset: 0,
            offset_x: 0,
            offset_y: 0,
            mask_set: false,
            mask_check: false,
            display_disabled: true,
            dma_direction: 0,
            display_x: 0,
            display_y: 0,
            display_mode: 0,
            allow_y_base_2: false,
            irq: false,
            irq_edge: false,
            state: Gp0State::Command,
            buffer: Vec::with_capacity(16),
            store: None,
            clut_cache: [0; 256],
            clut_cache_key: None,
            read_latch: 0,
            gp0_words: 0,
        };
        gpu.reset();
        gpu
    }

    /// psx-spx: "GP1(00h) - Reset GPU".
    fn reset(&mut self) {
        self.reset_command_buffer();
        self.irq = false;
        self.display_disabled = true;
        self.dma_direction = 0;
        self.display_x = 0;
        self.display_y = 0;
        self.display_mode = 0;
        self.video.reset_registers();
        // "GP0(E1h..E6h) ;rendering attributes (0)".
        for command in 0xE1..=0xE6u32 {
            self.set_attribute(command << 24);
        }
    }

    /// psx-spx: "GP1(01h) - Reset Command Buffer": "Resets the command
    /// buffer and CLUT cache."
    fn reset_command_buffer(&mut self) {
        self.state = Gp0State::Command;
        self.buffer.clear();
        self.store = None;
        self.clut_cache_key = None;
    }

    /// Stores GP0(E1h) bits, or the tpage attribute of a textured polygon
    /// (`mask` selects which bits the source may change).
    fn set_draw_mode(&mut self, value: u32, mask: u32) {
        let mut mode = (self.draw_mode & !mask) | (value & mask);
        // psx-spx: "GP1(09h)" controls "whether or not GP0(E1h).bit11 can be
        // used". On hardware (ps1-tests gpu/gp0-e1) the bit reads back as 0
        // when written while not allowed; a bit set earlier stays until the
        // next write.
        if !self.allow_y_base_2 && mask & MODE_Y_BASE_2 != 0 {
            mode &= !MODE_Y_BASE_2;
        }
        self.draw_mode = mode;
    }

    // ---- inspection --------------------------------------------------------

    /// The whole 1024x512 VRAM, row by row.
    pub fn vram(&self) -> &[u16] {
        &self.vram
    }

    pub fn gp0_words(&self) -> u64 {
        self.gp0_words
    }

    /// Returns true once after GP0(1Fh) raised IRQ1 (for the interrupt
    /// controller, which is edge triggered).
    pub fn take_irq(&mut self) -> bool {
        std::mem::take(&mut self.irq_edge)
    }

    // ---- status ------------------------------------------------------------

    /// psx-spx: "GPU Status Register".
    pub fn status(&self) -> u32 {
        let mode = self.display_mode;
        let mut status = self.draw_mode & 0x7FF; // bits 0-10
        status |= (self.mask_set as u32) << 11;
        status |= (self.mask_check as u32) << 12;
        // Bit 13: "Interlace Field (or, always 1 when GP1(08h).5=0)".
        let interlaced = mode & DISPLAY_INTERLACE != 0;
        status |= (if interlaced { self.video.field() } else { 1 }) << 13;
        status |= ((mode >> 7) & 1) << 14;
        status |= ((self.draw_mode >> 11) & 1) << 15;
        status |= ((mode >> 6) & 1) << 16;
        status |= (mode & 0x3F) << 17; // HDS, VDS, NPB, LBS, IRS -> bits 17-22
        status |= (self.display_disabled as u32) << 23;
        status |= (self.irq as u32) << 24;

        // "Ready Bits". Bit 26: ready to receive a command word; bit 27:
        // VRAM-to-CPU data available; bit 28: ready to receive (the FIFO is
        // never full here because commands run instantly).
        let ready_for_command = self.state == Gp0State::Command && self.buffer.is_empty();
        let read_ready = self.store.is_some();
        let fifo_empty = true;
        status |= (ready_for_command as u32) << 26;
        status |= (read_ready as u32) << 27;
        status |= (fifo_empty as u32) << 28;
        // Bit 25: "DMA / Data Request, meaning depends on GP1(04h)".
        let dreq = match self.dma_direction {
            0 => false,
            1 => true, // FIFO not full
            2 => fifo_empty,
            _ => read_ready,
        };
        status |= (dreq as u32) << 25;
        status |= self.dma_direction << 29;

        // Bit 31: "In 480-lines mode, bit31 changes per frame. And in
        // 240-lines mode, the bit changes per scanline. The bit is always
        // zero during Vblank".
        if !self.video.in_vblank() {
            let odd = if self.interlaced_480() { self.video.field() } else { self.video.line() & 1 };
            status |= odd << 31;
        }
        status
    }

    fn interlaced_480(&self) -> bool {
        let both = DISPLAY_480 | DISPLAY_INTERLACE;
        self.display_mode & both == both
    }

    // ---- GP1 ---------------------------------------------------------------

    /// psx-spx: "GPU Display Control Commands (GP1)".
    pub fn gp1(&mut self, word: u32) {
        // "GP1(40h..FFh) - N/A (Mirrors) of GP1(00h..3Fh)".
        match (word >> 24) & 0x3F {
            0x00 => self.reset(),
            0x01 => self.reset_command_buffer(),
            0x02 => self.irq = false,
            0x03 => self.display_disabled = word & 1 != 0,
            0x04 => self.dma_direction = word & 3,
            0x05 => {
                self.display_x = word & 0x3FF;
                self.display_y = (word >> 10) & 0x1FF;
            }
            0x06 => self.video.set_horizontal_range(word & 0xFFF, (word >> 12) & 0xFFF),
            0x07 => self.video.set_vertical_range(word & 0x3FF, (word >> 10) & 0x3FF),
            0x08 => {
                self.display_mode = word & 0xFF;
                self.video.set_display_mode(word);
            }
            // "GP1(10h) - Read GPU internal register" and its mirrors.
            0x10..=0x1F => {
                // v2 GPU: "10h-FFFFFFh = Mirrors of 00h..0Fh".
                match word & 0xF {
                    0x2 => self.read_latch = self.texture_window,
                    0x3 => self.read_latch = self.area_top_left,
                    0x4 => self.read_latch = self.area_bottom_right,
                    0x5 => self.read_latch = self.draw_offset,
                    0x7 => self.read_latch = 2, // "Read GPU version (1 or 2)"
                    0x8 => self.read_latch = 0,
                    // "Returns Nothing (old value in GP0.read remains unchanged)".
                    _ => {}
                }
            }
            // "GP1(09h) - Set VRAM size (v2)". Only 1 MB is fitted, so the
            // only visible effect is on GP0(E1h) bit 11. "GP1(09h) is NOT
            // affected by the reset command."
            0x09 => self.allow_y_base_2 = word & 1 != 0,
            // GP1(0Bh) unknown, and the unused numbers: ignored.
            _ => {}
        }
    }

    // ---- GP0 ---------------------------------------------------------------

    /// GPUREAD: "Receive responses to GP0(C0h) and GP1(10h) commands".
    pub fn read(&mut self) -> u32 {
        let Some(mut transfer) = self.store else { return self.read_latch };
        // "VRAM to CPU blitting": two pixels per word, "when the number of
        // halfwords is odd, an extra halfword is added at the end".
        let mut word = 0;
        let mut more = true;
        for half in 0..2 {
            if more {
                word |= (self.vram[transfer.index()] as u32) << (16 * half);
                more = transfer.advance();
            }
        }
        self.store = more.then_some(transfer);
        self.read_latch = word;
        word
    }

    /// Writes a word to the GP0 port.
    /// psx-spx: "GPU Command Summary".
    pub fn gp0(&mut self, word: u32) {
        self.gp0_words += 1;
        match self.state {
            Gp0State::Load(transfer) => self.load_word(transfer, word),
            Gp0State::Polyline { .. } => self.polyline_word(word),
            Gp0State::Command => {
                self.buffer.push(word);
                let command = self.buffer[0];
                if self.buffer.len() == Self::command_length(command) {
                    let words = std::mem::take(&mut self.buffer);
                    self.execute(&words);
                    self.buffer = words;
                    self.buffer.clear();
                }
            }
        }
    }

    /// Number of words (command included) a command needs before it runs.
    /// Polylines run after their first two vertices and then stay open.
    fn command_length(command: u32) -> usize {
        let op = command >> 24;
        let textured = (command >> 26) & 1 != 0;
        let gouraud = (command >> 28) & 1 != 0;
        match op >> 5 {
            // Polygons: each vertex has a position, plus a texcoord if
            // textured, plus a colour if shaded (the first colour is in the
            // command word itself).
            1 => {
                let vertices = if (command >> 27) & 1 != 0 { 4 } else { 3 };
                let per_vertex = 1 + textured as usize + gouraud as usize;
                vertices * per_vertex + !gouraud as usize
            }
            // Lines: two vertices, with a second colour if shaded.
            2 => 3 + gouraud as usize,
            // Rectangles: position, texcoord if textured, size if variable.
            3 => 2 + textured as usize + ((op >> 3) & 3 == 0) as usize,
            // VRAM-to-VRAM: source, destination, size.
            4 => 4,
            // CPU-to-VRAM and VRAM-to-CPU: position, size.
            5 | 6 => 3,
            // "Quick Rectangle Fill": position, size.
            0 if op == 0x02 => 3,
            // Everything else is a single word.
            _ => 1,
        }
    }

    fn execute(&mut self, words: &[u32]) {
        let command = words[0];
        let op = command >> 24;
        match op >> 5 {
            0 => match op {
                // "Clear Cache". psx-spx: "The CLUT cache is invalidated ...
                // when GP0(01h) is issued." The texture cache itself is not
                // emulated.
                0x01 => self.clut_cache_key = None,
                0x02 => self.fill(command, words[1], words[2]),
                // "GP0(1Fh) - Interrupt Request (IRQ1)".
                0x1F => {
                    if !self.irq {
                        self.irq_edge = true;
                    }
                    self.irq = true;
                }
                // GP0(00h) NOP and its mirrors; GP0(03h) unknown.
                _ => {}
            },
            1 => self.polygon(words),
            2 => self.line_command(words),
            3 => self.rectangle(words),
            4 => self.copy(words[1], words[2], words[3]),
            5 => self.state = Gp0State::Load(Transfer::new(words[1], words[2])),
            6 => self.store = Some(Transfer::new(words[1], words[2])),
            _ => self.set_attribute(command),
        }
    }

    /// GP0(E1h..E6h). psx-spx: "GPU Rendering Attributes".
    fn set_attribute(&mut self, word: u32) {
        match word >> 24 {
            0xE1 => self.set_draw_mode(word, MODE_WRITABLE),
            0xE2 => self.texture_window = word & 0xF_FFFF,
            0xE3 => {
                // v2 GPU: 10-bit X, 10-bit Y. Only 512 lines exist.
                self.area_top_left = word & 0xF_FFFF;
                self.clip_x1 = (word & 0x3FF) as i32;
                self.clip_y1 = ((word >> 10) & 0x1FF) as i32;
            }
            0xE4 => {
                self.area_bottom_right = word & 0xF_FFFF;
                self.clip_x2 = (word & 0x3FF) as i32;
                self.clip_y2 = ((word >> 10) & 0x1FF) as i32;
            }
            0xE5 => {
                self.draw_offset = word & 0x3F_FFFF;
                self.offset_x = sign_extend_11(word);
                self.offset_y = sign_extend_11(word >> 11);
            }
            0xE6 => {
                self.mask_set = word & 1 != 0;
                self.mask_check = word & 2 != 0;
            }
            // GP0(E0h), GP0(E7h..EFh): "Mirrors of GP0(00h) - NOP (?)".
            _ => {}
        }
    }

    // ---- VRAM transfers ----------------------------------------------------

    /// psx-spx: "Quick Rectangle Fill" and "Masking and Rounding for FILL
    /// Command parameters".
    fn fill(&mut self, color: u32, position: u32, size: u32) {
        let x0 = (position & 0x3F0) as usize;
        let y0 = ((position >> 16) & 0x1FF) as usize;
        let width = (((size & 0x3FF) + 0x0F) & !0x0F) as usize;
        let height = ((size >> 16) & 0x1FF) as usize;
        // "converts the 24bit RGB value to 15bit RGB by dropping the lower 3
        // bits of each color value and additionally sets the mask bit to 0".
        let pixel = rgb24_to_15(color);
        // "Fill does NOT occur when Xsiz=0 or Ysiz=0"; "Rectangle filling is
        // not affected by the GP0(E6h) mask setting".
        for row in 0..height {
            let y = (y0 + row) % VRAM_HEIGHT;
            // psx-spx: "the Vertical Interlace flag DOES affect GP0 draw
            // commands". The fill follows the same rule as the primitives:
            // otherwise a program that clears and redraws every field (the
            // OpenBIOS shell does) would erase the field on screen and only
            // ever redraw the other one.
            if self.skip_line(y as i32) {
                continue;
            }
            for col in 0..width {
                let x = (x0 + col) % VRAM_WIDTH;
                self.vram[y * VRAM_WIDTH + x] = pixel;
            }
        }
    }

    /// psx-spx: "VRAM to VRAM blitting": "The transfer is affected by Mask
    /// setting."
    fn copy(&mut self, source: u32, destination: u32, size: u32) {
        let src = Transfer::new(source, size);
        let dst = Transfer::new(destination, size);
        // psx-spx does not describe overlapping copies. The source is read
        // a line at a time before being written, which is what other
        // emulators do for whole lines; ps1-tests gpu/vram-to-vram-overlap
        // is the reference for the finer behaviour.
        let mut line = vec![0u16; src.width as usize];
        for row in 0..src.height {
            for (col, pixel) in line.iter_mut().enumerate() {
                let t = Transfer { col: col as u32, row, ..src };
                *pixel = self.vram[t.index()];
            }
            for (col, pixel) in line.iter().enumerate() {
                let t = Transfer { col: col as u32, row, ..dst };
                self.write_masked(t.index(), *pixel);
            }
        }
    }

    /// One data word of a "CPU to VRAM blitting": two pixels.
    fn load_word(&mut self, mut transfer: Transfer, word: u32) {
        let mut more = true;
        for half in 0..2 {
            if more {
                self.write_masked(transfer.index(), (word >> (16 * half)) as u16);
                more = transfer.advance();
            }
        }
        // "If the number of halfwords to be sent is odd, an extra halfword
        // should be sent": the surplus half of the last word is dropped.
        self.state = if more { Gp0State::Load(transfer) } else { Gp0State::Command };
    }

    /// Writes a pixel under the GP0(E6h) mask rules, as VRAM transfers do
    /// ("it acts on the separate halfwords, ie. as for 15bit textures").
    fn write_masked(&mut self, index: usize, pixel: u16) {
        if self.mask_check && self.vram[index] & 0x8000 != 0 {
            return;
        }
        self.vram[index] = pixel | ((self.mask_set as u16) << 15);
    }

    // ---- display output ----------------------------------------------------

    /// Size in pixels of the picture described by the display registers.
    pub fn display_size(&self) -> (usize, usize) {
        // psx-spx: "GP1(06h)": "The number of displayed pixels per line is
        // (((X2-X1)/cycles_per_pix)+2) AND NOT 3".
        let (x1, x2) = self.video.horizontal_range();
        let width = ((x2.saturating_sub(x1) / self.video.dot_divider()) + 2) & !3;
        // "GP1(07h)": "The number of lines is Y2-Y1"; twice that in
        // 480-line interlaced mode, where each field supplies half.
        let (y1, y2) = self.video.vertical_range();
        let lines = y2.saturating_sub(y1);
        let height = if self.interlaced_480() { lines * 2 } else { lines };
        (width as usize, height as usize)
    }

    /// The displayed part of VRAM as RGB, black while the display is off.
    /// psx-spx: "GP1(05h) - Start of Display area", "Framebuffer".
    pub fn frame(&self) -> Frame {
        let (width, height) = self.display_size();
        let mut rgb = vec![0u8; width * height * 3];
        if self.display_disabled {
            return Frame { width, height, rgb };
        }
        let (x0, y0) = (self.display_x as usize, self.display_y as usize);
        for row in 0..height {
            let line = ((y0 + row) % VRAM_HEIGHT) * VRAM_WIDTH;
            for col in 0..width {
                let out = &mut rgb[(row * width + col) * 3..][..3];
                if self.display_mode & DISPLAY_24BIT != 0 {
                    // "24bit pixels occupy 3 bytes ... each 6 bytes contain
                    // two 24bit pixels".
                    let byte = |n: usize| {
                        let half = self.vram[line + (x0 + n / 2) % VRAM_WIDTH];
                        (half >> (8 * (n % 2))) as u8
                    };
                    out.copy_from_slice(&[byte(col * 3), byte(col * 3 + 1), byte(col * 3 + 2)]);
                } else {
                    out.copy_from_slice(&rgb15_to_24(self.vram[line + (x0 + col) % VRAM_WIDTH]));
                }
            }
        }
        Frame { width, height, rgb }
    }
}

/// psx-spx: "Vertex" — 11-bit signed coordinates; the upper bits are
/// "ignored by hardware".
fn sign_extend_11(value: u32) -> i32 {
    ((value << 21) as i32) >> 21
}

/// 24-bit command colour to a 15-bit pixel, "dropping the lower 3 bits of
/// each color value".
fn rgb24_to_15(color: u32) -> u16 {
    let r = (color >> 3) & 0x1F;
    let g = (color >> 11) & 0x1F;
    let b = (color >> 19) & 0x1F;
    (r | (g << 5) | (b << 10)) as u16
}

/// 15-bit pixel to 8-bit-per-channel RGB for output. The 5-bit value is
/// replicated into the low bits so that 1Fh becomes FFh.
pub fn rgb15_to_24(pixel: u16) -> [u8; 3] {
    let expand = |c: u16| (((c & 0x1F) << 3) | ((c & 0x1F) >> 2)) as u8;
    [expand(pixel), expand(pixel >> 5), expand(pixel >> 10)]
}

#[cfg(test)]
mod tests;
