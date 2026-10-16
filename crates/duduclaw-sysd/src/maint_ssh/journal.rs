//! Reading maintenance SSH events out of the journal using only fields that
//! journald fills in itself (design §6.2): a sender can forge `MESSAGE` and
//! `SYSLOG_IDENTIFIER`, but not `_SYSTEMD_SLICE` or `_UID`, and only root can
//! place a process in `system-dropbear.slice`.
//!
//! - dropbear events: `_SYSTEMD_SLICE=system-dropbear.slice` and
//!   `_UID` ∈ {0, maint uid}; classified by dropbear 2025.89's fixed message
//!   prefixes (`svr-authpubkey.c`, `svr-session.c`).
//! - menu events: additionally `SYSLOG_IDENTIFIER=duduclaw-maint` and
//!   `_UID` = maint uid.
//!
//! Every string taken from an event (connecting-side text can reach dropbear's
//! log lines) is stripped of control and bidi characters and cut to
//! [`EVENT_TEXT_MAX_CHARS`] characters before it is stored or returned.

use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// `-o json` entries must come from this slice.
pub const DROPBEAR_SLICE: &str = "system-dropbear.slice";

/// What the trusted-field filter compares against: the maint uid and the
/// real paths (as journald records them in `_EXE`) of dropbear and the menu,
/// resolved at read time (`host::resolve_exe`; dropbear is usually the
/// `dropbearmulti` multi-call binary behind a symlink).
#[derive(Debug, Clone, Copy)]
pub struct Trust<'a> {
    pub maint_uid: u32,
    pub dropbear_exe: &'a str,
    pub menu_exe: &'a str,
}

/// Fields asked from `journalctl --output-fields=` (the cursor and timestamps
/// always come along).
pub const OUTPUT_FIELDS: &str = "MESSAGE,_UID,_SYSTEMD_SLICE,_COMM,_EXE,_PID,SYSLOG_IDENTIFIER";

/// `SYSLOG_IDENTIFIER` the maintenance menu logs with.
pub const MENU_IDENTIFIER: &str = "duduclaw-maint";

/// A successful login of the maintenance account, as dropbear 2025.89 logs
/// it (`svr-authpubkey.c`). Anchored on the account name, so a login of any
/// other user (which `-G` should make impossible) is not counted as one.
pub const LOGIN_PREFIX: &str = "Pubkey auth succeeded for 'duduclaw-maint' with ";

/// Longest string kept from an event.
pub const EVENT_TEXT_MAX_CHARS: usize = 128;

/// Longest journal cursor accepted (real ones are about 120 bytes).
pub const MAX_CURSOR_LEN: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Login,
    Logout,
    AuthFailed,
    MenuAction,
}

impl EventKind {
    /// Logins and logouts are kept in preference when the status buffer is
    /// full.
    pub fn is_session(self) -> bool {
        matches!(self, EventKind::Login | EventKind::Logout)
    }
}

/// One classified event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshEvent {
    pub cursor: String,
    pub kind: EventKind,
    /// `__REALTIME_TIMESTAMP` (µs since the epoch), as journald recorded it.
    pub realtime_usec: u64,
    pub pid: Option<u32>,
    /// Peer address without the port, when the message carried one.
    pub source: Option<String>,
    /// Key fingerprint (logins only).
    pub fingerprint: Option<String>,
    /// The sanitised message.
    pub message: String,
}

/// A journal cursor as printed by journalctl: printable, no spaces, bounded.
pub fn valid_cursor(c: &str) -> bool {
    !c.is_empty() && c.len() <= MAX_CURSOR_LEN && c.bytes().all(|b| b.is_ascii_graphic())
}

/// Strip control characters (C0, DEL, C1) and bidirectional overrides, then
/// cut to [`EVENT_TEXT_MAX_CHARS`] characters.
pub fn sanitize(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_control() && !is_bidi_control(*c))
        .collect();
    truncate_chars(&cleaned, EVENT_TEXT_MAX_CHARS)
}

/// At most `max` characters (never splits a code point). sysd carries its own
/// copy so the crate has no path dependencies.
pub fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn str_field<'a>(entry: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    entry.get(key).and_then(Value::as_str)
}

/// Parse one `journalctl -o json` line. Returns the entry's cursor (always,
/// when readable) and the event when the entry passes the trusted-field
/// filter and has a recognised message.
pub fn classify_line(line: &str, trust: &Trust<'_>) -> Option<(String, Option<SshEvent>)> {
    let value: Value = serde_json::from_str(line).ok()?;
    let entry = value.as_object()?;
    let cursor = str_field(entry, "__CURSOR").filter(|c| valid_cursor(c))?.to_string();
    let event = classify_entry(entry, &cursor, trust);
    Some((cursor, event))
}

fn classify_entry(entry: &Map<String, Value>, cursor: &str, trust: &Trust<'_>) -> Option<SshEvent> {
    let maint_uid = trust.maint_uid;
    if str_field(entry, "_SYSTEMD_SLICE")? != DROPBEAR_SLICE {
        return None;
    }
    let uid: u32 = str_field(entry, "_UID")?.parse().ok()?;
    // MESSAGE may be a byte array when it is not UTF-8; such entries are
    // never classified.
    let message = str_field(entry, "MESSAGE")?;
    let realtime_usec = str_field(entry, "__REALTIME_TIMESTAMP").and_then(|t| t.parse().ok()).unwrap_or(0);
    let pid = str_field(entry, "_PID").and_then(|p| p.parse().ok());
    let base = SshEvent {
        cursor: cursor.to_string(),
        kind: EventKind::MenuAction,
        realtime_usec,
        pid,
        source: None,
        fingerprint: None,
        message: sanitize(message),
    };

    if str_field(entry, "SYSLOG_IDENTIFIER") == Some(MENU_IDENTIFIER) {
        let from_menu = str_field(entry, "_EXE") == Some(trust.menu_exe);
        return (uid == maint_uid && from_menu).then_some(base);
    }
    if uid != 0 && uid != maint_uid {
        return None;
    }
    // Authentication runs in dropbear's root process: login and pre-auth
    // lines are only accepted from uid 0. After `send_msg_userauth_success`
    // the same process switches to the maint user (DROPBEAR_SVR_DROP_PRIVS),
    // so the logout line comes from the maint uid; then the process image
    // (`_EXE`, filled in by journald from /proc) must be dropbear's.
    let from_root = uid == 0;
    let exe_is_dropbear = str_field(entry, "_EXE") == Some(trust.dropbear_exe);
    if message.starts_with(LOGIN_PREFIX) {
        if !from_root {
            return None;
        }
        // "… with <type> key SHA256:<b64> from <ip>:<port>", optionally
        // followed by ", extra auth required" (not a login: refused).
        if message.ends_with("extra auth required") {
            return None;
        }
        let fingerprint = message
            .split(' ')
            .find(|t| {
                t.strip_prefix("SHA256:").is_some_and(|b64| {
                    !b64.is_empty() && b64.len() <= 64 && b64.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
                })
            })
            .map(str::to_string);
        let source = message.rsplit(" from ").next().and_then(parse_addr);
        return Some(SshEvent { kind: EventKind::Login, fingerprint, source, ..base });
    }
    if message.starts_with("Exit (") {
        if !from_root && !exe_is_dropbear {
            return None;
        }
        // "Exit (<user>) from <<addr>>: <reason>"; the user is a passwd name
        // that passed `-G`, the address comes from getpeername.
        return Some(SshEvent { kind: EventKind::Logout, source: first_bracketed_addr(message), ..base });
    }
    if message.starts_with("Exit before auth") {
        if !from_root {
            return None;
        }
        // "Exit before auth from <<addr>>: (user '<name>', N fails): …"; the
        // address precedes the attacker-chosen user name, so the first
        // bracketed value is dropbear's.
        return Some(SshEvent { kind: EventKind::AuthFailed, source: first_bracketed_addr(message), ..base });
    }
    None
}

/// The first `<…>` in the message, when it is `ip:port`.
fn first_bracketed_addr(message: &str) -> Option<String> {
    let (_, after) = message.split_once('<')?;
    let (inside, _) = after.split_once('>')?;
    parse_addr(inside)
}

/// `ip:port` (dropbear's addrstring) → the IP only, canonicalised.
fn parse_addr(text: &str) -> Option<String> {
    let text = text.trim();
    if let Ok(sa) = text.parse::<SocketAddr>() {
        return Some(sa.ip().to_string());
    }
    let (host, port) = text.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.parse::<IpAddr>().ok().map(|ip| ip.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// oe-core's multi-call layout: `/usr/sbin/dropbear` → `dropbearmulti`.
    pub(crate) const TEST_DROPBEAR_EXE: &str = "/usr/sbin/dropbearmulti";
    pub(crate) const TEST_MENU_EXE: &str = "/usr/libexec/duduclaw/maint-shell";
    pub(crate) const TRUST: Trust<'static> = Trust { maint_uid: 4800, dropbear_exe: TEST_DROPBEAR_EXE, menu_exe: TEST_MENU_EXE };

    pub(crate) fn entry(cursor: &str, slice: &str, uid: u32, ident: &str, pid: u32, msg: &str) -> String {
        serde_json::json!({
            "__CURSOR": cursor,
            "__REALTIME_TIMESTAMP": "1760600000000000",
            "_SYSTEMD_SLICE": slice,
            "_UID": uid.to_string(),
            "_PID": pid.to_string(),
            "SYSLOG_IDENTIFIER": ident,
            "_COMM": if ident == MENU_IDENTIFIER { "maint-shell" } else { "dropbear" },
            "_EXE": if ident == MENU_IDENTIFIER { TEST_MENU_EXE } else { TEST_DROPBEAR_EXE },
            "MESSAGE": msg,
        })
        .to_string()
    }

    pub(crate) const LOGIN: &str = "Pubkey auth succeeded for 'duduclaw-maint' with ssh-ed25519 key SHA256:YoPXM1Ypqr+D/3+TBuMYB3Z4IZH0k0ErqeSny62rhCE from 192.168.1.20:51515";
    pub(crate) const LOGOUT: &str = "Exit (duduclaw-maint) from <192.168.1.20:51515>: Disconnect received";
    pub(crate) const FAILED: &str = "Exit before auth from <100.64.3.4:4000>: (user 'root', 0 fails): Exited normally";

    fn kind(line: &str) -> Option<EventKind> {
        classify_line(line, &TRUST).and_then(|(_, e)| e).map(|e| e.kind)
    }

    #[test]
    fn dropbear_messages_are_classified() {
        let (c, e) = classify_line(&entry("s=1", DROPBEAR_SLICE, 0, "dropbear", 77, LOGIN), &TRUST).unwrap();
        let e = e.unwrap();
        assert_eq!(c, "s=1");
        assert_eq!(e.kind, EventKind::Login);
        assert_eq!(e.fingerprint.as_deref(), Some("SHA256:YoPXM1Ypqr+D/3+TBuMYB3Z4IZH0k0ErqeSny62rhCE"));
        assert_eq!(e.source.as_deref(), Some("192.168.1.20"));
        assert_eq!(e.pid, Some(77));
        assert_eq!(e.realtime_usec, 1_760_600_000_000_000);

        let e = classify_line(&entry("s=2", DROPBEAR_SLICE, 4800, "dropbear", 77, LOGOUT), &TRUST).unwrap().1.unwrap();
        assert_eq!((e.kind, e.source.as_deref()), (EventKind::Logout, Some("192.168.1.20")));
        let e = classify_line(&entry("s=3", DROPBEAR_SLICE, 0, "dropbear", 78, FAILED), &TRUST).unwrap().1.unwrap();
        assert_eq!((e.kind, e.source.as_deref()), (EventKind::AuthFailed, Some("100.64.3.4")));
        assert_eq!(kind(&entry("s=4", DROPBEAR_SLICE, 4800, MENU_IDENTIFIER, 80, "status")), Some(EventKind::MenuAction));
        // Unrecognised dropbear chatter yields a cursor but no event.
        assert_eq!(classify_line(&entry("s=5", DROPBEAR_SLICE, 0, "dropbear", 1, "Child connection from x"), &TRUST).unwrap().1, None);
        let ipv6 = "Exit before auth from <fe80::1%eth0:22>: x";
        let _ = kind(&entry("s=6", DROPBEAR_SLICE, 0, "dropbear", 1, ipv6));
    }

    #[test]
    fn forged_events_are_filtered_by_trusted_fields() {
        // uid 1000 using `logger -t dropbear` lands in its own user slice.
        assert_eq!(kind(&entry("s=1", "user-1000.slice", 1000, "dropbear", 5, LOGIN)), None);
        // Even inside the slice, another uid is refused.
        assert_eq!(kind(&entry("s=1", DROPBEAR_SLICE, 1000, "dropbear", 5, LOGIN)), None);
        // A menu identifier from root does not count as the maint user.
        assert_eq!(kind(&entry("s=1", DROPBEAR_SLICE, 0, MENU_IDENTIFIER, 5, "status")), None);
        // Slice spelled differently.
        assert_eq!(kind(&entry("s=1", "system-dropbear.slice.evil", 0, "dropbear", 5, LOGIN)), None);
        // The menu cannot fake a login (identifier wins and makes it a menu event).
        assert_eq!(kind(&entry("s=1", DROPBEAR_SLICE, 4800, MENU_IDENTIFIER, 5, LOGIN)), Some(EventKind::MenuAction));
        // The maint uid cannot produce logins or failures, and a logout from
        // it must come from a process the kernel calls dropbear.
        assert_eq!(kind(&entry("s=1", DROPBEAR_SLICE, 4800, "dropbear", 5, LOGIN)), None);
        assert_eq!(kind(&entry("s=1", DROPBEAR_SLICE, 4800, "dropbear", 5, FAILED)), None);
        let fake_logout = serde_json::json!({"__CURSOR":"s=1","_SYSTEMD_SLICE":DROPBEAR_SLICE,"_UID":"4800","_COMM":"logger","_EXE":"/usr/bin/logger","SYSLOG_IDENTIFIER":"dropbear","MESSAGE":LOGOUT}).to_string();
        assert_eq!(kind(&fake_logout), None);
        // `_COMM` is settable by the process (prctl); only `_EXE` counts.
        let renamed = serde_json::json!({"__CURSOR":"s=1","_SYSTEMD_SLICE":DROPBEAR_SLICE,"_UID":"4800","_COMM":"dropbear","_EXE":"/usr/bin/logger","SYSLOG_IDENTIFIER":"dropbear","MESSAGE":LOGOUT}).to_string();
        assert_eq!(kind(&renamed), None);
        // With the plain layout the logout comes from /usr/sbin/dropbear; the
        // multi-call path must then not be accepted, and vice versa.
        let plain = Trust { dropbear_exe: "/usr/sbin/dropbear", ..TRUST };
        let plain_logout = serde_json::json!({"__CURSOR":"s=1","_SYSTEMD_SLICE":DROPBEAR_SLICE,"_UID":"4800","_EXE":"/usr/sbin/dropbear","MESSAGE":LOGOUT}).to_string();
        assert_eq!(classify_line(&plain_logout, &plain).unwrap().1.map(|e| e.kind), Some(EventKind::Logout));
        assert_eq!(kind(&plain_logout), None);
        // A menu line must come from the menu binary.
        let fake_menu = serde_json::json!({"__CURSOR":"s=1","_SYSTEMD_SLICE":DROPBEAR_SLICE,"_UID":"4800","_EXE":"/usr/bin/logger","SYSLOG_IDENTIFIER":MENU_IDENTIFIER,"MESSAGE":"restart gateway"}).to_string();
        assert_eq!(kind(&fake_menu), None);
        // Another account's login line is not a maintenance login.
        assert_eq!(kind(&entry("s=1", DROPBEAR_SLICE, 0, "dropbear", 5, &LOGIN.replace("duduclaw-maint", "duduclaw-rescue"))), None);
        // Partial-success logins are not logins.
        assert_eq!(kind(&entry("s=1", DROPBEAR_SLICE, 0, "dropbear", 5, &format!("{LOGIN}, extra auth required"))), None);
        // Missing or non-string trusted fields.
        let no_slice = serde_json::json!({"__CURSOR":"s=1","_UID":"0","MESSAGE":LOGIN}).to_string();
        assert_eq!(kind(&no_slice), None);
        let bytes_msg = serde_json::json!({"__CURSOR":"s=1","_SYSTEMD_SLICE":DROPBEAR_SLICE,"_UID":"0","MESSAGE":[80,117]}).to_string();
        assert_eq!(kind(&bytes_msg), None);
        // Bad cursor ⇒ unreadable entry.
        assert!(classify_line(&entry("has space", DROPBEAR_SLICE, 0, "dropbear", 5, LOGIN), &TRUST).is_none());
        assert!(classify_line("not json", &TRUST).is_none());
    }

    #[test]
    fn attacker_text_cannot_steer_the_source() {
        let spoof = "Exit before auth from <10.9.9.9:22>: (user 'x <192.168.1.1:1>', 1 fails): bye";
        let e = classify_line(&entry("s=1", DROPBEAR_SLICE, 0, "dropbear", 5, spoof), &TRUST).unwrap().1.unwrap();
        assert_eq!(e.source.as_deref(), Some("10.9.9.9"));
        let junk = "Exit before auth from <not-an-ip>: x";
        let e = classify_line(&entry("s=1", DROPBEAR_SLICE, 0, "dropbear", 5, junk), &TRUST).unwrap().1.unwrap();
        assert_eq!(e.source, None);
    }

    #[test]
    fn sanitize_strips_controls_and_truncates_by_chars() {
        assert_eq!(sanitize("a\u{1b}[31mb\r\nc\u{0}d\u{7f}e\u{85}f"), "a[31mbcdef");
        assert_eq!(sanitize("x\u{202E}y\u{2066}z"), "xyz");
        let long = "維".repeat(300);
        let out = sanitize(&long);
        assert_eq!(out.chars().count(), EVENT_TEXT_MAX_CHARS);
        let e = classify_line(&entry("s=1", DROPBEAR_SLICE, 0, "dropbear", 5, &format!("Exit before auth from <1.2.3.4:5>: {}", "\u{1b}".repeat(10) + &"A".repeat(500))), &TRUST)
            .unwrap()
            .1
            .unwrap();
        assert!(e.message.chars().count() <= EVENT_TEXT_MAX_CHARS);
        assert!(!e.message.contains('\u{1b}'));
    }

    #[test]
    fn cursors_are_validated() {
        assert!(valid_cursor("s=6c2f;i=1a2b;b=0e1f;m=12ab;t=5f0e;x=9a"));
        assert!(!valid_cursor(""));
        assert!(!valid_cursor("a b"));
        assert!(!valid_cursor("a\nb"));
        assert!(!valid_cursor(&"a".repeat(MAX_CURSOR_LEN + 1)));
    }
}
