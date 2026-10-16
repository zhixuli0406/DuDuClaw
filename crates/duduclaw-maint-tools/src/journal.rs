//! One journald entry per menu action, over journald's native protocol.
//!
//! sysd reads these back as maintenance events only when journald's own
//! trusted fields match (`SYSLOG_IDENTIFIER=duduclaw-maint`, `_UID=4800`,
//! `_SYSTEMD_SLICE=system-dropbear.slice`); journald fills `_UID` and the
//! slice from the sending socket, so the identifier alone proves nothing and
//! this module does not try to make it.
//!
//! Messages are fixed strings chosen by the caller. Nothing the SSH client
//! typed is ever written here: a rejected command is recorded by its length.

use std::io;

/// `SYSLOG_IDENTIFIER` of menu events (design §5.1, §6.2).
pub const MENU_IDENTIFIER: &str = "duduclaw-maint";

/// `SYSLOG_IDENTIFIER` of the helper's own entries (refusals, served log
/// requests, restarts).
pub const HELPER_IDENTIFIER: &str = "duduclaw-maint-helper";

/// journald's native-protocol datagram socket.
pub const JOURNAL_SOCKET: &str = "/run/systemd/journal/socket";

/// syslog priorities used here.
pub const PRIORITY_WARNING: u8 = 4;
pub const PRIORITY_INFO: u8 = 6;

/// Build a native-protocol payload: `KEY=value\n` per field. Values are
/// forced onto one line (any `\n` becomes a space) so the simple text form
/// stays valid; keys that are not `[A-Z0-9_]+`, start with `_` (reserved for
/// journald's trusted fields) or are empty are skipped.
pub fn entry_payload(
    identifier: &str,
    priority: u8,
    message: &str,
    fields: &[(&str, &str)],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);
    push_field(&mut out, "SYSLOG_IDENTIFIER", identifier);
    push_field(&mut out, "PRIORITY", &priority.min(7).to_string());
    push_field(&mut out, "MESSAGE", message);
    for (k, v) in fields {
        if is_valid_key(k) {
            push_field(&mut out, k, v);
        }
    }
    out
}

fn is_valid_key(k: &str) -> bool {
    !k.is_empty()
        && !k.starts_with('_')
        && k.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

fn push_field(out: &mut Vec<u8>, key: &str, value: &str) {
    out.extend_from_slice(key.as_bytes());
    out.push(b'=');
    out.extend(value.bytes().map(|b| if b == b'\n' { b' ' } else { b }));
    out.push(b'\n');
}

/// Send one entry to journald.
#[cfg(target_os = "linux")]
pub fn send(payload: &[u8]) -> io::Result<()> {
    let sock = std::os::unix::net::UnixDatagram::unbound()?;
    sock.send_to(payload, JOURNAL_SOCKET)?;
    Ok(())
}

/// Development hosts (macOS) have no journald; there is nothing to record.
#[cfg(not(target_os = "linux"))]
pub fn send(_payload: &[u8]) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_has_identifier_priority_message_and_fields() {
        let p = entry_payload(
            MENU_IDENTIFIER,
            PRIORITY_INFO,
            "maint-shell action: status",
            &[("MAINT_ACTION", "status"), ("MAINT_MODE", "command")],
        );
        assert_eq!(
            String::from_utf8(p).unwrap(),
            "SYSLOG_IDENTIFIER=duduclaw-maint\nPRIORITY=6\nMESSAGE=maint-shell action: status\nMAINT_ACTION=status\nMAINT_MODE=command\n"
        );
    }

    #[test]
    fn newlines_cannot_inject_fields() {
        let p = entry_payload(
            MENU_IDENTIFIER,
            PRIORITY_INFO,
            "a\n_UID=0\nb",
            &[("K", "v\nSYSLOG_IDENTIFIER=x")],
        );
        let text = String::from_utf8(p).unwrap();
        assert_eq!(text.lines().count(), 4, "{text}");
        assert!(!text.lines().any(|l| l.starts_with("_UID=")));
    }

    #[test]
    fn trusted_and_malformed_keys_are_skipped() {
        let p = entry_payload(
            MENU_IDENTIFIER,
            PRIORITY_INFO,
            "m",
            &[
                ("_UID", "0"),
                ("_SYSTEMD_SLICE", "x"),
                ("lower", "x"),
                ("", "x"),
                ("A-B", "x"),
                ("OK_1", "y"),
            ],
        );
        let text = String::from_utf8(p).unwrap();
        assert!(text.ends_with("MESSAGE=m\nOK_1=y\n"), "{text}");
    }
}
