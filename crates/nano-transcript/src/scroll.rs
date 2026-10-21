//! Explicit, command-driven reading scroll.
//!
//! The viewport is moved by the app, never inferred from `scrollTop` equality.
//! Sticky bottom is a signal: the user leaves it by scrolling up and returns by
//! reaching the bottom again. A reading anchor (entry identity plus line
//! offset) preserves the visible position across streaming content, width
//! changes, and history rebuilds.

use std::time::{Duration, Instant};

/// Where the top of the viewport sits inside the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    /// Entry identity at the top of the viewport.
    pub identity: String,
    /// Offset in display rows from that entry's first row.
    pub offset: usize,
}

/// One entry's measured start row and height, used to resolve anchors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryOffset {
    /// Stable entry identity.
    pub identity: String,
    /// First display row of the entry in the whole transcript.
    pub start: usize,
    /// Number of display rows the entry occupies.
    pub height: usize,
}

/// Per-terminal wheel curve. Terminals deliver different event rates, so one
/// global multiplier is wrong: Ghostty already applies its own acceleration,
/// while xterm.js and native xterm profiles report smaller steps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WheelProfile {
    /// Display rows per one wheel event.
    pub lines_per_tick: f32,
    /// Hard cap so a chatty trackpad cannot jump the whole transcript.
    pub max_lines_per_tick: usize,
}

impl Default for WheelProfile {
    fn default() -> Self {
        Self {
            lines_per_tick: 2.0,
            max_lines_per_tick: 12,
        }
    }
}

impl WheelProfile {
    /// Build a profile from the environment. `term_program` is `TERM_PROGRAM`;
    /// `term` is `TERM`.
    pub fn for_terminal(term_program: Option<&str>, term: Option<&str>) -> Self {
        let program = term_program.unwrap_or_default().to_ascii_lowercase();
        if program.contains("ghostty") {
            // Ghostty sends pre-multiplied deltas; do not multiply again.
            Self {
                lines_per_tick: 1.0,
                max_lines_per_tick: 6,
            }
        } else if program.contains("vscode") || program.contains("iterm") {
            // xterm.js and iTerm2 report several rows per notch.
            Self {
                lines_per_tick: 3.0,
                max_lines_per_tick: 18,
            }
        } else if term.is_some_and(|term| term.starts_with("xterm")) {
            Self {
                lines_per_tick: 3.0,
                max_lines_per_tick: 18,
            }
        } else {
            Self::default()
        }
    }

    /// Detect from the current process environment.
    pub fn detect() -> Self {
        Self::for_terminal(
            std::env::var("TERM_PROGRAM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        )
    }
}

/// Fractional wheel accumulation and reversal state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WheelState {
    remainder: f32,
    last_direction: i8,
}

impl WheelState {
    /// Convert one wheel event into whole display rows. A direction reversal
    /// discards the previous fractional remainder so reversed gestures never
    /// inherit acceleration.
    pub fn rows(&mut self, direction: i8, profile: WheelProfile) -> isize {
        if direction == 0 {
            return 0;
        }
        if direction != self.last_direction {
            self.remainder = 0.0;
            self.last_direction = direction;
        }
        self.remainder += profile.lines_per_tick;
        let whole = self
            .remainder
            .trunc()
            .min(profile.max_lines_per_tick as f32) as isize;
        if whole != 0 {
            self.remainder -= whole as f32;
        }
        whole * direction.signum() as isize
    }

    /// Reset the fractional state (for example after a terminal resize).
    pub fn reset(&mut self) {
        self.remainder = 0.0;
        self.last_direction = 0;
    }
}

/// Reading scroll state.
#[derive(Debug, Clone)]
pub struct ScrollState {
    sticky: bool,
    back: usize,
    anchor: Option<Anchor>,
    wheel: WheelState,
    profile: WheelProfile,
    last_wheel: Option<Instant>,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            sticky: true,
            back: 0,
            anchor: None,
            wheel: WheelState::default(),
            profile: WheelProfile::detect(),
            last_wheel: None,
        }
    }
}

impl ScrollState {
    /// Build with an explicit wheel profile (tests and retuned terminals).
    pub fn with_profile(profile: WheelProfile) -> Self {
        Self {
            profile,
            ..Self::default()
        }
    }

    /// Whether the view follows new content.
    pub fn sticky(&self) -> bool {
        self.sticky
    }

    /// Rows currently scrolled back from the bottom.
    pub fn back(&self) -> usize {
        self.back
    }

    /// Current reading anchor.
    pub fn anchor(&self) -> Option<&Anchor> {
        self.anchor.as_ref()
    }

    /// Wheel profile in use.
    pub fn profile(&self) -> WheelProfile {
        self.profile
    }

    /// Whether a wheel gesture happened recently; the app uses this to skip
    /// background polling for the next tick.
    pub fn recent_wheel(&self, now: Instant) -> bool {
        self.last_wheel
            .is_some_and(|at| now.saturating_duration_since(at) < Duration::from_millis(32))
    }

    /// Apply one wheel gesture. `direction` is `-1` up, `+1` down.
    pub fn wheel(&mut self, direction: isize, now: Instant) -> bool {
        self.last_wheel = Some(now);
        let rows = self.wheel.rows(direction.signum() as i8, self.profile);
        if rows == 0 {
            return false;
        }
        if rows < 0 {
            self.back = self.back.saturating_add(rows.unsigned_abs());
            self.sticky = false;
        } else {
            self.back = self.back.saturating_sub(rows as usize);
            if self.back == 0 {
                self.sticky = true;
            }
        }
        self.anchor = None;
        true
    }

    /// Page up; leaves sticky bottom.
    pub fn page_up(&mut self, viewport: usize) {
        self.back = self.back.saturating_add(viewport.saturating_sub(1).max(1));
        self.sticky = false;
        self.anchor = None;
        self.wheel.reset();
    }

    /// Page down; reaching the bottom re-enables sticky mode.
    pub fn page_down(&mut self, viewport: usize) {
        self.back = self.back.saturating_sub(viewport.saturating_sub(1).max(1));
        if self.back == 0 {
            self.sticky = true;
        }
        self.anchor = None;
        self.wheel.reset();
    }

    /// Jump to the bottom and follow new content.
    pub fn to_bottom(&mut self) {
        self.back = 0;
        self.sticky = true;
        self.anchor = None;
        self.wheel.reset();
    }

    /// Jump to the top.
    pub fn to_top(&mut self, total: usize, viewport: usize) {
        self.back = total.saturating_sub(viewport);
        self.sticky = false;
        self.anchor = None;
        self.wheel.reset();
    }

    /// Compute the top row for this frame.
    pub fn top(&self, total: usize, viewport: usize, entries: &[EntryOffset]) -> usize {
        let bottom = total.saturating_sub(viewport);
        if self.sticky {
            return bottom;
        }
        if let Some(anchor) = &self.anchor {
            if let Some(entry) = entries
                .iter()
                .find(|entry| entry.identity == anchor.identity)
            {
                return (entry.start + anchor.offset).min(bottom);
            }
        }
        bottom.saturating_sub(self.back)
    }

    /// Record the rendered top so the next frame can preserve it.
    pub fn after_render(
        &mut self,
        total: usize,
        viewport: usize,
        entries: &[EntryOffset],
        top: usize,
    ) {
        let bottom = total.saturating_sub(viewport);
        self.back = bottom.saturating_sub(top);
        if self.sticky {
            self.anchor = None;
            return;
        }
        self.anchor = entries
            .iter()
            .rev()
            .find(|entry| entry.start <= top)
            .map(|entry| Anchor {
                identity: entry.identity.clone(),
                offset: top.saturating_sub(entry.start),
            });
        if top >= bottom {
            self.sticky = true;
            self.back = 0;
            self.anchor = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<EntryOffset> {
        vec![
            EntryOffset {
                identity: "a".into(),
                start: 0,
                height: 10,
            },
            EntryOffset {
                identity: "b".into(),
                start: 10,
                height: 20,
            },
            EntryOffset {
                identity: "c".into(),
                start: 30,
                height: 10,
            },
        ]
    }

    #[test]
    fn default_follows_the_bottom() {
        let scroll = ScrollState::default();
        assert!(scroll.sticky());
        assert_eq!(scroll.top(40, 10, &entries()), 30);
    }

    #[test]
    fn scrolling_up_leaves_sticky_and_records_an_anchor() {
        let mut scroll = ScrollState::with_profile(WheelProfile {
            lines_per_tick: 5.0,
            max_lines_per_tick: 12,
        });
        assert!(scroll.wheel(-1, Instant::now()));
        assert!(!scroll.sticky());
        let top = scroll.top(40, 10, &entries());
        assert_eq!(top, 25);
        scroll.after_render(40, 10, &entries(), top);
        assert_eq!(
            scroll.anchor(),
            Some(&Anchor {
                identity: "b".into(),
                offset: 15
            })
        );
    }

    #[test]
    fn the_anchor_survives_new_content_and_width_changes() {
        let mut scroll = ScrollState::with_profile(WheelProfile {
            lines_per_tick: 5.0,
            max_lines_per_tick: 12,
        });
        scroll.wheel(-1, Instant::now());
        let top = scroll.top(40, 10, &entries());
        scroll.after_render(40, 10, &entries(), top);
        // The transcript grew by 30 rows and entry b is now 40 rows tall.
        let grown = vec![
            EntryOffset {
                identity: "a".into(),
                start: 0,
                height: 10,
            },
            EntryOffset {
                identity: "b".into(),
                start: 10,
                height: 40,
            },
            EntryOffset {
                identity: "c".into(),
                start: 50,
                height: 10,
            },
        ];
        assert_eq!(scroll.top(70, 10, &grown), 25);
    }

    #[test]
    fn returning_to_the_bottom_re_enables_sticky() {
        let mut scroll = ScrollState::with_profile(WheelProfile {
            lines_per_tick: 5.0,
            max_lines_per_tick: 12,
        });
        scroll.wheel(-1, Instant::now());
        scroll.wheel(1, Instant::now());
        assert!(scroll.sticky());
        assert_eq!(scroll.top(40, 10, &entries()), 30);
    }

    #[test]
    fn reversing_direction_discards_the_fractional_remainder() {
        let mut wheel = WheelState::default();
        let profile = WheelProfile {
            lines_per_tick: 0.4,
            max_lines_per_tick: 12,
        };
        assert_eq!(wheel.rows(-1, profile), 0);
        assert_eq!(wheel.rows(-1, profile), 0);
        assert_eq!(wheel.rows(-1, profile), -1);
        // Reverse: the half-built up-gesture must not accelerate the down move.
        assert_eq!(wheel.rows(1, profile), 0);
    }

    #[test]
    fn ghostty_does_not_double_apply_its_acceleration() {
        let ghostty = WheelProfile::for_terminal(Some("ghostty"), Some("xterm-256color"));
        let xterm = WheelProfile::for_terminal(Some("vscode"), Some("xterm-256color"));
        assert!(ghostty.lines_per_tick < xterm.lines_per_tick);
        assert_eq!(ghostty.lines_per_tick, 1.0);
        assert_eq!(xterm.lines_per_tick, 3.0);
    }
}
