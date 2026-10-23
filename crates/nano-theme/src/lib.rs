//! Shared semantic colors for Nano's own TUI surfaces.
//!
//! The terminal owns the window background, transparency, font, and palette.
//! Nano owns only the colors it emits for its own text, rules, states, focus,
//! and diffs. `NANO_THEME` selects one immutable palette once at startup.

use ratatui::style::{Color, Style};

/// Selected TUI theme. Parsed once at the application boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Theme {
    /// Default theme.
    Dark,
    /// Selected by the exact value `NANO_THEME=light`.
    Light,
}

impl Theme {
    /// Read `NANO_THEME` once at startup.
    pub fn from_env() -> Self {
        Self::from_value(std::env::var("NANO_THEME").ok().as_deref())
    }

    /// Only the exact value `light` selects the light palette; every other
    /// value, including `dark` and malformed input, keeps the dark palette.
    pub fn from_value(value: Option<&str>) -> Self {
        match value {
            Some("light") => Self::Light,
            _ => Self::Dark,
        }
    }

    /// Fixed colors for this theme.
    pub fn colors(self) -> &'static Palette {
        match self {
            Self::Dark => &DARK,
            Self::Light => &LIGHT,
        }
    }

    /// Theme foreground over the terminal's default background.
    pub fn base_style(self) -> Style {
        Style::default().fg(self.colors().fg).bg(Color::Reset)
    }

    /// Ordinary border and separator style.
    pub fn border_style(self) -> Style {
        Style::default().fg(self.colors().rule)
    }
}

/// Semantic roles emitted by Nano's own surfaces. Background roles are only
/// used for explicit focus, selection, and diff highlights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Body, input, titles, ordinary code.
    pub fg: Color,
    /// Secondary information, process rows, line numbers.
    pub muted: Color,
    /// User messages, emphasis, focus pointer.
    pub accent: Color,
    /// Errors and failures.
    pub error: Color,
    /// Warnings, approval, termination.
    pub warning: Color,
    /// Completion, availability, selected state.
    pub success: Color,
    /// Ordinary borders and separators.
    pub rule: Color,
    /// Existing hover and dock focus background.
    pub hover_bg: Color,
    /// Existing selected-row background.
    pub selected_bg: Color,
    /// Diff added marker and statistics.
    pub diff_add_fg: Color,
    /// Diff deleted marker and statistics.
    pub diff_del_fg: Color,
    /// Diff added line background.
    pub diff_add_bg: Color,
    /// Diff deleted line background.
    pub diff_del_bg: Color,
    /// Diff added word background.
    pub diff_add_word_bg: Color,
    /// Diff deleted word background.
    pub diff_del_word_bg: Color,
}

/// Frozen dark palette.
pub const DARK: Palette = Palette {
    fg: Color::Rgb(0xe8, 0xe6, 0xe0),
    muted: Color::Rgb(0x8d, 0x95, 0xa6),
    accent: Color::Rgb(0x7d, 0xa1, 0xde),
    error: Color::Rgb(0xda, 0x8a, 0x93),
    warning: Color::Rgb(0xd8, 0xb2, 0x70),
    success: Color::Rgb(0x9a, 0xc4, 0xa6),
    rule: Color::Rgb(0x42, 0x49, 0x53),
    hover_bg: Color::Rgb(0x24, 0x2b, 0x35),
    selected_bg: Color::Rgb(0x3b, 0x4a, 0x66),
    diff_add_fg: Color::Rgb(0xa1, 0xc8, 0xaa),
    diff_del_fg: Color::Rgb(0xe2, 0xa0, 0xaa),
    diff_add_bg: Color::Rgb(0x24, 0x38, 0x2c),
    diff_del_bg: Color::Rgb(0x38, 0x25, 0x2a),
    diff_add_word_bg: Color::Rgb(0x38, 0x5b, 0x42),
    diff_del_word_bg: Color::Rgb(0x65, 0x36, 0x41),
};

/// Frozen light palette.
pub const LIGHT: Palette = Palette {
    fg: Color::Rgb(0x34, 0x39, 0x45),
    muted: Color::Rgb(0x62, 0x69, 0x78),
    accent: Color::Rgb(0x36, 0x5f, 0x9d),
    error: Color::Rgb(0xa3, 0x3e, 0x4c),
    warning: Color::Rgb(0x85, 0x5b, 0x18),
    success: Color::Rgb(0x31, 0x5f, 0x3e),
    rule: Color::Rgb(0xc4, 0xc6, 0xc9),
    hover_bg: Color::Rgb(0xe8, 0xe8, 0xe5),
    selected_bg: Color::Rgb(0xd5, 0xde, 0xf2),
    diff_add_fg: Color::Rgb(0x31, 0x5f, 0x3e),
    diff_del_fg: Color::Rgb(0x8f, 0x35, 0x45),
    diff_add_bg: Color::Rgb(0xdc, 0xeb, 0xdd),
    diff_del_bg: Color::Rgb(0xf1, 0xdc, 0xdf),
    diff_add_word_bg: Color::Rgb(0xb6, 0xd6, 0xbc),
    diff_del_word_bg: Color::Rgb(0xe9, 0xba, 0xc3),
};

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(value: u32) -> Color {
        Color::Rgb(
            ((value >> 16) & 0xff) as u8,
            ((value >> 8) & 0xff) as u8,
            (value & 0xff) as u8,
        )
    }

    #[test]
    fn selection_accepts_only_the_exact_light_value() {
        assert_eq!(Theme::from_value(None), Theme::Dark);
        assert_eq!(Theme::from_value(Some("dark")), Theme::Dark);
        assert_eq!(Theme::from_value(Some("light")), Theme::Light);
        assert_eq!(Theme::from_value(Some("")), Theme::Dark);
        assert_eq!(Theme::from_value(Some("LIGHT")), Theme::Dark);
        assert_eq!(Theme::from_value(Some("light ")), Theme::Dark);
        assert_eq!(Theme::from_value(Some("1")), Theme::Dark);
    }

    #[test]
    fn dark_palette_matches_the_frozen_table() {
        let colors = Theme::Dark.colors();
        assert_eq!(colors.fg, rgb(0xe8e6e0));
        assert_eq!(colors.muted, rgb(0x8d95a6));
        assert_eq!(colors.accent, rgb(0x7da1de));
        assert_eq!(colors.error, rgb(0xda8a93));
        assert_eq!(colors.warning, rgb(0xd8b270));
        assert_eq!(colors.success, rgb(0x9ac4a6));
        assert_eq!(colors.rule, rgb(0x424953));
        assert_eq!(colors.hover_bg, rgb(0x242b35));
        assert_eq!(colors.selected_bg, rgb(0x3b4a66));
        assert_eq!(colors.diff_add_fg, rgb(0xa1c8aa));
        assert_eq!(colors.diff_del_fg, rgb(0xe2a0aa));
        assert_eq!(colors.diff_add_bg, rgb(0x24382c));
        assert_eq!(colors.diff_del_bg, rgb(0x38252a));
        assert_eq!(colors.diff_add_word_bg, rgb(0x385b42));
        assert_eq!(colors.diff_del_word_bg, rgb(0x653641));
    }

    #[test]
    fn light_palette_matches_the_frozen_table() {
        let colors = Theme::Light.colors();
        assert_eq!(colors.fg, rgb(0x343945));
        assert_eq!(colors.muted, rgb(0x626978));
        assert_eq!(colors.accent, rgb(0x365f9d));
        assert_eq!(colors.error, rgb(0xa33e4c));
        assert_eq!(colors.warning, rgb(0x855b18));
        assert_eq!(colors.success, rgb(0x315f3e));
        assert_eq!(colors.rule, rgb(0xc4c6c9));
        assert_eq!(colors.hover_bg, rgb(0xe8e8e5));
        assert_eq!(colors.selected_bg, rgb(0xd5def2));
        assert_eq!(colors.diff_add_fg, rgb(0x315f3e));
        assert_eq!(colors.diff_del_fg, rgb(0x8f3545));
        assert_eq!(colors.diff_add_bg, rgb(0xdcebdd));
        assert_eq!(colors.diff_del_bg, rgb(0xf1dcdf));
        assert_eq!(colors.diff_add_word_bg, rgb(0xb6d6bc));
        assert_eq!(colors.diff_del_word_bg, rgb(0xe9bac3));
    }

    #[test]
    fn base_style_sets_theme_foreground_over_terminal_background() {
        let style = Theme::Light.base_style();
        assert_eq!(style.fg, Some(Theme::Light.colors().fg));
        assert_eq!(style.bg, Some(Color::Reset));
    }
}
