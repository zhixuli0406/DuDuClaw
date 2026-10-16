//! Terminal-safe text: what reaches a technician's terminal must not be able
//! to move the cursor, rewrite earlier lines, change the window title or
//! reorder text.

/// Whether `c` must not reach a terminal verbatim: C0/C1 control characters
/// (this includes ESC, CR, LF, TAB and DEL) and the Unicode bidirectional
/// formatting characters that can visually reorder a line.
pub fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}' // ARABIC LETTER MARK
                | '\u{200E}' // LEFT-TO-RIGHT MARK
                | '\u{200F}' // RIGHT-TO-LEFT MARK
                | '\u{202A}'..='\u{202E}' // LRE, RLE, PDF, LRO, RLO
                | '\u{2066}'..='\u{2069}' // LRI, RLI, FSI, PDI
        )
}

/// Replace every unsafe character with a space.
///
/// A space rather than deletion: deleting would glue the words on either side
/// together, which can turn two harmless words into one token-shaped word (or
/// hide a token boundary the redactor relies on).
pub fn replace_unsafe(s: &str) -> String {
    s.chars()
        .map(|c| if is_unsafe_char(c) { ' ' } else { c })
        .collect()
}

/// Byte-level filter for text the shell relays from the log socket: keeps
/// `\n`, drops every other ASCII control byte and DEL. Bytes ≥ 0x80 pass
/// through untouched (UTF-8 continuation bytes never fall in the control
/// range, so a multi-byte character can never be split by this). The log
/// service already sanitises each line; this is the shell's own second layer.
pub fn filter_relay_bytes(input: &[u8], out: &mut Vec<u8>) {
    out.extend(
        input
            .iter()
            .copied()
            .filter(|&b| b == b'\n' || (b >= 0x20 && b != 0x7f)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_and_line_breaks_become_spaces() {
        assert_eq!(
            replace_unsafe("a\x1b]0;pwned\x07b\r\nc\td\u{7f}"),
            "a ]0;pwned b  c d "
        );
    }

    #[test]
    fn bidi_overrides_become_spaces() {
        assert_eq!(replace_unsafe("x\u{202E}gnp.exe"), "x gnp.exe");
        assert_eq!(replace_unsafe("\u{2066}a\u{2069}"), " a ");
    }

    #[test]
    fn c1_controls_become_spaces() {
        assert_eq!(replace_unsafe("a\u{9b}31mb"), "a 31mb");
    }

    #[test]
    fn cjk_and_emoji_survive() {
        let s = "閘道已啟動 🔑 ok";
        assert_eq!(replace_unsafe(s), s);
    }

    #[test]
    fn relay_filter_keeps_newlines_and_utf8() {
        let mut out = Vec::new();
        filter_relay_bytes("紀錄\x1b[2J\r\nnext\x07\n".as_bytes(), &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "紀錄[2J\nnext\n");
    }
}
