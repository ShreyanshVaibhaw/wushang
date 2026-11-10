//! Keys in Windows' virtual-key codes, so the interface's key handling reads as it did on
//! Windows. The view maps each macOS key event to one of these (by the character the layout
//! types for letters, digits and punctuation, by key position for the rest).

use objc2_app_kit::{NSEvent, NSEventModifierFlags};

// The interface's own key type (gfx's d2d shim), so nothing converts between two.
pub use crate::d2d::VIRTUAL_KEY;
use crate::d2d::{
    VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F2, VK_F3, VK_F4, VK_F5, VK_F6,
    VK_F7, VK_F8, VK_F9, VK_F10, VK_F11, VK_F12, VK_HOME, VK_INSERT, VK_LEFT, VK_NEXT, VK_OEM_1,
    VK_OEM_2, VK_OEM_3, VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA, VK_OEM_MINUS,
    VK_OEM_PERIOD, VK_OEM_PLUS, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP,
};

/// The modifiers held right now, for code that asked GetKeyState on Windows.
fn held() -> NSEventModifierFlags {
    NSEvent::modifierFlags_class()
}

/// Shift is down.
pub fn shift_down() -> bool {
    held().contains(NSEventModifierFlags::Shift)
}

/// The virtual key for a macOS key event: by key position for keys that type no character,
/// else by the character the layout types without modifiers, so Ctrl+Z is the key marked Z
/// on an AZERTY keyboard too. Layouts without Latin letters fall back to the ANSI position,
/// as Windows' virtual keys do, and so does the top row unless it types a letter or digit.
/// With Command down it is what the layout types with Command, as the menu bar reads it:
/// "Dvorak - QWERTY ⌘" types QWERTY's letters then, so Command+W isn't Command+,.
pub(super) fn from_event(event: &NSEvent) -> Option<VIRTUAL_KEY> {
    // Without Shift, so Shift+Command+] is still ].
    let layer = event.modifierFlags() & NSEventModifierFlags::Command;
    let typed = event
        .charactersByApplyingModifiers(layer)
        .map(|s| s.to_string())
        .unwrap_or_default();
    virtual_key(event.keyCode(), &typed)
}

/// `from_event` for the key with macOS key code `code`, which types `typed`.
fn virtual_key(code: u16, typed: &str) -> Option<VIRTUAL_KEY> {
    if let Some(key) = by_position(code) {
        return Some(key);
    }
    let mut chars = typed.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        // AZERTY types punctuation on the top row and its digits with Shift: ' on 4, and - on
        // 6 on French - PC. Those keys stay digits, as on Windows, so Command+4 picks a tab.
        && (c.is_ascii_alphanumeric() || !is_digit_key(code))
        && let Some(key) = by_char(c)
    {
        return Some(key);
    }
    by_ansi_position(code)
}

/// The top row's 1 to 0 (kVK_ANSI_1 to kVK_ANSI_0).
fn is_digit_key(code: u16) -> bool {
    matches!(code, 0x12..=0x17 | 0x19 | 0x1A | 0x1C | 0x1D)
}

/// Keys that type no character, by their macOS key code (Carbon's kVK_ values).
fn by_position(code: u16) -> Option<VIRTUAL_KEY> {
    Some(match code {
        0x24 | 0x4C => VK_RETURN,
        0x30 => VK_TAB,
        0x31 => VK_SPACE,
        0x33 => VK_BACK,
        0x35 => VK_ESCAPE,
        0x75 => VK_DELETE,
        // The Help key sits where a PC keyboard's Insert is.
        0x72 => VK_INSERT,
        0x73 => VK_HOME,
        0x77 => VK_END,
        0x74 => VK_PRIOR,
        0x79 => VK_NEXT,
        0x7B => VK_LEFT,
        0x7C => VK_RIGHT,
        0x7D => VK_DOWN,
        0x7E => VK_UP,
        0x7A => VK_F1,
        0x78 => VK_F2,
        0x63 => VK_F3,
        0x76 => VK_F4,
        0x60 => VK_F5,
        0x61 => VK_F6,
        0x62 => VK_F7,
        0x64 => VK_F8,
        0x65 => VK_F9,
        0x6D => VK_F10,
        0x67 => VK_F11,
        0x6F => VK_F12,
        _ => return None,
    })
}

fn by_char(c: char) -> Option<VIRTUAL_KEY> {
    let c = c.to_ascii_uppercase();
    Some(match c {
        'A'..='Z' | '0'..='9' => VIRTUAL_KEY(c as u16),
        ';' => VK_OEM_1,
        '=' => VK_OEM_PLUS,
        ',' => VK_OEM_COMMA,
        '-' => VK_OEM_MINUS,
        '.' => VK_OEM_PERIOD,
        '/' => VK_OEM_2,
        '`' => VK_OEM_3,
        '[' => VK_OEM_4,
        '\\' => VK_OEM_5,
        ']' => VK_OEM_6,
        '\'' => VK_OEM_7,
        _ => return None,
    })
}

/// The US keyboard's keys by position (kVK_ANSI_*).
fn by_ansi_position(code: u16) -> Option<VIRTUAL_KEY> {
    const LETTERS: [(u16, u8); 36] = [
        (0x00, b'A'),
        (0x0B, b'B'),
        (0x08, b'C'),
        (0x02, b'D'),
        (0x0E, b'E'),
        (0x03, b'F'),
        (0x05, b'G'),
        (0x04, b'H'),
        (0x22, b'I'),
        (0x26, b'J'),
        (0x28, b'K'),
        (0x25, b'L'),
        (0x2E, b'M'),
        (0x2D, b'N'),
        (0x1F, b'O'),
        (0x23, b'P'),
        (0x0C, b'Q'),
        (0x0F, b'R'),
        (0x01, b'S'),
        (0x11, b'T'),
        (0x20, b'U'),
        (0x09, b'V'),
        (0x0D, b'W'),
        (0x07, b'X'),
        (0x10, b'Y'),
        (0x06, b'Z'),
        (0x1D, b'0'),
        (0x12, b'1'),
        (0x13, b'2'),
        (0x14, b'3'),
        (0x15, b'4'),
        (0x17, b'5'),
        (0x16, b'6'),
        (0x1A, b'7'),
        (0x1C, b'8'),
        (0x19, b'9'),
    ];
    if let Some((_, c)) = LETTERS.iter().find(|(k, _)| *k == code) {
        return Some(VIRTUAL_KEY(u16::from(*c)));
    }
    Some(match code {
        0x29 => VK_OEM_1,
        0x18 => VK_OEM_PLUS,
        0x2B => VK_OEM_COMMA,
        0x1B => VK_OEM_MINUS,
        0x2F => VK_OEM_PERIOD,
        0x2C => VK_OEM_2,
        0x32 => VK_OEM_3,
        0x21 => VK_OEM_4,
        0x2A => VK_OEM_5,
        0x1E => VK_OEM_6,
        0x27 => VK_OEM_7,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::d2d::{VK_1, VK_A, VK_M, VK_Q, VK_W, VK_Z};

    #[test]
    fn top_row_is_digits_on_us_and_azerty() {
        // Key codes from 1 to 0.
        let top_row = [0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1A, 0x1C, 0x19, 0x1D];
        let digits = "1234567890";
        // What the top row types without Shift on AZERTY, whose digits need Shift: French and
        // Belgian, then French - PC.
        let french = "&\u{e9}\"'(\u{a7}\u{e8}!\u{e7}\u{e0}";
        let french_pc = "&\u{e9}\"'(-\u{e8}_\u{e7}\u{e0}";
        for layout in [digits, french, french_pc] {
            for ((code, typed), digit) in
                top_row.into_iter().zip(layout.chars()).zip(digits.chars())
            {
                let key = virtual_key(code, &typed.to_string());
                assert_eq!(
                    key,
                    Some(VIRTUAL_KEY(digit as u16)),
                    "{typed:?} on key {code:#x}"
                );
            }
        }
    }

    #[test]
    fn punctuation_off_the_top_row_keeps_its_key() {
        // US: ' and - beside Return and 0.
        assert_eq!(virtual_key(0x27, "'"), Some(VK_OEM_7));
        assert_eq!(virtual_key(0x1B, "-"), Some(VK_OEM_MINUS));
        // French AZERTY: - where US has =, and ) where US has -.
        assert_eq!(virtual_key(0x18, "-"), Some(VK_OEM_MINUS));
        assert_eq!(virtual_key(0x1B, ")"), Some(VK_OEM_MINUS));
        assert_eq!(virtual_key(0x21, "["), Some(VK_OEM_4));
        assert_eq!(virtual_key(0x1E, "]"), Some(VK_OEM_6));
        assert_eq!(virtual_key(0x2B, ","), Some(VK_OEM_COMMA));
    }

    #[test]
    fn letters_follow_the_layout() {
        // AZERTY: A where US has Q, Q where US has A, M where US has ;, Z where US has W.
        assert_eq!(virtual_key(0x0C, "a"), Some(VK_A));
        assert_eq!(virtual_key(0x00, "q"), Some(VK_Q));
        assert_eq!(virtual_key(0x29, "m"), Some(VK_M));
        assert_eq!(virtual_key(0x0D, "z"), Some(VK_Z));
        assert_eq!(virtual_key(0x06, "w"), Some(VK_W));
        // Cyrillic: by US position.
        assert_eq!(virtual_key(0x0D, "\u{446}"), Some(VK_W));
        // A letter on the top row stays a letter.
        assert_eq!(virtual_key(0x12, "q"), Some(VK_Q));
        // Nothing typed: by US position.
        assert_eq!(virtual_key(0x12, ""), Some(VK_1));
    }
}
