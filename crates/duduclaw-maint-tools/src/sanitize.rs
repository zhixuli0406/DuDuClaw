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

/// Streaming filter for text the shell passes to the technician's terminal
/// (log-service output and the output of `systemctl`, `ip` & co., part of
/// which comes from uid 1000 — e.g. a unit's `Status:` text set through
/// `sd_notify`).
///
/// Bytes are decoded as UTF-8 across chunk boundaries (an incomplete
/// sequence at the end of a chunk is carried to the next one, at most three
/// bytes), so a control character can never slip through by being split
/// between two reads. `\n` is kept; every other [`is_unsafe_char`] — C0, DEL,
/// the UTF-8 encoded C1 range U+0080–U+009F (e.g. `C2 9B`, a one-character
/// CSI), and bidi formatting characters — becomes a space. Invalid UTF-8
/// becomes U+FFFD.
#[derive(Debug, Default)]
pub struct TerminalFilter {
    carry: Vec<u8>,
}

impl TerminalFilter {
    /// Filter one chunk, appending the safe bytes to `out`.
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) {
        self.carry.extend_from_slice(input);
        let consumed = {
            let mut rest: &[u8] = &self.carry;
            let mut consumed = 0;
            loop {
                match std::str::from_utf8(rest) {
                    Ok(text) => {
                        push_terminal_safe(text, out);
                        consumed += rest.len();
                        break;
                    }
                    Err(e) => {
                        let (valid, after) = rest.split_at(e.valid_up_to());
                        // `valid` is exactly the prefix from_utf8 accepted.
                        push_terminal_safe(std::str::from_utf8(valid).unwrap_or_default(), out);
                        consumed += valid.len();
                        match e.error_len() {
                            Some(n) => {
                                out.extend_from_slice("\u{FFFD}".as_bytes());
                                consumed += n;
                                rest = &after[n..];
                            }
                            // Incomplete sequence at the end: wait for more.
                            None => break,
                        }
                    }
                }
            }
            consumed
        };
        self.carry.drain(..consumed);
    }

    /// End of input: a dangling partial sequence becomes U+FFFD.
    pub fn finish(&mut self, out: &mut Vec<u8>) {
        if !self.carry.is_empty() {
            out.extend_from_slice("\u{FFFD}".as_bytes());
            self.carry.clear();
        }
    }
}

fn push_terminal_safe(text: &str, out: &mut Vec<u8>) {
    let mut tmp = [0u8; 4];
    for c in text.chars() {
        let c = if c == '\n' || !is_unsafe_char(c) {
            c
        } else {
            ' '
        };
        out.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
    }
}

/// One-shot form of [`TerminalFilter`] for a complete buffer.
pub fn filter_terminal_bytes(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut f = TerminalFilter::default();
    f.feed(input, &mut out);
    f.finish(&mut out);
    out
}

/// How [`pump_filtered`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpEnd {
    /// The source reached end of input.
    Eof,
    /// `max_bytes` were read; the rest was not.
    Capped,
}

/// Copy `input` to `out` through a [`TerminalFilter`], flushing after each
/// chunk (so long-running output appears as it comes), reading at most
/// `max_bytes`. `EINTR` (Ctrl-C reaching the menu's handler) is retried.
pub fn pump_filtered<R: std::io::Read + ?Sized, W: std::io::Write + ?Sized>(
    input: &mut R,
    out: &mut W,
    max_bytes: usize,
) -> std::io::Result<PumpEnd> {
    let mut buf = [0u8; 8192];
    let mut filtered = Vec::with_capacity(buf.len());
    let mut filter = TerminalFilter::default();
    let mut total = 0usize;
    loop {
        let want = buf.len().min(max_bytes - total);
        if want == 0 {
            filtered.clear();
            filter.finish(&mut filtered);
            out.write_all(&filtered)?;
            out.flush()?;
            return Ok(PumpEnd::Capped);
        }
        match input.read(&mut buf[..want]) {
            Ok(0) => {
                filtered.clear();
                filter.finish(&mut filtered);
                out.write_all(&filtered)?;
                out.flush()?;
                return Ok(PumpEnd::Eof);
            }
            Ok(n) => {
                total += n;
                filtered.clear();
                filter.feed(&buf[..n], &mut filtered);
                out.write_all(&filtered)?;
                out.flush()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
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
        for c in '\u{80}'..='\u{9f}' {
            assert!(is_unsafe_char(c), "{c:?}");
        }
    }

    #[test]
    fn cjk_and_emoji_survive() {
        let s = "閘道已啟動 🔑 ok";
        assert_eq!(replace_unsafe(s), s);
    }

    #[test]
    fn terminal_filter_keeps_newlines_and_utf8() {
        let out = filter_terminal_bytes("紀錄\x1b[2J\r\nnext\x07\n".as_bytes());
        assert_eq!(String::from_utf8(out).unwrap(), "紀錄 [2J \nnext \n");
    }

    #[test]
    fn terminal_filter_strips_utf8_encoded_c1_controls() {
        // C2 9B is U+009B (CSI): one character that starts an escape
        // sequence on terminals that honour 8-bit controls. C2 85 is NEL,
        // C2 80 and C2 9F the ends of the range.
        let input = b"a\xc2\x9b31mb\xc2\x85c\xc2\x80\xc2\x9fd\xc2\xa0e";
        let out = String::from_utf8(filter_terminal_bytes(input)).unwrap();
        assert_eq!(
            out, "a 31mb c  d\u{a0}e",
            "U+00A0 is not a control and stays"
        );
        assert!(!out.chars().any(|c| ('\u{80}'..='\u{9f}').contains(&c)));
    }

    #[test]
    fn terminal_filter_handles_sequences_split_across_chunks() {
        let mut f = TerminalFilter::default();
        let mut out = Vec::new();
        // C1 CSI split between two reads, then a CJK char split three ways,
        // then an RLO (E2 80 AE) split.
        for chunk in [
            &b"x\xc2"[..],
            b"\x9by\xe7",
            b"\xb4",
            b"\x80\xe2\x80",
            b"\xaez",
        ] {
            f.feed(chunk, &mut out);
        }
        f.finish(&mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "x y紀 z");
    }

    #[test]
    fn terminal_filter_replaces_invalid_and_dangling_bytes() {
        assert_eq!(
            String::from_utf8(filter_terminal_bytes(b"a\xffb\xc2")).unwrap(),
            "a\u{FFFD}b\u{FFFD}"
        );
    }

    #[test]
    fn pump_filters_and_caps() {
        let mut out = Vec::new();
        let end = pump_filtered(
            &mut std::io::Cursor::new(b"Status: \"\x1b]0;x\x07ok\xc2\x9b\"\n".to_vec()),
            &mut out,
            1 << 20,
        )
        .unwrap();
        assert_eq!(end, PumpEnd::Eof);
        assert_eq!(String::from_utf8(out).unwrap(), "Status: \" ]0;x ok \"\n");

        let mut out = Vec::new();
        let end = pump_filtered(
            &mut std::io::Cursor::new(vec![b'a'; 100_000]),
            &mut out,
            10_000,
        )
        .unwrap();
        assert_eq!(end, PumpEnd::Capped);
        assert_eq!(out.len(), 10_000);
    }

    #[test]
    fn terminal_filter_carry_stays_small() {
        let mut f = TerminalFilter::default();
        let mut out = Vec::new();
        f.feed(b"\xf0\x9f\x90", &mut out);
        assert!(out.is_empty());
        assert_eq!(f.carry.len(), 3);
        f.feed(b"\xbe", &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "\u{1F43E}");
        assert!(f.carry.is_empty());
    }
}
