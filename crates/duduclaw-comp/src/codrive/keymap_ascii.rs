// CD-0/CD-1 codrive spike — the `{"op":"text",...}` synthesizer's keymap,
// plus (CD-1) the `{"op":"key_name",...}` named-key allowlist.
//
// evdev → XKB keycode is always `evdev + 8` (XKB reserves the bottom 8
// codes historically occupied by the X11 core protocol). The evdev codes
// below are the standard `linux/input-event-codes.h` `KEY_*` values for a
// US layout.
//
// CD-1 status (task brief req 6): `ascii_to_xkb` now covers the FULL
// printable-ASCII range (0x20..=0x7E) — every letter, digit, space, and
// punctuation mark a standard US QWERTY layout can produce, shifted or
// not. This closes the CD-0-era honest limitation that only had a small
// subset (letters/digits/space/enter/tab plus a handful of punctuation
// marks needed for one specific live-run test).
//
// Non-ASCII (CJK/Unicode) — deliberately still unsupported, and the
// reasoning is now backed by actual research rather than "deferred,
// unresearched" (CD-1 checked, per repo doctrine "先查 vendored smithay
// 0.7 的實際 API" before deciding): `smithay::input::keyboard::
// KeyboardHandle` DOES expose the APIs that would be needed —
// `set_keymap_from_string` (loads a whole new XKB_KEYMAP_FORMAT_TEXT_V1
// keymap) and `set_xkb_config`/`with_xkb_state` (in-place xkb state
// access). The capability is real. What makes using it for arbitrary
// Unicode input a *new, non-trivial engineering effort* rather than a
// same-round bolt-on, and why this round keeps the CD-0-era "skip + warn"
// behavior instead:
//   1. `set_keymap_from_string` REPLACES the entire keymap (keycodes +
//      types + compat + symbols sections), not "add one symbol to the
//      existing map" — there is no incremental-add API. Synthesizing one
//      CJK character means generating a complete, syntactically valid XKB
//      keymap-text-format-v1 document (a small custom compiler, in effect)
//      that also still defines every OTHER key this seat might need,
//      re-parsing it through libxkbcommon on every keymap swap.
//   2. It's a whole-SEAT operation. `agent_seat`'s keyboard is the only
//      one the injection channel drives; swapping its keymap mid-stream
//      while a caller might have other keys logically "pressed" (shift
//      held for `ascii_to_xkb`'s two-event press/release synthesis, etc.)
//      risks corrupting modifier/pressed-key tracking that this file's
//      existing ASCII path never has to worry about.
//   3. No cheap way to validate a generated keymap string is correct
//      without a real xkbcommon parse — this crate's container-level
//      verification (BUILD.md) has no interactive way to visually confirm
//      "the right glyph appeared," unlike the ASCII path's proof-by-shell-
//      side-effect trick.
// Net judgment: real capability, non-trivial and independently risky
// engineering scope — not something to half-implement in the same round as
// five other requirements. Tracked for a future round with the concrete
// API names above already identified, so it isn't re-researched from
// scratch. Unicode/CJK chars still hit the `_ => return None` fallthrough
// below and are warned-and-skipped by `codrive::handle_agent_inject`'s
// `Text` handling in `mod.rs`, same as CD-0.

const EVDEV_TO_XKB_OFFSET: u32 = 8;

/// `KEY_LEFTSHIFT` (evdev 42) → XKB keycode, used to shift letters for the
/// uppercase-ASCII case.
pub const SHIFT_XKB_KEYCODE: u32 = 42 + EVDEV_TO_XKB_OFFSET;

/// a..z evdev `KEY_*` codes, standard US layout scancode table.
const LETTER_EVDEV: [u32; 26] = [
    30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,
];

/// Returns `(xkb_keycode, needs_shift)` for the given ASCII char, or `None`
/// if it's outside the printable-ASCII range this table covers (see module
/// doc for the non-ASCII decision).
pub fn ascii_to_xkb(c: char) -> Option<(u32, bool)> {
    let (evdev, shift) = match c {
        'a'..='z' => (LETTER_EVDEV[(c as u8 - b'a') as usize], false),
        'A'..='Z' => (LETTER_EVDEV[(c.to_ascii_lowercase() as u8 - b'a') as usize], true),
        '0' => (11, false),
        '1'..='9' => (2 + (c as u8 - b'1') as u32, false),
        ' ' => (57, false),   // KEY_SPACE
        '\n' => (28, false),  // KEY_ENTER
        '\t' => (15, false),  // KEY_TAB

        // Unshifted punctuation — each sits on its own evdev scancode.
        '-' => (12, false),   // KEY_MINUS
        '=' => (13, false),   // KEY_EQUAL
        ',' => (51, false),   // KEY_COMMA
        '.' => (52, false),   // KEY_DOT
        '/' => (53, false),   // KEY_SLASH
        ';' => (39, false),   // KEY_SEMICOLON
        '`' => (41, false),   // KEY_GRAVE
        '[' => (26, false),   // KEY_LEFTBRACE
        ']' => (27, false),   // KEY_RIGHTBRACE
        '\\' => (43, false),  // KEY_BACKSLASH
        '\'' => (40, false),  // KEY_APOSTROPHE

        // Shifted punctuation — same evdev scancode as the unshifted
        // character it shares a physical key with on a standard US QWERTY
        // layout.
        '_' => (12, true),    // shift+KEY_MINUS
        '+' => (13, true),    // shift+KEY_EQUAL
        '>' => (52, true),    // shift+KEY_DOT — needed for shell redirects
                               // (`cmd > file`) in the CD-0 live-run test.
        '<' => (51, true),    // shift+KEY_COMMA — added alongside '>' for
                               // symmetry, same reasoning.
        ':' => (39, true),    // shift+KEY_SEMICOLON
        '~' => (41, true),    // shift+KEY_GRAVE
        '{' => (26, true),    // shift+KEY_LEFTBRACE
        '}' => (27, true),    // shift+KEY_RIGHTBRACE
        '|' => (43, true),    // shift+KEY_BACKSLASH
        '"' => (40, true),    // shift+KEY_APOSTROPHE
        '?' => (53, true),    // shift+KEY_SLASH

        // Shifted number row (CD-1 req 6 — completes "全部 printable
        // ASCII").
        '!' => (2, true),     // shift+KEY_1
        '@' => (3, true),     // shift+KEY_2
        '#' => (4, true),     // shift+KEY_3
        '$' => (5, true),     // shift+KEY_4
        '%' => (6, true),     // shift+KEY_5
        '^' => (7, true),     // shift+KEY_6
        '&' => (8, true),     // shift+KEY_7
        '*' => (9, true),     // shift+KEY_8
        '(' => (10, true),    // shift+KEY_9
        ')' => (11, true),    // shift+KEY_0

        _ => return None,
    };
    Some((evdev + EVDEV_TO_XKB_OFFSET, shift))
}

/// CD-1 req 4: named functional keys with no printable-ASCII
/// representation for `ascii_to_xkb` to reach. Same `evdev + 8` XKB
/// derivation as everywhere else in this module. Deliberately an
/// allowlist, not "any `KEY_*` the caller names" — `listener.rs`'s
/// `validate()` rejects anything not in this table before it ever reaches
/// the main thread, and `codrive::handle_agent_inject` re-derives it
/// defensively rather than trusting that upstream check alone (repo
/// convention: validation gates fail closed, never "trust the caller
/// already checked").
pub fn key_name_to_xkb(name: &str) -> Option<u32> {
    let evdev = match name {
        "enter" => 28,      // KEY_ENTER
        "tab" => 15,        // KEY_TAB
        "backspace" => 14,  // KEY_BACKSPACE
        "escape" => 1,      // KEY_ESC
        "delete" => 111,    // KEY_DELETE
        "space" => 57,      // KEY_SPACE
        "up" => 103,        // KEY_UP
        "down" => 108,      // KEY_DOWN
        "left" => 105,      // KEY_LEFT
        "right" => 106,     // KEY_RIGHT
        "home" => 102,      // KEY_HOME
        "end" => 107,       // KEY_END
        "pageup" => 104,    // KEY_PAGEUP
        "pagedown" => 109,  // KEY_PAGEDOWN
        _ => return None,
    };
    Some(evdev + EVDEV_TO_XKB_OFFSET)
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

    /// CD-1 req 6's actual acceptance bar: every printable ASCII char
    /// (space through tilde, 0x20..=0x7E) must resolve to something —
    /// stronger than spot-checking a handful of symbols, and it would have
    /// caught the CD-0-era gap (`>`/`<` missing until a live-run test
    /// happened to need them) automatically.
    #[test]
    fn all_printable_ascii_is_covered() {
        for byte in 0x20u8..=0x7E {
            let c = byte as char;
            assert!(
                ascii_to_xkb(c).is_some(),
                "printable ASCII char {c:?} (0x{byte:02x}) has no entry in ascii_to_xkb"
            );
        }
    }

    #[test]
    fn shifted_number_row_symbols() {
        let cases = [
            ('!', 2, true),
            ('@', 3, true),
            ('#', 4, true),
            ('$', 5, true),
            ('%', 6, true),
            ('^', 7, true),
            ('&', 8, true),
            ('*', 9, true),
            ('(', 10, true),
            (')', 11, true),
            ('+', 13, true),
        ];
        for (c, evdev, shift) in cases {
            let (code, s) = ascii_to_xkb(c).unwrap();
            assert_eq!(code, evdev + EVDEV_TO_XKB_OFFSET, "char {c:?}");
            assert_eq!(s, shift, "char {c:?} shift flag");
        }
    }

    #[test]
    fn grave_and_tilde() {
        assert_eq!(ascii_to_xkb('`').unwrap(), (41 + EVDEV_TO_XKB_OFFSET, false));
        assert_eq!(ascii_to_xkb('~').unwrap(), (41 + EVDEV_TO_XKB_OFFSET, true));
    }

    #[test]
    fn brackets_braces_backslash_pipe() {
        assert_eq!(ascii_to_xkb('[').unwrap(), (26 + EVDEV_TO_XKB_OFFSET, false));
        assert_eq!(ascii_to_xkb('{').unwrap(), (26 + EVDEV_TO_XKB_OFFSET, true));
        assert_eq!(ascii_to_xkb(']').unwrap(), (27 + EVDEV_TO_XKB_OFFSET, false));
        assert_eq!(ascii_to_xkb('}').unwrap(), (27 + EVDEV_TO_XKB_OFFSET, true));
        assert_eq!(ascii_to_xkb('\\').unwrap(), (43 + EVDEV_TO_XKB_OFFSET, false));
        assert_eq!(ascii_to_xkb('|').unwrap(), (43 + EVDEV_TO_XKB_OFFSET, true));
    }

    #[test]
    fn quotes_and_remaining_shift_pairs() {
        assert_eq!(ascii_to_xkb('\'').unwrap(), (40 + EVDEV_TO_XKB_OFFSET, false));
        assert_eq!(ascii_to_xkb('"').unwrap(), (40 + EVDEV_TO_XKB_OFFSET, true));
        assert_eq!(ascii_to_xkb(':').unwrap(), (39 + EVDEV_TO_XKB_OFFSET, true));
        assert_eq!(ascii_to_xkb('?').unwrap(), (53 + EVDEV_TO_XKB_OFFSET, true));
    }

    #[test]
    fn key_name_allowlist_resolves() {
        let expect = [
            ("enter", 28),
            ("tab", 15),
            ("backspace", 14),
            ("escape", 1),
            ("delete", 111),
            ("space", 57),
            ("up", 103),
            ("down", 108),
            ("left", 105),
            ("right", 106),
            ("home", 102),
            ("end", 107),
            ("pageup", 104),
            ("pagedown", 109),
        ];
        for (name, evdev) in expect {
            assert_eq!(
                key_name_to_xkb(name),
                Some(evdev + EVDEV_TO_XKB_OFFSET),
                "key_name {name:?}"
            );
        }
    }

    #[test]
    fn key_name_unknown_is_none() {
        assert_eq!(key_name_to_xkb("f1"), None);
        assert_eq!(key_name_to_xkb(""), None);
        assert_eq!(key_name_to_xkb("Enter"), None); // case-sensitive, allowlist is lowercase
    }
}
