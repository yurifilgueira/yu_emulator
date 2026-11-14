//! The game selection menu's state: which entry is selected and what the
//! pad buttons (keyboard or controller, the same ones the games use) do.
//! Drawing it is `view`'s job.

use crate::library::Game;
use psx_core::sio::button;
use std::path::PathBuf;

/// Updates a direction must be held before it repeats, and between repeats.
const REPEAT_DELAY: u32 = 24;
const REPEAT_PERIOD: u32 = 4;

/// List rows on screen until the view says how many fit.
const DEFAULT_ROWS: usize = 8;

/// What the player picked from the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Game(PathBuf),
    /// No disc: the console's own shell.
    Bios,
}

/// What the menu asks the frontend to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Play(Choice),
    /// Ask for another games folder.
    PickFolder,
    /// Ask for one disc image to run.
    OpenDisc,
}

/// The buttons of the screen shown when no game was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyButton {
    PickFolder,
    OpenDisc,
}

pub struct Menu {
    pub(super) games: Vec<Game>,
    /// Where the games were looked for, if anywhere.
    pub(super) folder: Option<PathBuf>,
    pub(super) selected: usize,
    /// First row on screen.
    pub(super) top: usize,
    /// List rows that fit on screen.
    pub(super) rows: usize,
    /// Whether the panel about the selected game is shown.
    pub(super) details: bool,
    /// The button the pad is on, on the screen without games.
    pub(super) focus: EmptyButton,
    /// Buttons held at the previous update.
    previous: u16,
    /// Updates the current up/down direction has been held.
    held: u32,
    /// Why the last choice could not start, or the folder could not be read.
    pub message: Option<String>,
}

impl Menu {
    pub fn new(games: Vec<Game>, folder: Option<PathBuf>) -> Self {
        Self {
            games,
            folder,
            selected: 0,
            top: 0,
            rows: DEFAULT_ROWS,
            details: true,
            focus: EmptyButton::PickFolder,
            previous: 0xFFFF,
            held: 0,
            message: None,
        }
    }

    /// Replaces the list, as after picking another folder.
    pub fn set_games(&mut self, games: Vec<Game>, folder: Option<PathBuf>) {
        self.games = games;
        self.folder = folder;
        self.selected = 0;
        self.top = 0;
        self.focus = EmptyButton::PickFolder;
    }

    /// Where the games were looked for, if anywhere.
    pub fn folder(&self) -> Option<&std::path::Path> {
        self.folder.as_deref()
    }

    /// Entries in the list: the games and "no disc" at the end.
    pub(super) fn entries(&self) -> usize {
        self.games.len() + 1
    }

    /// Ignores the buttons held right now until they are released, so that
    /// the press that led to the menu does not also pick an entry.
    pub fn wait_for_release(&mut self) {
        self.previous = 0xFFFF;
        self.held = 0;
    }

    /// Tells the menu how many list rows the window has room for.
    pub(super) fn set_rows(&mut self, rows: usize) {
        self.rows = rows.max(1);
        self.select(self.selected);
    }

    pub(super) fn select(&mut self, index: usize) {
        self.selected = index.min(self.entries() - 1);
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + self.rows {
            self.top = self.selected + 1 - self.rows;
        }
        // No empty rows at the end while there are entries above.
        self.top = self.top.min(self.entries().saturating_sub(self.rows));
    }

    /// The entry at `index` as a choice.
    pub(super) fn choice(&self, index: usize) -> Choice {
        match self.games.get(index) {
            Some(game) => Choice::Game(game.path.clone()),
            None => Choice::Bios,
        }
    }

    /// Picks the entry at `index`.
    pub(super) fn play(&mut self, index: usize) -> Action {
        self.message = None;
        Action::Play(self.choice(index))
    }

    /// One update (a sixtieth of a second) with the given pad buttons held.
    pub fn update(&mut self, buttons: u16) -> Option<Action> {
        let pressed = buttons & !self.previous;
        let direction = buttons & (button::UP | button::DOWN);
        let same_direction = direction != 0 && direction == self.previous & (button::UP | button::DOWN);
        self.held = if same_direction { self.held + 1 } else { 0 };
        self.previous = buttons;

        if self.games.is_empty() {
            return self.update_empty(pressed);
        }

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
            self.select(self.selected.saturating_sub(self.rows));
        } else if pressed & (button::RIGHT | button::R1) != 0 {
            self.select((self.selected + self.rows).min(last));
        }

        if pressed & button::CIRCLE != 0 {
            self.details = !self.details;
        }
        if pressed & (button::CROSS | button::START) != 0 {
            return Some(self.play(self.selected));
        }
        None
    }

    /// The screen without games: two buttons, one above the other.
    fn update_empty(&mut self, pressed: u16) -> Option<Action> {
        if pressed & (button::UP | button::DOWN) != 0 {
            self.focus = match self.focus {
                EmptyButton::PickFolder => EmptyButton::OpenDisc,
                EmptyButton::OpenDisc => EmptyButton::PickFolder,
            };
        }
        if pressed & (button::CROSS | button::START) != 0 {
            self.message = None;
            return Some(match self.focus {
                EmptyButton::PickFolder => Action::PickFolder,
                EmptyButton::OpenDisc => Action::OpenDisc,
            });
        }
        None
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::library::Region;

    /// The rows the old fixed-size menu had; the paging tests count on it.
    const ROWS: usize = 12;

    pub fn games(count: usize) -> Vec<Game> {
        (0..count)
            .map(|i| Game {
                title: format!("Jogo {:02}", i + 1),
                path: PathBuf::from(format!("g{i}.cue")),
                format: "CUE/BIN".to_string(),
                region: [Some(Region::NtscU), Some(Region::Pal), Some(Region::NtscJ), None][i % 4],
                size: (612 - 37 * i as u64 % 400) * 1024 * 1024,
            })
            .collect()
    }

    /// A menu whose buttons have been released once.
    fn menu(count: usize) -> Menu {
        let mut menu = Menu::new(games(count), Some(PathBuf::from("games")));
        menu.set_rows(ROWS);
        assert_eq!(menu.update(0), None);
        menu
    }

    /// Presses and releases a button.
    fn tap(menu: &mut Menu, buttons: u16) -> Option<Action> {
        let action = menu.update(buttons);
        assert_eq!(menu.update(0), None);
        action
    }

    fn play(path: &str) -> Option<Action> {
        Some(Action::Play(Choice::Game(PathBuf::from(path))))
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
        assert_eq!(tap(&mut m, button::CROSS), play("g0.cue"));
        tap(&mut m, button::DOWN);
        assert_eq!(tap(&mut m, button::START), play("g1.cue"));
        tap(&mut m, button::DOWN);
        assert_eq!(tap(&mut m, button::CROSS), Some(Action::Play(Choice::Bios)));
        // Other buttons pick nothing, and holding does not pick again.
        assert_eq!(tap(&mut m, button::CIRCLE | button::TRIANGLE | button::SELECT), None);
        assert_eq!(m.update(button::CROSS), Some(Action::Play(Choice::Bios)));
        assert_eq!(m.update(button::CROSS), None);
    }

    #[test]
    fn buttons_held_on_entry_are_ignored_until_released() {
        let mut m = Menu::new(games(2), Some(PathBuf::from("games")));
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
    fn a_window_with_fewer_rows_keeps_the_selection_on_screen() {
        let mut m = menu(30);
        for _ in 0..10 {
            tap(&mut m, button::DOWN);
        }
        assert_eq!((m.selected, m.top), (10, 0));
        m.set_rows(4);
        assert_eq!((m.selected, m.top), (10, 7));
        // A taller window at the end of the list leaves no empty rows.
        tap(&mut m, button::UP);
        tap(&mut m, button::UP);
        m.select(30);
        assert_eq!(m.top, 27);
        m.set_rows(8);
        assert_eq!(m.top, 23);
        m.set_rows(0);
        assert_eq!(m.rows, 1, "there is always one row");
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
    fn circle_shows_and_hides_the_details() {
        let mut m = menu(2);
        assert!(m.details);
        assert_eq!(tap(&mut m, button::CIRCLE), None);
        assert!(!m.details);
        m.update(button::CIRCLE);
        m.update(button::CIRCLE);
        assert!(m.details, "holding toggles once");
    }

    #[test]
    fn without_games_the_pad_moves_between_the_two_buttons() {
        let mut m = Menu::new(Vec::new(), None);
        assert_eq!(m.update(0), None);
        assert_eq!(m.focus, EmptyButton::PickFolder);
        assert_eq!(tap(&mut m, button::CROSS), Some(Action::PickFolder));
        tap(&mut m, button::DOWN);
        assert_eq!(m.focus, EmptyButton::OpenDisc);
        m.message = Some("no such folder".to_string());
        assert_eq!(tap(&mut m, button::START), Some(Action::OpenDisc));
        assert_eq!(m.message, None);
        tap(&mut m, button::DOWN);
        assert_eq!(m.focus, EmptyButton::PickFolder, "wraps");
        tap(&mut m, button::UP);
        assert_eq!(m.focus, EmptyButton::OpenDisc);
        assert_eq!(tap(&mut m, button::CIRCLE | button::LEFT | button::SELECT), None);
    }

    #[test]
    fn a_new_list_starts_at_the_top() {
        let mut m = menu(30);
        tap(&mut m, button::R1);
        tap(&mut m, button::R1);
        assert_ne!((m.selected, m.top), (0, 0));
        m.set_games(games(2), Some(PathBuf::from("other")));
        assert_eq!((m.selected, m.top, m.entries()), (0, 0, 3));
        assert_eq!(m.folder, Some(PathBuf::from("other")));
    }
}
