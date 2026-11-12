//! Window frontend.
//!
//! Shows what the emulated console sends to the television, or the whole
//! VRAM, in a desktop window. The emulation itself lives in the core crates;
//! this crate only moves bytes: files in, pixels out.
//!
//! The keyboard plays a digital pad in port 1, and a file can back a memory
//! card in slot 1. There is no speed regulation beyond the window's 60 Hz
//! refresh: one emulated frame per window update.

mod audio;

use minifb::{Key, KeyRepeat, Window, WindowOptions};
use psx_core::disc::Disc;
use psx_core::exe::Exe;
use psx_core::gpu::{rgb15_to_24, VRAM_HEIGHT, VRAM_WIDTH};
use psx_core::sio::{button, DigitalPad, MemoryCard};
use psx_core::Psx;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

/// Cycles the BIOS may take to reach its shell before sideloading gives up.
const BOOT_CYCLES: u64 = 200_000_000;
/// Upper bound on the cycles emulated per window update, so the window
/// stays responsive even if the program stops producing frames
/// (an NTSC frame is about 571,000 cycles, a PAL frame about 680,000).
const MAX_CYCLES_PER_UPDATE: u64 = 1_000_000;

fn usage() -> ExitCode {
    eprintln!(
        "usage: yu-emulator <bios.bin> [program.exe] [--disc FILE] [--vram] [--card FILE] [--frames N] [--mute]\n\n  \
         <bios.bin>     PlayStation BIOS image (512 KB)\n  \
         [program.exe]  PS-EXE to run instead of the BIOS shell\n  \
         --disc FILE    disc in the drive: a .cue sheet (its .bin files next to it) or\n                 \
         a single .bin with one data track\n  \
         --vram         start with the whole 1024x512 VRAM instead of the display\n  \
         --card FILE    memory card in slot 1: a raw 128 KB image, created formatted if\n                 \
         missing and written back on exit if it changed\n  \
         --frames N     quit after N window updates (for automated checks)\n  \
         --mute         no sound\n\n\
         keys: Tab switches between display and VRAM, Esc quits\n\
         pad:  arrows = d-pad, Enter = START, Backspace = SELECT,\n      \
         Z = cross, X = circle, A = square, S = triangle, Q/W = L1/R1, 1/2 = L2/R2"
    );
    ExitCode::from(2)
}

struct Options {
    bios: String,
    exe: Option<String>,
    show_vram: bool,
    frames: Option<u64>,
    card: Option<String>,
    disc: Option<String>,
    mute: bool,
}

impl Options {
    fn parse(args: &[String]) -> Option<Self> {
        let mut positional = Vec::new();
        let mut options = Self { bios: String::new(), exe: None, show_vram: false, frames: None, card: None, disc: None, mute: false };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--vram" => options.show_vram = true,
                "--mute" => options.mute = true,
                "--frames" => options.frames = Some(args.next()?.parse().ok()?),
                "--card" => options.card = Some(args.next()?.clone()),
                "--disc" => options.disc = Some(args.next()?.clone()),
                flag if flag.starts_with("--") => return None,
                _ => positional.push(arg.clone()),
            }
        }
        match positional.as_slice() {
            [bios] => options.bios = bios.clone(),
            [bios, exe] => {
                options.bios = bios.clone();
                options.exe = Some(exe.clone());
            }
            _ => return None,
        }
        Some(options)
    }
}

/// Keyboard keys and the pad buttons they hold down.
const KEYS: [(Key, u16); 16] = [
    (Key::Up, button::UP),
    (Key::Down, button::DOWN),
    (Key::Left, button::LEFT),
    (Key::Right, button::RIGHT),
    (Key::Enter, button::START),
    (Key::Backspace, button::SELECT),
    (Key::Z, button::CROSS),
    (Key::X, button::CIRCLE),
    (Key::A, button::SQUARE),
    (Key::S, button::TRIANGLE),
    (Key::Q, button::L1),
    (Key::W, button::R1),
    (Key::Key1, button::L2),
    (Key::Key2, button::R2),
    (Key::E, button::L3),
    (Key::R, button::R3),
];

/// The pad buttons held by the given set of pressed keys.
fn pad_buttons(is_down: impl Fn(Key) -> bool) -> u16 {
    KEYS.iter().filter(|(key, _)| is_down(*key)).fold(0, |mask, (_, bit)| mask | bit)
}

/// A picture in the window's pixel format (0x00RRGGBB per pixel).
struct Picture {
    width: usize,
    height: usize,
    pixels: Vec<u32>,
}

fn pack(rgb: &[u8]) -> u32 {
    ((rgb[0] as u32) << 16) | ((rgb[1] as u32) << 8) | rgb[2] as u32
}

/// The displayed picture. While the GPU reports a zero-sized display (bad
/// display range registers) a 1x1 black picture stands in.
fn display_picture(psx: &Psx) -> Picture {
    let frame = psx.bus.gpu.frame();
    if frame.width == 0 || frame.height == 0 {
        return Picture { width: 1, height: 1, pixels: vec![0] };
    }
    Picture { width: frame.width, height: frame.height, pixels: frame.rgb.chunks(3).map(pack).collect() }
}

/// The whole VRAM, textures and palettes included.
fn vram_picture(psx: &Psx) -> Picture {
    let pixels = psx.bus.gpu.vram().iter().map(|pixel| pack(&rgb15_to_24(*pixel))).collect();
    Picture { width: VRAM_WIDTH, height: VRAM_HEIGHT, pixels }
}

/// Runs the console until the GPU finishes a frame.
fn run_frame(psx: &mut Psx, tty: &mut impl Write) {
    let frame = psx.bus.gpu.video.frame();
    let limit = psx.cycles() + MAX_CYCLES_PER_UPDATE;
    while psx.bus.gpu.video.frame() == frame && psx.cycles() < limit {
        psx.step();
    }
    let output = psx.take_tty();
    if !output.is_empty() {
        // The terminal may be gone; the emulator keeps running regardless.
        let _ = tty.write_all(&output).and_then(|()| tty.flush());
    }
}

/// Reads a disc image: a .cue sheet and the .bin files it names (looked up
/// next to the sheet), or a single .bin holding one data track.
fn load_disc(path: &str) -> Result<Disc, String> {
    let path = Path::new(path);
    let read = |file: &Path| std::fs::read(file).map_err(|e| format!("could not read {}: {e}", file.display()));
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cue")) {
        let cue = String::from_utf8_lossy(&read(path)?).into_owned();
        let dir = path.parent().unwrap_or(Path::new("."));
        Disc::from_cue(&cue, |name| read(&dir.join(name))).map_err(|e| format!("{}: {e}", path.display()))
    } else {
        Disc::from_data_track(read(path)?).map_err(|e| format!("{}: {e}", path.display()))
    }
}

fn boot(options: &Options) -> Result<Psx, String> {
    let bios = std::fs::read(&options.bios).map_err(|e| format!("could not read {}: {e}", options.bios))?;
    let mut psx = Psx::new(bios).map_err(|e| e.to_string())?;
    if let Some(path) = &options.disc {
        psx.bus.cdrom.insert(Some(load_disc(path)?));
    }
    if let Some(path) = &options.exe {
        let file = std::fs::read(path).map_err(|e| format!("could not read {path}: {e}"))?;
        let exe = Exe::parse(&file).map_err(|e| format!("{path}: {e}"))?;
        if !psx.run_to_shell(BOOT_CYCLES) {
            return Err(format!("BIOS did not reach its shell within {BOOT_CYCLES} cycles"));
        }
        psx.take_tty();
        psx.sideload(&exe);
    }
    psx.bus.sio0.ports[0].pad = Some(DigitalPad::default());
    if let Some(path) = &options.card {
        let card = match std::fs::read(path) {
            Ok(data) => MemoryCard::new(data).ok_or_else(|| format!("{path}: a memory card image must be 131072 bytes"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => MemoryCard::formatted(),
            Err(e) => return Err(format!("could not read {path}: {e}")),
        };
        psx.bus.sio0.ports[0].card = Some(card);
    }
    Ok(psx)
}

/// Writes the memory card back to its file if the program changed it.
fn save_card(options: &Options, psx: &Psx) -> Result<(), String> {
    let (Some(path), Some(card)) = (&options.card, &psx.bus.sio0.ports[0].card) else { return Ok(()) };
    if card.dirty {
        std::fs::write(path, card.data()).map_err(|e| format!("could not write {path}: {e}"))?;
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(options) = Options::parse(&args) else { return usage() };
    let mut psx = match boot(&options) {
        Ok(psx) => psx,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    // The window is created at twice the size of a 320x240 picture and can
    // be resized; pictures of any size are scaled to fit, keeping their
    // proportions. (A television shows every mode at 4:3; that correction
    // is not applied, so 640x240 and similar modes look stretched.)
    let window_options = WindowOptions { resize: true, scale_mode: minifb::ScaleMode::AspectRatioStretch, ..Default::default() };
    let mut window = match Window::new("Yu-Emulator", 640, 480, window_options) {
        Ok(window) => window,
        Err(e) => {
            eprintln!("could not open a window: {e}");
            return ExitCode::FAILURE;
        }
    };
    window.set_target_fps(60);

    // Sound is optional: without a device the emulator runs silently.
    let sound = if options.mute {
        None
    } else {
        audio::Output::open(psx_core::spu::SAMPLE_RATE).map_err(|e| eprintln!("no sound: {e}")).ok()
    };

    let mut show_vram = options.show_vram;
    let mut updates = 0u64;
    let stdout = std::io::stdout();
    while window.is_open() && !window.is_key_down(Key::Escape) {
        if window.is_key_pressed(Key::Tab, KeyRepeat::No) {
            show_vram = !show_vram;
        }
        if let Some(pad) = psx.bus.sio0.ports[0].pad.as_mut() {
            pad.pressed = pad_buttons(|key| window.is_key_down(key));
        }
        run_frame(&mut psx, &mut stdout.lock());
        let samples = psx.bus.spu.take_samples();
        if let Some(sound) = &sound {
            sound.push(&samples);
        }
        let picture = if show_vram { vram_picture(&psx) } else { display_picture(&psx) };
        if let Err(e) = window.update_with_buffer(&picture.pixels, picture.width, picture.height) {
            eprintln!("could not update the window: {e}");
            return ExitCode::FAILURE;
        }
        updates += 1;
        if options.frames.is_some_and(|limit| updates >= limit) {
            break;
        }
    }
    if let Err(e) = save_card(&options, &psx) {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_arguments() {
        let o = Options::parse(&args(&["bios.bin"])).unwrap();
        assert_eq!((o.bios.as_str(), o.exe, o.show_vram, o.frames), ("bios.bin", None, false, None));
        let o = Options::parse(&args(&["--vram", "bios.bin", "--frames", "3", "game.exe", "--card", "a.mcd", "--disc", "g.cue"])).unwrap();
        assert_eq!(o.disc.as_deref(), Some("g.cue"));
        assert_eq!((o.exe.as_deref(), o.show_vram, o.frames), (Some("game.exe"), true, Some(3)));
        assert_eq!(o.card.as_deref(), Some("a.mcd"));
        assert!(Options::parse(&args(&[])).is_none());
        assert!(Options::parse(&args(&["a", "b", "c"])).is_none());
        assert!(Options::parse(&args(&["a", "--frames"])).is_none());
        assert!(Options::parse(&args(&["a", "--unknown"])).is_none());
    }

    #[test]
    fn keys_map_to_pad_buttons() {
        assert_eq!(pad_buttons(|_| false), 0);
        assert_eq!(pad_buttons(|key| key == Key::Enter || key == Key::Z), button::START | button::CROSS);
        assert_eq!(pad_buttons(|_| true), 0xFFFF, "every button has a key");
    }

    #[test]
    fn memory_card_file_is_created_and_saved_when_changed() {
        let path = std::env::temp_dir().join(format!("yu-emulator-card-{}.mcd", std::process::id()));
        let bios = std::env::temp_dir().join(format!("yu-emulator-bios-{}.bin", std::process::id()));
        std::fs::write(&bios, vec![0u8; psx_core::map::BIOS_SIZE]).unwrap();
        let options = Options {
            bios: bios.display().to_string(),
            exe: None,
            show_vram: false,
            frames: None,
            card: Some(path.display().to_string()),
            disc: None,
            mute: true,
        };
        let mut psx = boot(&options).unwrap();
        assert!(psx.bus.sio0.ports[0].pad.is_some());
        // Unchanged card: nothing is written.
        save_card(&options, &psx).unwrap();
        assert!(!path.exists());
        // Changed card: the 128 KB image is written.
        psx.bus.sio0.ports[0].card.as_mut().unwrap().dirty = true;
        save_card(&options, &psx).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 131072);
        // And loaded again; a file of the wrong size is refused.
        assert!(boot(&options).is_ok());
        std::fs::write(&path, [0u8; 10]).unwrap();
        assert!(boot(&options).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(&bios).unwrap();
    }

    #[test]
    fn pictures_use_the_window_pixel_format() {
        assert_eq!(pack(&[0x12, 0x34, 0x56]), 0x0012_3456);
        let mut psx = Psx::new(vec![0; psx_core::map::BIOS_SIZE]).unwrap();
        // Fill 16x1 at (0,0) with pure red through GP0.
        for word in [0x0200_00FF, 0, (1 << 16) | 16] {
            psx.bus.store32(0x1F80_1810, word).unwrap();
        }
        let vram = vram_picture(&psx);
        assert_eq!((vram.width, vram.height, vram.pixels.len()), (1024, 512, 1024 * 512));
        assert_eq!(vram.pixels[0], 0x00FF_0000);
        assert_eq!(vram.pixels[16], 0);
        // The display is off after reset: black, at the reset size.
        let display = display_picture(&psx);
        assert_eq!((display.width, display.height), (256, 240));
        assert!(display.pixels.iter().all(|p| *p == 0));
    }

    #[test]
    fn run_frame_stops_at_a_frame_boundary_and_forwards_tty() {
        let mut psx = Psx::new(vec![0; psx_core::map::BIOS_SIZE]).unwrap();
        let mut tty = Vec::new();
        run_frame(&mut psx, &mut tty);
        assert_eq!(psx.bus.gpu.video.frame(), 1);
        // One NTSC frame: 263 lines of 3413 video cycles at 11/7 per CPU cycle.
        let expected = 263 * 3413 * 7 / 11;
        // Instructions in the BIOS ROM take 25 cycles each, so the frame is
        // overshot by at most one of them.
        assert!(psx.cycles().abs_diff(expected) <= 25, "{} cycles", psx.cycles());
        assert!(tty.is_empty());
    }
}
