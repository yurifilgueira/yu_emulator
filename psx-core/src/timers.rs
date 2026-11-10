//! Timers 0-2 ("root counters").
//!
//! psx-spx: "Timers".
//!
//! Deviations (psx-spx "Reset and Wrap"): the 1-2 cycle hold at 0000h after
//! a reset, write or wrap is not modelled, and INTF in pulse mode is never
//! observed low (the pulse lasts "a few clock cycles").

use crate::irq::{Interrupt, Irq};
use crate::video::VideoEvents;

// psx-spx: "0x1f801104 + 0x10*N: MODE".
const MODE_GATF: u32 = 1 << 0;
const MODE_ZRET: u32 = 1 << 3;
const MODE_CMP: u32 = 1 << 4;
const MODE_OVFL: u32 = 1 << 5;
const MODE_REPT: u32 = 1 << 6;
const MODE_LEVL: u32 = 1 << 7;
const MODE_EXTC: u32 = 1 << 8;
const MODE_PSCL: u32 = 1 << 9;
const MODE_INTF: u32 = 1 << 10;
const MODE_EQUF: u32 = 1 << 11;
const MODE_OVFF: u32 = 1 << 12;
/// Bits 0-9 are configuration; 10-12 are status; 13-15 unused/zero.
const MODE_WRITABLE: u32 = 0x03FF;

#[derive(Debug, Clone, Copy)]
struct Timer {
    count: u32,
    mode: u32,
    target: u32,
    /// One-shot mode: an IRQ was already produced since the last mode write.
    fired: bool,
    /// Sync mode 3: the blanking period has been seen, now free-running.
    released: bool,
}

impl Timer {
    fn new() -> Self {
        Self { count: 0, mode: MODE_INTF, target: 0, fired: false, released: false }
    }

    fn sync_mode(&self) -> u32 {
        (self.mode >> 1) & 3
    }

    /// Advances by `ticks` pulses of the selected clock. Returns true if an
    /// interrupt must be requested.
    fn advance(&mut self, ticks: u32) -> bool {
        let mut irq = false;
        for _ in 0..ticks {
            // "When the ZRET flag is set, the counter increments up to
            // (including) the selected target value, and does then restart
            // at 0000h"; otherwise it wraps after FFFFh.
            let at_target = self.count == self.target;
            if (self.mode & MODE_ZRET != 0 && at_target) || self.count == 0xFFFF {
                self.count = 0;
            } else {
                self.count += 1;
            }
            if self.count == self.target {
                self.mode |= MODE_EQUF;
                irq |= self.mode & MODE_CMP != 0 && self.raise();
            }
            if self.count == 0xFFFF {
                self.mode |= MODE_OVFF;
                irq |= self.mode & MODE_OVFL != 0 && self.raise();
            }
        }
        irq
    }

    /// An IRQ condition occurred; applies one-shot and toggle rules.
    /// Returns true when INTF goes from 1 to 0 (the edge that sets I_STAT).
    fn raise(&mut self) -> bool {
        // "one-shot mode doesn't stop the counter, it just suppresses any
        // further IRQs until a new write to the Mode register occurs".
        if self.mode & MODE_REPT == 0 && self.fired {
            return false;
        }
        self.fired = true;
        if self.mode & MODE_LEVL != 0 {
            // Toggle mode: "IRQ4/5/6 are triggered only each 2nd time, ie.
            // when INTF changes from 1 to 0".
            self.mode ^= MODE_INTF;
            self.mode & MODE_INTF == 0
        } else {
            // Pulse mode: INTF dips to 0 and returns to 1 (not observable).
            true
        }
    }
}

#[derive(Debug)]
pub struct Timers {
    timers: [Timer; 3],
    /// Timer 2 "System clock/8" prescaler remainder.
    prescaler: u32,
}

impl Default for Timers {
    fn default() -> Self {
        Self { timers: [Timer::new(); 3], prescaler: 0 }
    }
}

impl Timers {
    /// Advances all timers by `cycles` CPU cycles and the video events that
    /// happened in the same period.
    pub fn tick(&mut self, cycles: u32, video: &VideoEvents, irq: &mut Irq) {
        // Clock sources: timer 0 = system clock or dotclock, timer 1 = system
        // clock or hblank (EXTC), timer 2 = system clock or /8 (PSCL).
        let t0 = if self.timers[0].mode & MODE_EXTC != 0 { video.dots } else { cycles };
        let t1 = if self.timers[1].mode & MODE_EXTC != 0 { video.hblanks } else { cycles };
        let t2 = if self.timers[2].mode & MODE_PSCL != 0 {
            let total = self.prescaler + cycles;
            self.prescaler = total % 8;
            total / 8
        } else {
            cycles
        };

        // "Synchronization Modes for Counter 0" (hblank) and "Counter 1"
        // ("same as above, but using Vblank").
        let t0 = Self::gate(&mut self.timers[0], t0, video.in_hblank, video.hblanks > 0);
        let t1 = Self::gate(&mut self.timers[1], t1, video.in_vblank, video.vblank_started);
        // "Synchronization Modes for Counter 2: 0 or 3 = Stop counter at
        // current value, 1 or 2 = Free Run".
        let timer2 = &self.timers[2];
        let t2 = if timer2.mode & MODE_GATF != 0 && matches!(timer2.sync_mode(), 0 | 3) { 0 } else { t2 };

        let sources = [Interrupt::Timer0, Interrupt::Timer1, Interrupt::Timer2];
        for ((timer, ticks), source) in self.timers.iter_mut().zip([t0, t1, t2]).zip(sources) {
            if timer.advance(ticks) {
                irq.request(source);
            }
        }
    }

    /// Applies the blanking-synchronised modes; returns the ticks to count.
    fn gate(timer: &mut Timer, ticks: u32, in_blank: bool, blank_started: bool) -> u32 {
        if timer.mode & MODE_GATF == 0 {
            return ticks;
        }
        match timer.sync_mode() {
            // 0 = Pause counter during blank.
            0 => {
                if in_blank {
                    0
                } else {
                    ticks
                }
            }
            // 1 = Reset counter to 0000h at blank.
            1 => {
                if blank_started {
                    timer.count = 0;
                }
                ticks
            }
            // 2 = Reset counter to 0000h at blank and pause outside of it.
            2 => {
                if blank_started {
                    timer.count = 0;
                }
                if in_blank {
                    ticks
                } else {
                    0
                }
            }
            // 3 = Pause until blank occurs once, then switch to Free Run.
            _ => {
                if timer.released {
                    ticks
                } else {
                    timer.released = blank_started;
                    0
                }
            }
        }
    }

    /// Reads a register; `offset` is relative to 1F801100h, word-aligned.
    pub fn read(&mut self, offset: u32) -> u32 {
        let Some(timer) = self.timers.get_mut((offset >> 4) as usize) else { return 0 };
        match offset & 0xC {
            0x0 => timer.count,
            0x4 => {
                let mode = timer.mode;
                // EQUF/OVFF: "Reset after Reading".
                timer.mode &= !(MODE_EQUF | MODE_OVFF);
                mode
            }
            0x8 => timer.target,
            _ => 0,
        }
    }

    /// Writes a register; `offset` is relative to 1F801100h, word-aligned.
    pub fn write(&mut self, offset: u32, value: u32) {
        let Some(timer) = self.timers.get_mut((offset >> 4) as usize) else { return };
        match offset & 0xC {
            0x0 => timer.count = value & 0xFFFF,
            0x4 => {
                // The counter "gets forcefully reset to 0000h on any write to
                // the Counter Mode register"; INTF is "Set after Writing".
                timer.mode = (value & MODE_WRITABLE) | MODE_INTF;
                timer.count = 0;
                timer.fired = false;
                timer.released = false;
            }
            0x8 => timer.target = value & 0xFFFF,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COUNT: u32 = 0x0;
    const MODE: u32 = 0x4;
    const TARGET: u32 = 0x8;

    fn run(timers: &mut Timers, irq: &mut Irq, cycles: u32) {
        for _ in 0..cycles {
            timers.tick(1, &VideoEvents::default(), irq);
        }
    }

    #[test]
    fn free_run_counts_system_clock_and_wraps() {
        let (mut t, mut irq) = (Timers::default(), Irq::default());
        run(&mut t, &mut irq, 100);
        assert_eq!(t.read(COUNT), 100);
        assert_eq!(t.read(0x10 + COUNT), 100);
        assert_eq!(t.read(0x20 + COUNT), 100);
        t.write(COUNT, 0xFFFE);
        run(&mut t, &mut irq, 3);
        assert_eq!(t.read(COUNT), 1);
        assert_ne!(t.read(MODE) & MODE_OVFF, 0);
        assert_eq!(t.read(MODE) & MODE_OVFF, 0, "reset after reading");
        assert_eq!(irq.stat(), 0, "no IRQ enabled");
    }

    #[test]
    fn mode_write_resets_counter_and_sets_intf() {
        let (mut t, mut irq) = (Timers::default(), Irq::default());
        run(&mut t, &mut irq, 50);
        t.write(MODE, 0xFFFF);
        assert_eq!(t.read(COUNT), 0);
        assert_eq!(t.read(MODE), 0x03FF | MODE_INTF);
    }

    #[test]
    fn target_reset_and_repeating_irq() {
        let (mut t, mut irq) = (Timers::default(), Irq::default());
        t.write(TARGET, 10);
        t.write(MODE, MODE_ZRET | MODE_CMP | MODE_REPT);
        run(&mut t, &mut irq, 10);
        assert_eq!(t.read(COUNT), 10, "counts up to and including the target");
        assert_eq!(irq.stat(), 1 << 4);
        assert_ne!(t.read(MODE) & MODE_EQUF, 0);
        run(&mut t, &mut irq, 1);
        assert_eq!(t.read(COUNT), 0);
        irq.write_stat(0);
        run(&mut t, &mut irq, 10);
        assert_eq!(irq.stat(), 1 << 4, "repeat mode fires again");
    }

    #[test]
    fn one_shot_fires_once_until_mode_rewritten() {
        let (mut t, mut irq) = (Timers::default(), Irq::default());
        t.write(0x20 + TARGET, 5);
        t.write(0x20 + MODE, MODE_ZRET | MODE_CMP);
        run(&mut t, &mut irq, 6);
        assert_eq!(irq.stat(), 1 << 6);
        irq.write_stat(0);
        run(&mut t, &mut irq, 30);
        assert_eq!(irq.stat(), 0);
        t.write(0x20 + MODE, MODE_ZRET | MODE_CMP);
        run(&mut t, &mut irq, 6);
        assert_eq!(irq.stat(), 1 << 6);
    }

    #[test]
    fn toggle_mode_requests_every_second_time() {
        let (mut t, mut irq) = (Timers::default(), Irq::default());
        t.write(TARGET, 4);
        t.write(MODE, MODE_ZRET | MODE_CMP | MODE_REPT | MODE_LEVL);
        run(&mut t, &mut irq, 4);
        assert_eq!(irq.stat(), 1 << 4, "INTF 1 -> 0");
        assert_eq!(t.read(MODE) & MODE_INTF, 0);
        irq.write_stat(0);
        run(&mut t, &mut irq, 5);
        assert_eq!(irq.stat(), 0, "INTF 0 -> 1 does not request");
        assert_ne!(t.read(MODE) & MODE_INTF, 0);
        run(&mut t, &mut irq, 5);
        assert_eq!(irq.stat(), 1 << 4);
    }

    #[test]
    fn overflow_irq() {
        let (mut t, mut irq) = (Timers::default(), Irq::default());
        t.write(0x10 + MODE, MODE_OVFL | MODE_REPT);
        t.write(0x10 + COUNT, 0xFFF0);
        run(&mut t, &mut irq, 0xF);
        assert_eq!(irq.stat(), 1 << 5);
    }

    #[test]
    fn external_clock_sources() {
        let (mut t, mut irq) = (Timers::default(), Irq::default());
        t.write(MODE, MODE_EXTC); // dotclock
        t.write(0x10 + MODE, MODE_EXTC); // hblank
        t.write(0x20 + MODE, MODE_PSCL); // system clock / 8
        let video = VideoEvents { dots: 3, hblanks: 1, ..Default::default() };
        for _ in 0..16 {
            t.tick(1, &video, &mut irq);
        }
        assert_eq!(t.read(COUNT), 48);
        assert_eq!(t.read(0x10 + COUNT), 16);
        assert_eq!(t.read(0x20 + COUNT), 2);
    }

    #[test]
    fn timer0_hblank_sync_modes() {
        let blank = VideoEvents { in_hblank: true, hblanks: 1, ..Default::default() };
        let during = VideoEvents { in_hblank: true, ..Default::default() };
        let outside = VideoEvents::default();
        // Each mode sees: 5 cycles outside, blank starts, 2 more in blank,
        // 5 cycles outside.
        let run_mode = |sync: u32| {
            let (mut t, mut irq) = (Timers::default(), Irq::default());
            t.write(MODE, MODE_GATF | (sync << 1));
            let mut seen = Vec::new();
            for video in [(&outside, 5), (&blank, 1), (&during, 2), (&outside, 5)] {
                for _ in 0..video.1 {
                    t.tick(1, video.0, &mut irq);
                }
                seen.push(t.read(COUNT));
            }
            seen
        };
        assert_eq!(run_mode(0), [5, 5, 5, 10], "pause during hblank");
        assert_eq!(run_mode(1), [5, 1, 3, 8], "reset at hblank");
        assert_eq!(run_mode(2), [0, 1, 3, 3], "reset at hblank, pause outside");
        assert_eq!(run_mode(3), [0, 0, 2, 7], "pause until hblank, then free run");
    }

    #[test]
    fn timer1_uses_vblank_and_timer2_stops() {
        let (mut t, mut irq) = (Timers::default(), Irq::default());
        t.write(0x10 + MODE, MODE_GATF | (1 << 1)); // reset at vblank
        t.write(0x20 + MODE, MODE_GATF); // sync mode 0: stopped
        run(&mut t, &mut irq, 9);
        assert_eq!(t.read(0x10 + COUNT), 9);
        assert_eq!(t.read(0x20 + COUNT), 0);
        let vblank = VideoEvents { vblank_started: true, in_vblank: true, ..Default::default() };
        t.tick(1, &vblank, &mut irq);
        assert_eq!(t.read(0x10 + COUNT), 1);
        t.write(0x20 + MODE, MODE_GATF | (1 << 1)); // sync mode 1: free run
        run(&mut t, &mut irq, 4);
        assert_eq!(t.read(0x20 + COUNT), 4);
    }
}
