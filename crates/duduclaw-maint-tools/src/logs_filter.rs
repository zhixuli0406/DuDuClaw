//! journald JSON records → bounded, redacted, terminal-safe text lines.
//!
//! This is the part of `duduclaw-maint-helper` that decides what the
//! maintenance account may see. It runs on journald's own `MESSAGE` field
//! (`journalctl -o json`), never on a pre-formatted line, so a message cannot
//! smuggle a fake timestamp or unit name into the comparison.
//!
//! Per record, in order:
//! 1. parse the JSON object; anything else is counted as malformed;
//! 2. if any `MESSAGE` value, or any line inside one, starts with
//!    `password:` after leading whitespace/invisible characters (ASCII
//!    case-insensitive), the whole record is withheld —
//!    that is the gateway's first-run bootstrap password line
//!    (`     Password: <value>`);
//! 3. control and bidi characters become spaces;
//! 4. [`duduclaw_core::secret_redact::redact_secrets`];
//! 5. cut to [`MAX_MESSAGE_BYTES`] on a character boundary;
//! 6. prefix the record's own `__REALTIME_TIMESTAMP`, formatted here.
//!
//! Redaction runs before the cut on purpose: cutting first could leave the
//! head of a credential that is now too short for the redactor to recognise.

use std::io::{self, BufRead, Write};

use chrono::TimeZone;
use duduclaw_core::secret_redact::redact_secrets;
use duduclaw_core::text_utils::truncate_bytes;
use serde_json::Value;

use crate::sanitize::replace_unsafe;

/// Longest message text (bytes, after redaction) written per record.
pub const MAX_MESSAGE_BYTES: usize = 4096;

/// Longest raw JSON record read from `journalctl`. A larger record is skipped
/// without being buffered. journalctl itself prints any field over 4096 bytes
/// as `null` unless `--all` is given (it is not), so real records stay far
/// below this.
pub const MAX_RECORD_BYTES: usize = 64 * 1024;

/// Most lines returned for a non-follow request. `journalctl -n 1000` already
/// limits the record count; this is the service's own bound on top of it.
pub const MAX_LINES_NON_FOLLOW: usize = 1000;

/// Shown in place of a message journalctl elided (`null`, over 4096 bytes).
pub const ELIDED_MESSAGE: &str = "[message too large, omitted / 訊息過長，已略過]";

/// Appended when a message was cut at [`MAX_MESSAGE_BYTES`].
pub const TRUNCATION_MARK: &str = " …[truncated]";

/// What one journal record turned into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// A line to send (no trailing newline).
    Line(String),
    /// The bootstrap password line (or another `password:` line): dropped.
    Withheld,
    /// Not a JSON object.
    Malformed,
}

/// Turn one raw `journalctl -o json` record into a [`Record`].
pub fn format_record<Tz>(raw: &[u8], tz: &Tz) -> Record
where
    Tz: TimeZone,
    Tz::Offset: std::fmt::Display,
{
    let obj = match serde_json::from_slice::<Value>(raw) {
        Ok(Value::Object(map)) => map,
        _ => return Record::Malformed,
    };

    let message = match obj.get("MESSAGE") {
        None => String::new(),
        Some(Value::Null) => ELIDED_MESSAGE.to_string(),
        Some(v) => {
            let values = field_values(v);
            if values.iter().any(|m| is_password_line(m)) {
                return Record::Withheld;
            }
            values.join(" | ")
        }
    };

    let cleaned = replace_unsafe(&message);
    let redacted = redact_secrets(&cleaned);
    let mut text = truncate_bytes(&redacted, MAX_MESSAGE_BYTES).to_string();
    if text.len() < redacted.len() {
        text.push_str(TRUNCATION_MARK);
    }

    Record::Line(format!(
        "{} {}",
        format_timestamp(obj.get("__REALTIME_TIMESTAMP"), tz),
        text
    ))
}

/// `password:` at the start of the message or of any line inside it,
/// ignoring leading whitespace, control, bidi and invisible format
/// characters (zero-width space, BOM, soft hyphen …) and ASCII case. The real
/// bootstrap line is printed on its own; the per-line check covers a message
/// written through journald's native protocol with embedded newlines.
fn is_password_line(message: &str) -> bool {
    message.split(['\n', '\r']).any(|line| {
        line.trim_start_matches(is_ignorable_prefix_char)
            .as_bytes()
            .get(..9)
            .is_some_and(|p| p.eq_ignore_ascii_case(b"password:"))
    })
}

fn is_ignorable_prefix_char(c: char) -> bool {
    c.is_whitespace()
        || crate::sanitize::is_unsafe_char(c)
        || matches!(
            c,
            '\u{00AD}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}'
        )
}

/// journald JSON field encodings: a string; an array of byte values (binary
/// or non-printable content); or, when the field occurs several times in one
/// record, an array of those. Bytes are decoded lossily.
fn field_values(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_number) => {
            let bytes: Vec<u8> = items
                .iter()
                .map(|n| {
                    n.as_u64()
                        .and_then(|n| u8::try_from(n).ok())
                        .unwrap_or(b'?')
                })
                .collect();
            vec![String::from_utf8_lossy(&bytes).into_owned()]
        }
        Value::Array(items) => items.iter().flat_map(field_values).collect(),
        Value::Null => vec![ELIDED_MESSAGE.to_string()],
        other => vec![other.to_string()],
    }
}

/// `__REALTIME_TIMESTAMP` is microseconds since the epoch, as a decimal
/// string. Formatted in the device's time zone with an explicit offset.
fn format_timestamp<Tz>(v: Option<&Value>, tz: &Tz) -> String
where
    Tz: TimeZone,
    Tz::Offset: std::fmt::Display,
{
    v.and_then(Value::as_str)
        .and_then(|s| s.parse::<i64>().ok())
        .and_then(|us| tz.timestamp_micros(us).single())
        .map(|t| t.format("%Y-%m-%d %H:%M:%S%:z").to_string())
        .unwrap_or_else(|| "-".to_string())
}

/// Result of [`read_bounded_line`].
#[derive(Debug, PartialEq, Eq)]
pub enum BoundedLine {
    /// A complete line is in the buffer (newline stripped).
    Line,
    /// The line exceeded the limit; it was consumed and discarded.
    TooLong,
    /// End of input, nothing pending.
    Eof,
}

/// Read one `\n`-terminated line into `buf` without ever holding more than
/// `max` bytes of it. A final line without a newline still counts as a line.
pub fn read_bounded_line<R: BufRead + ?Sized>(
    r: &mut R,
    max: usize,
    buf: &mut Vec<u8>,
) -> io::Result<BoundedLine> {
    buf.clear();
    let mut overflow = false;
    let mut saw_any = false;
    loop {
        let available = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if available.is_empty() {
            return Ok(match (saw_any, overflow) {
                (false, _) => BoundedLine::Eof,
                (true, true) => BoundedLine::TooLong,
                (true, false) => BoundedLine::Line,
            });
        }
        saw_any = true;
        let (chunk, found_newline) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (&available[..i], Some(i + 1)),
            None => (available, None),
        };
        if !overflow {
            if buf.len() + chunk.len() > max {
                overflow = true;
                buf.clear();
            } else {
                buf.extend_from_slice(chunk);
            }
        }
        let consumed = found_newline.unwrap_or(available.len());
        r.consume(consumed);
        if found_newline.is_some() {
            return Ok(if overflow {
                BoundedLine::TooLong
            } else {
                BoundedLine::Line
            });
        }
    }
}

/// Counters for one streaming run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StreamStats {
    pub emitted: usize,
    pub withheld: usize,
    pub malformed: usize,
    pub too_long: usize,
    /// Stopped because [`MAX_LINES_NON_FOLLOW`] (or the given cap) was hit.
    pub capped: bool,
}

/// Read journal records from `input` and write formatted lines to `out`
/// until end of input, a write error, or `max_lines` lines. Each line is
/// flushed so a follow stream shows records as they arrive.
pub fn stream_records<R, W, Tz>(
    input: &mut R,
    out: &mut W,
    tz: &Tz,
    max_lines: Option<usize>,
) -> io::Result<StreamStats>
where
    R: BufRead + ?Sized,
    W: Write + ?Sized,
    Tz: TimeZone,
    Tz::Offset: std::fmt::Display,
{
    let mut stats = StreamStats::default();
    let mut buf = Vec::with_capacity(4096);
    loop {
        if max_lines.is_some_and(|m| stats.emitted >= m) {
            stats.capped = true;
            return Ok(stats);
        }
        match read_bounded_line(input, MAX_RECORD_BYTES, &mut buf)? {
            BoundedLine::Eof => return Ok(stats),
            BoundedLine::TooLong => stats.too_long += 1,
            BoundedLine::Line if buf.is_empty() => {}
            BoundedLine::Line => match format_record(&buf, tz) {
                Record::Line(line) => {
                    out.write_all(line.as_bytes())?;
                    out.write_all(b"\n")?;
                    out.flush()?;
                    stats.emitted += 1;
                }
                Record::Withheld => stats.withheld += 1,
                Record::Malformed => stats.malformed += 1,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::io::Cursor;

    /// 2026-10-16 07:00:00 UTC in microseconds.
    const TS: &str = "1792134000000000";

    fn rec(message: &str) -> String {
        serde_json::json!({ "__REALTIME_TIMESTAMP": TS, "MESSAGE": message }).to_string()
    }

    fn line(raw: &str) -> Record {
        format_record(raw.as_bytes(), &Utc)
    }

    #[test]
    fn formats_timestamp_and_message() {
        assert_eq!(
            line(&rec("gateway started on 0.0.0.0:18789")),
            Record::Line("2026-10-16 07:00:00+00:00 gateway started on 0.0.0.0:18789".into())
        );
    }

    #[test]
    fn bootstrap_password_line_is_withheld() {
        // Exactly what the gateway prints on a non-loopback first run.
        assert_eq!(
            line(&rec("     Password: Xy7-correct-horse")),
            Record::Withheld
        );
        assert_eq!(line(&rec("Password: abc")), Record::Withheld);
        assert_eq!(line(&rec("\t\r password:abc")), Record::Withheld);
        assert_eq!(line(&rec("PASSWORD: abc")), Record::Withheld);
        // The neighbouring lines are not secret and still come through.
        assert!(matches!(
            line(&rec("     Email:    admin@local")),
            Record::Line(_)
        ));
        assert!(matches!(
            line(&rec(
                "  🔑 First-run admin — log in with this, you'll be asked to change it:"
            )),
            Record::Line(_)
        ));
        // Hidden behind invisible characters, or on a later line of a
        // multi-line message.
        assert_eq!(line(&rec("\u{200B}\u{FEFF} Password: x")), Record::Withheld);
        assert_eq!(line(&rec("first line\n   Password: x")), Record::Withheld);
        // `password:` later in the line is ordinary text.
        assert!(matches!(
            line(&rec("reset password: done")),
            Record::Line(_)
        ));
    }

    #[test]
    fn password_line_in_binary_encoding_is_withheld() {
        // journald emits non-printable content as a byte array.
        let mut bytes: Vec<u8> = b"\x1b Password: secret".to_vec();
        bytes[0] = b'\t';
        let raw = serde_json::json!({ "__REALTIME_TIMESTAMP": TS, "MESSAGE": bytes }).to_string();
        assert_eq!(line(&raw), Record::Withheld);
    }

    #[test]
    fn password_line_among_repeated_fields_is_withheld() {
        let raw = serde_json::json!({
            "__REALTIME_TIMESTAMP": TS,
            "MESSAGE": ["harmless", "Password: hunter2-hunter2"]
        })
        .to_string();
        assert_eq!(line(&raw), Record::Withheld);
    }

    #[test]
    fn credentials_are_redacted() {
        let token = ["xoxb", "-1234567890-", "abcdefghijklmnop"].concat();
        let tg = ["7000000001:", "AAExampleExampleExampleExampleXYZ12"].concat();
        let key = ["sk-", "ant-", "api03-", "abcdefghijklmnopqrstuvwx"].concat();
        for (msg, secret) in [
            (format!("slack auth failed for {token}"), "abcdefghijklmnop".to_string()),
            (
                format!("error sending request for url (https://api.telegram.org/bot{tg}/getMe)"),
                "AAExampleExampleExampleExampleXYZ12".to_string(),
            ),
            (format!("Authorization: Bearer {key}"), "abcdefghijklmnopqrstuvwx".to_string()),
            (
                "GET https://qyapi.weixin.qq.com/cgi-bin/gettoken?corpid=ww1&corpsecret=SsuperSecretValue1234567890abcdef".to_string(),
                "SsuperSecretValue1234567890abcdef".to_string(),
            ),
        ] {
            match line(&rec(&msg)) {
                Record::Line(l) => assert!(!l.contains(&secret), "secret survived: {l}"),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn control_characters_cannot_reach_the_terminal() {
        match line(&rec("ok\x1b]0;owned\x07\r\nfake 2026-01-01 line\u{202E}")) {
            Record::Line(l) => {
                assert!(!l.chars().any(crate::sanitize::is_unsafe_char), "{l:?}");
                assert!(!l.contains('\n'));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn long_messages_are_cut_after_redaction() {
        let long = "界".repeat(3000); // 9000 bytes
        match line(&rec(&long)) {
            Record::Line(l) => {
                assert!(l.ends_with(TRUNCATION_MARK));
                let body = l.split_once(' ').unwrap().1.split_once(' ').unwrap().1;
                assert!(body.len() <= MAX_MESSAGE_BYTES + TRUNCATION_MARK.len());
            }
            other => panic!("unexpected {other:?}"),
        }
        // A credential straddling the cut is still masked.
        let key = [
            "sk-",
            "ant-",
            "api03-",
            "abcdefghijklmnopqrstuvwxyz0123456789",
        ]
        .concat();
        let msg = format!("{} {key}", "a".repeat(MAX_MESSAGE_BYTES - 10));
        match line(&rec(&msg)) {
            Record::Line(l) => assert!(!l.contains("sk-ant-api03-abc"), "{l}"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn elided_missing_and_bad_records() {
        let raw = serde_json::json!({ "__REALTIME_TIMESTAMP": TS, "MESSAGE": null }).to_string();
        assert_eq!(
            line(&raw),
            Record::Line(format!("2026-10-16 07:00:00+00:00 {ELIDED_MESSAGE}"))
        );
        assert_eq!(line(r#"{"MESSAGE":"x"}"#), Record::Line("- x".into()));
        assert_eq!(
            line(r#"{"__REALTIME_TIMESTAMP":"nope","MESSAGE":"x"}"#),
            Record::Line("- x".into())
        );
        assert_eq!(line("not json"), Record::Malformed);
        assert_eq!(line("[1,2]"), Record::Malformed);
    }

    #[test]
    fn only_message_and_timestamp_are_output() {
        let raw = serde_json::json!({
            "__REALTIME_TIMESTAMP": TS,
            "MESSAGE": "hello",
            "_CMDLINE": "claude -p secret-prompt",
            "_SYSTEMD_UNIT": "duduclaw-gateway.service"
        })
        .to_string();
        assert_eq!(
            line(&raw),
            Record::Line("2026-10-16 07:00:00+00:00 hello".into())
        );
    }

    #[test]
    fn bounded_reader_skips_oversized_lines() {
        let mut input = Cursor::new(b"short\nxxxxxxxxxxxxxxxxxxxx\nok\ntail".to_vec());
        let mut buf = Vec::new();
        assert_eq!(
            read_bounded_line(&mut input, 10, &mut buf).unwrap(),
            BoundedLine::Line
        );
        assert_eq!(buf, b"short");
        assert_eq!(
            read_bounded_line(&mut input, 10, &mut buf).unwrap(),
            BoundedLine::TooLong
        );
        assert!(buf.is_empty());
        assert_eq!(
            read_bounded_line(&mut input, 10, &mut buf).unwrap(),
            BoundedLine::Line
        );
        assert_eq!(buf, b"ok");
        assert_eq!(
            read_bounded_line(&mut input, 10, &mut buf).unwrap(),
            BoundedLine::Line
        );
        assert_eq!(buf, b"tail");
        assert_eq!(
            read_bounded_line(&mut input, 10, &mut buf).unwrap(),
            BoundedLine::Eof
        );
    }

    #[test]
    fn bounded_reader_handles_small_buffers() {
        // A BufReader with a tiny capacity forces lines to span many fills.
        let data = b"0123456789abcdef\n0123\n".to_vec();
        let mut input = io::BufReader::with_capacity(3, Cursor::new(data));
        let mut buf = Vec::new();
        assert_eq!(
            read_bounded_line(&mut input, 8, &mut buf).unwrap(),
            BoundedLine::TooLong
        );
        assert_eq!(
            read_bounded_line(&mut input, 8, &mut buf).unwrap(),
            BoundedLine::Line
        );
        assert_eq!(buf, b"0123");
    }

    #[test]
    fn stream_withholds_counts_and_caps() {
        let mut input = String::new();
        input.push_str(&rec("first"));
        input.push('\n');
        input.push_str(&rec("     Password: s3cret-value"));
        input.push('\n');
        input.push_str("garbage\n\n");
        input.push_str(&format!(
            "{{\"MESSAGE\":\"{}\"}}\n",
            "x".repeat(MAX_RECORD_BYTES)
        ));
        for i in 0..5 {
            input.push_str(&rec(&format!("line {i}")));
            input.push('\n');
        }
        let mut out = Vec::new();
        let stats = stream_records(&mut Cursor::new(input), &mut out, &Utc, Some(4)).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("s3cret"), "{text}");
        assert_eq!(text.lines().count(), 4);
        assert_eq!(
            stats,
            StreamStats {
                emitted: 4,
                withheld: 1,
                malformed: 1,
                too_long: 1,
                capped: true
            }
        );
    }

    #[test]
    fn stream_without_cap_reads_to_end() {
        let input = format!("{}\n{}\n", rec("a"), rec("b"));
        let mut out = Vec::new();
        let stats = stream_records(&mut Cursor::new(input), &mut out, &Utc, None).unwrap();
        assert_eq!(stats.emitted, 2);
        assert!(!stats.capped);
    }
}
