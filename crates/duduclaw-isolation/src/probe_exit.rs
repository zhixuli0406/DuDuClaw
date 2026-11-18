//! R7c: the exit-code table of the SSH denial probe
//! (`duduclaw-isolation-probe <account>.<port>`, `src/bin/probe.rs`), one code
//! per stage. The probe binary and spawnd (which maps the code PID 1 records
//! for the probe unit instance) both use these constants; neither copies
//! numbers. The unit file comment and the READMEs list the same table.
//!
//! 0 = denied is the only pass. [`EXIT_LOGIN_NOT_DENIED`] and
//! [`EXIT_FORWARD_NOT_DENIED`] are the only codes that prove the account
//! reachable, and the probe uses them only with sshd's own root `Accepted
//! publickey` journal record for the probe's ephemeral key and source port;
//! a client that succeeded WITHOUT that record is [`EXIT_LOGIN_UNVERIFIED`] /
//! [`EXIT_FORWARD_UNVERIFIED`] (inconclusive: e.g. a fake server on the port).
//! Every other code is a failed or inconclusive stage. Never 1 (generic), 101
//! (Rust panic), 125 (unsupported platform), 200–243 (systemd's own exec
//! failures) or 255 (ssh's own failure code). Retired codes are never reused:
//! 5 (`no_sshd`) and 7 (`effective_config`) belonged to stages the
//! unprivileged probe no longer has (spawnd checks `sshd -T` itself).

/// The account was refused both a public-key login and a forwarding session,
/// corroborated by sshd's own journal records. The only pass.
pub const EXIT_DENIED: i32 = 0;
/// Not exactly one `<account>.<port>` argument (or the `--relay` form).
pub const EXIT_USAGE: i32 = 2;
/// Running as root (only the unprivileged unit may run it), or the account
/// has no single /etc/passwd entry with a uid in the employee range.
pub const EXIT_IDENTITY: i32 = 3;
/// A tool (or this binary) is not a root-owned, non group/other-writable file.
pub const EXIT_TOOLS: i32 = 4;
pub const EXIT_WORKSPACE: i32 = 6;
/// `ssh-keygen` could not create the ephemeral key or print its fingerprint.
pub const EXIT_KEYGEN: i32 = 8;
/// Login attempt neither denied nor accepted (refused connection, timeout, …).
pub const EXIT_LOGIN_INCONCLUSIVE: i32 = 9;
/// Login attempt exited 0 AND sshd logged (root) `Accepted publickey` for the
/// account, this source port and the ephemeral key: reachable over SSH.
pub const EXIT_LOGIN_NOT_DENIED: i32 = 10;
/// Login denied by the client, but no matching sshd DenyUsers journal record.
pub const EXIT_LOGIN_JOURNAL: i32 = 11;
pub const EXIT_FORWARD_INCONCLUSIVE: i32 = 12;
/// Forwarding session exited 0 or stayed connected until the kill timer AND
/// sshd logged the matching root `Accepted publickey` record.
pub const EXIT_FORWARD_NOT_DENIED: i32 = 13;
pub const EXIT_FORWARD_JOURNAL: i32 = 14;
/// The 25 s overall deadline expired.
pub const EXIT_DEADLINE: i32 = 15;
pub const EXIT_OUTPUT: i32 = 16;
/// `--relay` mode only: socket, bind, connect or copy failed.
pub const EXIT_RELAY: i32 = 17;
/// Login client exited 0 but sshd logged no matching `Accepted publickey`
/// record: something answered on the port that is not proven to be sshd.
pub const EXIT_LOGIN_UNVERIFIED: i32 = 18;
/// Forwarding client exited 0 or stayed connected, without the matching
/// `Accepted publickey` record.
pub const EXIT_FORWARD_UNVERIFIED: i32 = 19;

/// The stage name the probe prints for `code` (`{"stage":"<name>"}`), for
/// logs; `None` for a code the probe never uses.
pub fn stage_name(code: i32) -> Option<&'static str> {
    Some(match code {
        EXIT_DENIED => "denied", EXIT_USAGE => "usage", EXIT_IDENTITY => "identity", EXIT_TOOLS => "tools",
        EXIT_WORKSPACE => "workspace", EXIT_KEYGEN => "keygen", EXIT_LOGIN_INCONCLUSIVE => "login_inconclusive",
        EXIT_LOGIN_NOT_DENIED => "login_not_denied", EXIT_LOGIN_JOURNAL => "login_journal",
        EXIT_FORWARD_INCONCLUSIVE => "forward_inconclusive", EXIT_FORWARD_NOT_DENIED => "forward_not_denied",
        EXIT_FORWARD_JOURNAL => "forward_journal", EXIT_DEADLINE => "deadline", EXIT_OUTPUT => "output",
        EXIT_RELAY => "relay", EXIT_LOGIN_UNVERIFIED => "login_unverified", EXIT_FORWARD_UNVERIFIED => "forward_unverified",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn probe_exit_table_is_pinned() {
        assert_eq!((EXIT_DENIED, EXIT_LOGIN_NOT_DENIED, EXIT_FORWARD_NOT_DENIED, EXIT_IDENTITY), (0, 10, 13, 3));
        assert_eq!((EXIT_LOGIN_UNVERIFIED, EXIT_FORWARD_UNVERIFIED), (18, 19));
        let named: Vec<i32> = (-1..=300).filter(|code| stage_name(*code).is_some()).collect();
        assert_eq!(named, (0..=19).filter(|code| ![1, 5, 7].contains(code)).collect::<Vec<_>>());
        for code in [1, 101, 125, 200, 243, 255] { assert_eq!(stage_name(code), None, "{code}"); }
        // Retired with the root probe's stages; never reused.
        for code in [5, 7] { assert_eq!(stage_name(code), None, "{code}"); }
        let mut names: Vec<&str> = named.iter().filter_map(|code| stage_name(*code)).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), named.len(), "one name per code");
        assert!(!names.contains(&"no_sshd") && !names.contains(&"effective_config"));
    }
}
