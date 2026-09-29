//! ANSI escape-sequence stripping for PTY-captured CLI output.
//!
//! A PTY stream interleaves the CLI's human-readable TUI render (cursor moves,
//! colour changes, window-title sequences) with the text a caller actually
//! wants. [`strip_ansi`] removes the escape sequences so the remaining bytes
//! are plain text.
//!
//! History: this module used to also carry an in-band sentinel framing
//! protocol (`Envelope` / `frame_request` / `parse_frame` /
//! `extract_payload_with_chrome_filter`) used by the long-lived PTY session
//! pool. The pool was removed in 2026-09 (see `docs/features/27`), and with it
//! the sentinel protocol; only the ANSI stripper has callers left.

/// Strip ANSI escape sequences (CSI + OSC + single-char ESC) from `s`.
///
/// Necessary because the Claude TUI positions text with cursor-move escapes
/// (`ESC[<n>C` cursor-forward, `ESC[<n>G` cursor-horizontal-absolute) rather
/// than literal spaces — without stripping, the sentinel bytes are
/// non-contiguous and `find` cannot locate them.
///
/// # Known lossy behaviour: horizontal spacing (WP11-B, 2026-08-04)
///
/// Because those cursor moves are *dropped* rather than translated into
/// spaces, any TUI text whose word gaps were painted as cursor moves comes
/// out glued together (`"esc to interrupt"` → `"esctointerrupt"` — which is
/// exactly why [`CHROME_MARKERS`] below is written in the space-less form).
/// A live capture of `claude` 2.1.220 confirms both forms occur: the first
/// full paint of a line uses literal spaces, later diff-repaints use cursor
/// moves. This is the root cause of the "ASCII spaces vanished" half of the
/// 2026-08-04 field report — the affected text was TUI chrome, not model
/// output.
///
/// The behaviour is deliberately **left as-is**: every chrome heuristic in
/// this module (and the sentinel scan itself) was tuned against the
/// space-less form, so translating cursor moves into spaces here would break
/// detection for a cosmetic gain on text that should never reach a user in the
/// first place. The leak itself is fixed downstream, at the shared reply
/// assembly point, by `duduclaw_gateway::cli_noise` — which matches
/// whitespace-insensitively and therefore catches both render forms.
///
/// Handled sequence forms:
/// - **CSI**: `ESC [ ... <final byte 0x40-0x7E>` — covers cursor moves,
///   colour changes, mode toggles, etc.
/// - **OSC**: `ESC ] ... BEL` or `ESC ] ... ST` (`ST` = `ESC \`) — used
///   for window titles, hyperlinks, etc.
/// - **Single-char escape**: `ESC <byte>` — keypad mode (`ESC =`),
///   character-set switches (`ESC ( B`), etc.
///
/// UTF-8 safe: text bytes are appended one full codepoint at a time.
///
/// Crate-tested against real `claude` v2.1.138 TUI output in
/// `examples/claude_interactive_spike.rs`; survives 84+ ANSI sequences
/// per 1.3 KB banner.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            match next {
                b'[' => {
                    // CSI: skip until final byte 0x40-0x7E (inclusive).
                    let mut j = i + 2;
                    while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                        j += 1;
                    }
                    i = j.saturating_add(1);
                }
                b']' => {
                    // OSC: skip until BEL (0x07) or ST (ESC \).
                    let mut j = i + 2;
                    while j < bytes.len() {
                        if bytes[j] == 0x07 {
                            j += 1;
                            break;
                        }
                        if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                            j += 2;
                            break;
                        }
                        j += 1;
                    }
                    i = j;
                }
                _ => {
                    // Single-char escape (e.g. ESC =, ESC c, ESC (B).
                    i += 2;
                }
            }
        } else if let Some(ch) = s[i..].chars().next() {
            // Push the full UTF-8 codepoint. `i` is a valid char boundary here
            // because the only `i` advancements are escape skips (ASCII-only
            // bytes) and prior codepoint widths.
            let len = ch.len_utf8();
            out.push(ch);
            i += len;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **WP11-B evidence pin (2026-08-04).** Byte-for-byte excerpt of a live
    /// `claude` 2.1.220 PTY capture taken on this machine. It documents where
    /// the customer's missing ASCII spaces actually go: the notice's *fresh*
    /// paint carries literal spaces (they survive `strip_ansi` untouched), but
    /// the surrounding column positioning is expressed as `ESC[<n>C` /
    /// `ESC[<n>G`, which `strip_ansi` drops — so a diff-repaint of the same
    /// line arrives glued together. Downstream mitigation lives in
    /// `duduclaw_gateway::cli_noise`, which matches both forms.
    #[test]
    fn strip_ansi_documents_where_horizontal_spacing_is_lost() {
        // Fresh paint: literal spaces present ⇒ preserved.
        let fresh = "\r\x1b[2C\x1b[1B\x1b[38;5;220m⚠ Transcript saving is off — inherited \
                     CLAUDE_CODE_CHILD_SESSION marker\x1b[38;5;246m · restart";
        let out = strip_ansi(fresh);
        assert!(out.contains("Transcript saving is off"), "got {out:?}");

        // Column positioning instead of spaces ⇒ words glue together. This is
        // the render form the customer saw.
        let repaint = "\x1b[38;5;220m⚠\x1b[4G1 MCP server\x1b[1Cneeds\x1b[1Cauthentication";
        let out = strip_ansi(repaint);
        assert_eq!(out, "⚠1 MCP serverneedsauthentication");
        assert!(!out.contains("server needs"), "cursor moves are dropped, not spaced");
    }

    #[test]
    fn strip_ansi_removes_csi_cursor_forward() {
        // Real TUI output pattern: each visible char preceded by `ESC[1C`.
        let input = "\x1b[1CH\x1b[1Ce\x1b[1Cl\x1b[1Cl\x1b[1Co";
        assert_eq!(strip_ansi(input), "Hello");
    }

    #[test]
    fn strip_ansi_removes_csi_colour_sgr() {
        // SGR = Select Graphic Rendition (colours, bold etc.).
        let input = "\x1b[31mred\x1b[0m\x1b[1;32mgreen\x1b[m end";
        assert_eq!(strip_ansi(input), "redgreen end");
    }

    #[test]
    fn strip_ansi_removes_osc_with_bel() {
        // OSC sequence for setting window title, terminated with BEL.
        let input = "\x1b]0;Window Title\x07keep";
        assert_eq!(strip_ansi(input), "keep");
    }

    #[test]
    fn strip_ansi_removes_osc_with_st() {
        // OSC terminated with ST (= ESC \).
        let input = "\x1b]8;;https://example.com\x1b\\link text\x1b]8;;\x1b\\done";
        assert_eq!(strip_ansi(input), "link textdone");
    }

    #[test]
    fn strip_ansi_handles_single_char_escape() {
        // ESC = (keypad app mode) — two-byte sequence.
        let input = "\x1b=hello\x1b>world";
        assert_eq!(strip_ansi(input), "helloworld");
    }

    #[test]
    fn strip_ansi_preserves_cjk_codepoints() {
        let input = "\x1b[1m你好\x1b[m世界 🐾";
        assert_eq!(strip_ansi(input), "你好世界 🐾");
    }

    #[test]
    fn strip_ansi_handles_lone_esc_at_eof() {
        // Truncated stream — final ESC has no following byte.
        let input = "complete\x1b";
        // We should not panic; the lone ESC is preserved as-is or dropped.
        let out = strip_ansi(input);
        assert!(out.contains("complete"), "got: {out:?}");
    }

    #[test]
    fn strip_ansi_passes_plain_text_unchanged() {
        let input = "no escapes here\nline 2\nline 3\n";
        assert_eq!(strip_ansi(input), input);
    }
}
