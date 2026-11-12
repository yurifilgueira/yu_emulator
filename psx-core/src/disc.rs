//! CD image: raw 2352-byte sectors plus a table of tracks.
//!
//! psx-spx: "CDROM Format" (sector layout) and "CDROM Disk Images" (CUE/BIN).
//! The core does no file access: [`Disc::from_cue`] parses the cue sheet text
//! and asks the caller for the contents of each file it names.

/// Bytes of a raw sector: sync, header, data and error correction.
pub const SECTOR_SIZE: usize = 2352;
/// Sectors per second, and the 2-second lead-in before sector 0: the
/// position 00:02:00 is the first sector of the first track.
pub const SECTORS_PER_SECOND: u32 = 75;
pub const LEAD_IN: u32 = 2 * SECTORS_PER_SECOND;

/// A position as minute, second, frame (sector within the second), binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Msf {
    pub minute: u8,
    pub second: u8,
    pub frame: u8,
}

impl Msf {
    /// The absolute position of logical sector `lba` (sector 0 = 00:02:00).
    pub fn from_lba(lba: u32) -> Self {
        Self::from_sectors(lba + LEAD_IN)
    }

    /// A length or relative position of `sectors` sectors.
    pub fn from_sectors(sectors: u32) -> Self {
        Self {
            minute: (sectors / (60 * SECTORS_PER_SECOND)) as u8,
            second: (sectors / SECTORS_PER_SECOND % 60) as u8,
            frame: (sectors % SECTORS_PER_SECOND) as u8,
        }
    }

    /// The logical sector at this absolute position, if it is past the
    /// lead-in.
    pub fn to_lba(self) -> Option<u32> {
        let sectors = (self.minute as u32 * 60 + self.second as u32) * SECTORS_PER_SECOND + self.frame as u32;
        sectors.checked_sub(LEAD_IN)
    }

    /// The three values as packed BCD, as the drive reports them.
    pub fn bcd(self) -> [u8; 3] {
        [to_bcd(self.minute), to_bcd(self.second), to_bcd(self.frame)]
    }
}

pub fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

/// Decodes packed BCD; `None` if either digit is not decimal.
pub fn from_bcd(value: u8) -> Option<u8> {
    (value >> 4 < 10 && value & 0xF < 10).then_some((value >> 4) * 10 + (value & 0xF))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Track {
    /// Track number, 1-99.
    pub number: u8,
    /// CD-DA audio (as opposed to a data track).
    pub audio: bool,
    /// Logical sector of the track's INDEX 01, where it officially starts.
    pub start: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disc {
    /// All sectors from 00:02:00 on, `SECTOR_SIZE` bytes each.
    data: Vec<u8>,
    tracks: Vec<Track>,
}

impl Disc {
    /// Builds a disc from a cue sheet. `load` returns the contents of a
    /// file named in it.
    ///
    /// Supported: one or more `FILE ... BINARY` entries, each holding whole
    /// 2352-byte sectors, `TRACK nn MODE1/2352 | MODE2/2352 | AUDIO`,
    /// `INDEX 01`, and `PREGAP` (silence that is not in any file). The files
    /// are laid end to end starting at 00:02:00.
    pub fn from_cue(cue: &str, mut load: impl FnMut(&str) -> Result<Vec<u8>, String>) -> Result<Self, String> {
        let mut data: Vec<u8> = Vec::new();
        let mut tracks: Vec<Track> = Vec::new();
        // Sector at which the file being described starts.
        let mut file_start = 0u32;
        let mut pending: Option<(u8, bool)> = None;
        let mut pregap = 0u32;

        for line in cue.lines() {
            let line = line.trim();
            let (keyword, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            let rest = rest.trim();
            match keyword.to_ascii_uppercase().as_str() {
                "FILE" => {
                    // FILE "name with spaces.bin" BINARY
                    let name = match rest.strip_prefix('"') {
                        Some(quoted) => quoted.split('"').next().unwrap_or(""),
                        None => rest.split_whitespace().next().unwrap_or(""),
                    };
                    if !rest.to_ascii_uppercase().ends_with("BINARY") {
                        return Err(format!("cue: only BINARY files are supported: {line}"));
                    }
                    let contents = load(name)?;
                    if contents.len() % SECTOR_SIZE != 0 {
                        return Err(format!("{name}: size is not a multiple of {SECTOR_SIZE} bytes"));
                    }
                    file_start = (data.len() / SECTOR_SIZE) as u32;
                    data.extend_from_slice(&contents);
                }
                "TRACK" => {
                    let mut words = rest.split_whitespace();
                    let number = words.next().and_then(|n| n.parse().ok()).ok_or_else(|| format!("cue: bad track: {line}"))?;
                    let audio = match words.next().map(str::to_ascii_uppercase).as_deref() {
                        Some("AUDIO") => true,
                        Some("MODE1/2352") | Some("MODE2/2352") => false,
                        _ => return Err(format!("cue: unsupported track type: {line}")),
                    };
                    pending = Some((number, audio));
                }
                "PREGAP" => {
                    pregap = parse_time(rest).ok_or_else(|| format!("cue: bad time: {line}"))?;
                }
                "INDEX" => {
                    let mut words = rest.split_whitespace();
                    let index: u32 = words.next().and_then(|n| n.parse().ok()).ok_or_else(|| format!("cue: bad index: {line}"))?;
                    let time = words.next().and_then(parse_time).ok_or_else(|| format!("cue: bad time: {line}"))?;
                    if index == 1 {
                        let (number, audio) = pending.take().ok_or_else(|| format!("cue: INDEX without TRACK: {line}"))?;
                        // A PREGAP is on the disc but in no file: silence is
                        // inserted before the track, and everything after it
                        // in the same file moves up by its length.
                        let at = ((file_start + time) as usize * SECTOR_SIZE).min(data.len());
                        data.splice(at..at, std::iter::repeat_n(0, pregap as usize * SECTOR_SIZE));
                        file_start += std::mem::take(&mut pregap);
                        tracks.push(Track { number, audio, start: file_start + time });
                    }
                }
                // REM, TITLE, PERFORMER, FLAGS, ... carry nothing needed.
                _ => {}
            }
        }
        if tracks.is_empty() {
            return Err("cue: no tracks".into());
        }
        Ok(Self { data, tracks })
    }

    /// A disc with a single data track holding the given raw sectors.
    pub fn from_data_track(data: Vec<u8>) -> Result<Self, String> {
        if data.is_empty() || !data.len().is_multiple_of(SECTOR_SIZE) {
            return Err(format!("disc image size is not a multiple of {SECTOR_SIZE} bytes"));
        }
        Ok(Self { data, tracks: vec![Track { number: 1, audio: false, start: 0 }] })
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// Number of sectors; also the position of the lead-out.
    pub fn sector_count(&self) -> u32 {
        (self.data.len() / SECTOR_SIZE) as u32
    }

    /// The raw sector at `lba`, if it is on the disc.
    pub fn sector(&self, lba: u32) -> Option<&[u8]> {
        let start = lba as usize * SECTOR_SIZE;
        self.data.get(start..start + SECTOR_SIZE)
    }

    /// The track that contains `lba` (the last one starting at or before it).
    pub fn track_at(&self, lba: u32) -> &Track {
        self.tracks.iter().rev().find(|t| t.start <= lba).unwrap_or(&self.tracks[0])
    }

    /// The region letter of a licensed disc: the last letter of the "SCEx"
    /// string the drive reports (psx-spx: "GetID"): 'A' America, 'E' Europe,
    /// 'I' Japan. Taken from the licence text in sector 4 of the data track
    /// (psx-spx: "CDROM File Formats" — "Licensed by Sony Computer
    /// Entertainment ..."), since an image does not hold the wobble groove
    /// the real drive reads it from. `None` for unlicensed or audio discs.
    pub fn region(&self) -> Option<u8> {
        if self.tracks[0].audio {
            return None;
        }
        let sector = self.sector(4)?;
        let text: String = sector[24..24 + 96].iter().filter(|b| b.is_ascii_graphic()).map(|b| *b as char).collect();
        if !text.starts_with("LicensedbySonyComputerEntertainment") {
            return None;
        }
        if text.contains("America") {
            Some(b'A')
        } else if text.contains("Europe") {
            Some(b'E')
        } else if text.contains("Inc.") {
            Some(b'I')
        } else {
            None
        }
    }
}

/// Parses a cue sheet time "mm:ss:ff" into a number of sectors.
fn parse_time(text: &str) -> Option<u32> {
    let mut parts = text.split(':').map(|p| p.parse::<u32>().ok());
    let (m, s, f) = (parts.next()??, parts.next()??, parts.next()??);
    (parts.next().is_none() && s < 60 && f < SECTORS_PER_SECOND).then_some((m * 60 + s) * SECTORS_PER_SECOND + f)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A Mode 2 data sector for `lba` whose 2048 data bytes are `fill`.
    pub(crate) fn data_sector(lba: u32, fill: u8) -> Vec<u8> {
        let mut sector = vec![0u8; SECTOR_SIZE];
        sector[1..11].fill(0xFF);
        sector[12..15].copy_from_slice(&Msf::from_lba(lba).bcd());
        sector[15] = 2;
        // Subheader (file, channel, submode, coding info), twice.
        sector[16..24].copy_from_slice(&[0, 0, 8, 0, 0, 0, 8, 0]);
        sector[24..24 + 2048].fill(fill);
        sector
    }

    /// A data track of `count` sectors, sector n filled with byte n.
    pub(crate) fn data_track(count: u32) -> Vec<u8> {
        (0..count).flat_map(|lba| data_sector(lba, lba as u8)).collect()
    }

    #[test]
    fn msf_and_bcd_conversions() {
        assert_eq!(Msf::from_lba(0), Msf { minute: 0, second: 2, frame: 0 });
        assert_eq!(Msf::from_lba(74), Msf { minute: 0, second: 2, frame: 74 });
        assert_eq!(Msf::from_lba(4350), Msf { minute: 1, second: 0, frame: 0 });
        assert_eq!(Msf { minute: 1, second: 0, frame: 0 }.to_lba(), Some(4350));
        assert_eq!(Msf { minute: 0, second: 1, frame: 74 }.to_lba(), None, "inside the lead-in");
        assert_eq!(Msf { minute: 12, second: 34, frame: 56 }.bcd(), [0x12, 0x34, 0x56]);
        assert_eq!(from_bcd(0x59), Some(59));
        assert_eq!(from_bcd(0x5A), None);
        assert_eq!(from_bcd(0xA0), None);
        assert_eq!(to_bcd(74), 0x74);
    }

    #[test]
    fn cue_with_one_file_per_track() {
        // The layout of the redump-style images: data track, then an audio
        // track whose file starts with its 2-second pregap (INDEX 00).
        let cue = "FILE \"Game (Track 1).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n\
                   FILE \"Game (Track 2).bin\" BINARY\n  TRACK 02 AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 00:02:00\n";
        let mut asked = Vec::new();
        let disc = Disc::from_cue(cue, |name| {
            asked.push(name.to_string());
            Ok(if name.contains("Track 1") { data_track(300) } else { vec![0x55; 200 * SECTOR_SIZE] })
        })
        .unwrap();
        assert_eq!(asked, ["Game (Track 1).bin", "Game (Track 2).bin"]);
        assert_eq!(disc.sector_count(), 500);
        assert_eq!(disc.tracks(), [Track { number: 1, audio: false, start: 0 }, Track { number: 2, audio: true, start: 450 }]);
        assert_eq!(disc.sector(7).unwrap()[24], 7);
        assert_eq!(disc.sector(300).unwrap()[0], 0x55);
        assert!(disc.sector(500).is_none());
        assert_eq!(disc.track_at(449).number, 1);
        assert_eq!(disc.track_at(450).number, 2);
    }

    #[test]
    fn cue_with_one_file_and_a_pregap() {
        let cue = "FILE game.bin BINARY\nTRACK 01 MODE2/2352\nINDEX 01 00:00:00\nTRACK 02 AUDIO\nPREGAP 00:02:00\nINDEX 01 00:04:00\n";
        let disc = Disc::from_cue(cue, |_| Ok(data_track(400))).unwrap();
        // 150 sectors of silence are added; track 2 moves up by them.
        assert_eq!(disc.sector_count(), 550);
        assert_eq!(disc.tracks()[1].start, 300 + 150);
    }

    #[test]
    fn cue_errors() {
        let ok = |_: &str| Ok(data_track(10));
        assert!(Disc::from_cue("", ok).is_err(), "no tracks");
        assert!(Disc::from_cue("FILE a.wav WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00", ok).is_err());
        assert!(Disc::from_cue("FILE a.bin BINARY\nTRACK 01 MODE2/2336\nINDEX 01 00:00:00", ok).is_err());
        assert!(Disc::from_cue("FILE a.bin BINARY\nTRACK 01 MODE2/2352\nINDEX 01 00:61:00", ok).is_err());
        assert!(Disc::from_cue("FILE a.bin BINARY\nTRACK 01 MODE2/2352\nINDEX 01 00:00:00", |_| Ok(vec![0; 1000])).is_err());
        assert!(Disc::from_cue("FILE a.bin BINARY\nTRACK 01 MODE2/2352\nINDEX 01 00:00:00", |n| Err(format!("{n}: missing"))).is_err());
    }

    #[test]
    fn region_comes_from_the_licence_sector() {
        let licensed = |text: &[u8]| {
            let mut data = data_track(16);
            data[4 * SECTOR_SIZE + 24..][..text.len()].copy_from_slice(text);
            Disc::from_data_track(data).unwrap().region()
        };
        let america = b"          Licensed  by          Sony Computer Entertainment Amer  ica ";
        let europe = b"          Licensed  by          Sony Computer Entertainment Euro pe   ";
        let japan = b"          Licensed  by          Sony Computer Entertainment Inc.";
        assert_eq!(licensed(america), Some(b'A'));
        assert_eq!(licensed(europe), Some(b'E'));
        assert_eq!(licensed(japan), Some(b'I'));
        assert_eq!(licensed(b"homebrew"), None);
        assert!(Disc::from_data_track(vec![0; 100]).is_err());
    }
}
