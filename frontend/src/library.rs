//! The game library: the disc images found in a folder, with what the menu
//! shows about each one (format, region, size).

use std::io::Read;
use std::path::{Path, PathBuf};

/// How deep below the games folder disc images are looked for.
const SCAN_DEPTH: usize = 4;

/// Bytes read from the start of a disc's first file to find its region:
/// the 16 sectors of the system area, at 2352 bytes per raw sector.
const SYSTEM_AREA_BYTES: u64 = 16 * 2352;

/// The television standard and market a disc was licensed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    NtscU,
    NtscJ,
    Pal,
}

impl Region {
    pub fn label(self) -> &'static str {
        match self {
            Region::NtscU => "NTSC-U",
            Region::NtscJ => "NTSC-J",
            Region::Pal => "PAL",
        }
    }
}

/// A disc image the menu offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Game {
    /// The file name without its extension.
    pub title: String,
    pub path: PathBuf,
    /// "CUE/BIN" for a cue sheet, otherwise the extension in capitals.
    pub format: String,
    /// `None` when the licence string was not found.
    pub region: Option<Region>,
    /// Bytes on disk: for a cue sheet, the files it names.
    pub size: u64,
}

impl Game {
    /// What is known about the disc image at `path`. Files that cannot be
    /// read just leave the region unknown and the size at zero.
    pub fn from_path(path: &Path) -> Self {
        let title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let is_cue = has_extension(path, &["cue"]);
        let format = if is_cue {
            "CUE/BIN".to_string()
        } else {
            path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default()
        };
        // The files holding the disc's sectors, the data track first.
        let files: Vec<PathBuf> = if is_cue {
            let dir = path.parent().unwrap_or(Path::new("."));
            let sheet = std::fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default();
            cue_files(&sheet).iter().map(|name| dir.join(name)).collect()
        } else {
            vec![path.to_path_buf()]
        };
        let size = files.iter().filter_map(|file| std::fs::metadata(file).ok()).map(|m| m.len()).sum();
        let region = files.first().and_then(|file| {
            let mut start = Vec::new();
            std::fs::File::open(file).ok()?.take(SYSTEM_AREA_BYTES).read_to_end(&mut start).ok()?;
            region_of(&start)
        });
        Self { title, path: path.to_path_buf(), format, region, size }
    }

    /// "CUE/BIN · NTSC-U", or just the format when the region is unknown.
    pub fn subtitle(&self) -> String {
        match self.region {
            Some(region) => format!("{} · {}", self.format, region.label()),
            None => self.format.clone(),
        }
    }
}

/// The file names in the FILE lines of a cue sheet, in order.
pub fn cue_files(sheet: &str) -> Vec<String> {
    sheet
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("FILE")?.trim();
            match rest.strip_prefix('"') {
                Some(quoted) => quoted.split('"').next(),
                None => rest.split_whitespace().next(),
            }
            .map(str::to_string)
        })
        .collect()
}

/// The region named by the licence string in a disc's system area.
// psx-spx: "CDROM ISO Volume Descriptors", System Area: sector 4 holds
// "Licensed by" and then "Sony Computer Entertainment " followed by
// "Euro pe", "Inc." (Japan) or "Amer  ica". The sector's position in the
// file depends on the sector size (2048 or 2352 bytes), so the text is
// searched for rather than read from a fixed offset.
pub fn region_of(system_area: &[u8]) -> Option<Region> {
    const OWNER: &[u8] = b"Sony Computer Entertainment ";
    let at = system_area.windows(OWNER.len()).position(|window| window == OWNER)?;
    let rest = &system_area[at + OWNER.len()..];
    if rest.starts_with(b"Amer") {
        Some(Region::NtscU)
    } else if rest.starts_with(b"Euro") {
        Some(Region::Pal)
    } else if rest.starts_with(b"Inc.") {
        Some(Region::NtscJ)
    } else {
        None
    }
}

/// A size in whole megabytes ("612 MB"), or kilobytes below one megabyte.
pub fn format_size(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if bytes >= MB {
        format!("{} MB", (bytes + MB / 2) / MB)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

/// A path with the home folder written as `~`.
pub fn display_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The user's home folder, if the environment names one.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).filter(|home| !home.is_empty()).map(PathBuf::from)
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| extensions.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn scan_into(dir: &Path, depth: usize, games: &mut Vec<Game>) -> std::io::Result<()> {
    let mut files = Vec::new();
    let mut folders = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            folders.push(path);
        } else {
            files.push(path);
        }
    }
    // A folder with cue sheets offers those; its .bin files are their
    // tracks. Without any, each raw image is a disc of its own.
    let sheets: Vec<&PathBuf> = files.iter().filter(|f| has_extension(f, &["cue"])).collect();
    let images: Vec<&PathBuf> =
        if sheets.is_empty() { files.iter().filter(|f| has_extension(f, &["bin", "iso", "img"])).collect() } else { sheets };
    for path in images {
        games.push(Game::from_path(path));
    }
    if depth > 0 {
        for folder in folders {
            // A folder that cannot be read just offers nothing.
            let _ = scan_into(&folder, depth - 1, games);
        }
    }
    Ok(())
}

/// The disc images in a folder and the folders below it, sorted by title.
pub fn scan(dir: &Path) -> Result<Vec<Game>, String> {
    let mut games = Vec::new();
    scan_into(dir, SCAN_DEPTH, &mut games).map_err(|e| format!("could not read {}: {e}", dir.display()))?;
    games.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()).then_with(|| a.path.cmp(&b.path)));
    Ok(games)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("yu-emulator-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn scan_finds_cue_sheets_and_lone_images_sorted_by_title() {
        let root = temp_dir("library-scan");
        for dir in ["zelda", "alpha/disc", "empty"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in [
            // A cue sheet hides the tracks next to it.
            "zelda/Zeta.cue",
            "zelda/Zeta (Track 1).bin",
            "zelda/Zeta (Track 2).bin",
            // Without one, the raw image is the disc.
            "alpha/disc/beta.BIN",
            "alpha/notes.txt",
            "Alpha.CUE",
            "save.mcd",
        ] {
            std::fs::write(root.join(file), b"").unwrap();
        }
        let found = scan(&root).unwrap();
        let titles: Vec<&str> = found.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["Alpha", "beta", "Zeta"]);
        assert_eq!(found[1].path, root.join("alpha/disc/beta.BIN"));
        assert_eq!(scan(&root.join("empty")).unwrap(), []);
        assert!(scan(&root.join("missing")).unwrap_err().contains("missing"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cue_sheets_name_their_files() {
        let sheet = "FILE \"Game (Track 1).bin\" BINARY\r\n  TRACK 01 MODE2/2352\r\n    INDEX 01 00:00:00\r\n\
                     file ignored.bin BINARY\nFILE plain.bin BINARY\n  TRACK 02 AUDIO\n";
        assert_eq!(cue_files(sheet), ["Game (Track 1).bin", "plain.bin"]);
        assert!(cue_files("").is_empty());
    }

    #[test]
    fn the_licence_string_names_the_region() {
        let system_area = |owner: &str, sector_size: usize| {
            let mut bytes = vec![0u8; 16 * sector_size];
            let text = format!("          Licensed  by          Sony Computer Entertainment {owner}");
            // Raw sectors start with 24 bytes of sync, header and subheader.
            let start = 4 * sector_size + if sector_size == 2352 { 24 } else { 0 };
            bytes[start..start + text.len()].copy_from_slice(text.as_bytes());
            bytes
        };
        assert_eq!(region_of(&system_area("Amer  ica ", 2352)), Some(Region::NtscU));
        assert_eq!(region_of(&system_area("Euro pe   ", 2352)), Some(Region::Pal));
        assert_eq!(region_of(&system_area("Inc.\n", 2352)), Some(Region::NtscJ));
        assert_eq!(region_of(&system_area("Euro pe   ", 2048)), Some(Region::Pal));
        assert_eq!(region_of(&system_area("Mars", 2352)), None);
        assert_eq!(region_of(&[0; 4096]), None);
        assert_eq!(region_of(&[]), None);
        assert_eq!(Region::NtscU.label(), "NTSC-U");
    }

    #[test]
    fn a_game_knows_its_format_region_and_size() {
        let root = temp_dir("library-game");
        let mut track = vec![0u8; 5 * 2352];
        let text = b"          Licensed  by          Sony Computer Entertainment Euro pe   ";
        track[4 * 2352 + 24..4 * 2352 + 24 + text.len()].copy_from_slice(text);
        std::fs::write(root.join("Game (Track 1).bin"), &track).unwrap();
        std::fs::write(root.join("Game (Track 2).bin"), [0u8; 1000]).unwrap();
        std::fs::write(root.join("Game.cue"), "FILE \"Game (Track 1).bin\" BINARY\nFILE \"Game (Track 2).bin\" BINARY\n").unwrap();
        std::fs::write(root.join("lone.iso"), [0u8; 3000]).unwrap();

        let game = Game::from_path(&root.join("Game.cue"));
        assert_eq!((game.title.as_str(), game.format.as_str()), ("Game", "CUE/BIN"));
        assert_eq!((game.region, game.size), (Some(Region::Pal), 5 * 2352 + 1000));
        assert_eq!(game.subtitle(), "CUE/BIN · PAL");

        let lone = Game::from_path(&root.join("lone.iso"));
        assert_eq!((lone.format.as_str(), lone.region, lone.size), ("ISO", None, 3000));
        assert_eq!(lone.subtitle(), "ISO");

        // A sheet naming files that are not there is still listed.
        std::fs::write(root.join("broken.cue"), "FILE \"gone.bin\" BINARY\n").unwrap();
        let broken = Game::from_path(&root.join("broken.cue"));
        assert_eq!((broken.region, broken.size), (None, 0));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sizes_are_shown_in_megabytes() {
        assert_eq!(format_size(0), "0 KB");
        assert_eq!(format_size(1), "1 KB");
        assert_eq!(format_size(1024 * 1024 - 1), "1024 KB");
        assert_eq!(format_size(1024 * 1024), "1 MB");
        assert_eq!(format_size(612 * 1024 * 1024 + 400_000), "612 MB");
        assert_eq!(format_size(612 * 1024 * 1024 + 600_000), "613 MB");
    }

    #[test]
    fn paths_under_home_start_with_a_tilde() {
        let home = Path::new("/home/someone");
        assert_eq!(display_path(Path::new("/home/someone/Jogos/a.cue"), Some(home)), "~/Jogos/a.cue");
        assert_eq!(display_path(home, Some(home)), "~");
        assert_eq!(display_path(Path::new("/mnt/games"), Some(home)), "/mnt/games");
        assert_eq!(display_path(Path::new("/mnt/games"), None), "/mnt/games");
    }
}
