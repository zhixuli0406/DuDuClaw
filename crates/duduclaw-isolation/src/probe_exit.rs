//! R7c: the exit-code table of the SSH denial probe
//! (`duduclaw-isolation-probe <account>`, `src/bin/probe.rs`), one code per
//! stage. The probe binary and spawnd (which maps the code PID 1 records for
//! the probe unit instance) both use these constants; neither copies numbers.
//!
//! 0 = denied is the only pass. [`EXIT_NO_SSHD`] means SSH checks do not apply
//! to the host; [`EXIT_LOGIN_NOT_DENIED`] and [`EXIT_FORWARD_NOT_DENIED`] are the
//! only codes that prove the account reachable; every other code is a failed
//! or inconclusive stage. Never 1 (generic), 101 (Rust panic), 125
//! (unsupported platform), 200–243 (systemd's own exec failures) or 255 (ssh's
//! own failure code).

/// The account was refused both a public-key login and a forwarding session,
/// corroborated by sshd's own journal records. The only pass.
pub const EXIT_DENIED: i32 = 0;
pub const EXIT_USAGE: i32 = 2;
/// euid not 0, name outside `^duduclaw-emp-[a-z0-9-]{1,48}$`, or no single
/// /etc/passwd entry with a uid in the employee range.
pub const EXIT_IDENTITY: i32 = 3;
/// A tool (or this binary) is not a root-owned, non group/other-writable file.
pub const EXIT_TOOLS: i32 = 4;
/// `/usr/sbin/sshd` does not exist: SSH checks are not applicable on this host.
pub const EXIT_NO_SSHD: i32 = 5;
pub const EXIT_WORKSPACE: i32 = 6;
/// `sshd -T -C …` failed or lacks DenyUsers duduclaw-emp-* / DisableForwarding
/// yes / PermitTTY no, or names no usable port.
pub const EXIT_EFFECTIVE_CONFIG: i32 = 7;
pub const EXIT_KEYGEN: i32 = 8;
/// Login attempt neither denied nor accepted (refused connection, timeout, …).
pub const EXIT_LOGIN_INCONCLUSIVE: i32 = 9;
/// Login attempt exited 0: the account is reachable over SSH.
pub const EXIT_LOGIN_NOT_DENIED: i32 = 10;
/// Login denied by the client, but no matching sshd DenyUsers journal record.
pub const EXIT_LOGIN_JOURNAL: i32 = 11;
pub const EXIT_FORWARD_INCONCLUSIVE: i32 = 12;
/// Forwarding session exited 0 or stayed connected until the kill timer.
pub const EXIT_FORWARD_NOT_DENIED: i32 = 13;
pub const EXIT_FORWARD_JOURNAL: i32 = 14;
/// The 25 s overall deadline expired.
pub const EXIT_DEADLINE: i32 = 15;
pub const EXIT_OUTPUT: i32 = 16;
/// `--relay` mode only: socket, bind, connect or copy failed.
pub const EXIT_RELAY: i32 = 17;

/// The stage name the probe prints for `code` (`{"stage":"<name>"}`), for
/// logs; `None` for a code the probe never uses.
pub fn stage_name(code: i32) -> Option<&'static str> {
    Some(match code {
        EXIT_DENIED => "denied", EXIT_USAGE => "usage", EXIT_IDENTITY => "identity", EXIT_TOOLS => "tools",
        EXIT_NO_SSHD => "no_sshd", EXIT_WORKSPACE => "workspace", EXIT_EFFECTIVE_CONFIG => "effective_config",
        EXIT_KEYGEN => "keygen", EXIT_LOGIN_INCONCLUSIVE => "login_inconclusive",
        EXIT_LOGIN_NOT_DENIED => "login_not_denied", EXIT_LOGIN_JOURNAL => "login_journal",
        EXIT_FORWARD_INCONCLUSIVE => "forward_inconclusive", EXIT_FORWARD_NOT_DENIED => "forward_not_denied",
        EXIT_FORWARD_JOURNAL => "forward_journal", EXIT_DEADLINE => "deadline", EXIT_OUTPUT => "output",
        EXIT_RELAY => "relay",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn probe_exit_table_is_pinned() {
        assert_eq!((EXIT_DENIED, EXIT_NO_SSHD, EXIT_LOGIN_NOT_DENIED, EXIT_FORWARD_NOT_DENIED, EXIT_IDENTITY), (0, 5, 10, 13, 3));
        let named: Vec<i32> = (-1..=300).filter(|code| stage_name(*code).is_some()).collect();
        assert_eq!(named, (0..=17).filter(|code| *code != 1).collect::<Vec<_>>());
        for code in [1, 101, 125, 200, 243, 255] { assert_eq!(stage_name(code), None, "{code}"); }
    }
}
