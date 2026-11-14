//! Window frontend.
//!
//! Shows what the emulated console sends to the television, or the whole
//! VRAM, in a desktop window. The emulation itself lives in the core crates;
//! this crate only moves bytes: files in, pixels out.
//!
//! The keyboard and any connected controller play a digital pad in port 1,
//! and a file can back a memory card in slot 1. With nothing named to run,
//! the frontend starts in a menu listing the disc images of the games
//! folder. The window is drawn with egui on wgpu; whatever the display's
//! refresh rate, one emulated frame runs per sixtieth of a second.

mod audio;
mod config;
mod input;
mod library;
mod ui;

use eframe::egui::{self, vec2, Color32, ColorImage, Key, Rect, TextureHandle, TextureOptions, ViewportCommand};
use input::Gamepads;
use psx_core::disc::Disc;
use psx_core::exe::Exe;
use psx_core::gpu::{rgb15_to_24, VRAM_HEIGHT, VRAM_WIDTH};
use psx_core::sio::{button, DigitalPad, MemoryCard};
use psx_core::Psx;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ui::menu::{Action, Choice, Menu};
use ui::theme;
use ui::view::Status;

/// Cycles the BIOS may take to reach its shell before sideloading gives up.
const BOOT_CYCLES: u64 = 200_000_000;
/// Upper bound on the cycles emulated per window update, so the window
/// stays responsive even if the program stops producing frames
/// (an NTSC frame is about 571,000 cycles, a PAL frame about 680,000).
const MAX_CYCLES_PER_UPDATE: u64 = 1_000_000;

fn usage() -> ExitCode {
    eprintln!(
        "usage: yu-emulator <bios.bin> [program.exe] [--games DIR] [--disc FILE] [--no-menu] [--vram] [--card FILE] [--frames N] [--mute]\n\n  \
         <bios.bin>     PlayStation BIOS image (512 KB)\n  \
         [program.exe]  PS-EXE to run instead of the BIOS shell\n  \
         --games DIR    the menu lists the disc images (.cue, or .bin/.iso/.img without a\n                 \
         cue sheet) found in DIR and the folders below it, instead of\n                 \
         those of the folder picked in the menu earlier\n  \
         --disc FILE    disc in the drive: a .cue sheet (its .bin files next to it) or\n                 \
         a single .bin with one data track\n  \
         --no-menu      start the BIOS shell without a disc instead of the menu\n  \
         --vram         start with the whole 1024x512 VRAM instead of the display\n  \
         --card FILE    memory card in slot 1: a raw 128 KB image, created formatted if\n                 \
         missing and written back on exit if it changed\n  \
         --frames N     quit after N window updates (for automated checks)\n  \
         --mute         no sound\n\n\
         Without a program or a disc the window opens in the menu.\n\
         keys: Tab switches between display and VRAM, Esc quits,\n      \
         F1 goes back to the menu\n\
         pad:  arrows = d-pad, Enter = START, Backspace = SELECT,\n      \
         Z = cross, X = circle, A = square, S = triangle, Q/W = L1/R1, 1/2 = L2/R2\n\
         controllers: buttons by position (bottom face button = cross), the left\n      \
         stick also works as the d-pad; the guide button, or both stick clicks\n      \
         together, go back to the menu"
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
    games: Option<String>,
    no_menu: bool,
    mute: bool,
}

impl Options {
    fn parse(args: &[String]) -> Option<Self> {
        let mut positional = Vec::new();
        let mut options = Self { bios: String::new(), exe: None, show_vram: false, frames: None, card: None, disc: None, games: None, no_menu: false, mute: false };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--vram" => options.show_vram = true,
                "--mute" => options.mute = true,
                "--no-menu" => options.no_menu = true,
                "--frames" => options.frames = Some(args.next()?.parse().ok()?),
                "--card" => options.card = Some(args.next()?.clone()),
                "--disc" => options.disc = Some(args.next()?.clone()),
                "--games" => options.games = Some(args.next()?.clone()),
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
    (Key::ArrowUp, button::UP),
    (Key::ArrowDown, button::DOWN),
    (Key::ArrowLeft, button::LEFT),
    (Key::ArrowRight, button::RIGHT),
    (Key::Enter, button::START),
    (Key::Backspace, button::SELECT),
    (Key::Z, button::CROSS),
    (Key::X, button::CIRCLE),
    (Key::A, button::SQUARE),
    (Key::S, button::TRIANGLE),
    (Key::Q, button::L1),
    (Key::W, button::R1),
    (Key::Num1, button::L2),
    (Key::Num2, button::R2),
    (Key::E, button::L3),
    (Key::R, button::R3),
];

/// The pad buttons held by the given set of pressed keys.
fn pad_buttons(is_down: impl Fn(Key) -> bool) -> u16 {
    KEYS.iter().filter(|(key, _)| is_down(*key)).fold(0, |mask, (_, bit)| mask | bit)
}

/// The displayed picture. While the GPU reports a zero-sized display (bad
/// display range registers) a 1x1 black picture stands in.
fn display_picture(psx: &Psx) -> ColorImage {
    let frame = psx.bus.gpu.frame();
    if frame.width == 0 || frame.height == 0 {
        return ColorImage::filled([1, 1], Color32::BLACK);
    }
    ColorImage::from_rgb([frame.width, frame.height], &frame.rgb)
}

/// The whole VRAM, textures and palettes included.
fn vram_picture(psx: &Psx) -> ColorImage {
    let colour = |pixel: &u16| {
        let [r, g, b] = rgb15_to_24(*pixel);
        Color32::from_rgb(r, g, b)
    };
    ColorImage::new([VRAM_WIDTH, VRAM_HEIGHT], psx.bus.gpu.vram().iter().map(colour).collect())
}

/// Where a picture goes in the window: as large as fits, in the middle,
/// keeping its proportions. (A television shows every mode at 4:3; that
/// correction is not applied, so 640x240 and similar modes look stretched.)
fn fit(picture: [usize; 2], window: Rect) -> Rect {
    let size = vec2(picture[0].max(1) as f32, picture[1].max(1) as f32);
    let scale = (window.width() / size.x).min(window.height() / size.y);
    Rect::from_center_size(window.center(), size * scale)
}

/// Decides when the next sixtieth of a second starts, whatever the
/// display's refresh rate: an emulated frame (or a menu update) runs only
/// when one is due.
struct Pacer {
    next: Instant,
}

impl Pacer {
    const TICK: Duration = Duration::from_nanos(1_000_000_000 / 60);
    /// How early a window update may come and still count: a 60 Hz display
    /// never refreshes exactly on time, and each refresh should get a frame.
    const SLACK: Duration = Duration::from_millis(2);

    fn new(now: Instant) -> Self {
        Self { next: now }
    }

    /// Whether a tick is due at `now`. Time lost in a stall is not made up.
    fn due(&mut self, now: Instant) -> bool {
        if now + Self::SLACK < self.next {
            return false;
        }
        self.next += Self::TICK;
        if self.next < now {
            self.next = now + Self::TICK;
        }
        true
    }
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
fn load_disc(path: &Path) -> Result<Disc, String> {
    let read = |file: &Path| std::fs::read(file).map_err(|e| format!("could not read {}: {e}", file.display()));
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cue")) {
        let cue = String::from_utf8_lossy(&read(path)?).into_owned();
        let dir = path.parent().unwrap_or(Path::new("."));
        Disc::from_cue(&cue, |name| read(&dir.join(name))).map_err(|e| format!("{}: {e}", path.display()))
    } else {
        Disc::from_data_track(read(path)?).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Starts the console with the given disc in the drive (or none).
fn boot(options: &Options, disc: Option<&Path>) -> Result<Psx, String> {
    let bios = std::fs::read(&options.bios).map_err(|e| format!("could not read {}: {e}", options.bios))?;
    let mut psx = Psx::new(bios).map_err(|e| e.to_string())?;
    if let Some(path) = disc {
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

/// What the window title says while a disc runs.
fn title(disc: Option<&Path>) -> String {
    match disc.and_then(|path| path.file_stem()) {
        Some(name) => format!("Yu-Emulator - {}", name.to_string_lossy()),
        None => "Yu-Emulator".to_string(),
    }
}

/// The disc images a file dialog offers.
const DISC_EXTENSIONS: [&str; 4] = ["cue", "bin", "iso", "img"];

/// The window's contents: the menu, or the console when one is running.
struct App {
    options: Options,
    menu: Menu,
    /// The running console; `None` while the menu is on screen.
    psx: Option<Psx>,
    sound: Option<audio::Output>,
    gamepads: Gamepads,
    /// What the controllers held at the last update.
    controllers: input::State,
    show_vram: bool,
    /// Window updates so far, for `--frames`.
    updates: u64,
    pacer: Pacer,
    /// The texture the console's picture is uploaded to.
    screen: Option<TextureHandle>,
    home: Option<PathBuf>,
    /// Why the frontend has to stop, for `run` to report after the window closes.
    failure: Arc<Mutex<Option<String>>>,
}

impl App {
    /// Reads the games folder and starts what the command line names.
    fn new(options: Options) -> Result<Self, String> {
        // A folder named on the command line must be readable; the one
        // remembered from an earlier run may be gone, which the menu says.
        let mut menu = match &options.games {
            Some(folder) => Menu::new(library::scan(Path::new(folder))?, Some(PathBuf::from(folder))),
            None => Menu::new(Vec::new(), None),
        };
        if options.games.is_none() {
            if let Some(folder) = config::games_folder() {
                match library::scan(&folder) {
                    Ok(games) => menu.set_games(games, Some(folder)),
                    Err(e) => menu.message = Some(e),
                }
            }
        }
        // With nothing named to run, the menu comes first.
        let disc = options.disc.as_deref().map(Path::new);
        let psx = if disc.is_some() || options.exe.is_some() || options.no_menu { Some(boot(&options, disc)?) } else { None };

        // Sound is optional: without a device the emulator runs silently.
        let sound = if options.mute {
            None
        } else {
            audio::Output::open(psx_core::spu::SAMPLE_RATE).map_err(|e| eprintln!("no sound: {e}")).ok()
        };
        Ok(Self {
            show_vram: options.show_vram,
            options,
            menu,
            psx,
            sound,
            gamepads: Gamepads::open(),
            controllers: input::State::default(),
            updates: 0,
            pacer: Pacer::new(Instant::now()),
            screen: None,
            home: library::home_dir(),
            failure: Arc::new(Mutex::new(None)),
        })
    }

    /// Stops the frontend with an error.
    fn fail(&mut self, ctx: &egui::Context, error: String) {
        *self.failure.lock().unwrap() = Some(error);
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    /// Starts the console with the given disc (or none). A disc that cannot
    /// be read leaves the menu on screen with the reason.
    fn start(&mut self, ctx: &egui::Context, disc: Option<&Path>) {
        match boot(&self.options, disc) {
            Ok(console) => {
                ctx.send_viewport_cmd(ViewportCommand::Title(title(disc)));
                self.psx = Some(console);
            }
            Err(e) => self.menu.message = Some(e),
        }
    }

    /// Leaves the running game for the menu. The game is not kept; only
    /// the memory card is saved.
    fn back_to_menu(&mut self, ctx: &egui::Context) {
        if let Some(console) = self.psx.take() {
            if let Err(e) = save_card(&self.options, &console) {
                self.fail(ctx, e);
            }
        }
        self.menu.wait_for_release();
        ctx.send_viewport_cmd(ViewportCommand::Title(title(None)));
    }

    /// Lists the games of another folder and remembers it for the next runs.
    fn open_folder(&mut self, folder: PathBuf) {
        match library::scan(&folder) {
            Ok(games) => {
                self.menu.message = config::save_games_folder(&folder).err();
                self.menu.set_games(games, Some(folder));
            }
            Err(e) => self.menu.message = Some(e),
        }
    }

    /// Does what the menu asked for.
    fn perform(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Play(Choice::Game(path)) => self.start(ctx, Some(&path)),
            Action::Play(Choice::Bios) => self.start(ctx, None),
            // The dialogs are the system's own and block the window while
            // open; they need a mouse or a keyboard.
            Action::PickFolder => {
                let mut dialog = rfd::FileDialog::new().set_title("Escolher a pasta de jogos");
                if let Some(folder) = &self.menu_folder() {
                    dialog = dialog.set_directory(folder);
                }
                if let Some(folder) = dialog.pick_folder() {
                    self.open_folder(folder);
                }
                self.menu.wait_for_release();
            }
            Action::OpenDisc => {
                let dialog = rfd::FileDialog::new().set_title("Abrir um disco").add_filter("Imagens de disco", &DISC_EXTENSIONS);
                if let Some(file) = dialog.pick_file() {
                    self.start(ctx, Some(&file));
                }
                self.menu.wait_for_release();
            }
        }
    }

    /// The folder the menu lists, if it still exists, for a dialog to start in.
    fn menu_folder(&self) -> Option<PathBuf> {
        self.menu.folder().filter(|folder| folder.is_dir()).map(Path::to_path_buf)
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Controllers do not wake the window up, and a running game needs
        // every refresh: keep the updates coming.
        ctx.request_repaint();
        if self.options.frames.is_some() {
            // An automated check counts window updates, and a window that
            // is minimised or covered would otherwise get none.
            ctx.send_viewport_cmd(ViewportCommand::RequestPaintWhileHidden);
        }
        let (keys, escape, to_menu, tab) =
            ctx.input(|i| (pad_buttons(|key| i.key_down(key)), i.key_pressed(Key::Escape), i.key_pressed(Key::F1), i.key_pressed(Key::Tab)));
        if escape {
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }
        self.controllers = self.gamepads.poll();
        let buttons = keys | self.controllers.buttons;
        let due = self.pacer.due(Instant::now());

        match &mut self.psx {
            Some(_) if to_menu || self.controllers.menu => self.back_to_menu(ctx),
            Some(console) => {
                if tab {
                    self.show_vram = !self.show_vram;
                }
                if due {
                    if let Some(pad) = console.bus.sio0.ports[0].pad.as_mut() {
                        pad.pressed = buttons;
                    }
                    run_frame(console, &mut std::io::stdout().lock());
                    let samples = console.bus.spu.take_samples();
                    if let Some(sound) = &self.sound {
                        sound.push(&samples);
                    }
                }
            }
            None if due => {
                // In the menu Tab moves on like Down, as on any form.
                let tab_down = ctx.input(|i| i.key_down(Key::Tab));
                let buttons = buttons | if tab_down { button::DOWN } else { 0 };
                if let Some(action) = self.menu.update(buttons) {
                    self.perform(ctx, action);
                }
            }
            None => {}
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        match &self.psx {
            Some(console) => {
                let picture = if self.show_vram { vram_picture(console) } else { display_picture(console) };
                let window = ctx.content_rect();
                let place = fit(picture.size, window);
                // Nearest neighbour: the console's pixels stay sharp.
                let texture = match &mut self.screen {
                    Some(texture) => {
                        texture.set(picture, TextureOptions::NEAREST);
                        texture
                    }
                    empty => empty.insert(ctx.load_texture("screen", picture, TextureOptions::NEAREST)),
                };
                let painter = ctx.layer_painter(ui.layer_id());
                painter.rect_filled(window, 0.0, Color32::BLACK);
                painter.image(texture.id(), place, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            }
            None => {
                let status = Status { controllers: self.controllers.connected, memory_card: self.options.card.is_some(), home: self.home.clone() };
                if let Some(action) = ui::view::show(ui, &mut self.menu, &status) {
                    self.perform(&ctx, action);
                }
            }
        }
        self.updates += 1;
        if self.options.frames.is_some_and(|limit| self.updates >= limit) {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    fn on_exit(&mut self) {
        if let Some(console) = &self.psx {
            if let Err(e) = save_card(&self.options, console) {
                *self.failure.lock().unwrap() = Some(e);
            }
        }
    }
}

fn run(options: Options) -> Result<(), String> {
    let app = App::new(options)?;
    let failure = app.failure.clone();
    let disc = app.psx.as_ref().and(app.options.disc.as_deref()).map(Path::new);
    // The window opens at the size the menu was designed for (4:3) and can
    // be resized; the menu follows, and pictures are scaled to fit.
    let viewport = egui::ViewportBuilder::default()
        .with_title(title(disc))
        .with_app_id("yu-emulator")
        .with_inner_size([theme::WINDOW_WIDTH, theme::WINDOW_HEIGHT])
        .with_min_inner_size([880.0, 660.0]);
    let native_options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        // Only the embedded fonts, for the same text on every machine.
        system_font_fallback: false,
        ..Default::default()
    };
    eframe::run_native(
        "Yu-Emulator",
        native_options,
        Box::new(move |creation| {
            theme::install(&creation.egui_ctx);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| format!("could not open a window: {e}"))?;
    let failure = failure.lock().unwrap().take();
    failure.map_or(Ok(()), Err)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(options) = Options::parse(&args) else { return usage() };
    match run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
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
        assert_eq!((o.games, o.no_menu), (None, false));
        assert_eq!((o.exe.as_deref(), o.show_vram, o.frames), (Some("game.exe"), true, Some(3)));
        assert_eq!(o.card.as_deref(), Some("a.mcd"));
        let o = Options::parse(&args(&["bios.bin", "--games", "roms"])).unwrap();
        assert_eq!((o.games.as_deref(), o.disc), (Some("roms"), None));
        assert!(Options::parse(&args(&["bios.bin", "--no-menu"])).unwrap().no_menu);
        assert!(Options::parse(&args(&["a", "--games"])).is_none());
        assert!(Options::parse(&args(&[])).is_none());
        assert!(Options::parse(&args(&["a", "b", "c"])).is_none());
        assert!(Options::parse(&args(&["a", "--frames"])).is_none());
        assert!(Options::parse(&args(&["a", "--unknown"])).is_none());
    }

    #[test]
    fn window_title_names_the_disc() {
        assert_eq!(title(None), "Yu-Emulator");
        assert_eq!(title(Some(Path::new("roms/Some Game (USA).cue"))), "Yu-Emulator - Some Game (USA)");
    }

    #[test]
    fn a_disc_that_cannot_be_read_is_an_error_not_a_panic() {
        let bios = std::env::temp_dir().join(format!("yu-emulator-bios-disc-{}.bin", std::process::id()));
        std::fs::write(&bios, vec![0u8; psx_core::map::BIOS_SIZE]).unwrap();
        let options = Options::parse(&args(&[&bios.display().to_string(), "--mute"])).unwrap();
        let error = boot(&options, Some(Path::new("no/such/disc.cue"))).err().unwrap();
        assert!(error.contains("disc.cue"), "{error}");
        std::fs::remove_file(&bios).unwrap();
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
            games: None,
            no_menu: false,
            mute: true,
        };
        let mut psx = boot(&options, None).unwrap();
        assert!(psx.bus.sio0.ports[0].pad.is_some());
        // Unchanged card: nothing is written.
        save_card(&options, &psx).unwrap();
        assert!(!path.exists());
        // Changed card: the 128 KB image is written.
        psx.bus.sio0.ports[0].card.as_mut().unwrap().dirty = true;
        save_card(&options, &psx).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 131072);
        // And loaded again; a file of the wrong size is refused.
        assert!(boot(&options, None).is_ok());
        std::fs::write(&path, [0u8; 10]).unwrap();
        assert!(boot(&options, None).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(&bios).unwrap();
    }

    #[test]
    fn pictures_use_the_window_pixel_format() {
        let mut psx = Psx::new(vec![0; psx_core::map::BIOS_SIZE]).unwrap();
        // Fill 16x1 at (0,0) with pure red through GP0.
        for word in [0x0200_00FF, 0, (1 << 16) | 16] {
            psx.bus.store32(0x1F80_1810, word).unwrap();
        }
        let vram = vram_picture(&psx);
        assert_eq!((vram.size, vram.pixels.len()), ([1024, 512], 1024 * 512));
        assert_eq!(vram.pixels[0], Color32::from_rgb(0xFF, 0, 0));
        assert_eq!(vram.pixels[16], Color32::BLACK);
        // The display is off after reset: black, at the reset size.
        let display = display_picture(&psx);
        assert_eq!(display.size, [256, 240]);
        assert!(display.pixels.iter().all(|p| *p == Color32::BLACK));
    }

    #[test]
    fn pictures_fill_the_window_keeping_their_proportions() {
        let window = Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(1024.0, 768.0));
        assert_eq!(fit([320, 240], window), window);
        // Wider than the window: bars above and below.
        let vram = fit([1024, 512], window);
        assert_eq!((vram.left(), vram.top(), vram.width(), vram.height()), (0.0, 128.0, 1024.0, 512.0));
        // Taller: bars at the sides.
        let tall = fit([240, 480], window);
        assert_eq!((tall.left(), tall.top(), tall.width(), tall.height()), (320.0, 0.0, 384.0, 768.0));
        // A picture without pixels does not divide by zero.
        assert!(fit([0, 0], window).is_finite());
    }

    #[test]
    fn one_tick_is_due_per_sixtieth_of_a_second() {
        let start = Instant::now();
        let ticks = |refresh_hz: u64, updates: u64| {
            let mut pacer = Pacer::new(start);
            (0..updates).filter(|update| pacer.due(start + Duration::from_nanos(update * 1_000_000_000 / refresh_hz))).count()
        };
        assert_eq!(ticks(60, 60), 60, "a 60 Hz display gets a frame every refresh");
        assert_eq!(ticks(120, 120), 60);
        assert_eq!(ticks(144, 144), 60);
        assert_eq!(ticks(240, 240), 60);
        // A slower display cannot keep up: one frame per refresh, no more.
        assert_eq!(ticks(30, 30), 30);

        // Refreshes a little early still count, and a stall is not made up.
        let mut pacer = Pacer::new(start);
        assert!(pacer.due(start));
        assert!(pacer.due(start + Duration::from_millis(15)));
        assert!(!pacer.due(start + Duration::from_millis(20)));
        assert!(pacer.due(start + Duration::from_secs(5)));
        assert!(!pacer.due(start + Duration::from_secs(5) + Duration::from_millis(5)));
        assert!(pacer.due(start + Duration::from_secs(5) + Duration::from_millis(17)));
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
