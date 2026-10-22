//! Frozen VT compatibility samples for `vt100 0.15.2` / `tui-term 0.2.0`.
//!
//! These pin the parser behaviors S09 relies on: SGR colors, wide and combining
//! characters, alternate screen, scrollback, cursor addressing, application
//! cursor, bracketed paste and mouse reporting.

use nano_terminal::selection::Selection;

fn parser(rows: u16, cols: u16, scrollback: usize) -> vt100::Parser {
    vt100::Parser::new(rows, cols, scrollback)
}

#[test]
fn sgr_colors_are_parsed() {
    let mut parser = parser(4, 20, 0);
    parser.process(b"\x1b[31mred\x1b[0m plain");
    let red = parser.screen().cell(0, 0).expect("cell");
    assert_eq!(red.contents(), "r");
    assert_eq!(red.fgcolor(), vt100::Color::Idx(1));
    let plain = parser.screen().cell(0, 4).expect("cell");
    assert_eq!(plain.contents(), "p");
    assert_eq!(plain.fgcolor(), vt100::Color::Default);
}

#[test]
fn wide_characters_keep_their_cells() {
    let mut parser = parser(4, 20, 0);
    parser.process("你好世界".as_bytes());
    assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), "你");
    assert_eq!(parser.screen().cell(0, 1).unwrap().contents(), "");
    assert_eq!(parser.screen().cell(0, 2).unwrap().contents(), "好");
    let mut selection = Selection::start((0, 0));
    selection.update((0, 3));
    assert!(selection.text(parser.screen()).unwrap().contains("你好"));
}

#[test]
fn combining_characters_stay_in_one_cell() {
    let mut parser = parser(4, 20, 0);
    parser.process("e\u{301}x".as_bytes());
    assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), "e\u{301}");
    assert_eq!(parser.screen().cell(0, 1).unwrap().contents(), "x");
}

#[test]
fn alternate_screen_and_cursor_addressing_round_trip() {
    let mut parser = parser(4, 20, 0);
    parser.process(b"normal");
    parser.process(b"\x1b[?1049h");
    assert!(parser.screen().alternate_screen());
    parser.process(b"\x1b[2;3H");
    assert_eq!(parser.screen().cursor_position(), (1, 2));
    parser.process(b"ALT");
    assert_eq!(parser.screen().cell(1, 2).unwrap().contents(), "A");
    parser.process(b"\x1b[?1049l");
    assert!(!parser.screen().alternate_screen());
    assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), "n");
}

#[test]
fn scrollback_is_visible_through_the_same_screen() {
    let mut parser = parser(2, 20, 10);
    parser.process(b"one\r\ntwo\r\nthree");
    assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), "t");
    parser.set_scrollback(1);
    assert_eq!(parser.screen().scrollback(), 1);
    assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), "o");
    parser.set_scrollback(0);
    assert_eq!(parser.screen().scrollback(), 0);
}

#[test]
fn input_modes_are_reported() {
    let mut parser = parser(4, 20, 0);
    parser.process(b"\x1b[?1h\x1b[?2004h");
    assert!(parser.screen().application_cursor());
    assert!(parser.screen().bracketed_paste());
    parser.process(b"\x1b[?1l\x1b[?2004l");
    assert!(!parser.screen().application_cursor());
    assert!(!parser.screen().bracketed_paste());
}

#[test]
fn mouse_reporting_mode_and_encoding_are_reported() {
    let mut parser = parser(4, 20, 0);
    assert_eq!(
        parser.screen().mouse_protocol_mode(),
        vt100::MouseProtocolMode::None
    );
    // vt100 maps DECSET 9 to X10 press-only and 1000 to press/release.
    parser.process(b"\x1b[?9h");
    assert_eq!(
        parser.screen().mouse_protocol_mode(),
        vt100::MouseProtocolMode::Press
    );
    parser.process(b"\x1b[?1000h");
    assert_eq!(
        parser.screen().mouse_protocol_mode(),
        vt100::MouseProtocolMode::PressRelease
    );
    parser.process(b"\x1b[?1006h");
    assert_eq!(
        parser.screen().mouse_protocol_encoding(),
        vt100::MouseProtocolEncoding::Sgr
    );
}

#[test]
fn scroll_regions_and_erasure_do_not_corrupt_rows() {
    let mut parser = parser(4, 20, 0);
    parser.process(b"a\r\nb\r\nc\r\nd");
    parser.process(b"\x1b[2;3r");
    parser.process(b"\x1b[3;1H\x1b[K");
    let rows: Vec<String> = parser.screen().rows(0, 20).collect();
    assert_eq!(rows[0].trim_end(), "a");
    assert_eq!(rows[1].trim_end(), "b");
    assert_eq!(rows[2].trim_end(), "");
    assert_eq!(rows[3].trim_end(), "d");
}

#[test]
fn frozen_parser_identity_matches_the_widget_reexport() {
    assert_eq!(nano_terminal::PARSER_CRATE, "vt100 0.15.2");
    assert_eq!(nano_terminal::WIDGET_CRATE, "tui-term 0.2.0");
    // tui-term re-exports its exact vt100; the parser and renderer cannot drift.
    let _parser: tui_term::vt100::Parser = tui_term::vt100::Parser::new(4, 20, 0);
}
