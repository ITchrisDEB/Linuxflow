//! Animated state of the pill (pure logic, testable without GTK).
//!
//! `PillState` receives phases and levels, advances with `tick(dt)` and produces a
//! `PillVisual`: everything the drawing needs to know (offset, opacities, bar heights, check mark).

use linuxflow_core::domain::Phase;

use crate::overlay::anim::{
    cubic_bezier, lerp, smooth_toward, think_wave, APPEAR_CURVE, APPEAR_MS, BAR_TAU_MS, EASE,
    FADE_MS, POP_CURVE, POP_MS, THINK_MS,
};
use crate::overlay::geometry::{bar_height, N_BARS, PILL_H, SLIDE_FACTOR};

/// Rendering of one bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarVisual {
    pub height: f64,
    pub opacity: f64,
}

/// Check mark of the "done" state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CheckVisual {
    pub scale: f64,
    pub opacity: f64,
}

/// What the drawing must show at a given instant.
#[derive(Debug, Clone, PartialEq)]
pub struct PillVisual {
    /// Vertical offset downwards, in px (appearance from the bottom).
    pub slide: f64,
    /// Overall opacity of the capsule.
    pub opacity: f64,
    pub phase: Phase,
    /// Red outline, frozen red bars.
    pub error: bool,
    /// `None` outside the "done" state.
    pub check: Option<CheckVisual>,
    /// Empty if the check mark replaces the bars.
    pub bars: Vec<BarVisual>,
}

#[derive(Debug, Clone)]
pub struct PillState {
    phase: Phase,
    visible: bool,
    /// Linear progress of the appearance, 0 (hidden) to 1 (shown).
    appear: f64,
    phase_age_ms: f64,
    clock_ms: f64,
    targets: [f64; N_BARS],
    shown: [f64; N_BARS],
    hidden_pending: bool,
}

impl Default for PillState {
    fn default() -> Self {
        Self::new()
    }
}

impl PillState {
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            visible: false,
            appear: 0.0,
            phase_age_ms: 0.0,
            clock_ms: 0.0,
            targets: [0.0; N_BARS],
            shown: [0.0; N_BARS],
            hidden_pending: false,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Changes phase; `Idle` is equivalent to `hide()`. Any other phase shows the pill.
    pub fn set_phase(&mut self, phase: Phase) {
        if phase == Phase::Idle {
            self.hide();
            return;
        }
        if phase != self.phase {
            if phase == Phase::Listening {
                self.targets = [0.0; N_BARS];
                self.shown = [0.0; N_BARS];
            }
            self.phase = phase;
            self.phase_age_ms = 0.0;
        }
        self.visible = true;
        self.hidden_pending = false;
    }

    /// Shows the pill in the current phase (listening by default).
    pub fn show(&mut self) {
        if self.phase == Phase::Idle {
            self.set_phase(Phase::Listening);
        } else {
            self.visible = true;
            self.hidden_pending = false;
        }
    }

    pub fn hide(&mut self) {
        if self.visible || self.appear > 0.0 {
            self.visible = false;
        }
    }

    /// Adds a microphone level (queue of 22 levels scrolling to the left).
    pub fn push_level(&mut self, level: f32) {
        self.targets.rotate_left(1);
        self.targets[N_BARS - 1] = f64::from(level.clamp(0.0, 1.0));
    }

    /// Advances by `dt_ms` milliseconds.
    pub fn tick(&mut self, dt_ms: f64) {
        let dt = dt_ms.clamp(0.0, 250.0);
        self.clock_ms += dt;
        self.phase_age_ms += dt;
        let step = dt / APPEAR_MS;
        if self.visible {
            self.appear = (self.appear + step).min(1.0);
        } else if self.appear > 0.0 {
            self.appear = (self.appear - step).max(0.0);
            if self.appear == 0.0 {
                self.hidden_pending = true;
                self.phase = Phase::Idle;
            }
        }
        for (shown, target) in self.shown.iter_mut().zip(self.targets.iter()) {
            *shown = smooth_toward(*shown, *target, dt, BAR_TAU_MS);
        }
    }

    /// `true` only once, when the disappearance has just finished.
    pub fn take_hidden(&mut self) -> bool {
        std::mem::take(&mut self.hidden_pending)
    }

    /// An animation is in progress: the frame loop must continue.
    pub fn is_animating(&self) -> bool {
        let settled = if self.visible {
            self.appear >= 1.0
        } else {
            self.appear <= 0.0
        };
        if !settled {
            return true;
        }
        if !self.visible {
            return false;
        }
        match self.phase {
            Phase::Transcribing | Phase::Formatting => true,
            Phase::Done => self.phase_age_ms < POP_MS,
            Phase::Listening => self
                .shown
                .iter()
                .zip(self.targets.iter())
                .any(|(s, t)| (s - t).abs() > 0.002),
            _ => false,
        }
    }

    pub fn visual(&self) -> PillVisual {
        let eased = cubic_bezier(APPEAR_CURVE, self.appear);
        let slide = (1.0 - eased) * PILL_H * SLIDE_FACTOR;
        let opacity = cubic_bezier(EASE, (self.appear * APPEAR_MS / FADE_MS).min(1.0));
        let error = self.phase == Phase::Error;
        let thinking = matches!(self.phase, Phase::Transcribing | Phase::Formatting);

        let bars = match self.phase {
            Phase::Done => Vec::new(),
            _ => (0..N_BARS)
                .map(|i| {
                    if thinking {
                        // Even-index bars (odd in CSS, 1-based): shifted by half a period.
                        let offset = if i % 2 == 0 { THINK_MS / 2.0 } else { 0.0 };
                        let w = think_wave(self.clock_ms, offset);
                        BarVisual {
                            height: lerp(5.0, 16.0, w),
                            opacity: lerp(0.55, 1.0, w),
                        }
                    } else if error {
                        BarVisual {
                            height: bar_height(0.0),
                            opacity: 1.0,
                        }
                    } else {
                        BarVisual {
                            height: bar_height(self.shown[i]),
                            opacity: 1.0,
                        }
                    }
                })
                .collect(),
        };

        let check = (self.phase == Phase::Done).then(|| {
            let p = cubic_bezier(POP_CURVE, self.phase_age_ms / POP_MS);
            CheckVisual {
                scale: lerp(0.4, 1.0, p),
                opacity: p.clamp(0.0, 1.0),
            }
        });

        PillVisual {
            slide,
            opacity,
            phase: self.phase,
            error,
            check,
            bars,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn run(state: &mut PillState, ms: f64) {
        let mut left = ms;
        while left > 0.0 {
            let dt = left.min(16.0);
            state.tick(dt);
            left -= dt;
        }
    }

    #[test]
    fn starts_hidden() {
        let s = PillState::new();
        let v = s.visual();
        assert_eq!(v.opacity, 0.0);
        assert!((v.slide - 46.0 * 1.4).abs() < 1e-9);
        assert!(!s.is_animating());
    }

    #[test]
    fn appears_in_300ms_then_settles() {
        let mut s = PillState::new();
        s.set_phase(Phase::Listening);
        assert!(s.is_animating());
        run(&mut s, 310.0);
        let v = s.visual();
        assert_eq!(v.opacity, 1.0);
        assert!(v.slide.abs() < 1e-9);
        assert!(!s.is_animating());
    }

    #[test]
    fn appear_overshoots_upwards() {
        let mut s = PillState::new();
        s.set_phase(Phase::Listening);
        let mut min_slide = f64::MAX;
        for _ in 0..30 {
            s.tick(10.0);
            min_slide = min_slide.min(s.visual().slide);
        }
        assert!(min_slide < 0.0, "no overshoot: {min_slide}");
    }

    #[test]
    fn hide_animates_then_reports_once() {
        let mut s = PillState::new();
        s.set_phase(Phase::Listening);
        run(&mut s, 400.0);
        s.hide();
        assert!(s.is_animating());
        run(&mut s, 100.0);
        assert!(!s.take_hidden());
        run(&mut s, 250.0);
        assert!(s.take_hidden());
        assert!(!s.take_hidden());
        assert_eq!(s.visual().opacity, 0.0);
        assert_eq!(s.phase(), Phase::Idle);
    }

    #[test]
    fn show_during_hide_cancels_it() {
        let mut s = PillState::new();
        s.set_phase(Phase::Listening);
        run(&mut s, 400.0);
        s.hide();
        run(&mut s, 100.0);
        s.set_phase(Phase::Transcribing);
        run(&mut s, 400.0);
        assert!(!s.take_hidden());
        assert_eq!(s.visual().opacity, 1.0);
    }

    #[test]
    fn levels_scroll_left_and_smooth() {
        let mut s = PillState::new();
        s.set_phase(Phase::Listening);
        run(&mut s, 400.0);
        s.push_level(1.0);
        run(&mut s, 200.0);
        let v = s.visual();
        assert!((v.bars[N_BARS - 1].height - 26.0).abs() < 0.05);
        assert!((v.bars[0].height - 4.0).abs() < 0.05);
        s.push_level(0.0);
        run(&mut s, 200.0);
        let v = s.visual();
        assert!((v.bars[N_BARS - 2].height - 26.0).abs() < 0.05);
        assert!((v.bars[N_BARS - 1].height - 4.0).abs() < 0.05);
    }

    #[test]
    fn bar_follows_target_within_70ms() {
        let mut s = PillState::new();
        s.set_phase(Phase::Listening);
        run(&mut s, 400.0);
        s.push_level(1.0);
        run(&mut s, 70.0);
        let h = s.visual().bars[N_BARS - 1].height;
        assert!(h > 4.0 + 0.9 * 22.0 && h < 26.0, "{h}");
    }

    #[test]
    fn levels_are_clamped() {
        let mut s = PillState::new();
        s.set_phase(Phase::Listening);
        run(&mut s, 400.0);
        s.push_level(7.0);
        s.push_level(-3.0);
        run(&mut s, 300.0);
        for b in s.visual().bars {
            assert!(b.height >= 4.0 - 1e-6 && b.height <= 26.0 + 1e-6);
        }
    }

    #[test]
    fn thinking_bars_oscillate_between_5_and_16_with_opposite_halves() {
        let mut s = PillState::new();
        s.set_phase(Phase::Transcribing);
        for _ in 0..200 {
            s.tick(7.0);
            let v = s.visual();
            for b in &v.bars {
                assert!(b.height >= 5.0 - 1e-9 && b.height <= 16.0 + 1e-9);
                assert!(b.opacity >= 0.55 - 1e-9 && b.opacity <= 1.0 + 1e-9);
            }
            // Two neighbouring bars are in opposite phase.
            assert!((v.bars[0].height + v.bars[1].height - 21.0).abs() < 1e-6);
        }
        assert!(s.is_animating());
    }

    #[test]
    fn formatting_animates_like_transcribing() {
        let mut a = PillState::new();
        let mut b = PillState::new();
        a.set_phase(Phase::Transcribing);
        b.set_phase(Phase::Formatting);
        run(&mut a, 333.0);
        run(&mut b, 333.0);
        assert_eq!(a.visual().bars, b.visual().bars);
    }

    #[test]
    fn done_shows_check_with_pop_and_no_bars() {
        let mut s = PillState::new();
        s.set_phase(Phase::Done);
        let v = s.visual();
        assert!(v.bars.is_empty());
        let c = v.check.expect("coche");
        assert!((c.scale - 0.4).abs() < 1e-9 && c.opacity == 0.0);
        run(&mut s, 330.0);
        let c = s.visual().check.expect("coche");
        assert!((c.scale - 1.0).abs() < 1e-9 && c.opacity == 1.0);
        run(&mut s, 400.0);
        assert!(!s.is_animating());
    }

    #[test]
    fn error_freezes_red_bars_at_4px() {
        let mut s = PillState::new();
        s.set_phase(Phase::Listening);
        s.push_level(1.0);
        run(&mut s, 300.0);
        s.set_phase(Phase::Error);
        run(&mut s, 300.0);
        let v = s.visual();
        assert!(v.error);
        assert!(v.bars.iter().all(|b| b.height == 4.0 && b.opacity == 1.0));
        assert!(!s.is_animating());
    }

    #[test]
    fn entering_listening_resets_levels() {
        let mut s = PillState::new();
        s.set_phase(Phase::Listening);
        for _ in 0..N_BARS {
            s.push_level(1.0);
        }
        run(&mut s, 300.0);
        s.set_phase(Phase::Transcribing);
        s.set_phase(Phase::Listening);
        assert!(s.visual().bars.iter().all(|b| b.height == 4.0));
    }

    #[test]
    fn idle_phase_hides() {
        let mut s = PillState::new();
        s.set_phase(Phase::Listening);
        run(&mut s, 400.0);
        s.set_phase(Phase::Idle);
        assert!(!s.is_visible());
    }
}
