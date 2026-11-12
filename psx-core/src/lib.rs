//! PlayStation emulator core.
//!
//! Rules for this crate (see CLAUDE.md):
//! - No file, window, audio or network access. Everything comes in as bytes.
//! - Every hardware behaviour cites the psx-spx section it implements.

pub mod bus;
pub mod cdrom;
pub mod cpu;
pub mod disasm;
pub mod disc;
pub mod dma;
pub mod exe;
pub mod gpu;
pub mod gte;
pub mod irq;
pub mod map;
pub mod mdec;
pub mod sio;
pub mod spu;
pub mod system;
pub mod timers;
pub mod video;
pub mod xa;

pub use bus::{BiosError, Bus, BusFault};
pub use cpu::Cpu;
pub use system::Psx;
