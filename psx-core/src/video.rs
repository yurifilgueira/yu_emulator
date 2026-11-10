//! Video timing generator: the scanline/frame counters behind the hblank,
//! vblank and dot clock signals that the timers, the VBLANK interrupt and the
//! GPU status register depend on. Owned and configured by the GPU.
//!
//! psx-spx: "GPU Timings", "GPU Display Control Commands (GP1)".
//!
//! Deviations / assumptions:
//! - psx-spx: "Horizontal blanking and vertical blanking signals occur on
//!   the video output side ... These are not necessarily the same as the
//!   timer/interrupt HBLANK and VBLANK", without saying what the latter are.
//!   Here both are derived from the display range registers: hblank is
//!   everything outside X1..X2 of GP1(06h) and vblank everything outside
//!   Y1..Y2 of GP1(07h), counted from the sync pulses.
//! - The video clock is the nominal CPU clock * 11/7 for both regions, and
//!   a scanline is a whole number of video cycles (3413 / 3406).

/// psx-spx: "Video Clock" — the video clock is the CPU clock times 11/7.
const VIDEO_PER_CPU_NUM: u32 = 11;
const VIDEO_PER_CPU_DEN: u32 = 7;
/// psx-spx: "Horizontal Timings" — video cycles per scanline.
const NTSC_CYCLES_PER_LINE: u32 = 3413;
const PAL_CYCLES_PER_LINE: u32 = 3406;
/// psx-spx: "Vertical Timings" — scanlines per frame.
const NTSC_LINES: u32 = 263;
const PAL_LINES: u32 = 314;

// psx-spx: "GP1(08h) - Display mode".
const MODE_PAL: u32 = 1 << 3;
const MODE_INTERLACE: u32 = 1 << 5;
const MODE_HRES_368: u32 = 1 << 6;

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
    /// GP1(08h) display mode bits.
    mode: u32,
    /// GP1(06h) horizontal display range, in video cycles from hsync.
    x1: u32,
    x2: u32,
    /// GP1(07h) vertical display range, in scanlines from vsync.
    y1: u32,
    y2: u32,
    /// Remainder of the CPU -> video clock conversion.
    clock_rem: u32,
    dot_rem: u32,
    /// Position inside the scanline, in video cycles from hsync.
    x: u32,
    /// Scanline, counted from vsync.
    line: u32,
    /// Frames completed; its low bit is the interlace field.
    frame: u64,
}

impl Default for Video {
    fn default() -> Self {
        let mut video = Self { mode: 0, x1: 0, x2: 0, y1: 0, y2: 0, clock_rem: 0, dot_rem: 0, x: 0, line: 0, frame: 0 };
        video.reset_registers();
        video
    }
}

impl Video {
    /// psx-spx: "GP1(00h) - Reset GPU": display mode 0, "display x1,x2
    /// (x1=200h, x2=200h+256*10)", "display y1,y2 (y1=010h, y2=010h+240)".
    pub fn reset_registers(&mut self) {
        self.mode = 0;
        self.x1 = 0x200;
        self.x2 = 0x200 + 256 * 10;
        self.y1 = 0x010;
        self.y2 = 0x010 + 240;
    }

    /// psx-spx: "GP1(08h) - Display mode" (low 8 bits of the command).
    pub fn set_display_mode(&mut self, mode: u32) {
        self.mode = mode & 0xFF;
    }

    /// psx-spx: "GP1(06h) - Horizontal Display range (on Screen)".
    pub fn set_horizontal_range(&mut self, x1: u32, x2: u32) {
        self.x1 = x1;
        self.x2 = x2;
    }

    /// psx-spx: "GP1(07h) - Vertical Display range (on Screen)".
    pub fn set_vertical_range(&mut self, y1: u32, y2: u32) {
        self.y1 = y1;
        self.y2 = y2;
    }

    pub fn horizontal_range(&self) -> (u32, u32) {
        (self.x1, self.x2)
    }

    pub fn vertical_range(&self) -> (u32, u32) {
        (self.y1, self.y2)
    }

    /// psx-spx: "Dotclocks" — 256/320/512/640/368 pixels divide the video
    /// clock by 10/8/5/4/7.
    pub fn dot_divider(&self) -> u32 {
        if self.mode & MODE_HRES_368 != 0 {
            7
        } else {
            [10, 8, 5, 4][(self.mode & 3) as usize]
        }
    }

    fn cycles_per_line(&self) -> u32 {
        if self.mode & MODE_PAL != 0 {
            PAL_CYCLES_PER_LINE
        } else {
            NTSC_CYCLES_PER_LINE
        }
    }

    fn lines_per_frame(&self) -> u32 {
        if self.mode & MODE_PAL != 0 {
            PAL_LINES
        } else {
            NTSC_LINES
        }
    }

    /// Scanline being output, counted from vsync.
    pub fn line(&self) -> u32 {
        self.line
    }

    /// Interlace field: toggles every frame.
    pub fn field(&self) -> u32 {
        (self.frame & 1) as u32
    }

    /// Frames completed since power-on.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    pub fn interlaced(&self) -> bool {
        self.mode & MODE_INTERLACE != 0
    }

    pub fn in_vblank(&self) -> bool {
        self.line < self.y1 || self.line >= self.y2
    }

    fn in_hblank(&self) -> bool {
        self.x < self.x1 || self.x >= self.x2
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
        events.dots = dots / self.dot_divider();
        self.dot_rem = dots % self.dot_divider();

        let line_end = self.cycles_per_line();
        // Hblank starts where the display range ends, at the latest at the
        // end of the line.
        let hblank_start = self.x2.min(line_end);
        let mut remaining = video_cycles;
        while remaining > 0 {
            let next = if self.x < hblank_start { hblank_start } else { line_end };
            let step = remaining.min(next - self.x);
            self.x += step;
            remaining -= step;
            if next == hblank_start && self.x == hblank_start {
                // psx-spx: "The hblank signal is generated even during
                // vertical blanking/retrace".
                events.hblanks += 1;
            }
            if self.x >= line_end {
                self.x = 0;
                self.line += 1;
                if self.line >= self.lines_per_frame() {
                    self.line = 0;
                    self.frame += 1;
                }
                // psx-spx: "GP1(07h)": "If Y2 is set to a much too large
                // value, then the hardware stops to generate vblank
                // interrupts" — the line counter then never reaches Y2.
                if self.line == self.y2 {
                    events.vblank_started = true;
                }
            }
        }
        events.in_hblank = self.in_hblank();
        events.in_vblank = self.in_vblank();
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CPU cycles that span at least `video_cycles` video cycles.
    fn cpu_cycles(video_cycles: u32) -> u32 {
        (video_cycles * 7).div_ceil(11)
    }

    #[test]
    fn one_ntsc_frame() {
        let mut video = Video::default();
        let cycles = cpu_cycles(NTSC_LINES * NTSC_CYCLES_PER_LINE);
        let (mut hblanks, mut vblanks, mut dots) = (0, 0, 0);
        for _ in 0..cycles {
            let e = video.tick(1);
            hblanks += e.hblanks;
            vblanks += e.vblank_started as u32;
            dots += e.dots;
        }
        assert_eq!(hblanks, 263);
        assert_eq!(vblanks, 1);
        assert_eq!(dots, cycles * 11 / 7 / 10, "one dot per 10 video cycles");
        assert_eq!(video.frame(), 1);
        assert_eq!(video.field(), 1);
    }

    #[test]
    fn pal_frames_are_longer() {
        let mut video = Video::default();
        video.set_display_mode(MODE_PAL);
        let e = video.tick(cpu_cycles(313 * PAL_CYCLES_PER_LINE));
        assert_eq!(e.hblanks, 313);
        assert_eq!(video.frame(), 0);
        video.tick(cpu_cycles(PAL_CYCLES_PER_LINE));
        assert_eq!(video.frame(), 1);
    }

    #[test]
    fn large_ticks_do_not_lose_lines() {
        let mut video = Video::default();
        let e = video.tick(cpu_cycles(3 * NTSC_CYCLES_PER_LINE));
        assert_eq!(e.hblanks, 3);
        assert_eq!(video.line(), 3);
    }

    #[test]
    fn hblank_follows_the_horizontal_display_range() {
        let mut video = Video::default();
        video.set_horizontal_range(600, 3160);
        assert!(video.tick(1).in_hblank, "before X1");
        let e = video.tick(cpu_cycles(600));
        assert!(!e.in_hblank);
        assert_eq!(e.hblanks, 0);
        let e = video.tick(cpu_cycles(2560));
        assert!(e.in_hblank, "after X2");
        assert_eq!(e.hblanks, 1);
    }

    #[test]
    fn vblank_follows_the_vertical_display_range() {
        let mut video = Video::default();
        assert!(video.in_vblank(), "lines before Y1 are blank");
        let e = video.tick(cpu_cycles(0x10 * NTSC_CYCLES_PER_LINE));
        assert!(!e.in_vblank);
        assert!(!e.vblank_started);
        let e = video.tick(cpu_cycles(240 * NTSC_CYCLES_PER_LINE));
        assert!(e.in_vblank);
        assert!(e.vblank_started);
        assert_eq!(video.line(), 0x100);
    }

    #[test]
    fn vblank_never_starts_when_y2_is_past_the_frame() {
        let mut video = Video::default();
        video.set_vertical_range(0x10, 400);
        let e = video.tick(cpu_cycles(2 * NTSC_LINES * NTSC_CYCLES_PER_LINE));
        assert!(!e.vblank_started);
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

    #[test]
    fn reset_restores_registers() {
        let mut video = Video::default();
        video.set_display_mode(0x49);
        video.set_horizontal_range(1, 2);
        video.set_vertical_range(3, 4);
        video.reset_registers();
        assert_eq!(video.dot_divider(), 10);
        assert_eq!(video.horizontal_range(), (0x200, 0xC00));
        assert_eq!(video.vertical_range(), (0x10, 0x100));
    }
}
