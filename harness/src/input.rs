//! Scripted controller input for headless runs.
//!
//! A script is a list of presses, each written `BUTTON@FRAME` or
//! `BUTTON@FRAME+LENGTH`: hold the button from that video frame (counted
//! from the start of the run) for LENGTH frames. Several buttons can be
//! held together with `+` on the left side: `l1+r1@200+5`.

use psx_core::sio::button;

/// Frames a press lasts when no length is given: long enough for programs
/// that poll once per frame and debounce, short enough to count as one
/// press.
const DEFAULT_LENGTH: u64 = 6;

const NAMES: [(&str, u16); 16] = [
    ("select", button::SELECT),
    ("l3", button::L3),
    ("r3", button::R3),
    ("start", button::START),
    ("up", button::UP),
    ("right", button::RIGHT),
    ("down", button::DOWN),
    ("left", button::LEFT),
    ("l2", button::L2),
    ("r2", button::R2),
    ("l1", button::L1),
    ("r1", button::R1),
    ("triangle", button::TRIANGLE),
    ("circle", button::CIRCLE),
    ("cross", button::CROSS),
    ("square", button::SQUARE),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Press {
    buttons: u16,
    /// Frames `from..until`.
    from: u64,
    until: u64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Script {
    presses: Vec<Press>,
}

impl Script {
    /// Adds one `BUTTON[+BUTTON...]@FRAME[+LENGTH]` entry.
    pub fn add(&mut self, spec: &str) -> Result<(), String> {
        let error = || format!("bad press {spec:?}: expected BUTTON@FRAME or BUTTON@FRAME+LENGTH");
        let (names, when) = spec.split_once('@').ok_or_else(error)?;
        let mut buttons = 0;
        for name in names.split('+') {
            let name = name.to_ascii_lowercase();
            let (_, bit) = NAMES.iter().find(|(n, _)| *n == name).ok_or_else(|| {
                let known: Vec<&str> = NAMES.iter().map(|(n, _)| *n).collect();
                format!("unknown button {name:?}; buttons are: {}", known.join(", "))
            })?;
            buttons |= bit;
        }
        let (from, length) = match when.split_once('+') {
            Some((from, length)) => (from.parse().map_err(|_| error())?, length.parse().map_err(|_| error())?),
            None => (when.parse().map_err(|_| error())?, DEFAULT_LENGTH),
        };
        self.presses.push(Press { buttons, from, until: from + length });
        Ok(())
    }

    /// Buttons held during `frame`, as a mask of `psx_core::sio::button` bits.
    pub fn pressed(&self, frame: u64) -> u16 {
        self.presses.iter().filter(|p| (p.from..p.until).contains(&frame)).fold(0, |mask, p| mask | p.buttons)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_presses() {
        let mut script = Script::default();
        script.add("start@100").unwrap();
        script.add("L1+r1@200+5").unwrap();
        assert_eq!(script.pressed(99), 0);
        assert_eq!(script.pressed(100), button::START);
        assert_eq!(script.pressed(100 + DEFAULT_LENGTH - 1), button::START);
        assert_eq!(script.pressed(100 + DEFAULT_LENGTH), 0);
        assert_eq!(script.pressed(204), button::L1 | button::R1);
        assert_eq!(script.pressed(205), 0);
    }

    #[test]
    fn overlapping_presses_combine() {
        let mut script = Script::default();
        script.add("up@10+10").unwrap();
        script.add("cross@15+2").unwrap();
        assert_eq!(script.pressed(15), button::UP | button::CROSS);
        assert_eq!(script.pressed(17), button::UP);
    }

    #[test]
    fn rejects_bad_specs() {
        let mut script = Script::default();
        for spec in ["start", "start@", "start@x", "start@10+", "jump@10", "@10", "start@10+5+1"] {
            assert!(script.add(spec).is_err(), "{spec}");
        }
        assert!(script.add("jump@10").unwrap_err().contains("triangle"));
        assert_eq!(script, Script::default());
    }
}
