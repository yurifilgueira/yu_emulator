//! PNG input/output for VRAM dumps, screenshots and reference images.

use psx_core::gpu::{rgb15_to_24, Frame, VRAM_HEIGHT, VRAM_WIDTH};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

/// An 8-bit RGB image.
pub struct Rgb {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl Rgb {
    /// The whole VRAM as a 1024x512 picture (the mask bit is not shown).
    pub fn from_vram(vram: &[u16]) -> Self {
        let data = vram.iter().flat_map(|pixel| rgb15_to_24(*pixel)).collect();
        Self { width: VRAM_WIDTH, height: VRAM_HEIGHT, data }
    }

    pub fn from_frame(frame: Frame) -> Self {
        Self { width: frame.width, height: frame.height, data: frame.rgb }
    }

    pub fn write_png(&self, path: &Path) -> Result<(), String> {
        let describe = |e: &dyn std::fmt::Display| format!("could not write {}: {e}", path.display());
        let file = File::create(path).map_err(|e| describe(&e))?;
        let mut encoder = png::Encoder::new(BufWriter::new(file), self.width as u32, self.height as u32);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| describe(&e))?;
        writer.write_image_data(&self.data).map_err(|e| describe(&e))
    }

    /// Reads a PNG of any colour type as 8-bit RGB (alpha is dropped).
    pub fn read_png(path: &Path) -> Result<Self, String> {
        let describe = |e: &dyn std::fmt::Display| format!("could not read {}: {e}", path.display());
        let file = File::open(path).map_err(|e| describe(&e))?;
        let mut decoder = png::Decoder::new(BufReader::new(file));
        // Palettes and low bit depths are expanded to 8-bit samples.
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().map_err(|e| describe(&e))?;
        let size = reader.output_buffer_size().ok_or_else(|| describe(&"image too large"))?;
        let mut buffer = vec![0; size];
        let info = reader.next_frame(&mut buffer).map_err(|e| describe(&e))?;
        let channels = match info.color_type {
            png::ColorType::Grayscale => 1,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            png::ColorType::Indexed => return Err(describe(&"palette was not expanded")),
        };
        let pixels = info.width as usize * info.height as usize;
        let mut data = Vec::with_capacity(pixels * 3);
        for pixel in buffer[..pixels * channels].chunks(channels) {
            match channels {
                1 | 2 => data.extend_from_slice(&[pixel[0]; 3]),
                _ => data.extend_from_slice(&pixel[..3]),
            }
        }
        Ok(Self { width: info.width as usize, height: info.height as usize, data })
    }
}

/// Result of comparing two images at the precision of VRAM (5 bits per
/// channel): reference images are 8-bit expansions of 15-bit pixels, and
/// tools differ in how they fill the low 3 bits.
pub struct Comparison {
    pub total: usize,
    pub different: usize,
    /// Largest difference of any channel, in 5-bit steps.
    pub max_delta: u8,
    /// First differing pixel: x, y, expected RGB, actual RGB (5-bit).
    pub first: Option<(usize, usize, [u8; 3], [u8; 3])>,
}

pub fn compare(expected: &Rgb, actual: &Rgb) -> Result<Comparison, String> {
    if (expected.width, expected.height) != (actual.width, actual.height) {
        return Err(format!(
            "size mismatch: reference is {}x{}, output is {}x{}",
            expected.width, expected.height, actual.width, actual.height
        ));
    }
    let mut result = Comparison { total: expected.width * expected.height, different: 0, max_delta: 0, first: None };
    for (i, (e, a)) in expected.data.chunks(3).zip(actual.data.chunks(3)).enumerate() {
        let e5 = [e[0] >> 3, e[1] >> 3, e[2] >> 3];
        let a5 = [a[0] >> 3, a[1] >> 3, a[2] >> 3];
        if e5 != a5 {
            result.different += 1;
            let delta = (0..3).map(|k| e5[k].abs_diff(a5[k])).max().unwrap_or(0);
            result.max_delta = result.max_delta.max(delta);
            result.first.get_or_insert((i % expected.width, i / expected.width, e5, a5));
        }
    }
    Ok(result)
}

/// A picture marking where two images differ: matching pixels are shown
/// darkened, differing ones in bright magenta.
pub fn difference(expected: &Rgb, actual: &Rgb) -> Rgb {
    let mut data = Vec::with_capacity(actual.data.len());
    for (e, a) in expected.data.chunks(3).zip(actual.data.chunks(3)) {
        if (0..3).all(|k| e[k] >> 3 == a[k] >> 3) {
            data.extend_from_slice(&[a[0] / 3, a[1] / 3, a[2] / 3]);
        } else {
            data.extend_from_slice(&[255, 0, 255]);
        }
    }
    Rgb { width: actual.width, height: actual.height, data }
}

/// FNV-1a hash of VRAM, for tracking changes between runs.
pub fn vram_hash(vram: &[u16]) -> u64 {
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    for byte in vram.iter().flat_map(|pixel| pixel.to_le_bytes()) {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(pixels: &[[u8; 3]]) -> Rgb {
        Rgb { width: pixels.len(), height: 1, data: pixels.concat() }
    }

    #[test]
    fn comparison_ignores_the_low_three_bits() {
        let a = image(&[[0xF8, 0x00, 0x80], [8, 8, 8]]);
        let b = image(&[[0xFF, 0x07, 0x87], [8, 8, 8]]);
        assert_eq!(compare(&a, &b).unwrap().different, 0);
    }

    #[test]
    fn comparison_reports_differences() {
        let a = image(&[[0, 0, 0], [0xF8, 0, 0], [0, 0, 0]]);
        let b = image(&[[0, 0, 0], [0xE0, 0, 8], [0, 0, 0]]);
        let c = compare(&a, &b).unwrap();
        assert_eq!((c.total, c.different, c.max_delta), (3, 1, 3));
        assert_eq!(c.first, Some((1, 0, [31, 0, 0], [28, 0, 1])));
        assert_eq!(&difference(&a, &b).data[3..6], &[255, 0, 255]);
        assert!(compare(&a, &image(&[[0, 0, 0]])).is_err(), "size mismatch");
    }

    #[test]
    fn png_roundtrip() {
        let path = std::env::temp_dir().join(format!("yu-emulator-test-{}.png", std::process::id()));
        let original = image(&[[1, 2, 3], [250, 128, 0]]);
        original.write_png(&path).unwrap();
        let read = Rgb::read_png(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!((read.width, read.height), (2, 1));
        assert_eq!(read.data, original.data);
    }

    #[test]
    fn hash_changes_with_content() {
        assert_ne!(vram_hash(&[0, 1]), vram_hash(&[1, 0]));
        assert_eq!(vram_hash(&[]), 0xCBF2_9CE4_8422_2325);
    }
}
