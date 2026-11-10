//! Video timing generator: the hblank, vblank and dot clock signals that the
//! timers and the VBLANK interrupt depend on.
//!
//! psx-spx: "GPU Timings". There is no GPU yet (Phase 3), so this models a
//! fixed NTSC, non-interlaced display. Only the horizontal resolution is
//! configurable (it selects the dot clock); the GPU will take over the rest.
//!
//! Deviation: psx-spx gives line and frame lengths but not where inside them
//! the blanking signals seen by the timers begin. Here a scanline starts
//! with hblank and a frame starts with its first visible line, with the
//! blanking lengths derived from the usual display range (2560 video cycles
//! wide, 240 lines tall).

/// psx-spx: "Video Clock" — the video clock is the CPU clock times 11/7.
const VIDEO_PER_CPU_NUM: u32 = 11;
const VIDEO_PER_CPU_DEN: u32 = 7;
/// psx-spx: "Horizontal Timings" — NTSC: 3413 video cycles per scanline.
const CYCLES_PER_LINE: u32 = 3413;
/// psx-spx: "Vertical Timings" — NTSC: 263 scanlines per frame.
const LINES_PER_FRAME: u32 = 263;
const HBLANK_CYCLES: u32 = CYCLES_PER_LINE - 2560;
const VISIBLE_LINES: u32 = 240;
/// psx-spx: "Dotclocks" — 256-pixel mode (the reset state) divides the
/// video clock by 10.
const DEFAULT_DOT_DIVIDER: u32 = 10;

/// What happened on the video side during one `tick`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct VideoEvents {
    /// Dot clock pulses.
    pub dots: u32,
    /// Number of hblanks that started.
    pub hblanks: u32,
    /// A vblank started.
    pub vblank_started: bool,
    /// Signal levels at the end of the tick.
    pub in_hblank: bool,
    pub in_vblank: bool,
}

#[derive(Debug)]
pub struct Video {
    dot_divider: u32,
    /// Remainder of the CPU -> video clock conversion.
    clock_rem: u32,
    dot_rem: u32,
    /// Position inside the scanline, in video cycles.
    x: u32,
    line: u32,
}

impl Default for Video {
    fn default() -> Self {
        Self { dot_divider: DEFAULT_DOT_DIVIDER, clock_rem: 0, dot_rem: 0, x: 0, line: 0 }
    }
}

impl Video {
    /// Applies the horizontal resolution bits of a GP1(08h) display mode
    /// command (or of GP1(00h) reset, with `mode` = 0).
    /// psx-spx: "GP1(08h) - Display mode" (bits 0-1 and 6) and "Dotclocks":
    /// 256/320/512/640/368 pixels divide the video clock by 10/8/5/4/7.
    pub fn set_display_mode(&mut self, mode: u32) {
        self.dot_divider = if mode & 0x40 != 0 {
            7
        } else {
            [10, 8, 5, 4][(mode & 3) as usize]
        };
    }

    /// Advances by `cpu_cycles` CPU clock cycles.
    pub fn tick(&mut self, cpu_cycles: u32) -> VideoEvents {
        let scaled = self.clock_rem + cpu_cycles * VIDEO_PER_CPU_NUM;
        let video_cycles = scaled / VIDEO_PER_CPU_DEN;
        self.clock_rem = scaled % VIDEO_PER_CPU_DEN;

        let mut events = VideoEvents::default();
        // psx-spx: "The dotclock signal is generated even during
        // horizontal/vertical blanking".
        let dots = self.dot_rem + video_cycles;
        events.dots = dots / self.dot_divider;
        self.dot_rem = dots % self.dot_divider;

        self.x += video_cycles;
        while self.x >= CYCLES_PER_LINE {
            self.x -= CYCLES_PER_LINE;
            // psx-spx: "The hblank signal is generated even during vertical
            // blanking/retrace".
            events.hblanks += 1;
            self.line += 1;
            if self.line == VISIBLE_LINES {
                events.vblank_started = true;
            }
            if self.line == LINES_PER_FRAME {
                self.line = 0;
            }
        }
        events.in_hblank = self.x < HBLANK_CYCLES;
        events.in_vblank = self.line >= VISIBLE_LINES;
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_frame_of_cpu_cycles() {
        // 263 lines * 3413 video cycles * 7/11 CPU cycles.
        let frame_cpu_cycles = LINES_PER_FRAME * CYCLES_PER_LINE * 7 / 11;
        let mut video = Video::default();
        let (mut hblanks, mut vblanks, mut dots) = (0, 0, 0);
        // One extra cycle: the frame length is not a whole number of CPU
        // cycles, so the last line ends during cycle `frame_cpu_cycles + 1`.
        let cpu_cycles = frame_cpu_cycles + 1;
        for _ in 0..cpu_cycles {
            let e = video.tick(1);
            hblanks += e.hblanks;
            vblanks += e.vblank_started as u32;
            dots += e.dots;
        }
        assert_eq!(hblanks, 263);
        assert_eq!(vblanks, 1);
        assert_eq!(dots, cpu_cycles * 11 / 7 / 10, "one dot per 10 video cycles");
    }

    #[test]
    fn large_ticks_do_not_lose_lines() {
        let mut video = Video::default();
        let e = video.tick(7 * CYCLES_PER_LINE * 3 / 11 + 1);
        assert_eq!(e.hblanks, 3);
    }

    #[test]
    fn blanking_levels() {
        let mut video = Video::default();
        assert!(video.tick(1).in_hblank, "a line starts in hblank");
        let e = video.tick(HBLANK_CYCLES * 7 / 11 + 1);
        assert!(!e.in_hblank);
        assert!(!e.in_vblank);
        let e = video.tick(VISIBLE_LINES * CYCLES_PER_LINE * 7 / 11);
        assert!(e.in_vblank);
        assert!(e.vblank_started);
    }

    #[test]
    fn display_mode_selects_dot_clock() {
        let dots = |mode: u32| {
            let mut video = Video::default();
            video.set_display_mode(mode);
            video.tick(700).dots // exactly 1100 video cycles
        };
        assert_eq!([dots(0), dots(1), dots(2), dots(3), dots(0x40), dots(0x41)], [110, 137, 220, 275, 157, 157]);
    }
}
