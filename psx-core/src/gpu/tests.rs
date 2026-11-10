//! GPU unit tests. Expected values come from psx-spx statements (quoted or
//! named next to each test), not from running the code.

use super::*;

/// GPU with the whole VRAM as drawing area and no offset.
fn gpu() -> Gpu {
    let mut gpu = Gpu::new();
    gpu.gp0(0xE300_0000);
    gpu.gp0(0xE400_0000 | (511 << 10) | 1023);
    gpu.gp0(0xE500_0000);
    gpu
}

fn send(gpu: &mut Gpu, words: &[u32]) {
    for word in words {
        gpu.gp0(*word);
    }
}

fn pixel(gpu: &Gpu, x: usize, y: usize) -> u16 {
    gpu.vram()[y * VRAM_WIDTH + x]
}

fn xy(x: i32, y: i32) -> u32 {
    ((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF)
}

/// Number of non-zero pixels in VRAM.
fn drawn(gpu: &Gpu) -> usize {
    gpu.vram().iter().filter(|p| **p != 0).count()
}

/// Uploads 15-bit pixels with a CPU-to-VRAM transfer.
fn upload(gpu: &mut Gpu, x: i32, y: i32, width: u32, height: u32, pixels: &[u16]) {
    send(gpu, &[0xA000_0000, xy(x, y), (height << 16) | width]);
    for pair in pixels.chunks(2) {
        gpu.gp0(pair[0] as u32 | ((*pair.get(1).unwrap_or(&0) as u32) << 16));
    }
}

const WHITE: u16 = 0x7FFF;
const RED: u16 = 0x001F;
const BLUE: u16 = 0x7C00;

// ---- status and GP1 --------------------------------------------------------

#[test]
fn status_after_reset() {
    // psx-spx: "GP1(00h) - Reset GPU": "GP1.read becomes 14802000h".
    let gpu = Gpu::new();
    assert_eq!(gpu.status() & 0x7FFF_FFFF, 0x1480_2000);
}

#[test]
fn gp1_commands_show_in_status() {
    let mut gpu = Gpu::new();
    gpu.gp1(0x0300_0000); // display on
    assert_eq!(gpu.status() & (1 << 23), 0);
    gpu.gp1(0x0400_0002); // DMA direction 2
    assert_eq!((gpu.status() >> 29) & 3, 2);
    assert_ne!(gpu.status() & (1 << 25), 0, "DREQ = FIFO empty in mode 2");
    gpu.gp1(0x0400_0001);
    assert_ne!(gpu.status() & (1 << 25), 0, "DREQ = FIFO not full in mode 1");
    gpu.gp1(0x0400_0003);
    assert_eq!(gpu.status() & (1 << 25), 0, "DREQ = read FIFO in mode 3");
    // Display mode: 640 pixels (3), 480 lines, PAL, 24 bit, interlaced, 368.
    gpu.gp1(0x0800_007F);
    assert_eq!((gpu.status() >> 16) & 0x7F, 0x7F);
    // Reset restores everything.
    gpu.gp1(0x0000_0000);
    assert_eq!(gpu.status() & 0x7FFF_FFFF, 0x1480_2000);
}

#[test]
fn draw_mode_and_mask_bits_show_in_status() {
    let mut gpu = Gpu::new();
    gpu.gp0(0xE100_07FF);
    assert_eq!(gpu.status() & 0x7FF, 0x7FF);
    gpu.gp0(0xE600_0003);
    assert_eq!((gpu.status() >> 11) & 3, 3);
}

#[test]
fn draw_mode_bit11_needs_gp1_09() {
    // psx-spx: "GP1(09h) - Set VRAM size (v2)"; sequence and results from
    // the console log of ps1-tests gpu/gp0-e1.
    let mut gpu = Gpu::new();
    let bit15 = |gpu: &Gpu| gpu.status() & (1 << 15) != 0;
    gpu.gp0(0xE100_0800);
    assert!(!bit15(&gpu), "ignored while not allowed");
    gpu.gp1(0x0900_0001);
    gpu.gp0(0xE100_0800);
    assert!(bit15(&gpu), "written when allowed");
    gpu.gp1(0x0900_0000);
    assert!(bit15(&gpu), "un-allowing preserves the bit");
    gpu.gp0(0xE100_0800);
    assert!(!bit15(&gpu), "...until the next write");
    // "GP1(09h) is NOT affected by the reset command."
    gpu.gp1(0x0900_0001);
    gpu.gp1(0x0000_0000);
    gpu.gp0(0xE100_0800);
    assert!(bit15(&gpu));
}

#[test]
fn interrupt_request_and_acknowledge() {
    // psx-spx: "GP0(1Fh) - Interrupt Request", "GP1(02h)".
    let mut gpu = Gpu::new();
    assert!(!gpu.take_irq());
    gpu.gp0(0x1F00_0000);
    assert_ne!(gpu.status() & (1 << 24), 0);
    assert!(gpu.take_irq());
    assert!(!gpu.take_irq(), "edge reported once");
    gpu.gp1(0x0200_0000);
    assert_eq!(gpu.status() & (1 << 24), 0);
}

#[test]
fn read_internal_registers() {
    // psx-spx: "GP1(10h) - Read GPU internal register" (v2 GPU).
    let mut gpu = Gpu::new();
    gpu.gp0(0xE201_2345);
    gpu.gp0(0xE300_0000 | (20 << 10) | 10);
    gpu.gp0(0xE400_0000 | (200 << 10) | 300);
    gpu.gp0(0xE500_0000 | (5 << 11) | 7);
    let mut read = |index: u32| {
        gpu.gp1(0x1000_0000 | index);
        gpu.read()
    };
    assert_eq!(read(2), 0x01_2345);
    assert_eq!(read(3), (20 << 10) | 10);
    assert_eq!(read(4), (200 << 10) | 300);
    assert_eq!(read(5), (5 << 11) | 7);
    assert_eq!(read(7), 2, "GPU version");
    assert_eq!(read(6), 2, "returns nothing: old value remains");
    assert_eq!(read(0x17), 2, "indices mirror every 16");
    assert_eq!(read(8), 0);
}

#[test]
fn status_ready_bits_follow_the_command_state() {
    // psx-spx: "Ready Bits".
    let mut gpu = gpu();
    assert_ne!(gpu.status() & (1 << 26), 0);
    gpu.gp0(0x2000_00FF); // triangle: waiting for vertices
    assert_eq!(gpu.status() & (1 << 26), 0);
    send(&mut gpu, &[xy(0, 0), xy(4, 0), xy(0, 4)]);
    assert_ne!(gpu.status() & (1 << 26), 0);
    // Bit 27 while VRAM-to-CPU data is pending.
    send(&mut gpu, &[0xC000_0000, xy(0, 0), (1 << 16) | 2]);
    assert_ne!(gpu.status() & (1 << 27), 0);
    gpu.read();
    assert_eq!(gpu.status() & (1 << 27), 0);
}

#[test]
fn status_bit31_alternates_per_scanline_outside_vblank() {
    // psx-spx: "in 240-lines mode, the bit changes per scanline. The bit is
    // always zero during Vblank".
    let mut gpu = Gpu::new();
    assert_eq!(gpu.status() >> 31, 0, "line 0 is in vblank");
    let line = 3413 * 7 / 11 + 1;
    gpu.video.tick(line * 0x11);
    assert_eq!(gpu.video.line(), 0x11);
    assert_eq!(gpu.status() >> 31, 1);
    gpu.video.tick(line);
    assert_eq!(gpu.status() >> 31, 0);
}

// ---- VRAM transfers --------------------------------------------------------

#[test]
fn fill_rounds_to_16_pixels_and_clears_the_mask_bit() {
    // psx-spx: "Quick Rectangle Fill", "Masking and Rounding for FILL".
    let mut gpu = gpu();
    gpu.gp0(0xE600_0003); // mask settings must not affect fills
    send(&mut gpu, &[0x02FF_8040, xy(0x25, 3), (2 << 16) | 5]);
    // X is rounded down to 0x20, width up to 16; colour drops 3 bits.
    let expected = (0x40 >> 3) | ((0x80 >> 3) << 5) | ((0xFF >> 3) << 10);
    assert_eq!(pixel(&gpu, 0x20, 3), expected);
    assert_eq!(pixel(&gpu, 0x2F, 4), expected);
    assert_eq!(pixel(&gpu, 0x1F, 3), 0);
    assert_eq!(pixel(&gpu, 0x30, 3), 0);
    assert_eq!(pixel(&gpu, 0x20, 5), 0);
    assert_eq!(drawn(&gpu), 32);
}

#[test]
fn fill_with_zero_size_does_nothing_and_fill_wraps() {
    let mut gpu = gpu();
    send(&mut gpu, &[0x02FF_FFFF, xy(0, 0), 16]); // height 0
    send(&mut gpu, &[0x02FF_FFFF, xy(0, 0), 5 << 16]); // width 0
    assert_eq!(drawn(&gpu), 0);
    // "Wrapping": no carry from X into Y.
    send(&mut gpu, &[0x02FF_FFFF, xy(0x3F0, 511), (2 << 16) | 32]);
    assert_eq!(pixel(&gpu, 0x3F0, 511), WHITE);
    assert_eq!(pixel(&gpu, 0x00F, 511), WHITE);
    assert_eq!(pixel(&gpu, 0x3FF, 0), WHITE);
    assert_eq!(pixel(&gpu, 0x00F, 0), WHITE);
    assert_eq!(drawn(&gpu), 64);
}

#[test]
fn cpu_to_vram_and_back() {
    // psx-spx: "CPU to VRAM blitting", "VRAM to CPU blitting".
    let mut gpu = gpu();
    upload(&mut gpu, 10, 20, 3, 1, &[0x1111, 0x2222, 0x3333]);
    assert_eq!([pixel(&gpu, 10, 20), pixel(&gpu, 11, 20), pixel(&gpu, 12, 20)], [0x1111, 0x2222, 0x3333]);
    assert_eq!(pixel(&gpu, 13, 20), 0, "the extra halfword of an odd transfer is dropped");
    assert_ne!(gpu.status() & (1 << 26), 0, "transfer finished");

    send(&mut gpu, &[0xC000_0000, xy(10, 20), (1 << 16) | 3]);
    assert_eq!(gpu.read(), 0x2222_1111);
    assert_eq!(gpu.read(), 0x0000_3333);
}

#[test]
fn transfer_size_zero_means_maximum_and_coordinates_wrap() {
    // psx-spx: "Masking for COPY Commands parameters".
    let t = Transfer::new(xy(0x7FF, 0x3FF), 0);
    assert_eq!((t.x, t.y, t.width, t.height), (0x3FF, 0x1FF, 0x400, 0x200));
    let mut gpu = gpu();
    upload(&mut gpu, 1023, 511, 2, 2, &[1, 2, 3, 4]);
    assert_eq!([pixel(&gpu, 1023, 511), pixel(&gpu, 0, 511), pixel(&gpu, 1023, 0), pixel(&gpu, 0, 0)], [1, 2, 3, 4]);
}

#[test]
fn transfers_respect_mask_settings() {
    // psx-spx: "GP0(E6h)": the mask setting affects "CPU-to-VRAM and
    // VRAM-to-VRAM transfer commands".
    let mut gpu = gpu();
    upload(&mut gpu, 0, 0, 2, 1, &[0x8001, 0x0002]);
    gpu.gp0(0xE600_0002); // check mask
    upload(&mut gpu, 0, 0, 2, 1, &[0x0111, 0x0222]);
    assert_eq!([pixel(&gpu, 0, 0), pixel(&gpu, 1, 0)], [0x8001, 0x0222]);
    gpu.gp0(0xE600_0001); // set mask
    upload(&mut gpu, 4, 0, 1, 1, &[0x0333]);
    assert_eq!(pixel(&gpu, 4, 0), 0x8333);
}

#[test]
fn vram_to_vram_copy() {
    let mut gpu = gpu();
    upload(&mut gpu, 0, 0, 2, 2, &[1, 2, 3, 4]);
    send(&mut gpu, &[0x8000_0000, xy(0, 0), xy(100, 50), (2 << 16) | 2]);
    assert_eq!([pixel(&gpu, 100, 50), pixel(&gpu, 101, 50), pixel(&gpu, 100, 51), pixel(&gpu, 101, 51)], [1, 2, 3, 4]);
    gpu.gp0(0xE600_0001);
    send(&mut gpu, &[0x8000_0000, xy(0, 0), xy(200, 50), (1 << 16) | 1]);
    assert_eq!(pixel(&gpu, 200, 50), 0x8001);
}

// ---- polygons --------------------------------------------------------------

#[test]
fn flat_triangle_excludes_lower_right_edges() {
    // psx-spx: "Polygons are displayed up to <excluding> their lower-right
    // coordinates". A right triangle with legs of 4 covers x + y < 4.
    let mut gpu = gpu();
    send(&mut gpu, &[0x20FF_0000, xy(0, 0), xy(4, 0), xy(0, 4)]);
    for y in 0..6 {
        for x in 0..6 {
            let expected = if x + y < 4 { BLUE } else { 0 };
            assert_eq!(pixel(&gpu, x, y), expected, "({x},{y})");
        }
    }
}

#[test]
fn vertex_order_does_not_matter() {
    // psx-spx: "the ordering of the vertices doesn't matter on the GPU side".
    let corners = [xy(10, 10), xy(30, 14), xy(17, 40)];
    let render = |order: [usize; 3]| {
        let mut gpu = gpu();
        send(&mut gpu, &[0x2000_00FF, corners[order[0]], corners[order[1]], corners[order[2]]]);
        gpu.vram().to_vec()
    };
    let reference = render([0, 1, 2]);
    assert!(reference.iter().any(|p| *p != 0));
    for order in [[0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
        assert!(render(order) == reference, "order {order:?}");
    }
}

#[test]
fn quad_covers_its_rectangle_exactly_once() {
    // Two triangles sharing an edge: with additive blending a pixel drawn
    // twice would be brighter.
    let mut gpu = gpu();
    gpu.gp0(0xE100_0020); // semi-transparency mode 1: B+F
    send(&mut gpu, &[0x2A00_0040, xy(2, 2), xy(10, 2), xy(2, 9), xy(10, 9)]);
    for y in 0..12 {
        for x in 0..12 {
            let inside = (2..10).contains(&x) && (2..9).contains(&y);
            assert_eq!(pixel(&gpu, x, y), if inside { 0x0008 } else { 0 }, "({x},{y})");
        }
    }
}

#[test]
fn oversized_polygons_are_not_rendered() {
    // psx-spx: "Vertex" — maximum distance 1023 horizontally, 511 vertically.
    let mut gpu = gpu();
    send(&mut gpu, &[0x2000_00FF, xy(-1, 0), xy(1023, 0), xy(0, 10)]); // 1024 wide
    send(&mut gpu, &[0x2000_00FF, xy(0, -1), xy(10, 0), xy(0, 511)]); // 512 tall
    assert_eq!(drawn(&gpu), 0);
    send(&mut gpu, &[0x2000_00FF, xy(0, 0), xy(1023, 0), xy(0, 10)]); // 1023 wide
    assert_ne!(drawn(&gpu), 0);
}

#[test]
fn drawing_area_clips_and_offset_moves() {
    // psx-spx: "GP0(E3h)/(E4h)", "GP0(E5h)".
    let mut gpu = gpu();
    gpu.gp0(0xE300_0000 | (12 << 10) | 12);
    gpu.gp0(0xE400_0000 | (15 << 10) | 17); // x 12..=17, y 12..=15
    gpu.gp0(0xE500_0000 | (10 << 11) | 10); // offset (10,10)
    send(&mut gpu, &[0x6000_00FF, xy(0, 0), (20 << 16) | 20]); // rect at (10,10)
    assert_eq!(drawn(&gpu), 6 * 4);
    assert_eq!(pixel(&gpu, 12, 12), RED);
    assert_eq!(pixel(&gpu, 17, 15), RED);
    assert_eq!(pixel(&gpu, 11, 12), 0);
    assert_eq!(pixel(&gpu, 18, 15), 0);
    assert_eq!(pixel(&gpu, 12, 16), 0);
    // Negative offset.
    let mut gpu = self::gpu();
    gpu.gp0(0xE500_0000 | ((-5i32 as u32 & 0x7FF) << 11) | (-5i32 as u32 & 0x7FF));
    send(&mut gpu, &[0x6800_00FF, xy(8, 9)]); // 1x1 at (3,4)
    assert_eq!(pixel(&gpu, 3, 4), RED);
}

#[test]
fn gouraud_triangle_interpolates_between_vertex_colours() {
    let mut gpu = gpu();
    // Red at the left, blue at the right, along a thin horizontal sliver.
    send(&mut gpu, &[0x3000_00FF, xy(0, 0), 0x00FF_0000, xy(64, 0), 0x0000_00FF, xy(0, 2)]);
    let left = pixel(&gpu, 0, 0);
    let middle = pixel(&gpu, 32, 0);
    let right = pixel(&gpu, 62, 0);
    assert_eq!(left & 0x1F, 31, "pure red at the red vertex");
    assert_eq!(left >> 10, 0);
    assert!((14..=17).contains(&(middle & 0x1F)), "half red in the middle: {middle:04X}");
    assert!((14..=17).contains(&(middle >> 10)), "half blue in the middle: {middle:04X}");
    assert!(right >> 10 >= 29 && right & 0x1F <= 2, "nearly blue at the right: {right:04X}");
}

#[test]
fn dithering_applies_to_gouraud_but_not_flat_polygons() {
    // psx-spx: "POLYGONs are dithered ONLY if they do use gouraud shading
    // or modulation." Colour 0x84 with offsets -4..+3 gives 16 or 17.
    let mut gpu = gpu();
    gpu.gp0(0xE100_0200); // dither on
    send(&mut gpu, &[0x2084_8484, xy(0, 0), xy(8, 0), xy(0, 8)]);
    assert_eq!(pixel(&gpu, 0, 0), 0x4210, "flat: 0x84 >> 3 = 16 everywhere");
    assert_eq!(pixel(&gpu, 1, 0), 0x4210);

    send(&mut gpu, &[0x3084_8484, xy(16, 0), 0x0084_8484, xy(24, 0), 0x0084_8484, xy(16, 8)]);
    // Offsets of the first row: -4, +0, -3, +1 -> 0x80, 0x84, 0x81, 0x85.
    assert_eq!(pixel(&gpu, 16, 0) & 0x1F, 16);
    assert_eq!(pixel(&gpu, 17, 0) & 0x1F, 16);
    // Second row: +2, -2, +3, -1 -> 0x86, 0x82, 0x87, 0x83; 0x87 >> 3 = 16.
    // Fourth row starts with +3: 0x87 -> 16. Use a brighter value to see it.
    send(&mut gpu, &[0x3086_8686, xy(32, 0), 0x0086_8686, xy(40, 0), 0x0086_8686, xy(32, 8)]);
    // 0x86: -4 -> 0x82 (16), +0 -> 0x86 (16), +2 -> 0x88 (17), +3 -> 0x89 (17).
    assert_eq!(pixel(&gpu, 32, 0) & 0x1F, 16);
    assert_eq!(pixel(&gpu, 32, 1) & 0x1F, 17);
    assert_eq!(pixel(&gpu, 34, 1) & 0x1F, 17);
    assert_eq!(pixel(&gpu, 33, 1) & 0x1F, 16);
}

// ---- semi-transparency and masking -----------------------------------------

#[test]
fn semi_transparency_modes() {
    // psx-spx: "Semi-transparency": B/2+F/2, B+F, B-F, B+F/4.
    let blend = |mode: u32, back: u16, front: u32| {
        let mut gpu = gpu();
        upload(&mut gpu, 0, 0, 1, 1, &[back]);
        gpu.gp0(0xE100_0000 | (mode << 5));
        send(&mut gpu, &[0x6A00_0000 | front, xy(0, 0)]); // 1x1, semi-transparent
        pixel(&gpu, 0, 0)
    };
    // Back: r=20 g=4 b=31. Front: r=16 (0x80) g=8 (0x40) b=31 (0xFF).
    let back = 20 | (4 << 5) | (31 << 10);
    let front = 0xFF_4080;
    assert_eq!(blend(0, back, front), 18 | (6 << 5) | (31 << 10));
    assert_eq!(blend(1, back, front), 31 | (12 << 5) | (31 << 10));
    assert_eq!(blend(2, back, front), 4, "green and blue saturate at 0");
    assert_eq!(blend(3, back, front), 24 | (6 << 5) | (31 << 10));
}

#[test]
fn opaque_primitives_overwrite() {
    let mut gpu = gpu();
    upload(&mut gpu, 0, 0, 1, 1, &[WHITE]);
    gpu.gp0(0xE100_0020);
    send(&mut gpu, &[0x6800_0080, xy(0, 0)]);
    assert_eq!(pixel(&gpu, 0, 0), 0x0010);
}

#[test]
fn mask_bit_set_and_check_for_drawing() {
    // psx-spx: "GP0(E6h) - Mask Bit Setting".
    let mut gpu = gpu();
    gpu.gp0(0xE600_0001);
    send(&mut gpu, &[0x6800_00FF, xy(0, 0)]);
    assert_eq!(pixel(&gpu, 0, 0), 0x8000 | RED, "bit 15 forced");
    gpu.gp0(0xE600_0002);
    send(&mut gpu, &[0x68FF_0000, xy(0, 0)]);
    assert_eq!(pixel(&gpu, 0, 0), 0x8000 | RED, "masked pixel is write-protected");
    send(&mut gpu, &[0x68FF_0000, xy(1, 0)]);
    assert_eq!(pixel(&gpu, 1, 0), BLUE, "unmasked pixel is drawn, bit 15 clear");
}

// ---- textures --------------------------------------------------------------

/// Texture page 0 at (0,0) is where the tests draw, so textures live in
/// page 5 (x = 320) of the upper row.
const PAGE: u32 = 5;
const PAGE_X: i32 = 320;

#[test]
fn textured_rectangle_15bit_raw_with_transparent_texel() {
    let mut gpu = gpu();
    upload(&mut gpu, PAGE_X, 0, 4, 1, &[0x1234, 0x0000, 0x8000, 0x7FFF]);
    gpu.gp0(0xE100_0000 | (2 << 7) | PAGE); // 15-bit textures
    send(&mut gpu, &[0x6500_0000, xy(0, 0), 0, (1 << 16) | 4]);
    assert_eq!(pixel(&gpu, 0, 0), 0x1234);
    assert_eq!(pixel(&gpu, 1, 0), 0, "0000h is fully transparent");
    assert_eq!(pixel(&gpu, 2, 0), 0x8000, "8000h is opaque black with the mask bit");
    assert_eq!(pixel(&gpu, 3, 0), 0x7FFF);
}

#[test]
fn clut_textures_4bit_and_8bit() {
    // psx-spx: "Texture Bitmaps", "Clut Attribute".
    let mut gpu = gpu();
    // Palette at (16, 300): index n -> colour 0x100 + n.
    let palette: Vec<u16> = (0..256).map(|n| 0x100 + n as u16).collect();
    upload(&mut gpu, 16, 300, 256, 1, &palette);
    let clut = ((300 << 6) | 1) << 16;
    // One halfword holds indices 1,2,3,4 (4-bit, left first) or 0x21,0x43.
    upload(&mut gpu, PAGE_X, 0, 1, 1, &[0x4321]);

    gpu.gp0(0xE100_0000 | PAGE); // 4-bit
    send(&mut gpu, &[0x6500_0000, xy(0, 0), clut, (1 << 16) | 4]);
    let row: Vec<u16> = (0..4).map(|x| pixel(&gpu, x, 0)).collect();
    assert_eq!(row, [0x101, 0x102, 0x103, 0x104]);

    gpu.gp0(0xE100_0000 | (1 << 7) | PAGE); // 8-bit
    send(&mut gpu, &[0x6500_0000, xy(0, 2), clut, (1 << 16) | 2]);
    assert_eq!([pixel(&gpu, 0, 2), pixel(&gpu, 1, 2)], [0x121, 0x143]);
}

#[test]
fn clut_cache_keeps_the_palette_until_invalidated() {
    // psx-spx: "GPU Texture Caching": "The CLUT cache is invalidated when
    // different CLUT index values are used or when GP0(01h) is issued."
    let mut gpu = gpu();
    upload(&mut gpu, 16, 300, 1, 1, &[0x0111]);
    upload(&mut gpu, 32, 300, 1, 1, &[0x0333]);
    upload(&mut gpu, PAGE_X, 0, 1, 1, &[0x0000]); // index 0 everywhere
    gpu.gp0(0xE100_0000 | PAGE);
    let clut_a = ((300 << 6) | 1) << 16;
    let clut_b = ((300 << 6) | 2) << 16;
    let draw = |gpu: &mut Gpu, clut: u32| {
        send(gpu, &[0x6D00_0000, xy(0, 0), clut]); // 1x1 raw textured
        pixel(gpu, 0, 0)
    };
    assert_eq!(draw(&mut gpu, clut_a), 0x0111);
    upload(&mut gpu, 16, 300, 1, 1, &[0x0222]); // palette changes in VRAM
    assert_eq!(draw(&mut gpu, clut_a), 0x0111, "still the cached colour");
    gpu.gp0(0x0100_0000);
    assert_eq!(draw(&mut gpu, clut_a), 0x0222, "GP0(01h) reloads");
    upload(&mut gpu, 16, 300, 1, 1, &[0x0444]);
    assert_eq!(draw(&mut gpu, clut_b), 0x0333, "another CLUT is loaded");
    assert_eq!(draw(&mut gpu, clut_a), 0x0444, "and switching back reloads");
}

#[test]
fn clut_cache_reloads_for_more_entries_but_not_for_fewer() {
    // Behaviour shown by the reference image of ps1-tests gpu/clut-cache.
    let mut gpu = gpu();
    let clut = ((300 << 6) | 1) << 16;
    upload(&mut gpu, PAGE_X, 0, 1, 1, &[0x0000]);
    let draw = |gpu: &mut Gpu, depth: u32| {
        gpu.gp0(0xE100_0000 | (depth << 7) | PAGE);
        send(gpu, &[0x6D00_0000, xy(0, 0), clut]);
        pixel(gpu, 0, 0)
    };
    upload(&mut gpu, 16, 300, 1, 1, &[0x0111]);
    assert_eq!(draw(&mut gpu, 0), 0x0111, "4-bit: 16 entries cached");
    upload(&mut gpu, 16, 300, 1, 1, &[0x0222]);
    assert_eq!(draw(&mut gpu, 1), 0x0222, "8-bit needs 256 entries: reloaded");
    upload(&mut gpu, 16, 300, 1, 1, &[0x0333]);
    assert_eq!(draw(&mut gpu, 0), 0x0222, "back to 4-bit: cached entries are enough");
    // 15-bit textures do not touch the cache.
    assert_eq!(draw(&mut gpu, 2), 0x0222, "texel 0000h is transparent: pixel unchanged");
    assert_eq!(draw(&mut gpu, 0), 0x0222);
}

#[test]
fn modulation_80h_is_neutral_and_brighter_values_saturate() {
    // psx-spx: "Modulation": texel * colour / 128.
    let mut gpu = gpu();
    upload(&mut gpu, PAGE_X, 0, 1, 1, &[10 | (20 << 5) | (30 << 10)]);
    gpu.gp0(0xE100_0000 | (2 << 7) | PAGE);
    send(&mut gpu, &[0x6C80_8080, xy(0, 0), 0]); // 1x1 textured, modulated
    assert_eq!(pixel(&gpu, 0, 0), 10 | (20 << 5) | (30 << 10));
    send(&mut gpu, &[0x6CFF_4000, xy(1, 0), 0]); // r*0, g*0.5, b*~2
    assert_eq!(pixel(&gpu, 1, 0), (10 << 5) | (31 << 10));
}

#[test]
fn textured_semi_transparency_needs_texel_bit15() {
    // psx-spx: ABE "(if textured: texture bit15 must also be set)".
    let mut gpu = gpu();
    upload(&mut gpu, PAGE_X, 0, 2, 1, &[0x0010, 0x8010]);
    upload(&mut gpu, 0, 0, 2, 1, &[0x0008, 0x0008]);
    gpu.gp0(0xE100_0000 | (2 << 7) | (1 << 5) | PAGE); // B+F
    send(&mut gpu, &[0x6700_0000, xy(0, 0), 0, (1 << 16) | 2]); // raw, semi-transparent
    assert_eq!(pixel(&gpu, 0, 0), 0x0010, "bit 15 clear: written as-is");
    assert_eq!(pixel(&gpu, 1, 0), 0x8018, "bit 15 set: blended, mask bit kept");
}

#[test]
fn texture_window_repeats_a_region() {
    // psx-spx: "GP0(E2h)": Texcoord = (Texcoord AND NOT (Mask*8)) OR
    // ((Offset AND Mask)*8). Mask 0x1F / offset 2: every U maps to 16..23.
    let mut gpu = gpu();
    let texels: Vec<u16> = (0..32).map(|n| 0x200 + n as u16).collect();
    upload(&mut gpu, PAGE_X, 0, 32, 1, &texels);
    gpu.gp0(0xE100_0000 | (2 << 7) | PAGE);
    gpu.gp0(0xE200_0000 | (2 << 10) | 0x1F);
    send(&mut gpu, &[0x6500_0000, xy(0, 0), 0, (1 << 16) | 10]);
    assert_eq!(pixel(&gpu, 0, 0), 0x210);
    assert_eq!(pixel(&gpu, 7, 0), 0x217);
    assert_eq!(pixel(&gpu, 8, 0), 0x210, "repeats every 8 texels");
}

#[test]
fn rectangle_texture_flip() {
    // psx-spx: "Texture Origin and X/Y-Flip" (TPage bits 12/13).
    let mut gpu = gpu();
    let texels: Vec<u16> = (0..8).map(|n| 0x300 + n as u16).collect();
    upload(&mut gpu, PAGE_X, 0, 8, 1, &texels);
    gpu.gp0(0xE100_0000 | (1 << 12) | (2 << 7) | PAGE);
    send(&mut gpu, &[0x6500_0000, xy(0, 0), 7, (1 << 16) | 4]);
    let row: Vec<u16> = (0..4).map(|x| pixel(&gpu, x, 0)).collect();
    assert_eq!(row, [0x307, 0x306, 0x305, 0x304]);
}

#[test]
fn textured_triangle_maps_texels_one_to_one() {
    let mut gpu = gpu();
    let mut texels = Vec::new();
    for v in 0..8u16 {
        for u in 0..8u16 {
            texels.push(0x400 + v * 16 + u);
        }
    }
    upload(&mut gpu, PAGE_X, 0, 8, 8, &texels);
    let page = ((2 << 7) | PAGE) << 16;
    // Raw textured quad of the same size as the texture.
    let uv = |u: u32, v: u32| (v << 8) | u;
    send(
        &mut gpu,
        &[0x2D00_0000, xy(0, 0), uv(0, 0), xy(8, 0), page | uv(8, 0), xy(0, 8), uv(0, 8), xy(8, 8), uv(8, 8)],
    );
    for y in 0..8 {
        for x in 0..8 {
            assert_eq!(pixel(&gpu, x, y), 0x400 + (y * 16 + x) as u16, "({x},{y})");
        }
    }
    // The page attribute of the polygon became the current draw mode.
    assert_eq!(gpu.status() & 0x1FF, (2 << 7) | PAGE);
}

// ---- rectangles and lines --------------------------------------------------

#[test]
fn rectangle_sizes() {
    let count = |command: u32, extra: &[u32]| {
        let mut gpu = gpu();
        gpu.gp0(command);
        gpu.gp0(xy(5, 5));
        send(&mut gpu, extra);
        drawn(&gpu)
    };
    assert_eq!(count(0x6800_00FF, &[]), 1);
    assert_eq!(count(0x7000_00FF, &[]), 64);
    assert_eq!(count(0x7800_00FF, &[]), 256);
    assert_eq!(count(0x6000_00FF, &[(3 << 16) | 7]), 21);
    assert_eq!(count(0x6000_00FF, &[0]), 0);
}

#[test]
fn line_includes_both_end_points() {
    // psx-spx: "Lines are displayed up to <including> their lower-right
    // coordinates".
    let mut gpu = gpu();
    send(&mut gpu, &[0x4000_00FF, xy(2, 3), xy(6, 3)]);
    for x in 0..9 {
        assert_eq!(pixel(&gpu, x, 3), if (2..=6).contains(&x) { RED } else { 0 }, "x={x}");
    }
    send(&mut gpu, &[0x4000_00FF, xy(10, 10), xy(14, 14)]); // diagonal
    for i in 0..5 {
        assert_eq!(pixel(&gpu, 10 + i, 10 + i), RED);
    }
    assert_eq!(drawn(&gpu), 10);
}

#[test]
fn zero_length_line_draws_one_pixel() {
    // psx-spx: "If the 2 vertices in a line overlap, then the GPU will draw
    // a 1x1 rectangle".
    let mut gpu = gpu();
    send(&mut gpu, &[0x4000_00FF, xy(7, 7), xy(7, 7)]);
    assert_eq!(pixel(&gpu, 7, 7), RED);
    assert_eq!(drawn(&gpu), 1);
}

#[test]
fn gouraud_line_runs_from_first_to_second_colour() {
    let mut gpu = gpu();
    send(&mut gpu, &[0x5000_00FF, xy(0, 0), 0x00FF_0000, xy(31, 0)]);
    assert_eq!(pixel(&gpu, 0, 0), RED);
    assert_eq!(pixel(&gpu, 31, 0), BLUE);
    let middle = pixel(&gpu, 16, 0);
    assert!((14..=17).contains(&(middle & 0x1F)) && (14..=17).contains(&(middle >> 10)));
}

#[test]
fn polyline_runs_until_the_terminator() {
    // psx-spx: terminator "(word & 0xF000F000) == 0x50005000".
    let mut gpu = gpu();
    send(&mut gpu, &[0x4800_00FF, xy(0, 0), xy(4, 0), xy(4, 4), xy(0, 4), 0x5555_5555]);
    assert_eq!(pixel(&gpu, 2, 0), RED);
    assert_eq!(pixel(&gpu, 4, 2), RED);
    assert_eq!(pixel(&gpu, 2, 4), RED);
    assert_eq!(pixel(&gpu, 0, 2), 0, "the outline is not closed automatically");
    assert_eq!(drawn(&gpu), 13);
    // The port accepts commands again.
    send(&mut gpu, &[0x6800_00FF, xy(100, 100)]);
    assert_eq!(pixel(&gpu, 100, 100), RED);
}

#[test]
fn gouraud_polyline_terminator_is_on_the_colour_word() {
    let mut gpu = gpu();
    send(
        &mut gpu,
        &[0x5800_00FF, xy(0, 0), 0x0000_00FF, xy(3, 0), 0x0000_00FF, 0x5000_5000, 0x5000_5000],
    );
    // 0x50005000 after a colour word is a vertex (x=0x5000 -> 0, y likewise),
    // so a second segment back to (0,0) was drawn; the next word ends it.
    assert_ne!(gpu.status() & (1 << 26), 0, "polyline closed");
    assert_eq!(pixel(&gpu, 3, 0), RED);
}

// ---- display ---------------------------------------------------------------

#[test]
fn frame_shows_the_display_area() {
    // psx-spx: "GP1(05h)", "GP1(06h)", "GP1(07h)".
    let mut gpu = gpu();
    assert_eq!(gpu.display_size(), (256, 240), "reset: 2560 cycles / 10, 240 lines");
    upload(&mut gpu, 32, 16, 1, 1, &[RED]);
    assert!(gpu.frame().rgb.iter().all(|b| *b == 0), "display disabled: black");

    gpu.gp1(0x0300_0000);
    gpu.gp1(0x0500_0000 | (16 << 10) | 32);
    gpu.gp1(0x0800_0001); // 320 pixels
    gpu.gp1(0x0600_0000 | ((0x260 + 320 * 8) << 12) | 0x260);
    let frame = gpu.frame();
    assert_eq!((frame.width, frame.height), (320, 240));
    assert_eq!(&frame.rgb[0..6], &[255, 0, 0, 0, 0, 0]);
}

#[test]
fn frame_24bit_mode_packs_three_bytes_per_pixel() {
    let mut gpu = gpu();
    // Bytes R0 G0 B0 R1 G1 B1 = 11 22 33 44 55 66 -> halfwords 2211 4433 6655.
    upload(&mut gpu, 0, 0, 3, 1, &[0x2211, 0x4433, 0x6655]);
    gpu.gp1(0x0300_0000);
    gpu.gp1(0x0800_0010);
    assert_eq!(&gpu.frame().rgb[0..6], &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
}
