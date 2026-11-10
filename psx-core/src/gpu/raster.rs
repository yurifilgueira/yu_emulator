//! Software rasteriser: polygons, rectangles, lines and the per-pixel
//! pipeline (texturing, modulation, dithering, semi-transparency, masking).
//!
//! psx-spx: "GPU Render Polygon Commands", "GPU Render Line Commands",
//! "GPU Render Rectangle Commands", "GPU Rendering Attributes", "GPU (MISC)".
//!
//! psx-spx specifies *what* is drawn (fill rules, formulas, limits) but not
//! the fixed-point arithmetic the hardware uses to walk edges and to
//! interpolate colours and texture coordinates, and that arithmetic decides
//! individual pixels. Where psx-spx is silent, the stepping below follows
//! the scheme used by the Mednafen/DuckStation software renderers, which
//! were tuned against hardware. It is reproduced from memory, not copied,
//! so treat ps1-tests' reference images as the judge, not this comment.

use super::{sign_extend_11, Gp0State, Gpu, DISPLAY_480, DISPLAY_INTERLACE, MODE_DITHER};
use super::{MODE_DRAW_TO_DISPLAY, MODE_FLIP_X, MODE_FLIP_Y, MODE_TPAGE_ATTRIBUTE, VRAM_HEIGHT, VRAM_WIDTH};

/// A vertex in VRAM coordinates with its colour and texture coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Vertex {
    x: i32,
    y: i32,
    r: i32,
    g: i32,
    b: i32,
    u: i32,
    v: i32,
}

impl Vertex {
    fn with_color(mut self, color: u32) -> Self {
        self.r = (color & 0xFF) as i32;
        self.g = ((color >> 8) & 0xFF) as i32;
        self.b = ((color >> 16) & 0xFF) as i32;
        self
    }

    /// The interpolated attributes: r, g, b, u, v.
    fn attributes(&self) -> [i32; 5] {
        [self.r, self.g, self.b, self.u, self.v]
    }
}

/// Where texels come from. psx-spx: "Texture Bitmaps", "Clut Attribute".
#[derive(Debug, Clone, Copy)]
struct Texture {
    page_x: usize,
    page_y: usize,
    /// 0 = 4-bit CLUT, 1 = 8-bit CLUT, 2 = 15-bit direct.
    depth: u32,
    clut_x: usize,
    clut_y: usize,
}

/// How the pixels of one primitive are produced.
#[derive(Debug, Clone, Copy)]
struct Style {
    texture: Option<Texture>,
    /// TGE: "Texture Color Mode (0=Shaded/Modulated, 1=Raw)".
    raw: bool,
    /// Semi-transparency mode (GP0(E1h) bits 5-6) when ABE is set.
    blend: Option<u32>,
    dither: bool,
}

/// psx-spx: "24bit RGB to 15bit RGB Dithering" — offsets per position in
/// each 4x4 block of VRAM.
const DITHER: [[i32; 4]; 4] = [[-4, 0, -3, 1], [2, -2, 3, -1], [-3, 1, -4, 0], [3, -1, 2, -2]];

/// Fractional bits of interpolated attributes (colours and texcoords).
const ATTR_FRACTION: u32 = 24;

impl Gpu {
    // ---- pixel pipeline ----------------------------------------------------

    fn texture(&self, clut: u32) -> Texture {
        Texture {
            // psx-spx: GP0(E1h) bits 0-3 "Texture page X Base (N*64)", bit 4
            // "Y Base 1 (N*256)". Bit 11 (Y base 2) needs 2 MB of VRAM,
            // which retail consoles do not have, and is ignored.
            page_x: (self.draw_mode & 0xF) as usize * 64,
            page_y: ((self.draw_mode >> 4) & 1) as usize * 256,
            // "Texture page colors setting 3 (reserved) is same as setting 2".
            depth: ((self.draw_mode >> 7) & 3).min(2),
            // "Clut Attribute": X in 16-halfword steps, Y in lines.
            clut_x: (clut & 0x3F) as usize * 16,
            clut_y: ((clut >> 6) & 0x1FF) as usize,
        }
    }

    /// Builds the texture description for a primitive and makes sure the
    /// CLUT cache holds its palette.
    ///
    /// psx-spx: "GPU Texture Caching" — "The CLUT cache is invalidated when
    /// different CLUT index values are used or when GP0(01h) is issued."
    /// So a palette changed in VRAM is not seen until one of those happens.
    /// Not in psx-spx, from the reference image of ps1-tests gpu/clut-cache:
    /// a 4-bit texture loads 16 entries and an 8-bit one 256, so going from
    /// 4-bit to 8-bit with the same CLUT reloads it, while going from 8-bit
    /// to 4-bit keeps using the cached entries.
    fn prepare_texture(&mut self, clut: u32) -> Texture {
        let texture = self.texture(clut);
        if texture.depth < 2 {
            let clut = clut & 0x7FFF;
            let full = texture.depth == 1;
            let cached = matches!(self.clut_cache_key, Some((c, has_full)) if c == clut && (has_full || !full));
            if !cached {
                let entries = if full { 256 } else { 16 };
                for index in 0..entries {
                    let x = (texture.clut_x + index) % VRAM_WIDTH;
                    self.clut_cache[index] = self.vram[texture.clut_y * VRAM_WIDTH + x];
                }
                self.clut_cache_key = Some((clut, full));
            }
        }
        texture
    }

    /// Reads a texel. Deviation: the texture cache (psx-spx: "GPU Texture
    /// Caching") is not emulated; texture pages are read from VRAM directly.
    fn texel(&self, texture: &Texture, u: i32, v: i32) -> u16 {
        // psx-spx: "GP0(E2h) - Texture Window setting":
        // Texcoord = (Texcoord AND (NOT (Mask*8))) OR ((Offset AND Mask)*8).
        let window = self.texture_window as i32;
        let (mask_x, mask_y) = ((window & 0x1F) * 8, ((window >> 5) & 0x1F) * 8);
        let (offset_x, offset_y) = (((window >> 10) & 0x1F) * 8, ((window >> 15) & 0x1F) * 8);
        // Texture coordinates are 8 bits wide ("the hardware clips their
        // texture coordinates to 8bit").
        let u = (((u & 0xFF) & !mask_x) | (offset_x & mask_x)) as usize;
        let v = (((v & 0xFF) & !mask_y) | (offset_y & mask_y)) as usize;

        let row = ((texture.page_y + v) % VRAM_HEIGHT) * VRAM_WIDTH;
        let at = |x: usize| self.vram[row + x % VRAM_WIDTH];
        let clut = |index: u16| self.clut_cache[index as usize];
        match texture.depth {
            // "4bit Texture": four palette indices per halfword, left first.
            0 => clut((at(texture.page_x + u / 4) >> ((u & 3) * 4)) & 0xF),
            // "8bit Texture": two palette indices per halfword.
            1 => clut((at(texture.page_x + u / 2) >> ((u & 1) * 8)) & 0xFF),
            _ => at(texture.page_x + u),
        }
    }

    /// Produces and writes one pixel of a primitive. `r`, `g`, `b` are the
    /// 8-bit colour at this pixel; `u`, `v` its texture coordinate.
    #[allow(clippy::too_many_arguments)]
    fn shade(&mut self, style: &Style, x: i32, y: i32, r: i32, g: i32, b: i32, u: i32, v: i32) {
        let offset = if style.dither { DITHER[(y & 3) as usize][(x & 3) as usize] } else { 0 };
        // "the corresponding dither offset is added to the 8bit R/G/B
        // values, the result is saturated ... and then divided by 8".
        let to5 = |value: i32| ((value + offset) >> 3).clamp(0, 31) as u16;

        let Some(texture) = &style.texture else {
            // Untextured: "for untextured polygons, bit15 is set to zero";
            // the whole primitive is semi-transparent when ABE is set.
            let pixel = to5(r) | (to5(g) << 5) | (to5(b) << 10);
            self.plot(x, y, pixel, style.blend, false);
            return;
        };

        let texel = self.texel(texture, u, v);
        // "Color 0000h = Fully-transparent".
        if texel == 0 {
            return;
        }
        let pixel = if style.raw {
            texel
        } else {
            // psx-spx: "Modulation": texel * colour / 128 per 8-bit channel,
            // i.e. (5-bit texel * colour) / 16 as an 8-bit value ("GPU
            // Versions": v2 "Shaded Textures (color*texel)/16"), saturated.
            let channel = |shift: u32, colour: i32| to5((((texel >> shift) & 0x1F) as i32 * colour) >> 4);
            (texel & 0x8000) | channel(0, r) | (channel(5, g) << 5) | (channel(10, b) << 10)
        };
        // "if textured: texture bit15 must also be set" for semi-transparency.
        let blend = if texel & 0x8000 != 0 { style.blend } else { None };
        self.plot(x, y, pixel, blend, true);
    }

    /// Writes a pixel, applying semi-transparency and the mask settings.
    /// The caller has already clipped `x`, `y` to the drawing area.
    fn plot(&mut self, x: i32, y: i32, pixel: u16, blend: Option<u32>, textured: bool) {
        let index = (y as usize % VRAM_HEIGHT) * VRAM_WIDTH + (x as usize % VRAM_WIDTH);
        let background = self.vram[index];
        // psx-spx: "GP0(E6h)": "any (old) pixels in the framebuffer with
        // bit15=1 are write-protected".
        if self.mask_check && background & 0x8000 != 0 {
            return;
        }
        let color = match blend {
            Some(mode) => blend_pixel(background, pixel, mode),
            None => pixel & 0x7FFF,
        };
        // "the upper bit of the data written to the framebuffer is equal to
        // bit15 of the texture color ... (for untextured polygons, bit15 is
        // set to zero)", or forced to 1 by GP0(E6h).0.
        let texture_mask = if textured { pixel & 0x8000 } else { 0 };
        self.vram[index] = color | texture_mask | ((self.mask_set as u16) << 15);
    }

    /// psx-spx: "GP1(08h)": "the Vertical Interlace flag DOES affect GP0
    /// draw commands". psx-spx does not say how. As commonly emulated: in
    /// 480-line interlaced mode, unless drawing to the display area is
    /// allowed (GP0(E1h).10), lines of the field being displayed are not
    /// drawn.
    fn skip_line(&self, y: i32) -> bool {
        let interlaced = DISPLAY_480 | DISPLAY_INTERLACE;
        self.display_mode & interlaced == interlaced
            && self.draw_mode & MODE_DRAW_TO_DISPLAY == 0
            && (y as u32 & 1) == ((self.display_y + self.video.field()) & 1)
    }

    fn blend_mode(&self, command: u32) -> Option<u32> {
        // ABE (bit 25) selects semi-transparency; the mode is GP0(E1h).5-6.
        (command & (1 << 25) != 0).then_some((self.draw_mode >> 5) & 3)
    }

    fn vertex(&self, position: u32) -> Vertex {
        Vertex {
            x: sign_extend_11(position) + self.offset_x,
            y: sign_extend_11(position >> 16) + self.offset_y,
            ..Vertex::default()
        }
    }

    // ---- polygons ----------------------------------------------------------

    /// psx-spx: "GPU Render Polygon Commands".
    pub(super) fn polygon(&mut self, words: &[u32]) {
        let command = words[0];
        let raw = command & (1 << 24) != 0;
        let textured = command & (1 << 26) != 0;
        let quad = command & (1 << 27) != 0;
        let gouraud = command & (1 << 28) != 0;

        let count = if quad { 4 } else { 3 };
        let mut vertices = [Vertex::default(); 4];
        let (mut clut, mut next) = (0, 1);
        for (k, vertex) in vertices.iter_mut().enumerate().take(count) {
            // "If doing gouraud shading, there will be one more color per
            // vertex sent, and the initial color will be the one for
            // vertex 0."
            let color = if gouraud && k > 0 {
                next += 1;
                words[next - 1]
            } else {
                command
            };
            *vertex = self.vertex(words[next]).with_color(color);
            next += 1;
            if textured {
                let uv = words[next];
                next += 1;
                vertex.u = (uv & 0xFF) as i32;
                vertex.v = ((uv >> 8) & 0xFF) as i32;
                // "The first word holds the Clut index. The second word
                // contains texture page information."
                match k {
                    0 => clut = uv >> 16,
                    1 => self.set_draw_mode(uv >> 16, MODE_TPAGE_ATTRIBUTE),
                    _ => {}
                }
            }
        }

        let texture = textured.then(|| self.prepare_texture(clut));
        let style = Style {
            texture,
            raw,
            blend: self.blend_mode(command),
            // "POLYGONs (triangles/quads) are dithered ONLY if they do use
            // gouraud shading or modulation."
            dither: self.draw_mode & MODE_DITHER != 0 && (gouraud || (textured && !raw)),
        };
        // "Quads are internally processed as two triangles, the first
        // consisting of vertices 1,2,3, and the second of vertices 2,3,4."
        self.triangle([vertices[0], vertices[1], vertices[2]], &style);
        if quad {
            self.triangle([vertices[1], vertices[2], vertices[3]], &style);
        }
    }

    fn triangle(&mut self, mut v: [Vertex; 3], style: &Style) {
        // The "core" vertex (the leftmost one, with ties resolved towards
        // later vertices) is where interpolation starts; it is tracked as
        // a one-hot index while the vertices are sorted by Y.
        let mut core_bit: u32 = if v[1].x <= v[0].x {
            if v[2].x <= v[1].x {
                4
            } else {
                2
            }
        } else if v[2].x < v[0].x {
            4
        } else {
            1
        };
        let swap_12 = |bit: u32| ((bit >> 1) & 2) | ((bit << 1) & 4) | (bit & 1);
        let swap_01 = |bit: u32| ((bit >> 1) & 1) | ((bit << 1) & 2) | (bit & 4);
        if v[2].y < v[1].y {
            v.swap(2, 1);
            core_bit = swap_12(core_bit);
        }
        if v[1].y < v[0].y {
            v.swap(1, 0);
            core_bit = swap_01(core_bit);
        }
        if v[2].y < v[1].y {
            v.swap(2, 1);
            core_bit = swap_12(core_bit);
        }
        let core = (core_bit >> 1) as usize;

        if v[0].y == v[2].y {
            return;
        }
        // psx-spx: "Vertex" — "The maximum distance between two vertices is
        // 1023 horizontally, and 511 vertically. Polygons and lines that
        // are exceeding that dimensions are NOT rendered."
        let too_wide = |a: usize, b: usize| (v[a].x - v[b].x).abs() >= 1024;
        if v[2].y - v[0].y >= 512 || too_wide(2, 0) || too_wide(2, 1) || too_wide(1, 0) {
            return;
        }

        // Attribute gradients per pixel in X and per line in Y.
        let cross = |ax: i32, ay: i32, bx: i32, by: i32| ax as i64 * by as i64 - bx as i64 * ay as i64;
        let denominator = cross(v[1].x - v[0].x, v[1].y - v[0].y, v[2].x - v[1].x, v[2].y - v[1].y);
        if denominator == 0 {
            return;
        }
        let attrs = [v[0].attributes(), v[1].attributes(), v[2].attributes()];
        let mut dx = [0u32; 5];
        let mut dy = [0u32; 5];
        let mut start = [0u32; 5];
        for k in 0..5 {
            let (a, b, c) = (attrs[0][k], attrs[1][k], attrs[2][k]);
            let gx = cross(b - a, v[1].y - v[0].y, c - b, v[2].y - v[1].y);
            let gy = cross(v[1].x - v[0].x, b - a, v[2].x - v[1].x, c - b);
            dx[k] = ((gx * (1 << 12) / denominator) as u32) << 12;
            dy[k] = ((gy * (1 << 12) / denominator) as u32) << 12;
            // Value at the core vertex, plus half a unit, moved back to the
            // origin so that a pixel's value is start + x*dx + y*dy.
            start[k] = (((attrs[core][k] << 12) + (1 << 11)) as u32) << 12;
            start[k] = start[k]
                .wrapping_sub(dx[k].wrapping_mul(v[core].x as u32))
                .wrapping_sub(dy[k].wrapping_mul(v[core].y as u32));
        }

        // Edges in 32.32 fixed point. The long edge runs from v0 to v2;
        // the two short edges bound the upper and lower halves.
        let x_fp = |x: i32| ((x as i64) << 32) + ((1i64 << 32) - (1 << 11));
        let step = |dx: i32, dy: i32| {
            let mut wide = (dx as i64) << 32;
            if wide < 0 {
                wide -= dy as i64 - 1;
            }
            if wide > 0 {
                wide += dy as i64 - 1;
            }
            wide / dy as i64
        };
        let base_coord = x_fp(v[0].x);
        let base_step = step(v[2].x - v[0].x, v[2].y - v[0].y);
        let (upper_step, right_facing) = if v[1].y == v[0].y {
            (0, v[1].x > v[0].x)
        } else {
            let s = step(v[1].x - v[0].x, v[1].y - v[0].y);
            (s, s > base_step)
        };
        let lower_step = if v[2].y == v[1].y { 0 } else { step(v[2].x - v[1].x, v[2].y - v[1].y) };

        // Each half is walked away from the core vertex: downwards
        // normally, upwards (`decrement`) when the core vertex is below.
        struct Half {
            x: [i64; 2],
            step: [i64; 2],
            y: i32,
            y_bound: i32,
            decrement: bool,
        }
        let vo = (core != 0) as usize;
        let vp = if core == 2 { 3 } else { 0 };
        let short = right_facing as usize;
        let long = !right_facing as usize;
        let make = |from: usize, to: usize, short_step: i64, decrement: bool| {
            let mut half = Half { x: [0; 2], step: [0; 2], y: v[from].y, y_bound: v[to].y, decrement };
            half.x[short] = x_fp(v[from].x);
            half.step[short] = short_step;
            half.x[long] = base_coord + (v[from].y - v[0].y) as i64 * base_step;
            half.step[long] = base_step;
            half
        };
        let upper = make(vo, 1 ^ vo, upper_step, vo != 0);
        let lower = make(1 ^ vp, 2 ^ vp, lower_step, vp != 0);
        let halves = if vo == 0 { [upper, lower] } else { [lower, upper] };

        for half in halves {
            let (mut y, mut left, mut right) = (half.y, half.x[0], half.x[1]);
            if half.decrement {
                while y > half.y_bound {
                    y -= 1;
                    left -= half.step[0];
                    right -= half.step[1];
                    let line = sign_extend_11(y as u32);
                    if line < self.clip_y1 {
                        break;
                    }
                    if line <= self.clip_y2 {
                        self.span(line, (left >> 32) as i32, (right >> 32) as i32, &start, &dx, &dy, style);
                    }
                }
            } else {
                while y < half.y_bound {
                    let line = sign_extend_11(y as u32);
                    if line > self.clip_y2 {
                        break;
                    }
                    if line >= self.clip_y1 {
                        self.span(line, (left >> 32) as i32, (right >> 32) as i32, &start, &dx, &dy, style);
                    }
                    y += 1;
                    left += half.step[0];
                    right += half.step[1];
                }
            }
        }
    }

    /// Draws the pixels `x_start..x_bound` of line `y` ("Polygons are
    /// displayed up to <excluding> their lower-right coordinates").
    #[allow(clippy::too_many_arguments)]
    fn span(&mut self, y: i32, x_start: i32, x_bound: i32, start: &[u32; 5], dx: &[u32; 5], dy: &[u32; 5], style: &Style) {
        if self.skip_line(y) {
            return;
        }
        let mut first = x_start;
        let mut width = x_bound - x_start;
        let mut x = sign_extend_11(x_start as u32);
        if x < self.clip_x1 {
            let delta = self.clip_x1 - x;
            first += delta;
            x += delta;
            width -= delta;
        }
        if x + width > self.clip_x2 + 1 {
            width = self.clip_x2 + 1 - x;
        }
        if width <= 0 {
            return;
        }
        let mut value = [0u32; 5];
        for k in 0..5 {
            value[k] = start[k].wrapping_add(dx[k].wrapping_mul(first as u32)).wrapping_add(dy[k].wrapping_mul(y as u32));
        }
        for _ in 0..width {
            let [r, g, b, u, v] = value.map(|a| (a >> ATTR_FRACTION) as i32);
            self.shade(style, x, y, r, g, b, u, v);
            x += 1;
            for k in 0..5 {
                value[k] = value[k].wrapping_add(dx[k]);
            }
        }
    }

    // ---- rectangles --------------------------------------------------------

    /// psx-spx: "GPU Render Rectangle Commands".
    pub(super) fn rectangle(&mut self, words: &[u32]) {
        let command = words[0];
        let textured = command & (1 << 26) != 0;
        let mut next = 2;
        let (mut u, mut v, mut clut) = (0, 0, 0);
        if textured {
            u = (words[2] & 0xFF) as i32;
            v = ((words[2] >> 8) & 0xFF) as i32;
            clut = words[2] >> 16;
            next = 3;
        }
        // "SIZ Rectangle Size (0=Variable, 1=1x1, 2=8x8, 3=16x16)";
        // variable sizes are "max 1023x511".
        let (width, height) = match (command >> 27) & 3 {
            0 => ((words[next] & 0x3FF) as i32, ((words[next] >> 16) & 0x1FF) as i32),
            1 => (1, 1),
            2 => (8, 8),
            _ => (16, 16),
        };
        let mut x0 = sign_extend_11((sign_extend_11(words[1]) + self.offset_x) as u32);
        let mut y0 = sign_extend_11((sign_extend_11(words[1] >> 16) + self.offset_y) as u32);
        let x_bound = (x0 + width).min(self.clip_x2 + 1);
        let y_bound = (y0 + height).min(self.clip_y2 + 1);

        // "Texture Origin and X/Y-Flip": TPage bits 12/13 make the texture
        // coordinates count down instead of up (v2 GPU).
        let (mut u_step, mut v_step) = (1, 1);
        if textured && self.draw_mode & MODE_FLIP_X != 0 {
            u_step = -1;
            // Not in psx-spx: other emulators report that the hardware
            // forces the lowest bit of U when flipping horizontally.
            u |= 1;
        }
        if textured && self.draw_mode & MODE_FLIP_Y != 0 {
            v_step = -1;
        }
        if x0 < self.clip_x1 {
            u += (self.clip_x1 - x0) * u_step;
            x0 = self.clip_x1;
        }
        if y0 < self.clip_y1 {
            v += (self.clip_y1 - y0) * v_step;
            y0 = self.clip_y1;
        }

        // "the texture page must be set up separately for Rectangles, via
        // GP0(E1h)".
        let texture = textured.then(|| self.prepare_texture(clut));
        let style = Style {
            texture,
            raw: command & (1 << 24) != 0,
            blend: self.blend_mode(command),
            // "RECTs are NOT dithered (no matter if they do use modulation
            // or not)."
            dither: false,
        };
        let (r, g, b) = ((command & 0xFF) as i32, ((command >> 8) & 0xFF) as i32, ((command >> 16) & 0xFF) as i32);
        for y in y0..y_bound {
            if !self.skip_line(y) {
                let mut u_now = u;
                for x in x0..x_bound {
                    self.shade(&style, x, y, r, g, b, u_now, v);
                    u_now += u_step;
                }
            }
            v += v_step;
        }
    }

    // ---- lines -------------------------------------------------------------

    /// psx-spx: "GPU Render Line Commands".
    pub(super) fn line_command(&mut self, words: &[u32]) {
        let command = words[0];
        let gouraud = command & (1 << 28) != 0;
        let blend = command & (1 << 25) != 0;
        let first = self.vertex(words[1]).with_color(command);
        let second = if gouraud {
            self.vertex(words[3]).with_color(words[2])
        } else {
            self.vertex(words[2]).with_color(command)
        };
        self.line(first, second, gouraud, blend);
        // "PLL Vertex Count (0=Single, 1=Polyline)": further vertices follow
        // until the terminator.
        if command & (1 << 27) != 0 {
            self.state = Gp0State::Polyline { gouraud, blend, previous: second, pending_color: None };
        }
    }

    /// A word received while a polyline is open.
    pub(super) fn polyline_word(&mut self, word: u32) {
        let Gp0State::Polyline { gouraud, blend, previous, pending_color } = self.state else { return };
        // "The vertex list is terminated by ... (word & 0xF000F000) ==
        // 0x50005000. The terminator value occurs on the first word of the
        // vertex (i.e. the color word if it's a gouraud shaded)."
        let is_first_word = !gouraud || pending_color.is_none();
        if is_first_word && word & 0xF000_F000 == 0x5000_5000 {
            self.state = Gp0State::Command;
            return;
        }
        if gouraud && pending_color.is_none() {
            self.state = Gp0State::Polyline { gouraud, blend, previous, pending_color: Some(word) };
            return;
        }
        // Flat polylines keep the colour of the command word.
        let mut vertex = Vertex { r: previous.r, g: previous.g, b: previous.b, ..self.vertex(word) };
        if let Some(color) = pending_color {
            vertex = vertex.with_color(color);
        }
        self.line(previous, vertex, gouraud, blend);
        self.state = Gp0State::Polyline { gouraud, blend, previous: vertex, pending_color: None };
    }

    fn line(&mut self, mut p0: Vertex, mut p1: Vertex, gouraud: bool, blend: bool) {
        let (dx, dy) = ((p1.x - p0.x).abs(), (p1.y - p0.y).abs());
        // Same size restriction as polygons (psx-spx: "Vertex").
        if dx >= 1024 || dy >= 512 {
            return;
        }
        // "If the 2 vertices in a line overlap, then the GPU will draw a
        // 1x1 rectangle ... using the colour of the first vertex": with
        // `steps` = 0 the loop below draws exactly p0.
        let steps = dx.max(dy);
        // Lines are always walked left to right.
        if p0.x >= p1.x && steps != 0 {
            std::mem::swap(&mut p0, &mut p1);
        }

        // 32.32 fixed-point position, 20.12 colour.
        let divide = |delta: i32| -> i64 {
            if steps == 0 {
                return 0;
            }
            let mut wide = (delta as i64) << 32;
            if wide < 0 {
                wide -= steps as i64 - 1;
            }
            if wide > 0 {
                wide += steps as i64 - 1;
            }
            wide / steps as i64
        };
        let (x_step, y_step) = (divide(p1.x - p0.x), divide(p1.y - p0.y));
        let mut x = (((p0.x as i64) << 32) | (1 << 31)) - 1024;
        let mut y = ((p0.y as i64) << 32) | (1 << 31);
        if y_step < 0 {
            y -= 1024;
        }
        let from = [p0.r, p0.g, p0.b];
        let to = [p1.r, p1.g, p1.b];
        let mut color = from.map(|c| (c << 12) | (1 << 11));
        let mut color_step = [0i32; 3];
        if gouraud && steps != 0 {
            for k in 0..3 {
                color_step[k] = ((to[k] - from[k]) << 12) / steps;
            }
        }

        let style = Style {
            texture: None,
            raw: false,
            blend: blend.then_some((self.draw_mode >> 5) & 3),
            // "LINEs are dithered (no matter if they are mono or do use
            // gouraud shading)."
            dither: self.draw_mode & MODE_DITHER != 0,
        };
        // "Lines are displayed up to <including> their lower-right
        // coordinates".
        for _ in 0..=steps {
            let (px, py) = (((x >> 32) & 2047) as i32, ((y >> 32) & 2047) as i32);
            let inside = px >= self.clip_x1 && px <= self.clip_x2 && py >= self.clip_y1 && py <= self.clip_y2;
            if inside && !self.skip_line(py) {
                self.shade(&style, px, py, color[0] >> 12, color[1] >> 12, color[2] >> 12, 0, 0);
            }
            x += x_step;
            y += y_step;
            for k in 0..3 {
                color[k] += color_step[k];
            }
        }
    }
}

/// psx-spx: "Semi-transparency" — B=Back (old pixel), F=Front (new pixel):
/// 0 = B/2+F/2, 1 = B+F, 2 = B-F, 3 = B+F/4, saturating per channel.
fn blend_pixel(back: u16, front: u16, mode: u32) -> u16 {
    let mut out = 0;
    for shift in [0, 5, 10] {
        let b = ((back >> shift) & 0x1F) as i32;
        let f = ((front >> shift) & 0x1F) as i32;
        let c = match mode {
            0 => (b + f) >> 1,
            1 => (b + f).min(31),
            2 => (b - f).max(0),
            _ => (b + (f >> 2)).min(31),
        };
        out |= (c as u16) << shift;
    }
    out
}
