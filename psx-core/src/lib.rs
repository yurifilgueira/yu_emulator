//! PlayStation emulator core.
//!
//! Rules for this crate (see CLAUDE.md):
//! - No file, window, audio or network access. Everything comes in as bytes.
//! - Every hardware behaviour cites the psx-spx section it implements.

pub mod bus;
pub mod map;

pub use bus::{BiosError, Bus, BusFault};
