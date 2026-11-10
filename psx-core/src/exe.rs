//! PS-EXE parser, for sideloading test programs without a CD.
//!
//! psx-spx: "CDROM File Formats" — "PSX.EXE (Boot-Executable)".

use std::fmt;

const HEADER_SIZE: usize = 0x800;
const MAGIC: &[u8; 8] = b"PS-X EXE";

#[derive(Debug, PartialEq, Eq)]
pub enum ExeError {
    TooShort { got: usize },
    BadMagic,
}

impl fmt::Display for ExeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExeError::TooShort { got } => {
                write!(f, "PS-EXE needs a {HEADER_SIZE}-byte header, file has {got} bytes")
            }
            ExeError::BadMagic => write!(f, "missing \"PS-X EXE\" signature"),
        }
    }
}

impl std::error::Error for ExeError {}

/// A parsed executable. Field names follow the psx-spx header table.
#[derive(Debug, PartialEq, Eq)]
pub struct Exe {
    /// 010h: initial PC.
    pub pc: u32,
    /// 014h: initial GP/R28.
    pub gp: u32,
    /// 018h: destination address in RAM.
    pub dest: u32,
    /// 028h/02Ch: region to zero-fill (size 0 = none).
    pub bss_start: u32,
    pub bss_size: u32,
    /// 030h + 034h: initial SP/FP, or `None` when the base is 0.
    pub sp: Option<u32>,
    /// Code/data following the header.
    pub data: Vec<u8>,
}

impl Exe {
    pub fn parse(file: &[u8]) -> Result<Self, ExeError> {
        if file.len() < HEADER_SIZE {
            return Err(ExeError::TooShort { got: file.len() });
        }
        if &file[0..8] != MAGIC {
            return Err(ExeError::BadMagic);
        }
        let word = |off: usize| u32::from_le_bytes(file[off..off + 4].try_into().unwrap());

        // 01Ch: file size excluding the header. Homebrew tools sometimes
        // write a size that disagrees with the file; never read past it.
        let body = &file[HEADER_SIZE..];
        let size = (word(0x1C) as usize).min(body.len());
        let sp_base = word(0x30);
        Ok(Self {
            pc: word(0x10),
            gp: word(0x14),
            dest: word(0x18),
            bss_start: word(0x28),
            bss_size: word(0x2C),
            sp: (sp_base != 0).then(|| sp_base.wrapping_add(word(0x34))),
            data: body[..size].to_vec(),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a PS-EXE image around `code` (little-endian words).
    pub(crate) fn build(pc: u32, dest: u32, sp: u32, code: &[u32]) -> Vec<u8> {
        let mut file = vec![0u8; HEADER_SIZE];
        file[0..8].copy_from_slice(MAGIC);
        let mut put = |off: usize, v: u32| file[off..off + 4].copy_from_slice(&v.to_le_bytes());
        put(0x10, pc);
        put(0x14, 0x1234);
        put(0x18, dest);
        put(0x1C, (code.len() * 4) as u32);
        put(0x30, sp);
        put(0x34, 0x10);
        file.extend(code.iter().flat_map(|w| w.to_le_bytes()));
        file
    }

    #[test]
    fn parses_header_fields() {
        let exe = Exe::parse(&build(0x8001_0008, 0x8001_0000, 0x801F_FF00, &[1, 2, 3])).unwrap();
        assert_eq!(exe.pc, 0x8001_0008);
        assert_eq!(exe.gp, 0x1234);
        assert_eq!(exe.dest, 0x8001_0000);
        assert_eq!(exe.sp, Some(0x801F_FF10));
        assert_eq!(exe.data.len(), 12);
        assert_eq!(&exe.data[4..8], &[2, 0, 0, 0]);
    }

    #[test]
    fn zero_stack_base_means_keep_callers_stack() {
        let exe = Exe::parse(&build(0x8001_0000, 0x8001_0000, 0, &[])).unwrap();
        assert_eq!(exe.sp, None);
    }

    #[test]
    fn rejects_bad_files() {
        assert_eq!(Exe::parse(&[0; 100]), Err(ExeError::TooShort { got: 100 }));
        assert_eq!(Exe::parse(&[0; 0x800]), Err(ExeError::BadMagic));
    }

    #[test]
    fn size_field_larger_than_file_is_clamped() {
        let mut file = build(0, 0x8001_0000, 0, &[7]);
        file[0x1C..0x20].copy_from_slice(&0x1000u32.to_le_bytes());
        assert_eq!(Exe::parse(&file).unwrap().data.len(), 4);
    }
}
