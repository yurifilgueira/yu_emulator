//! The volume envelope generator shared by ADSR and volume sweeps.
//!
//! psx-spx: "Envelope Operation depending on Shift/Step/Mode/Direction".

/// How an envelope moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rate {
    pub exponential: bool,
    pub decreasing: bool,
    /// Sweep phase bit; always false for ADSR.
    pub negative: bool,
    /// 0..1Fh = fast..slow.
    pub shift: u8,
    /// 0..3 = "+7,+6,+5,+4" or "-8,-7,-6,-5".
    pub step: u8,
    /// All rate bits are ones: "causes the volume to never step".
    pub stuck: bool,
}

impl Rate {
    /// A rate from the 7-bit field (shift in bits 6-2, step in bits 1-0)
    /// that attack, sustain and sweeps use.
    pub fn from_bits(bits: u16, exponential: bool, decreasing: bool, negative: bool) -> Self {
        let bits = (bits & 0x7F) as u8;
        Self { exponential, decreasing, negative, shift: bits >> 2, step: bits & 3, stuck: bits == 0x7F }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Envelope {
    pub level: i16,
    counter: u32,
}

impl Envelope {
    /// Restarts the step counter (a new phase or a new sweep).
    pub fn restart(&mut self) {
        self.counter = 0;
    }

    /// One 44.1 kHz cycle.
    pub fn tick(&mut self, rate: Rate) {
        let level = self.level as i32;
        // "AdsrStep = 7 - StepValue; IF Decreasing XOR PhaseNegative THEN
        // AdsrStep = NOT AdsrStep".
        let mut step = 7 - rate.step as i32;
        if rate.decreasing ^ rate.negative {
            step = !step;
        }
        step <<= 11u32.saturating_sub(rate.shift as u32);
        let mut increment = 0x8000u32 >> (rate.shift as u32).saturating_sub(11);
        if rate.exponential && !rate.decreasing && level > 0x6000 {
            // "Exponential Increase is a fake (simply changes to a slower
            // linear increase rate at higher volume levels)".
            if rate.shift < 10 {
                step >>= 2;
            } else if rate.shift >= 11 {
                increment >>= 2;
            } else {
                step >>= 1;
                increment >>= 1;
            }
        } else if rate.exponential && rate.decreasing {
            // "AdsrStep=AdsrStep*AdsrLevel/8000h".
            step = (step * level) >> 15;
        }
        if !rate.stuck {
            increment = increment.max(1);
        }

        self.counter += increment;
        if self.counter & 0x8000 == 0 {
            return;
        }
        // Deviation: psx-spx does not say what happens to the counter after
        // a step; it starts again from zero here.
        self.counter = 0;

        let level = level + step;
        self.level = if !rate.decreasing {
            level.clamp(-0x8000, 0x7FFF)
        } else if rate.negative {
            level.clamp(-0x8000, 0)
        } else {
            level.max(0)
        } as i16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(shift: u8, step: u8, exponential: bool, decreasing: bool) -> Rate {
        Rate { exponential, decreasing, negative: false, shift, step, stuck: false }
    }

    #[test]
    fn linear_increase_steps_every_cycle_at_low_shifts() {
        // Shift 0, step value 0: +7 << 11 per cycle; saturates at 7FFFh.
        let mut e = Envelope::default();
        e.tick(rate(0, 0, false, false));
        assert_eq!(e.level, 7 << 11);
        e.tick(rate(0, 0, false, false));
        assert_eq!(e.level, 14 << 11);
        for _ in 0..3 {
            e.tick(rate(0, 0, false, false));
        }
        assert_eq!(e.level, 0x7FFF);
        // Shift 11, step value 3: +4 per cycle.
        let mut e = Envelope::default();
        e.tick(rate(11, 3, false, false));
        assert_eq!(e.level, 4);
    }

    #[test]
    fn shifts_above_11_step_every_few_cycles() {
        // Shift 13: the counter advances 2000h per cycle, so one step of +7
        // every 4 cycles.
        let mut e = Envelope::default();
        let levels: Vec<i16> = (0..8)
            .map(|_| {
                e.tick(rate(13, 0, false, false));
                e.level
            })
            .collect();
        assert_eq!(levels, [0, 0, 0, 7, 7, 7, 7, 14]);
    }

    #[test]
    fn linear_decrease_stops_at_zero() {
        // Step value 0 decreasing: -8 << (11 - shift).
        let mut e = Envelope { level: 20, ..Default::default() };
        e.tick(rate(11, 0, false, true));
        assert_eq!(e.level, 12);
        e.tick(rate(11, 0, false, true));
        e.tick(rate(11, 0, false, true));
        assert_eq!(e.level, 0);
    }

    #[test]
    fn exponential_decrease_is_proportional_to_the_level() {
        // Step -8 << 11 = -4000h, times level / 8000h: half the level.
        let mut e = Envelope { level: 0x4000, ..Default::default() };
        e.tick(rate(0, 0, true, true));
        assert_eq!(e.level, 0x2000);
        e.tick(rate(0, 0, true, true));
        assert_eq!(e.level, 0x1000);
    }

    #[test]
    fn exponential_increase_slows_down_above_6000h() {
        // Shift 0: +3800h per cycle up to 6000h, a quarter of that above.
        let mut e = Envelope::default();
        e.tick(rate(0, 0, true, false));
        e.tick(rate(0, 0, true, false));
        assert_eq!(e.level, 0x7000);
        e.tick(rate(0, 0, true, false));
        assert_eq!(e.level, 0x7000 + 0xE00);
        // Shift 12 above 6000h: the counter runs at a quarter of the speed.
        let mut e = Envelope { level: 0x6001, ..Default::default() };
        let mut cycles = 0;
        while e.level == 0x6001 {
            e.tick(rate(12, 0, true, false));
            cycles += 1;
        }
        assert_eq!(cycles, 8);
    }

    #[test]
    fn negative_phase_sweeps_towards_minus_8000h() {
        // Increase with the phase bit: the step is inverted (-8).
        let negative = Rate { negative: true, ..rate(0, 0, false, false) };
        let mut e = Envelope::default();
        e.tick(negative);
        assert_eq!(e.level, -0x4000);
        e.tick(negative);
        e.tick(negative);
        assert_eq!(e.level, -0x8000);
    }

    #[test]
    fn all_ones_rate_never_steps() {
        let stuck = Rate::from_bits(0x7F, false, false, false);
        assert!(stuck.stuck);
        let mut e = Envelope { level: 100, ..Default::default() };
        for _ in 0..100_000 {
            e.tick(stuck);
        }
        assert_eq!(e.level, 100);
        // One bit less and it does, very slowly (shift 31: 0x8000 >> 20 = 0,
        // forced to 1: a step every 8000h cycles).
        let slow = Rate::from_bits(0x7E, false, false, false);
        let mut e = Envelope { level: 100, ..Default::default() };
        for _ in 0..0x8000 {
            e.tick(slow);
        }
        assert_eq!(e.level, 105);
    }
}
