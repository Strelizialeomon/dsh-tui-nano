//! Terminal input encoding and the unacknowledged-byte budget.
//!
//! Keys become xterm sequences using the parser's application-cursor mode;
//! mouse events become X10/UTF-8/SGR reports only while the process asked for
//! mouse reporting. Nano never writes input into its local parser: the PTY echo
//! is the only source of screen content.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use vt100::{MouseProtocolEncoding, MouseProtocolMode};

/// Why a local input command was rejected before it reached the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputRejection {
    /// This attachment is read-only or the process is not running.
    Disabled,
    /// The command is larger than `maxInputBytes`.
    TooLarge,
    /// Unacknowledged input exhausted `maxBufferedBytes`.
    Full,
}

/// Attachment-local sequence allocation and byte accounting.
#[derive(Debug, Clone, Default)]
pub struct InputBudget {
    next_sequence: u64,
    unacked: BTreeMap<u64, usize>,
    unacked_bytes: usize,
}

impl InputBudget {
    /// Reset for a new attachment; sequences start at one.
    pub fn reset(&mut self) {
        self.next_sequence = 0;
        self.unacked.clear();
        self.unacked_bytes = 0;
    }

    /// Allocate one sequence and account for its bytes.
    pub fn allocate(
        &mut self,
        bytes: usize,
        max_input_bytes: usize,
        max_buffered_bytes: usize,
    ) -> Result<u64, InputRejection> {
        if bytes > max_input_bytes {
            return Err(InputRejection::TooLarge);
        }
        if self.unacked_bytes.saturating_add(bytes) > max_buffered_bytes {
            return Err(InputRejection::Full);
        }
        self.next_sequence += 1;
        let sequence = self.next_sequence;
        self.unacked.insert(sequence, bytes);
        self.unacked_bytes += bytes;
        Ok(sequence)
    }

    /// Release one acknowledged sequence.
    pub fn acknowledge(&mut self, sequence: u64) {
        if let Some(bytes) = self.unacked.remove(&sequence) {
            self.unacked_bytes = self.unacked_bytes.saturating_sub(bytes);
        }
    }

    /// Unacknowledged raw bytes.
    pub fn unacked_bytes(&self) -> usize {
        self.unacked_bytes
    }

    /// Whether any command is awaiting acknowledgement.
    pub fn in_flight(&self) -> bool {
        !self.unacked.is_empty()
    }
}

/// Encode one key event using xterm conventions.
pub fn encode_key(key: &KeyEvent, application_cursor: bool) -> Option<Vec<u8>> {
    let modifiers = key.modifiers;
    let modifier = modifier_parameter(modifiers);
    let control = modifiers.contains(KeyModifiers::CONTROL)
        && !modifiers.contains(KeyModifiers::SUPER)
        && !modifiers.contains(KeyModifiers::HYPER);
    let alt = modifiers.contains(KeyModifiers::ALT);

    let bytes = match key.code {
        KeyCode::Char(character) => {
            if control {
                let mut bytes = Vec::new();
                if alt {
                    bytes.push(0x1b);
                }
                bytes.push(control_byte(character)?);
                bytes
            } else {
                let mut bytes = Vec::new();
                if alt {
                    bytes.push(0x1b);
                }
                let mut buffer = [0u8; 4];
                bytes.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
                bytes
            }
        }
        KeyCode::Enter => prefixed(alt, b"\r"),
        KeyCode::Backspace => prefixed(alt, b"\x7f"),
        KeyCode::Tab => prefixed(alt, b"\t"),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => prefixed(alt, b"\x1b"),
        KeyCode::Null => prefixed(alt, b"\x00"),
        KeyCode::Up => cursor_key(b'A', application_cursor, modifier),
        KeyCode::Down => cursor_key(b'B', application_cursor, modifier),
        KeyCode::Right => cursor_key(b'C', application_cursor, modifier),
        KeyCode::Left => cursor_key(b'D', application_cursor, modifier),
        KeyCode::Home => cursor_key(b'H', application_cursor, modifier),
        KeyCode::End => cursor_key(b'F', application_cursor, modifier),
        KeyCode::Insert => tilde_key(2, modifier),
        KeyCode::Delete => tilde_key(3, modifier),
        KeyCode::PageUp => tilde_key(5, modifier),
        KeyCode::PageDown => tilde_key(6, modifier),
        KeyCode::F(number) => function_key(number, modifier)?,
        _ => return None,
    };
    Some(bytes)
}

fn prefixed(alt: bool, bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len() + usize::from(alt));
    if alt {
        output.push(0x1b);
    }
    output.extend_from_slice(bytes);
    output
}

fn control_byte(character: char) -> Option<u8> {
    let character = character.to_ascii_lowercase();
    if character.is_ascii_alphabetic() {
        return Some(character as u8 - b'a' + 1);
    }
    match character {
        '@' | ' ' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' | '/' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

fn modifier_parameter(modifiers: KeyModifiers) -> u8 {
    let mut value = 1;
    if modifiers.contains(KeyModifiers::SHIFT) {
        value += 1;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        value += 2;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        value += 4;
    }
    value
}

fn cursor_key(final_byte: u8, application_cursor: bool, modifier: u8) -> Vec<u8> {
    if modifier > 1 {
        format!("\x1b[1;{modifier}{}", char::from(final_byte)).into_bytes()
    } else if application_cursor {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

fn tilde_key(number: u8, modifier: u8) -> Vec<u8> {
    if modifier > 1 {
        format!("\x1b[{number};{modifier}~").into_bytes()
    } else {
        format!("\x1b[{number}~").into_bytes()
    }
}

fn function_key(number: u8, modifier: u8) -> Option<Vec<u8>> {
    let bytes = match number {
        1..=4 => {
            let final_byte = b'P' + (number - 1);
            if modifier > 1 {
                format!("\x1b[1;{modifier}{}", char::from(final_byte)).into_bytes()
            } else {
                vec![0x1b, b'O', final_byte]
            }
        }
        5 => tilde_key(15, modifier),
        6 => tilde_key(17, modifier),
        7 => tilde_key(18, modifier),
        8 => tilde_key(19, modifier),
        9 => tilde_key(20, modifier),
        10 => tilde_key(21, modifier),
        11 => tilde_key(23, modifier),
        12 => tilde_key(24, modifier),
        _ => return None,
    };
    Some(bytes)
}

/// Encode a bracketed paste when the terminal enabled it.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut bytes = Vec::with_capacity(text.len() + 12);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        text.as_bytes().to_vec()
    }
}

/// Encode one mouse event for the process, if its mode reports this event.
pub fn encode_mouse(
    event: &MouseEvent,
    area: Rect,
    mode: MouseProtocolMode,
    encoding: MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    if mode == MouseProtocolMode::None {
        return None;
    }
    if event.column < area.x
        || event.row < area.y
        || event.column >= area.x.saturating_add(area.width)
        || event.row >= area.y.saturating_add(area.height)
    {
        return None;
    }
    let (kind, base) = match event.kind {
        MouseEventKind::ScrollUp => ('M', 64),
        MouseEventKind::ScrollDown => ('M', 65),
        MouseEventKind::ScrollLeft => ('M', 66),
        MouseEventKind::ScrollRight => ('M', 67),
        MouseEventKind::Down(button) => ('M', button_code(button)),
        MouseEventKind::Up(button) => ('m', button_code(button)),
        MouseEventKind::Drag(button) => ('M', 32 + button_code(button)),
        MouseEventKind::Moved => ('M', 35),
    };
    if !mode_reports(mode, event.kind) {
        return None;
    }
    let modifiers = mouse_modifiers(event.modifiers);
    let code = base + modifiers;
    let column = event.column - area.x + 1;
    let row = event.row - area.y + 1;
    match encoding {
        MouseProtocolEncoding::Sgr => {
            Some(format!("\x1b[<{code};{column};{row}{kind}").into_bytes())
        }
        MouseProtocolEncoding::Default | MouseProtocolEncoding::Utf8 => {
            // Values above 127 are carried as UTF-8 code points, so the
            // protocol's `bytes` field stays valid UTF-8.
            let mut text = String::from("\x1b[M");
            text.push(char::from(32 + code));
            text.push(char::from((32 + column.min(222)).min(255) as u8));
            text.push(char::from((32 + row.min(222)).min(255) as u8));
            Some(text.into_bytes())
        }
    }
}

fn button_code(button: crossterm::event::MouseButton) -> u8 {
    match button {
        crossterm::event::MouseButton::Left => 0,
        crossterm::event::MouseButton::Middle => 1,
        crossterm::event::MouseButton::Right => 2,
    }
}

fn mouse_modifiers(modifiers: KeyModifiers) -> u8 {
    let mut value = 0;
    if modifiers.contains(KeyModifiers::SHIFT) {
        value += 4;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        value += 8;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        value += 16;
    }
    value
}

fn mode_reports(mode: MouseProtocolMode, kind: MouseEventKind) -> bool {
    match mode {
        MouseProtocolMode::None => false,
        MouseProtocolMode::Press => matches!(
            kind,
            MouseEventKind::Down(_)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight
        ),
        MouseProtocolMode::PressRelease => matches!(
            kind,
            MouseEventKind::Down(_)
                | MouseEventKind::Up(_)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight
        ),
        MouseProtocolMode::ButtonMotion => !matches!(kind, MouseEventKind::Moved),
        MouseProtocolMode::AnyMotion => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState, MouseButton};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn encodes_plain_and_control_characters() {
        assert_eq!(
            encode_key(&key(KeyCode::Char('a'), KeyModifiers::NONE), false),
            Some(b"a".to_vec())
        );
        assert_eq!(
            encode_key(&key(KeyCode::Char('c'), KeyModifiers::CONTROL), false),
            Some(vec![3])
        );
        assert_eq!(
            encode_key(&key(KeyCode::Char('x'), KeyModifiers::ALT), false),
            Some(vec![0x1b, b'x'])
        );
        assert_eq!(
            encode_key(&key(KeyCode::Enter, KeyModifiers::NONE), false),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            encode_key(&key(KeyCode::Backspace, KeyModifiers::NONE), false),
            Some(vec![0x7f])
        );
    }

    #[test]
    fn arrows_follow_application_cursor_mode() {
        assert_eq!(
            encode_key(&key(KeyCode::Up, KeyModifiers::NONE), false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key(&key(KeyCode::Up, KeyModifiers::NONE), true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode_key(&key(KeyCode::Right, KeyModifiers::CONTROL), false),
            Some(b"\x1b[1;5C".to_vec())
        );
    }

    #[test]
    fn function_and_navigation_keys() {
        assert_eq!(
            encode_key(&key(KeyCode::F(1), KeyModifiers::NONE), false),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            encode_key(&key(KeyCode::PageUp, KeyModifiers::NONE), false),
            Some(b"\x1b[5~".to_vec())
        );
        assert_eq!(
            encode_key(&key(KeyCode::Delete, KeyModifiers::SHIFT), false),
            Some(b"\x1b[3;2~".to_vec())
        );
        assert_eq!(
            encode_key(&key(KeyCode::F(13), KeyModifiers::NONE), false),
            None
        );
    }

    #[test]
    fn bracketed_paste_wraps_the_payload_exactly() {
        assert_eq!(
            encode_paste("a\nb", true),
            b"\x1b[200~a\nb\x1b[201~".to_vec()
        );
        assert_eq!(encode_paste("a\nb", false), b"a\nb".to_vec());
    }

    #[test]
    fn input_budget_allocates_and_acknowledges() {
        let mut budget = InputBudget::default();
        assert_eq!(budget.allocate(4, 8, 8).unwrap(), 1);
        assert_eq!(budget.allocate(4, 8, 8).unwrap(), 2);
        assert_eq!(budget.allocate(1, 8, 8), Err(InputRejection::Full));
        budget.acknowledge(1);
        assert_eq!(budget.allocate(4, 8, 8).unwrap(), 3);
        assert_eq!(budget.allocate(9, 8, 8), Err(InputRejection::TooLarge));
        assert_eq!(budget.unacked_bytes(), 8);
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn mouse_reports_only_in_an_enabled_mode() {
        let area = Rect::new(0, 0, 20, 10);
        assert_eq!(
            encode_mouse(
                &mouse(MouseEventKind::Down(MouseButton::Left), 2, 3),
                area,
                MouseProtocolMode::None,
                MouseProtocolEncoding::Sgr,
            ),
            None
        );
        assert_eq!(
            encode_mouse(
                &mouse(MouseEventKind::Down(MouseButton::Left), 2, 3),
                area,
                MouseProtocolMode::PressRelease,
                MouseProtocolEncoding::Sgr,
            ),
            Some(b"\x1b[<0;3;4M".to_vec())
        );
        assert_eq!(
            encode_mouse(
                &mouse(MouseEventKind::ScrollUp, 0, 0),
                area,
                MouseProtocolMode::Press,
                MouseProtocolEncoding::Default,
            ),
            Some(vec![0x1b, b'[', b'M', 32 + 64, 32 + 1, 32 + 1])
        );
        assert_eq!(
            encode_mouse(
                &mouse(MouseEventKind::Down(MouseButton::Left), 99, 99),
                area,
                MouseProtocolMode::AnyMotion,
                MouseProtocolEncoding::Sgr,
            ),
            None
        );
    }
}
