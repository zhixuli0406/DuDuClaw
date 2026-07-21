//! In-band sentinel framing protocol for CLI request / response.
//!
//! The PTY stream is a single byte sequence shared by the human-readable TUI render,
//! ANSI escapes, and the model's final answer. To extract the final answer reliably
//! without scrolling-back scraping (the brittle approach `maude` and friends take),
//! we ask the CLI — via system-prompt injection — to wrap its final response in a
//! magic sentinel that includes the request UUID:
//!
//! ```text
//! =====DUDUCLAW.RSP.550e8400-e29b-41d4-a716-446655440000.MARK=====
//! ...answer payload...
//! =====DUDUCLAW.RSP.550e8400-e29b-41d4-a716-446655440000.MARK=====
//! ```
//!
//! Sentinel design constraints:
//! - **High uniqueness**: a 36-char UUID makes accidental collision with model output
//!   astronomically unlikely.
//! - **Markdown-safe**: the original `<<<...>>>` sentinel was mangled by the
//!   Claude TUI's markdown renderer (which ate one `<` and one `>` per side,
//!   treating it as an HTML tag — verified in the 2026-05-14 spike). The new
//!   `=====...=====` sentinel survives the renderer intact.
//! - **Chunk-boundary tolerant**: parsers accumulate into a buffer and only commit
//!   once both sentinels are found.
//! - **UTF-8 safe**: all string slicing uses `char_indices` / `find`, never raw byte
//!   ranges that could split a multi-byte codepoint.
//! - **CRLF tolerant**: Windows ConPTY may inject CRLF — strip on parse.
//! - **ANSI-tolerant via [`strip_ansi`]**: callers strip CSI/OSC sequences before
//!   scanning so the sentinel's contiguous-byte structure is preserved (the TUI
//!   interleaves `ESC[1C` cursor-forward codes between every visible character).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const REQ_START: &str = "=====DUDUCLAW.REQ.";
pub const REQ_END: &str = ".MARK=====";
pub const RSP_START: &str = "=====DUDUCLAW.RSP.";
pub const RSP_END: &str = ".MARK=====";
pub const ERR_START: &str = "=====DUDUCLAW.ERR.";
pub const ERR_END: &str = ".MARK=====";

/// **Phase 3.C.2 (refined)**: fixed interactive-mode sentinel without UUID.
///
/// Empirical observation from the 2026-05-14 spike: the Claude TUI renders
/// the **opening** sentinel inline with the `⏺` assistant marker, and the
/// rendering machinery eats one character from the UUID (e.g. opens
/// `3c1991a-…` while closes correctly with `33c1991a-…`). UUID-based pair
/// matching fails because only one of the two sentinels survives intact.
///
/// We sidestep the problem entirely by removing the UUID from the sentinel
/// string in interactive mode. The session drains the rolling buffer
/// before each invoke so there's exactly one expected sentinel pair per
/// turn; positional pairing (first occurrence = open, second = close) is
/// reliable.
///
/// The non-interactive (echo-server) path still uses the UUID-bearing
/// sentinels above because it relies on `parse_frame` for unit tests
/// against synthetic streams.
pub const INTERACTIVE_SENTINEL: &str = "=====DUDUCLAW.MARK=====";

/// Format hint for the final response. Used to choose system-prompt instruction
/// language and downstream parsing strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ResponseFormat {
    #[default]
    Text,
    Json,
    StreamJson,
}

/// A request envelope. `req_id` MUST be unique per outstanding request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub req_id: Uuid,
    pub payload: String,
    #[serde(default)]
    pub format: ResponseFormat,
}

impl Envelope {
    pub fn new(payload: impl Into<String>) -> Self {
        Self {
            req_id: Uuid::new_v4(),
            payload: payload.into(),
            format: ResponseFormat::Text,
        }
    }

    pub fn with_format(mut self, format: ResponseFormat) -> Self {
        self.format = format;
        self
    }
}

/// Parsed frame extracted from a streamed buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// CLI returned its final answer wrapped in matching RSP sentinels.
    Response { req_id: Uuid, payload: String },
    /// CLI reported an error wrapped in matching ERR sentinels.
    Error { req_id: Uuid, message: String },
}

/// Build the wire bytes injected into the CLI's stdin (via PTY) for one request.
///
/// The shape:
///
/// 1. Reminder sentinel (helps the model see the request boundary in its own context).
/// 2. The actual user payload.
/// 3. A trailing instruction: "wrap your final answer in `=====DUDUCLAW.RSP.<id>.MARK=====`".
///
/// We DELIBERATELY put the wrapping instruction *after* the payload so it dominates
/// recency bias and survives long payloads.
pub fn frame_request(env: &Envelope) -> String {
    let id = env.req_id;
    let format_hint = match env.format {
        ResponseFormat::Text => "raw text (markdown allowed)",
        ResponseFormat::Json => "a single JSON object (no prose, no code fences)",
        ResponseFormat::StreamJson => {
            "newline-delimited JSON events (one event per line)"
        }
    };

    format!(
        "{REQ_START}{id}{REQ_END}\n\
         {payload}\n\
         \n\
         [DUDUCLAW protocol] Your final response MUST be wrapped exactly like:\n\
         {RSP_START}{id}{RSP_END}\n\
         <your final answer as {format_hint}>\n\
         {RSP_START}{id}{RSP_END}\n\
         If you encountered an unrecoverable error, instead emit:\n\
         {ERR_START}{id}{ERR_END}\n\
         <short error description>\n\
         {ERR_START}{id}{ERR_END}\n",
        payload = env.payload,
    )
}

/// Try to extract a complete frame from `buf`. On success, **advances `buf` past the
/// consumed bytes** so callers can keep accumulating partial data on the tail.
///
/// Returns `None` when no complete frame is available yet (buffer remains untouched
/// in that case — the caller must keep reading).
pub fn parse_frame(buf: &mut String) -> Option<Frame> {
    // Try RSP first (the happy path).
    if let Some(frame) = try_parse_pair(buf, RSP_START, RSP_END, false) {
        let (req_id, payload) = frame;
        return Some(Frame::Response { req_id, payload });
    }
    // Then ERR.
    if let Some(frame) = try_parse_pair(buf, ERR_START, ERR_END, true) {
        let (req_id, message) = frame;
        return Some(Frame::Error { req_id, message });
    }
    None
}

/// Returns `(req_id, payload)` between two matching sentinels.
///
/// `is_error` flips the parse intent so we can share code; semantically identical.
fn try_parse_pair(
    buf: &mut String,
    start_marker: &str,
    end_marker: &str,
    _is_error: bool,
) -> Option<(Uuid, String)> {
    // First sentinel
    let first_open = buf.find(start_marker)?;
    let after_first_open = first_open + start_marker.len();
    let first_close_rel = buf[after_first_open..].find(end_marker)?;
    let first_close = after_first_open + first_close_rel;

    // UUID lives between start_marker and end_marker on the first occurrence.
    let id_str = &buf[after_first_open..first_close];
    let req_id = Uuid::parse_str(id_str.trim()).ok()?;

    // Second sentinel — same id, somewhere after the first close marker.
    let after_first_full = first_close + end_marker.len();
    if after_first_full > buf.len() {
        return None;
    }
    let tail = &buf[after_first_full..];
    // Must find another `start_marker<same_uuid>end_marker` pair.
    let second_open_rel = tail.find(start_marker)?;
    let second_open = after_first_full + second_open_rel;
    let after_second_open = second_open + start_marker.len();
    let second_close_rel = buf[after_second_open..].find(end_marker)?;
    let second_close = after_second_open + second_close_rel;
    let second_id = &buf[after_second_open..second_close];
    if Uuid::parse_str(second_id.trim()).ok()? != req_id {
        return None;
    }
    let after_second_full = second_close + end_marker.len();

    // Payload is everything between the first close and the second open.
    let payload_raw = buf[after_first_full..second_open].to_string();
    let payload = normalize_payload(&payload_raw);

    // Drain consumed prefix.
    buf.drain(..after_second_full);

    Some((req_id, payload))
}

/// Strip leading/trailing whitespace and normalise CRLF → LF.
fn normalize_payload(s: &str) -> String {
    let stripped = s.trim_matches(|c: char| c == '\r' || c == '\n');
    stripped.replace("\r\n", "\n")
}

/// Strip ANSI escape sequences (CSI + OSC + single-char ESC) from `s`.
///
/// Necessary because the Claude TUI emits `\x1b[1C` (cursor-forward 1)
/// between every visible character of rendered text — without stripping,
/// the sentinel bytes are non-contiguous and `find` cannot locate them.
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

/// TUI chrome line markers that appear between sentinels when the model
/// finishes and the input box re-renders before the closing sentinel
/// streams in (see spike report 2026-05-14 §Q7).
///
/// A line is treated as chrome if any of these substrings appears OR if
/// the entire line is made of separator / spinner glyphs.
const CHROME_MARKERS: &[&str] = &[
    "esctointerrupt", // "esc to interrupt" with TUI-stripped spaces
    "MCPserverneedsauth",
    "MCPserverfailed",
    "Inferring",
    "Cooked for",
    "Recombobulating",
    "Thinking",
    "Pondering",
    "forshortcuts", // "? for shortcuts"
    "skilldescriptions",
    "tokenstosend",
    "tokensused",
];

/// Boot / welcome-screen chrome fingerprints (whitespace-insensitive, lowercased).
///
/// On a FRESH session's first turn, the full-screen redraw of the "Welcome back
/// …" box + "What's new" panel + release-notes / org / agent-path lines can land
/// BETWEEN the two answer sentinels, and the plain `CHROME_MARKERS` above don't
/// recognise it — so the box text was surfaced to the user as the "answer"
/// (production Bug2, 2026-07-21). These are matched against a whitespace-stripped
/// + lowercased line so both the spaced and cursor-positioned render forms hit.
/// Deliberately welcome-box-specific so a normal answer can't trip them.
const BOOT_CHROME_MARKERS: &[&str] = &[
    "welcomeback",       // "Welcome back Louis!"
    "what'snew",         // "What's new"
    "whatsnew",
    "tipsforgetting",    // "Tips for getting started"
    "run/inittocreate",  // "Run /init to create a ..."
    "release-notes",     // "/release-notes for more"
    "releasenotes",
    "claudecodev",       // box header "Claude Code v2.1.173"
    "improvedautomode",  // What's-new bullet
    "automodeon",        // footer "auto mode on (shift+tab to cycle)"
    "foragents",         // footer "← for agents"
];

/// Composer input-box status-line fingerprints matched by **whole-line
/// equality** on the whitespace-stripped, lowercased line.
///
/// These are the footer/status strings the TUI paints under the input box; on a
/// full-screen redraw they can stick to the tail of the answer between the
/// sentinels (production 2026-07-21: `ctrl+g to edit in Vim` appeared at the end
/// of a reply). Matched by EQUALITY, never substring — so an answer that merely
/// *mentions* one ("Ctrl+G to edit in Vim opens the editor", or a reply about
/// vim shortcuts) is not misclassified as chrome.
const COMPOSER_STATUS_EXACT: &[&str] = &[
    "ctrl+gtoeditinvim",  // "ctrl+g to edit in Vim"
    "←foragents",         // "← for agents" footer
    "?forshortcuts",      // "? for shortcuts"
    "shift+tabtocycle",   // "shift+tab to cycle" (when shown on its own line)
];

/// Composer status lines matched by **whole-line prefix** — reserved for
/// symbol-led footers a normal answer cannot begin with (so the prefix match is
/// still safe from misclassifying real answer text).
const COMPOSER_STATUS_PREFIX: &[&str] = &[
    "⏵⏵", // "⏵⏵ automode on (shift+tab to cycle) · ← for agents"
];

/// True for box-drawing (U+2500–U+257F) and block-element (U+2580–U+259F)
/// glyphs. Any of these on a line marks it as TUI frame chrome — the welcome
/// box, panels, and the logo (`▐▛███▜▌`) are all built from them, and a normal
/// chat answer never contains them. Fail-closed: dropping such a line can at
/// worst empty the payload, which routes to the empty-payload retry/fallback
/// rather than surfacing chrome as an answer.
fn is_box_or_block_glyph(c: char) -> bool {
    ('\u{2500}'..='\u{259F}').contains(&c)
}

/// True when a line is BOOT/WELCOME chrome specifically (as opposed to input-box
/// / status chrome). Only this kind is skipped when it *leads* the payload; see
/// [`extract_payload_with_chrome_filter`]. Box-drawing / block glyphs (welcome
/// box + logo, and the full-width `────` separators around it) and the welcome
/// keyword list qualify.
fn is_boot_welcome_chrome_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.chars().any(is_box_or_block_glyph) {
        return true;
    }
    let compact: String = trimmed
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    BOOT_CHROME_MARKERS.iter().any(|m| compact.contains(m))
}

/// Filter TUI chrome out of a sentinel-bounded payload.
///
/// Strategy:
/// 1. Walk the (already ANSI-stripped) payload line by line.
/// 2. For each line, truncate at the first **inline** chrome marker
///    (separator run, REPL prompt cursor, status string). This handles
///    the common case where the TUI renders the answer + chrome on a
///    single line because of cursor-positioning instead of newlines.
/// 3. If the whole resulting line is empty / chrome, drop it and stop.
/// 4. Otherwise keep it and continue to the next line until a line is
///    classified as chrome.
///
/// **Heuristic** — works for configurations observed in the 2026-05-14
/// spike and refinement runs. Add new markers to [`CHROME_MARKERS`] as
/// new TUI states emerge.
pub fn extract_payload_with_chrome_filter(stripped_between_sentinels: &str) -> String {
    let mut kept_lines: Vec<String> = Vec::new();
    // **Bug2 fix (2026-07-21)**: distinguish LEADING boot/welcome chrome (skip
    // it — the real answer may follow) from chrome AFTER the answer (stop — it's
    // the input-box redraw). Before this, the first chrome line always broke the
    // loop, so a first-turn welcome box that preceded the answer either wiped the
    // payload or, when the box itself sat between the sentinels, was surfaced as
    // the answer. Now leading chrome is dropped and collection continues.
    let mut seen_content = false;
    for line in stripped_between_sentinels.lines() {
        let truncated = truncate_at_inline_chrome(line);
        let trimmed = truncated.trim_end();
        // A line whose content was entirely truncated as inline chrome, OR that
        // classifies as a chrome line, is chrome.
        let is_chrome = (!line.trim().is_empty() && trimmed.trim().is_empty())
            || is_chrome_line(trimmed);
        if is_chrome {
            // Only BOOT/WELCOME chrome (box-drawing rows, "What's new", etc.) is
            // skipped when it LEADS — the real answer may follow it on the first
            // turn. Input-box / status chrome (❯ cursor, spinner, esc-to-interrupt,
            // separators) always stops collection, matching the pre-Bug2 contract
            // (a leading input-box line means there's no answer to keep).
            if !seen_content && is_boot_welcome_chrome_line(trimmed) {
                continue;
            }
            break;
        }
        if !trimmed.trim().is_empty() {
            seen_content = true;
        }
        kept_lines.push(trimmed.to_string());
    }
    // Trim leading + trailing empty lines.
    let start = kept_lines.iter().position(|l| !l.trim().is_empty()).unwrap_or(0);
    let end = kept_lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map(|i| i + 1)
        .unwrap_or(start);
    kept_lines[start..end].join("\n").trim().to_string()
}

/// Find the byte index of the earliest inline chrome marker on `line` and
/// return the slice before it. Returns the full line when no marker is
/// present.
///
/// Recognises:
/// - A run of 4+ consecutive separator chars (`─`, `━`, `═`, `-`) — TUI
///   input-box separator that follows the answer on the same line.
/// - The `❯` REPL prompt cursor.
/// - Any [`CHROME_MARKERS`] substring.
fn truncate_at_inline_chrome(line: &str) -> &str {
    let mut earliest: Option<usize> = None;

    // Box-drawing separator run (4+ consecutive box-draw or hyphen chars).
    {
        let bytes = line.as_bytes();
        let chars: Vec<(usize, char)> = line.char_indices().collect();
        let mut run_start: Option<usize> = None;
        let mut run_len = 0usize;
        for &(idx, ch) in &chars {
            if matches!(ch, '─' | '━' | '═' | '-') {
                if run_start.is_none() {
                    run_start = Some(idx);
                }
                run_len += 1;
                if run_len >= 4 {
                    if let Some(s) = run_start {
                        earliest = Some(earliest.map_or(s, |e| e.min(s)));
                    }
                    break;
                }
            } else {
                run_start = None;
                run_len = 0;
            }
        }
        let _ = bytes; // silence unused-warning on path the compiler can elide
    }

    // REPL cursor.
    if let Some(idx) = line.find('❯') {
        earliest = Some(earliest.map_or(idx, |e| e.min(idx)));
    }

    // Named markers.
    for marker in CHROME_MARKERS {
        if let Some(idx) = line.find(marker) {
            earliest = Some(earliest.map_or(idx, |e| e.min(idx)));
        }
    }

    match earliest {
        Some(idx) => &line[..idx],
        None => line,
    }
}

/// True if `line` is recognisably TUI chrome (separator, spinner, status,
/// known marker substring).
fn is_chrome_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false; // empty lines aren't chrome, just whitespace
    }

    // **Bug2 fix**: ANY box-drawing / block-element glyph ⇒ TUI frame chrome
    // (welcome box, "What's new" panel, logo). A normal answer never contains
    // these, so this cleanly drops the entire multi-line welcome box (every row
    // of which carries a `│`), including the org/email/agent-path rows inside it.
    if trimmed.chars().any(is_box_or_block_glyph) {
        return true;
    }

    // Separator line of box-drawing horizontals or hyphens.
    if trimmed.chars().all(|c| matches!(c, '─' | '━' | '-' | '=' | '·')) {
        return true;
    }

    // Boot / welcome-screen fingerprints (whitespace-insensitive, lowercased).
    let compact: String = trimmed
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if BOOT_CHROME_MARKERS.iter().any(|m| compact.contains(m)) {
        return true;
    }

    // Composer input-box status lines (e.g. "ctrl+g to edit in Vim",
    // "⏵⏵ automode on (shift+tab to cycle) · ← for agents"). WHOLE-LINE match
    // only (equality, or a symbol-led prefix) so an answer that merely mentions
    // one of these shortcuts is never misclassified as chrome.
    if COMPOSER_STATUS_EXACT.contains(&compact.as_str())
        || COMPOSER_STATUS_PREFIX.iter().any(|p| compact.starts_with(p))
    {
        return true;
    }

    // Pure spinner / progress glyphs.
    if trimmed.chars().all(|c| {
        matches!(
            c,
            '✶' | '✳' | '✢' | '✻' | '✽' | '✺' | '✷' | '⠋' | '⠙' | '⠹' | '⠸'
                | '⠼' | '⠴' | '⠦' | '⠧' | '⠇' | '⠏' | '⏺' | '✔' | '✓' | '·'
        ) || c.is_whitespace()
    }) {
        return true;
    }

    // REPL prompt cursor line (`❯` followed by input box content).
    if trimmed.starts_with('❯') {
        return true;
    }

    // Status / hint markers.
    for marker in CHROME_MARKERS {
        if trimmed.contains(marker) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_pair(start: &str, end: &str, id: Uuid, body: &str) -> String {
        format!("{start}{id}{end}\n{body}\n{start}{id}{end}\n")
    }

    #[test]
    fn frame_request_contains_magic_bytes() {
        let env = Envelope::new("hello world");
        let wire = frame_request(&env);
        assert!(wire.contains(REQ_START));
        assert!(wire.contains(REQ_END));
        assert!(wire.contains(RSP_START));
        assert!(wire.contains(RSP_END));
        assert!(wire.contains(&env.req_id.to_string()));
        assert!(wire.contains("hello world"));
    }

    #[test]
    fn parse_response_happy_path() {
        let id = Uuid::new_v4();
        let mut buf = format!(
            "some preamble\n{wire}trailing junk",
            wire = frame_pair(RSP_START, RSP_END, id, "the answer")
        );
        let frame = parse_frame(&mut buf).expect("expected frame");
        assert_eq!(
            frame,
            Frame::Response {
                req_id: id,
                payload: "the answer".to_string()
            }
        );
        // Trailing data should remain after parse so subsequent invocations can
        // continue from the next request boundary.
        assert!(buf.contains("trailing junk"));
        // Preamble + frame should have been drained.
        assert!(!buf.contains("preamble"));
    }

    #[test]
    fn parse_returns_none_when_only_one_sentinel_present() {
        let id = Uuid::new_v4();
        let mut buf = format!("{RSP_START}{id}{RSP_END}\nhalf the answer");
        assert!(parse_frame(&mut buf).is_none());
        // Must not have drained any bytes — caller will keep reading.
        assert!(buf.contains(&id.to_string()));
    }

    #[test]
    fn parse_handles_crlf_payload() {
        let id = Uuid::new_v4();
        let mut buf = format!(
            "{RSP_START}{id}{RSP_END}\r\nline1\r\nline2\r\n{RSP_START}{id}{RSP_END}\r\n"
        );
        let frame = parse_frame(&mut buf).expect("expected frame");
        let Frame::Response { payload, .. } = frame else {
            panic!("expected response");
        };
        assert_eq!(payload, "line1\nline2");
    }

    #[test]
    fn parse_handles_cjk_payload_safely() {
        let id = Uuid::new_v4();
        let mut buf = format!(
            "{wire}",
            wire = frame_pair(RSP_START, RSP_END, id, "你好世界 emoji=🐾")
        );
        let frame = parse_frame(&mut buf).expect("expected frame");
        let Frame::Response { payload, .. } = frame else {
            panic!("expected response");
        };
        assert_eq!(payload, "你好世界 emoji=🐾");
    }

    #[test]
    fn parse_rejects_id_mismatch_between_sentinels() {
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let mut buf = format!(
            "{RSP_START}{id1}{RSP_END}\npayload\n{RSP_START}{id2}{RSP_END}\n"
        );
        // The parser should NOT match because the closing UUID differs.
        let result = parse_frame(&mut buf);
        assert!(result.is_none(), "must not pair mismatched IDs, got {result:?}");
    }

    #[test]
    fn parse_error_frame() {
        let id = Uuid::new_v4();
        let mut buf = format!(
            "{wire}",
            wire = frame_pair(ERR_START, ERR_END, id, "boom")
        );
        let frame = parse_frame(&mut buf).expect("expected frame");
        assert_eq!(
            frame,
            Frame::Error {
                req_id: id,
                message: "boom".to_string()
            }
        );
    }

    #[test]
    fn parse_handles_chunked_buffer_growth() {
        let id = Uuid::new_v4();
        let full = frame_pair(RSP_START, RSP_END, id, "chunked answer");
        let mut buf = String::new();
        // Feed one character at a time. Parser must keep returning None until the
        // full frame is buffered.
        let mut emitted: Option<Frame> = None;
        for ch in full.chars() {
            buf.push(ch);
            if let Some(f) = parse_frame(&mut buf) {
                emitted = Some(f);
                break;
            }
        }
        let frame = emitted.expect("frame must eventually parse");
        assert_eq!(
            frame,
            Frame::Response {
                req_id: id,
                payload: "chunked answer".to_string()
            }
        );
    }

    #[test]
    fn parse_with_invalid_uuid_returns_none() {
        let mut buf = format!("{RSP_START}not-a-uuid{RSP_END}\nbody\n{RSP_START}not-a-uuid{RSP_END}\n");
        assert!(parse_frame(&mut buf).is_none());
    }

    #[test]
    fn parse_consumes_only_one_frame_per_call() {
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let wire1 = frame_pair(RSP_START, RSP_END, id1, "first");
        let wire2 = frame_pair(RSP_START, RSP_END, id2, "second");
        let mut buf = format!("{wire1}{wire2}");

        let f1 = parse_frame(&mut buf).expect("first frame");
        assert_eq!(
            f1,
            Frame::Response {
                req_id: id1,
                payload: "first".to_string()
            }
        );
        let f2 = parse_frame(&mut buf).expect("second frame");
        assert_eq!(
            f2,
            Frame::Response {
                req_id: id2,
                payload: "second".to_string()
            }
        );
        // After both frames are consumed, only trailing whitespace from the test
        // fixture remains (a single newline between the closing sentinel and EOF).
        assert!(buf.trim().is_empty(), "expected only whitespace, got: {buf:?}");
    }

    // ── Phase 3.C.2: strip_ansi tests ─────────────────────────────────

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

    // ── Phase 3.C.2: chrome filter tests ──────────────────────────────

    #[test]
    fn chrome_filter_keeps_plain_answer() {
        assert_eq!(
            extract_payload_with_chrome_filter("Hello, world!"),
            "Hello, world!"
        );
    }

    #[test]
    fn chrome_filter_stops_at_repl_prompt() {
        let payload = "Real answer line\n❯  prompt cursor\nshould not appear";
        assert_eq!(extract_payload_with_chrome_filter(payload), "Real answer line");
    }

    #[test]
    fn chrome_filter_stops_at_separator() {
        let payload = "Real answer\n────────────────────────\nesctointerrupt";
        assert_eq!(extract_payload_with_chrome_filter(payload), "Real answer");
    }

    #[test]
    fn chrome_filter_stops_at_inferring_marker() {
        let payload = "Real answer\nInferring… (4s · ↓ 255 tokens)";
        assert_eq!(extract_payload_with_chrome_filter(payload), "Real answer");
    }

    #[test]
    fn chrome_filter_stops_at_mcp_marker() {
        let payload = "Real answer\nesctointerrupt 1MCPserverneedsauth · /mcp";
        assert_eq!(extract_payload_with_chrome_filter(payload), "Real answer");
    }

    #[test]
    fn chrome_filter_stops_at_pure_spinner_line() {
        let payload = "Real answer\n✶ ✳ ✢";
        assert_eq!(extract_payload_with_chrome_filter(payload), "Real answer");
    }

    #[test]
    fn chrome_filter_trims_surrounding_blank_lines() {
        let payload = "\n\n\nReal answer here\n\n";
        assert_eq!(
            extract_payload_with_chrome_filter(payload),
            "Real answer here"
        );
    }

    #[test]
    fn chrome_filter_preserves_multi_line_answer_until_chrome() {
        let payload = "Line one of answer\nLine two of answer\n❯";
        assert_eq!(
            extract_payload_with_chrome_filter(payload),
            "Line one of answer\nLine two of answer"
        );
    }

    #[test]
    fn chrome_filter_returns_empty_when_first_line_is_chrome() {
        let payload = "❯ prompt\nstuff after";
        assert_eq!(extract_payload_with_chrome_filter(payload), "");
    }

    #[test]
    fn chrome_filter_truncates_inline_separator_after_answer() {
        // Real spike output: model answer + long ── separator on same line
        // because TUI uses cursor positioning instead of newlines.
        let payload = "嗨，很高興見到你！────────────────────────────────────────❯ ";
        assert_eq!(
            extract_payload_with_chrome_filter(payload),
            "嗨，很高興見到你！"
        );
    }

    #[test]
    fn chrome_filter_truncates_inline_prompt_cursor() {
        let payload = "Answer here ❯ user input box";
        assert_eq!(extract_payload_with_chrome_filter(payload), "Answer here");
    }

    #[test]
    fn chrome_filter_truncates_inline_named_marker() {
        let payload = "Real answer Inferring… 4s · ↓ 255 tokens";
        assert_eq!(extract_payload_with_chrome_filter(payload), "Real answer");
    }

    // ── Bug2 (2026-07-21): boot/welcome chrome must never surface as answer ──

    /// Real welcome box captured from claude 2.1.173 via python PTY (2026-07-21).
    /// Every row carries box-drawing/block glyphs; this is what leaked to the
    /// user as a 217-token "answer" on a fresh session's first turn.
    const WELCOME_FIXTURE: &str = "\
╭───ClaudeCodev2.1.173─────────────────────────────────────────────────────╮
││Tipsforgetting│
│WelcomebackLouis!│started│
││Run/inittocreatea…│
│▐▛███▜▌│───────────────────────│
│▝▜█████▛▘│What'snew│
│▘▘▝▝│Improvedautomodesaf…│
│Haiku4.5·ClaudeMax·a6693432@gmail.com's│Addedawarningwhent…│
│Organization│Added`attribution.ses…│
│/…/scratchpad/bugdir│/release-notesformore│
╰──────────────────────────────────────────────────────────────────────────────╯";

    #[test]
    fn welcome_box_alone_yields_empty_payload_fail_closed() {
        // The whole welcome box is chrome → empty payload → routes to the
        // empty-payload retry/fallback rather than being surfaced as the answer.
        assert_eq!(extract_payload_with_chrome_filter(WELCOME_FIXTURE), "");
    }

    #[test]
    fn welcome_box_before_answer_is_skipped_answer_kept() {
        // First-turn full-screen redraw: welcome box precedes the real answer.
        // Leading welcome chrome is dropped; the answer is preserved.
        let payload = format!("{WELCOME_FIXTURE}\n你好，我可以幫你深度搜尋 AGI 論文。");
        assert_eq!(
            extract_payload_with_chrome_filter(&payload),
            "你好，我可以幫你深度搜尋 AGI 論文。"
        );
    }

    #[test]
    fn welcome_box_after_answer_stops_collection() {
        // Answer first, then a welcome/panel redraw → stop at the box.
        let payload = format!("The answer is 42.\n{WELCOME_FIXTURE}");
        assert_eq!(extract_payload_with_chrome_filter(&payload), "The answer is 42.");
    }

    #[test]
    fn composer_status_lines_are_chrome_whole_line_only() {
        // Real composer footers (claude 2.1.173). The `ctrl+g to edit in Vim`
        // one leaked to a user at the end of a reply (production 2026-07-21).
        assert!(is_chrome_line("ctrl+g to edit in Vim"));
        assert!(is_chrome_line("⏵⏵ automode on (shift+tab to cycle) · ← for agents"));
        assert!(is_chrome_line("← for agents"));
        assert!(is_chrome_line("? for shortcuts"));
        assert!(is_chrome_line("shift+tab to cycle"));
        // MUST NOT misclassify an answer that MENTIONS a shortcut (whole-line
        // equality / symbol-prefix only, never substring).
        assert!(!is_chrome_line(
            "Ctrl+G to edit in Vim opens the editor and drops you into insert mode."
        ));
        assert!(!is_chrome_line("你可以用 Ctrl+G 在 Vim 裡編輯這個檔案。"));
        assert!(!is_chrome_line("The shortcut you want is shift+tab to cycle modes."));
    }

    #[test]
    fn answer_with_trailing_composer_status_is_trimmed() {
        // The exact production shape: answer text then the input-box footer.
        let payload = "AGI 指通用人工智慧。\nctrl+g to edit in Vim";
        assert_eq!(
            extract_payload_with_chrome_filter(payload),
            "AGI 指通用人工智慧。"
        );
        let payload2 = "Here is your answer.\n⏵⏵ automode on (shift+tab to cycle) · ← for agents";
        assert_eq!(
            extract_payload_with_chrome_filter(payload2),
            "Here is your answer."
        );
    }

    #[test]
    fn box_and_block_glyphs_flag_chrome() {
        assert!(is_chrome_line("│ Welcome back │"));
        assert!(is_chrome_line("▐▛███▜▌"));
        assert!(is_chrome_line("╰────────╯"));
        assert!(is_chrome_line("What's new"));
        assert!(is_chrome_line("/release-notes for more"));
        // A normal answer with an em-dash or hyphen is NOT box chrome.
        assert!(!is_chrome_line("The result — surprisingly — was 42."));
        assert!(!is_chrome_line("Use foo-bar-baz style names."));
    }

    #[test]
    fn chrome_filter_short_separator_run_is_not_chrome() {
        // 3 dashes on their own should not trigger separator detection
        // (we set the threshold at 4 to avoid eating things like "—".)
        let payload = "Answer with --- in it";
        assert_eq!(
            extract_payload_with_chrome_filter(payload),
            "Answer with --- in it"
        );
    }

    // ── Phase 3.C.2: new sentinel format roundtrip ────────────────────

    #[test]
    fn new_sentinel_format_uses_equals_delimiter() {
        // Verify the constants flipped to `=====...=====` form.
        assert!(RSP_START.starts_with("====="), "RSP_START={RSP_START}");
        assert!(RSP_END.ends_with("====="), "RSP_END={RSP_END}");
        // Critically: no `<` or `>` chars (TUI markdown renderer eats them).
        for delim in [REQ_START, REQ_END, RSP_START, RSP_END, ERR_START, ERR_END] {
            assert!(
                !delim.contains('<') && !delim.contains('>'),
                "delimiter contains TUI-hostile angle bracket: {delim}"
            );
        }
    }

    #[test]
    fn parse_frame_then_strip_ansi_then_extract_end_to_end() {
        // Simulate a realistic TUI emission: response sentinel pair with
        // ANSI cursor codes between every character + chrome between.
        let id = Uuid::new_v4();
        let raw = format!(
            "preamble {start}{id}{end}\n\
             \x1b[1Ch\x1b[1Ci\n\
             ❯ prompt\n\
             {start}{id}{end}\n",
            start = RSP_START,
            end = RSP_END,
        );

        let stripped = strip_ansi(&raw);
        let mut buf = stripped.clone();
        let frame = parse_frame(&mut buf).expect("parse_frame should match");
        let Frame::Response { req_id, payload } = frame else {
            panic!("expected Response");
        };
        assert_eq!(req_id, id);
        let cleaned = extract_payload_with_chrome_filter(&payload);
        assert_eq!(cleaned, "hi");
    }
}
