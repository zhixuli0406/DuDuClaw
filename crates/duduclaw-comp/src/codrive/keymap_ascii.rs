// CD-0 codrive spike — the `{"op":"text",...}` synthesizer's keymap.
//
// Honest limitation (see BUILD.md "CD-0 codrive spike" honest-stub list):
// this only covers the printable US-QWERTY ASCII subset actually needed to
// drive the live-run test (`echo <token>` + Enter into `foot`). No CJK, no
// full Unicode, no non-US layouts, no dead keys. A real implementation would
// walk the agent seat's actual xkb keymap (`xkb_keymap_key_get_syms_by_level`
// reverse lookup) to support arbitrary layouts and character sets — deferred
// past CD-0, tracked in DESIGN-codrive-desktop-2026-08.md §5 CD-1+.
//
// evdev → XKB keycode is always `evdev + 8` (XKB reserves the bottom 8
// codes historically occupied by the X11 core protocol). The evdev codes
// below are the standard `linux/input-event-codes.h` `KEY_*` values for a
// US layout.

const EVDEV_TO_XKB_OFFSET: u32 = 8;

/// `KEY_LEFTSHIFT` (evdev 42) → XKB keycode, used to shift letters for the
/// uppercase-ASCII case.
pub const SHIFT_XKB_KEYCODE: u32 = 42 + EVDEV_TO_XKB_OFFSET;

/// a..z evdev `KEY_*` codes, standard US layout scancode table.
const LETTER_EVDEV: [u32; 26] = [
    30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,
];

/// Returns `(xkb_keycode, needs_shift)` for the given ASCII char, or `None`
/// if it's outside this spike's tiny table.
pub fn ascii_to_xkb(c: char) -> Option<(u32, bool)> {
    let (evdev, shift) = match c {
        'a'..='z' => (LETTER_EVDEV[(c as u8 - b'a') as usize], false),
        'A'..='Z' => (LETTER_EVDEV[(c.to_ascii_lowercase() as u8 - b'a') as usize], true),
        '0' => (11, false),
        '1'..='9' => (2 + (c as u8 - b'1') as u32, false),
        ' ' => (57, false),   // KEY_SPACE
        '\n' => (28, false),  // KEY_ENTER
        '\t' => (15, false),  // KEY_TAB
        '-' => (12, false),   // KEY_MINUS
        '=' => (13, false),   // KEY_EQUAL
        ',' => (51, false),   // KEY_COMMA
        '.' => (52, false),   // KEY_DOT
        '/' => (53, false),   // KEY_SLASH
        ';' => (39, false),   // KEY_SEMICOLON
        '_' => (12, true),    // shift+KEY_MINUS
        '>' => (52, true),    // shift+KEY_DOT — needed for shell redirects
                               // (`cmd > file`) in the CD-0 live-run test.
        '<' => (51, true),    // shift+KEY_COMMA — added alongside '>' for
                               // symmetry, same reasoning.
        _ => return None,
    };
    Some((evdev + EVDEV_TO_XKB_OFFSET, shift))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercase_letters_no_shift() {
        let (code, shift) = ascii_to_xkb('a').unwrap();
        assert_eq!(code, 30 + EVDEV_TO_XKB_OFFSET);
        assert!(!shift);
    }

    #[test]
    fn uppercase_letters_need_shift() {
        let (code, shift) = ascii_to_xkb('A').unwrap();
        assert_eq!(code, 30 + EVDEV_TO_XKB_OFFSET);
        assert!(shift);
    }

    #[test]
    fn digits_and_space_and_enter() {
        assert_eq!(ascii_to_xkb('0').unwrap().0, 11 + EVDEV_TO_XKB_OFFSET);
        assert_eq!(ascii_to_xkb('1').unwrap().0, 2 + EVDEV_TO_XKB_OFFSET);
        assert_eq!(ascii_to_xkb(' ').unwrap().0, 57 + EVDEV_TO_XKB_OFFSET);
        assert_eq!(ascii_to_xkb('\n').unwrap().0, 28 + EVDEV_TO_XKB_OFFSET);
    }

    #[test]
    fn unsupported_char_is_none() {
        assert!(ascii_to_xkb('中').is_none());
        assert!(ascii_to_xkb('€').is_none());
    }

    #[test]
    fn shell_redirect_chars_need_shift() {
        let (gt, gt_shift) = ascii_to_xkb('>').unwrap();
        assert_eq!(gt, 52 + EVDEV_TO_XKB_OFFSET);
        assert!(gt_shift);
        let (lt, lt_shift) = ascii_to_xkb('<').unwrap();
        assert_eq!(lt, 51 + EVDEV_TO_XKB_OFFSET);
        assert!(lt_shift);
    }
}
