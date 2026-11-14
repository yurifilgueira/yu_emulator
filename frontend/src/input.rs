//! Game controllers, through the `gilrs` crate.
//!
//! Every connected controller plays the digital pad in port 1, together
//! with the keyboard. The left stick doubles as the d-pad, because only the
//! digital pad is emulated so far; that is a frontend convenience, not
//! something a PlayStation does.

use gilrs::{Axis, Button, Gilrs};
use psx_core::sio::button;

/// Controller buttons and the pad buttons they hold down. The face buttons
/// go by position: the bottom one is cross, as on a PlayStation pad.
const BUTTONS: [(Button, u16); 16] = [
    (Button::DPadUp, button::UP),
    (Button::DPadDown, button::DOWN),
    (Button::DPadLeft, button::LEFT),
    (Button::DPadRight, button::RIGHT),
    (Button::Start, button::START),
    (Button::Select, button::SELECT),
    (Button::South, button::CROSS),
    (Button::East, button::CIRCLE),
    (Button::West, button::SQUARE),
    (Button::North, button::TRIANGLE),
    (Button::LeftTrigger, button::L1),
    (Button::RightTrigger, button::R1),
    (Button::LeftTrigger2, button::L2),
    (Button::RightTrigger2, button::R2),
    (Button::LeftThumb, button::L3),
    (Button::RightThumb, button::R3),
];

/// How far the stick must be pushed to count as a d-pad direction.
const STICK_THRESHOLD: f32 = 0.5;

/// The pad buttons held on a controller with the given buttons down.
pub fn pad_buttons(is_down: impl Fn(Button) -> bool) -> u16 {
    BUTTONS.iter().filter(|(button, _)| is_down(*button)).fold(0, |mask, (_, bit)| mask | bit)
}

/// The d-pad directions a stick position stands for (x grows to the right,
/// y grows upwards, both from -1 to 1).
pub fn stick_buttons(x: f32, y: f32) -> u16 {
    let mut mask = 0;
    if x <= -STICK_THRESHOLD {
        mask |= button::LEFT;
    }
    if x >= STICK_THRESHOLD {
        mask |= button::RIGHT;
    }
    if y <= -STICK_THRESHOLD {
        mask |= button::DOWN;
    }
    if y >= STICK_THRESHOLD {
        mask |= button::UP;
    }
    mask
}

/// Whether the held buttons ask for the frontend's menu: both sticks
/// clicked at once, for controllers without a guide button.
pub fn menu_combo(buttons: u16) -> bool {
    let combo = button::L3 | button::R3;
    buttons & combo == combo
}

/// What the controllers hold down at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State {
    /// Pad buttons, as a `psx_core::sio::button` mask.
    pub buttons: u16,
    /// The guide button or the menu combination.
    pub menu: bool,
    /// Controllers connected.
    pub connected: usize,
}

/// The connected controllers. Without controller support on the machine
/// this is empty and the keyboard still works.
pub struct Gamepads {
    gilrs: Option<Gilrs>,
}

impl Gamepads {
    pub fn open() -> Self {
        let gilrs = Gilrs::new().map_err(|e| eprintln!("no controller support: {e}")).ok();
        Self { gilrs }
    }

    /// The buttons held right now, over all controllers.
    pub fn poll(&mut self) -> State {
        let Some(gilrs) = self.gilrs.as_mut() else { return State::default() };
        // The events only matter for keeping the cached state up to date
        // (and for noticing controllers plugged in while running).
        while gilrs.next_event().is_some() {}
        let mut state = State::default();
        for (_, pad) in gilrs.gamepads() {
            state.connected += 1;
            state.buttons |= pad_buttons(|button| pad.is_pressed(button));
            state.buttons |= stick_buttons(pad.value(Axis::LeftStickX), pad.value(Axis::LeftStickY));
            state.menu |= pad.is_pressed(Button::Mode);
        }
        state.menu |= menu_combo(state.buttons);
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_buttons_map_to_pad_buttons() {
        assert_eq!(pad_buttons(|_| false), 0);
        assert_eq!(pad_buttons(|b| b == Button::South || b == Button::Start), button::CROSS | button::START);
        assert_eq!(pad_buttons(|b| b == Button::East), button::CIRCLE);
        assert_eq!(pad_buttons(|b| b == Button::LeftTrigger2), button::L2);
        assert_eq!(pad_buttons(|_| true), 0xFFFF, "every pad button has a controller button");
        // The guide button is not a pad button.
        assert_eq!(pad_buttons(|b| b == Button::Mode), 0);
    }

    #[test]
    fn stick_acts_as_the_d_pad_past_half_way() {
        assert_eq!(stick_buttons(0.0, 0.0), 0);
        assert_eq!(stick_buttons(0.49, -0.49), 0);
        assert_eq!(stick_buttons(0.5, 0.0), button::RIGHT);
        assert_eq!(stick_buttons(-1.0, 0.0), button::LEFT);
        assert_eq!(stick_buttons(0.0, 1.0), button::UP);
        assert_eq!(stick_buttons(-0.8, -0.8), button::LEFT | button::DOWN);
    }

    #[test]
    fn both_stick_clicks_ask_for_the_menu() {
        assert!(!menu_combo(0));
        assert!(!menu_combo(button::L3));
        assert!(!menu_combo(button::R3 | button::START));
        assert!(menu_combo(button::L3 | button::R3));
    }
}
