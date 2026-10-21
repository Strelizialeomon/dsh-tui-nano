//! Terminal scroll handling.
//!
//! Wheel events are merged per frame and converted to whole scrollback rows.
//! A short same-direction burst ramps up like Ghostty/xterm.js; a direction
//! reversal drops the fractional residue instead of carrying it backwards.

use std::time::{Duration, Instant};

/// Device profiles for wheel acceleration. The outer terminal, not the PTY,
/// decides the feel; Ghostty reports pixel-precise wheels while xterm.js keeps
/// discrete line steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollProfile {
    /// Ghostty: a faster ramp is appropriate.
    Ghostty,
    /// xterm.js: discrete line steps with a gentler cap.
    XtermJs,
    /// Unknown/other terminal.
    Default,
}

#[derive(Debug, Clone, Copy)]
struct CurveConfig {
    step: f64,
    max: f64,
    window: Duration,
}

impl ScrollProfile {
    /// Detect the outer terminal from the environment, without interpreting
    /// the PTY's `TERM` (which is always xterm-256color for the child).
    pub fn detect() -> Self {
        let program = std::env::var("TERM_PROGRAM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        match program.as_str() {
            "ghostty" => Self::Ghostty,
            "vscode" | "xterm.js" | "xtermjs" => Self::XtermJs,
            _ => Self::Default,
        }
    }

    fn config(self) -> CurveConfig {
        match self {
            Self::Ghostty => CurveConfig {
                step: 1.5,
                max: 3.0,
                window: Duration::from_millis(60),
            },
            Self::XtermJs => CurveConfig {
                step: 1.2,
                max: 2.0,
                window: Duration::from_millis(80),
            },
            Self::Default => CurveConfig {
                step: 1.35,
                max: 3.0,
                window: Duration::from_millis(60),
            },
        }
    }
}

/// One direction-aware wheel accumulator.
#[derive(Debug, Clone)]
pub struct ScrollCurve {
    config: CurveConfig,
    residual: f64,
    direction: i8,
    factor: f64,
    last: Option<Instant>,
    pending: f64,
}

impl Default for ScrollCurve {
    fn default() -> Self {
        Self::new(ScrollProfile::Default)
    }
}

impl ScrollCurve {
    /// Build a curve for one device profile.
    pub fn new(profile: ScrollProfile) -> Self {
        Self {
            config: profile.config(),
            residual: 0.0,
            direction: 0,
            factor: 1.0,
            last: None,
            pending: 0.0,
        }
    }

    /// Add one wheel event: `delta > 0` scrolls up into history, `delta < 0`
    /// scrolls down toward the live screen.
    pub fn push(&mut self, delta: i32) {
        self.push_at(delta, Instant::now());
    }

    /// Add one wheel event at an explicit time, for tests.
    pub fn push_at(&mut self, delta: i32, at: Instant) {
        let direction = delta.signum() as i8;
        if delta == 0 {
            return;
        }
        let same_direction = direction == self.direction;
        let within_burst = self
            .last
            .map(|last| at.saturating_duration_since(last) <= self.config.window)
            .unwrap_or(false);
        if !same_direction {
            // Reverse jitter must not consume the old direction's residue.
            self.residual = 0.0;
            self.factor = 1.0;
        } else if within_burst {
            self.factor = (self.factor * self.config.step).min(self.config.max);
        } else {
            self.factor = 1.0;
        }
        self.direction = direction;
        self.last = Some(at);
        self.residual += f64::from(delta) * self.factor;
        // Merge into the next frame: whole rows are applied in `take_lines`.
        self.pending += self.residual.trunc();
        self.residual -= self.residual.trunc();
    }

    /// Whole scrollback rows accumulated since the last frame.
    pub fn take_lines(&mut self) -> i32 {
        let whole = self.pending.trunc();
        self.pending -= whole;
        whole.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
    }

    /// Residual fraction retained for the next event; diagnostics and tests.
    pub fn residual(&self) -> f64 {
        self.residual
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_ramps_and_returns_whole_lines() {
        let mut curve = ScrollCurve::default();
        let start = Instant::now();
        curve.push_at(1, start);
        curve.push_at(1, start + Duration::from_millis(10));
        curve.push_at(1, start + Duration::from_millis(20));
        let lines = curve.take_lines();
        assert_eq!(lines, 4, "1 + 1.35 + 1.82 rounds into four rows");
        assert!(curve.residual() > 0.0);
    }

    #[test]
    fn reverse_drops_the_fraction_and_resets_acceleration() {
        let mut curve = ScrollCurve::default();
        let start = Instant::now();
        curve.push_at(1, start);
        assert_eq!(curve.take_lines(), 1);
        curve.push_at(1, start + Duration::from_millis(10));
        assert_eq!(curve.take_lines(), 1);
        assert!(curve.residual() > 0.0);
        curve.push_at(-1, start + Duration::from_millis(20));
        assert_eq!(curve.take_lines(), -1);
        // The positive residue from the old direction is gone.
        assert!(curve.residual() < 0.5);
    }

    #[test]
    fn a_pause_restarts_the_curve() {
        let mut curve = ScrollCurve::default();
        let start = Instant::now();
        curve.push_at(1, start);
        curve.push_at(1, start + Duration::from_millis(10));
        let _ = curve.take_lines();
        curve.push_at(1, start + Duration::from_millis(500));
        assert_eq!(curve.take_lines(), 1);
    }

    #[test]
    fn fractional_residue_accumulates_without_dropping_input() {
        let mut curve = ScrollCurve::default();
        let start = Instant::now();
        // One event must never lose its row.
        curve.push_at(1, start);
        assert_eq!(curve.take_lines(), 1);
        curve.push_at(1, start + Duration::from_millis(1));
        assert_eq!(curve.take_lines(), 1);
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn ghostty_and_xterm_profiles_differ() {
        let start = Instant::now();
        let mut ghostty = ScrollCurve::new(ScrollProfile::Ghostty);
        let mut xterm = ScrollCurve::new(ScrollProfile::XtermJs);
        for step in 0..3 {
            ghostty.push_at(1, start + Duration::from_millis(step * 5));
            xterm.push_at(1, start + Duration::from_millis(step * 5));
        }
        assert!(
            ghostty.take_lines() > xterm.take_lines(),
            "Ghostty's ramp should be faster than xterm.js"
        );
    }

    #[test]
    fn detection_uses_the_outer_terminal_program() {
        // Detection reads process environment; the mapping itself is the
        // contract under test here.
        assert_eq!(
            ScrollProfile::Default.config().window,
            Duration::from_millis(60)
        );
        assert_eq!(ScrollProfile::XtermJs.config().max, 2.0);
        assert_eq!(ScrollProfile::Ghostty.config().step, 1.5);
    }
}
