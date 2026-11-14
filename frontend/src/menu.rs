//! The game selection menu: the disc images found in a folder, chosen with
//! the same pad buttons the games use (keyboard or controller).

use crate::font::{draw_text, fill, text_width, GLYPH};
use crate::Picture;
use psx_core::sio::button;
use std::path::{Path, PathBuf};

pub const WIDTH: usize = 320;
pub const HEIGHT: usize = 240;

/// List rows on screen at once.
const ROWS: usize = 12;
const ROW_HEIGHT: usize = 12;
const LIST_TOP: usize = 40;
const MARGIN: usize = 16;
/// Characters of a title that fit in a row.
const TITLE_CHARS: usize = 34;
/// Characters of a message line.
const MESSAGE_CHARS: usize = (WIDTH - 2 * MARGIN) / GLYPH;

/// Updates a direction must be held before it repeats, and between repeats.
const REPEAT_DELAY: u32 = 24;
const REPEAT_PERIOD: u32 = 4;

/// How deep below the games folder disc images are looked for.
const SCAN_DEPTH: usize = 4;

const BACKGROUND: u32 = 0x0010_1828;
const BAR: u32 = 0x0030_50A0;
const BRIGHT: u32 = 0x00FF_FFFF;
const TEXT: u32 = 0x00C8_D0E0;
const DIM: u32 = 0x0080_90A8;
const ERROR: u32 = 0x00FF_9080;

/// A disc image the menu offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Game {
    /// The file name without its extension.
    pub title: String,
    pub path: PathBuf,
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
        let title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        games.push(Game { title, path: path.clone() });
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

/// What the player picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Game(PathBuf),
    /// No disc: the console's own shell.
    Bios,
}

pub struct Menu {
    games: Vec<Game>,
    /// Where the games were looked for, for the empty list message.
    folder: String,
    selected: usize,
    /// First row on screen.
    top: usize,
    /// Buttons held at the previous update.
    previous: u16,
    /// Updates the current up/down direction has been held.
    held: u32,
    /// Shown under the list (why the last choice could not start).
    pub message: Option<String>,
}

impl Menu {
    pub fn new(games: Vec<Game>, folder: &str) -> Self {
        Self { games, folder: folder.to_string(), selected: 0, top: 0, previous: 0xFFFF, held: 0, message: None }
    }

    /// Entries in the list: the games and "no disc" at the end.
    fn entries(&self) -> usize {
        self.games.len() + 1
    }

    /// Ignores the buttons held right now until they are released, so that
    /// the press that led to the menu does not also pick an entry.
    pub fn wait_for_release(&mut self) {
        self.previous = 0xFFFF;
        self.held = 0;
    }

    fn select(&mut self, index: usize) {
        self.selected = index;
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + ROWS {
            self.top = self.selected + 1 - ROWS;
        }
    }

    /// One window update with the given pad buttons held.
    pub fn update(&mut self, buttons: u16) -> Option<Choice> {
        let pressed = buttons & !self.previous;
        let direction = buttons & (button::UP | button::DOWN);
        let same_direction = direction != 0 && direction == self.previous & (button::UP | button::DOWN);
        self.held = if same_direction { self.held + 1 } else { 0 };
        self.previous = buttons;

        let last = self.entries() - 1;
        let repeat = self.held >= REPEAT_DELAY && (self.held - REPEAT_DELAY).is_multiple_of(REPEAT_PERIOD);
        // A fresh press wraps around the ends; a held direction stops there.
        if pressed & button::UP != 0 {
            self.select(if self.selected == 0 { last } else { self.selected - 1 });
        } else if pressed & button::DOWN != 0 {
            self.select(if self.selected == last { 0 } else { self.selected + 1 });
        } else if repeat && direction == button::UP {
            self.select(self.selected.saturating_sub(1));
        } else if repeat && direction == button::DOWN {
            self.select((self.selected + 1).min(last));
        } else if pressed & (button::LEFT | button::L1) != 0 {
            self.select(self.selected.saturating_sub(ROWS));
        } else if pressed & (button::RIGHT | button::R1) != 0 {
            self.select((self.selected + ROWS).min(last));
        }

        if pressed & (button::CROSS | button::START) != 0 {
            self.message = None;
            return Some(match self.games.get(self.selected) {
                Some(game) => Choice::Game(game.path.clone()),
                None => Choice::Bios,
            });
        }
        None
    }

    fn label(&self, index: usize) -> String {
        match self.games.get(index) {
            Some(game) if game.title.chars().count() > TITLE_CHARS => {
                let start: String = game.title.chars().take(TITLE_CHARS - 3).collect();
                format!("{start}...")
            }
            Some(game) => game.title.clone(),
            None => "No disc (BIOS)".to_string(),
        }
    }

    pub fn render(&self) -> Picture {
        let mut picture = Picture { width: WIDTH, height: HEIGHT, pixels: vec![BACKGROUND; WIDTH * HEIGHT] };
        draw_text(&mut picture, MARGIN, 12, 2, BRIGHT, "Yu-Emulator");
        let count = format!("{}/{}", self.selected + 1, self.entries());
        draw_text(&mut picture, WIDTH - MARGIN - text_width(&count, 1), 20, 1, DIM, &count);

        for row in 0..ROWS.min(self.entries() - self.top) {
            let index = self.top + row;
            let y = LIST_TOP + row * ROW_HEIGHT;
            let colour = if index == self.selected {
                fill(&mut picture, MARGIN - 4, y, WIDTH - 2 * MARGIN + 8, ROW_HEIGHT, BAR);
                BRIGHT
            } else {
                TEXT
            };
            draw_text(&mut picture, MARGIN, y + 2, 1, colour, &self.label(index));
        }
        // Marks for entries above and below the rows on screen.
        if self.top > 0 {
            draw_text(&mut picture, WIDTH - MARGIN - GLYPH, LIST_TOP + 2, 1, DIM, "^");
        }
        if self.top + ROWS < self.entries() {
            draw_text(&mut picture, WIDTH - MARGIN - GLYPH, LIST_TOP + (ROWS - 1) * ROW_HEIGHT + 2, 1, DIM, "v");
        }

        let (message, colour) = match &self.message {
            Some(message) => (message.clone(), ERROR),
            None if self.games.is_empty() => (format!("No disc images (.cue, .bin) in {}", self.folder), DIM),
            None => (String::new(), DIM),
        };
        let characters: Vec<char> = message.chars().collect();
        for (line, text) in characters.chunks(MESSAGE_CHARS).take(2).enumerate() {
            draw_text(&mut picture, MARGIN, 192 + line * 10, 1, colour, &text.iter().collect::<String>());
        }

        draw_text(&mut picture, MARGIN, 218, 1, DIM, "Up/Down: choose   Cross/Start: play");
        draw_text(&mut picture, MARGIN, 228, 1, DIM, "Esc: quit");
        picture
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn games(count: usize) -> Vec<Game> {
        (0..count).map(|i| Game { title: format!("Game {i:02}"), path: PathBuf::from(format!("g{i}.cue")) }).collect()
    }

    /// A menu whose buttons have been released once.
    fn menu(count: usize) -> Menu {
        let mut menu = Menu::new(games(count), "games");
        assert_eq!(menu.update(0), None);
        menu
    }

    /// Presses and releases a button.
    fn tap(menu: &mut Menu, buttons: u16) -> Option<Choice> {
        let choice = menu.update(buttons);
        assert_eq!(menu.update(0), None);
        choice
    }

    #[test]
    fn scan_finds_cue_sheets_and_lone_images_sorted_by_title() {
        let root = std::env::temp_dir().join(format!("yu-emulator-menu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
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
    fn up_and_down_move_and_wrap() {
        let mut m = menu(3);
        assert_eq!(m.selected, 0);
        tap(&mut m, button::DOWN);
        assert_eq!(m.selected, 1);
        tap(&mut m, button::UP);
        tap(&mut m, button::UP);
        assert_eq!(m.selected, 3, "wraps to the last entry (no disc)");
        tap(&mut m, button::DOWN);
        assert_eq!(m.selected, 0);
    }

    #[test]
    fn a_held_direction_moves_once_then_repeats_and_stops_at_the_end() {
        let mut m = menu(5);
        m.update(button::DOWN);
        assert_eq!(m.selected, 1);
        for _ in 0..REPEAT_DELAY - 1 {
            m.update(button::DOWN);
        }
        assert_eq!(m.selected, 1, "nothing during the delay");
        m.update(button::DOWN);
        assert_eq!(m.selected, 2);
        for _ in 0..REPEAT_PERIOD {
            m.update(button::DOWN);
        }
        assert_eq!(m.selected, 3);
        for _ in 0..100 {
            m.update(button::DOWN);
        }
        assert_eq!(m.selected, 5, "a held direction does not wrap");
    }

    #[test]
    fn cross_or_start_picks_the_selected_entry() {
        let mut m = menu(2);
        assert_eq!(tap(&mut m, button::CROSS), Some(Choice::Game(PathBuf::from("g0.cue"))));
        tap(&mut m, button::DOWN);
        assert_eq!(tap(&mut m, button::START), Some(Choice::Game(PathBuf::from("g1.cue"))));
        tap(&mut m, button::DOWN);
        assert_eq!(tap(&mut m, button::CROSS), Some(Choice::Bios));
        // Other buttons pick nothing, and holding does not pick again.
        assert_eq!(tap(&mut m, button::CIRCLE | button::TRIANGLE | button::SELECT), None);
        assert_eq!(m.update(button::CROSS), Some(Choice::Bios));
        assert_eq!(m.update(button::CROSS), None);
    }

    #[test]
    fn buttons_held_on_entry_are_ignored_until_released() {
        let mut m = Menu::new(games(2), "games");
        assert_eq!(m.update(button::START | button::DOWN), None);
        assert_eq!(m.selected, 0);
        assert_eq!(m.update(0), None);
        assert!(m.update(button::START).is_some());
        m.wait_for_release();
        assert_eq!(m.update(button::START), None);
    }

    #[test]
    fn the_list_scrolls_to_keep_the_selection_on_screen() {
        let mut m = menu(30);
        for _ in 0..ROWS - 1 {
            tap(&mut m, button::DOWN);
        }
        assert_eq!((m.selected, m.top), (ROWS - 1, 0));
        tap(&mut m, button::DOWN);
        assert_eq!((m.selected, m.top), (ROWS, 1));
        // A page down and up; pages stop at the ends.
        tap(&mut m, button::RIGHT);
        assert_eq!((m.selected, m.top), (2 * ROWS, ROWS + 1));
        tap(&mut m, button::R1);
        assert_eq!(m.selected, 30);
        tap(&mut m, button::L1);
        tap(&mut m, button::LEFT);
        tap(&mut m, button::LEFT);
        assert_eq!((m.selected, m.top), (0, 0));
        // Wrapping upwards shows the end of the list.
        tap(&mut m, button::UP);
        assert_eq!((m.selected, m.top), (30, 31 - ROWS));
    }

    #[test]
    fn a_choice_clears_the_message() {
        let mut m = menu(1);
        m.message = Some("could not read".to_string());
        tap(&mut m, button::DOWN);
        assert!(m.message.is_some());
        tap(&mut m, button::CROSS);
        assert!(m.message.is_none());
    }

    #[test]
    fn long_titles_are_cut() {
        let long = Game { title: "é".repeat(60), path: PathBuf::from("x.cue") };
        let m = Menu::new(vec![long], "games");
        let label = m.label(0);
        assert_eq!(label.chars().count(), TITLE_CHARS);
        assert!(label.ends_with("..."));
        assert_eq!(m.label(1), "No disc (BIOS)");
    }

    #[test]
    fn render_highlights_the_selected_row() {
        let mut m = menu(3);
        let row_colour = |m: &Menu, row: usize| m.render().pixels[(LIST_TOP + row * ROW_HEIGHT) * WIDTH + MARGIN - 2];
        let picture = m.render();
        assert_eq!((picture.width, picture.height, picture.pixels.len()), (WIDTH, HEIGHT, WIDTH * HEIGHT));
        assert_eq!(row_colour(&m, 0), BAR);
        assert_eq!(row_colour(&m, 1), BACKGROUND);
        tap(&mut m, button::DOWN);
        assert_eq!(row_colour(&m, 0), BACKGROUND);
        assert_eq!(row_colour(&m, 1), BAR);
        // Text is drawn in every row that has an entry, and none below.
        let has_text = |row: usize| {
            let y = LIST_TOP + row * ROW_HEIGHT;
            picture.pixels[y * WIDTH..(y + ROW_HEIGHT) * WIDTH].iter().any(|p| *p == TEXT || *p == BRIGHT)
        };
        assert!((0..4).all(has_text));
        assert!(!has_text(4));
    }

    #[test]
    fn render_explains_an_empty_list_and_shows_messages() {
        let coloured = |m: &Menu, colour: u32| m.render().pixels[190 * WIDTH..214 * WIDTH].contains(&colour);
        let mut m = Menu::new(Vec::new(), "games");
        assert!(coloured(&m, DIM));
        assert!(!coloured(&m, ERROR));
        m.message = Some("x".repeat(500));
        assert!(coloured(&m, ERROR));
        let m = menu(1);
        assert!(!coloured(&m, DIM) && !coloured(&m, ERROR));
    }
}
