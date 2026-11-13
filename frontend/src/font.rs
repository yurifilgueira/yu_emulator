//! Text for the frontend's own screens, drawn with an 8x8 bitmap font.
//!
//! The glyphs come from the `font8x8` crate (public domain): one byte per
//! row, bit 0 being the leftmost pixel.

use crate::Picture;
use font8x8::legacy::{BASIC_LEGACY, LATIN_LEGACY};

/// Width and height of a glyph at scale 1.
pub const GLYPH: usize = 8;

/// The glyph of a character; ASCII and Latin-1 are covered, anything else
/// shows as a question mark.
fn glyph(c: char) -> [u8; 8] {
    match c as u32 {
        code @ 0x20..=0x7E => BASIC_LEGACY[code as usize],
        code @ 0xA0..=0xFF => LATIN_LEGACY[(code - 0xA0) as usize],
        _ => BASIC_LEGACY[b'?' as usize],
    }
}

/// Fills a rectangle, clipped to the picture.
pub fn fill(picture: &mut Picture, x: usize, y: usize, width: usize, height: usize, colour: u32) {
    for row in y..(y + height).min(picture.height) {
        for column in x..(x + width).min(picture.width) {
            picture.pixels[row * picture.width + column] = colour;
        }
    }
}

/// Draws one line of text with its top left corner at (x, y), clipped to
/// the picture. Every font pixel becomes a square of `scale` pixels.
pub fn draw_text(picture: &mut Picture, x: usize, y: usize, scale: usize, colour: u32, text: &str) {
    for (index, c) in text.chars().enumerate() {
        let left = x + index * GLYPH * scale;
        for (row, bits) in glyph(c).iter().enumerate() {
            for column in 0..GLYPH {
                if bits & (1 << column) != 0 {
                    fill(picture, left + column * scale, y + row * scale, scale, scale, colour);
                }
            }
        }
    }
}

/// The width in pixels of a line of text.
pub fn text_width(text: &str, scale: usize) -> usize {
    text.chars().count() * GLYPH * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank(width: usize, height: usize) -> Picture {
        Picture { width, height, pixels: vec![0; width * height] }
    }

    /// The picture as rows of '#' and '.'.
    fn rows(picture: &Picture) -> Vec<String> {
        picture.pixels.chunks(picture.width).map(|row| row.iter().map(|p| if *p != 0 { '#' } else { '.' }).collect()).collect()
    }

    #[test]
    fn draws_a_glyph_with_the_leftmost_pixel_in_bit_0() {
        let mut picture = blank(8, 8);
        draw_text(&mut picture, 0, 0, 1, 0xFFFFFF, "L");
        // An "L": a vertical stroke on the left and a foot at the bottom.
        let rows = rows(&picture);
        assert_eq!(rows[0], "####....");
        assert_eq!(rows[1], ".##.....");
        assert_eq!(rows[6], "#######.");
        assert_eq!(rows[7], "........");
    }

    #[test]
    fn scales_and_advances_by_whole_glyphs() {
        let mut one = blank(16, 8);
        draw_text(&mut one, 0, 0, 1, 1, "LL");
        let rows1 = rows(&one);
        assert_eq!(rows1[0], "####....####....");
        let mut two = blank(16, 16);
        draw_text(&mut two, 0, 0, 2, 1, "L");
        let rows2 = rows(&two);
        assert_eq!(rows2[0], "########........");
        assert_eq!(rows2[1], rows2[0]);
        assert_eq!(rows2[2], "..####..........");
        assert_eq!(text_width("LL", 2), 32);
    }

    #[test]
    fn clips_at_the_picture_edges() {
        let mut picture = blank(10, 4);
        draw_text(&mut picture, 6, 2, 1, 1, "LLLL");
        assert_eq!(rows(&picture)[2], "......####");
        fill(&mut picture, 8, 0, 100, 100, 7);
        assert_eq!(picture.pixels[9], 7);
        assert_eq!(picture.pixels.len(), 40);
    }

    #[test]
    fn covers_latin_1_and_replaces_the_rest() {
        assert_ne!(glyph('é'), glyph('e'));
        assert_ne!(glyph('é'), [0; 8]);
        assert_eq!(glyph('漢'), glyph('?'));
        assert_eq!(glyph('\n'), glyph('?'));
        assert_eq!(glyph(' '), [0; 8]);
    }
}
