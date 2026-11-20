//! R7c: the facts spawnd (`accounts.rs` `ssh_effective_denied`, and the port
//! it passes to the SSH denial probe in the unit instance name) and the root
//! tests read from `sshd -T -C user=<account>,host=localhost,addr=127.0.0.1`.
//! One parser for all of them: none keeps a copy. The probe itself runs
//! unprivileged and cannot run `sshd -T`; [`port_number`] is also its (and the
//! instance name's, `ids::probe_instance`) port rule.
//!
//! sshd prints one lower-case `key value…` per line, and ONE `denyusers` line
//! PER pattern (OpenSSH 9.6p1: `DenyUsers duduclaw-emp-* duduclaw …` prints
//! `denyusers duduclaw-emp-*`, then `denyusers duduclaw`, …), so the employee
//! pattern is accumulated over every `denyusers` line, never taken from the
//! last one.

/// The pattern spawnd's sshd drop-in denies every employee account with.
pub const EMPLOYEE_DENY_PATTERN: &str = "duduclaw-emp-*";

/// What `sshd -T` says about the employee account it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SshdEffective {
    /// Some `denyusers` line carries exactly [`EMPLOYEE_DENY_PATTERN`] among
    /// its arguments (one or several patterns per line; OR over all lines).
    pub employees_denied: bool,
    /// `disableforwarding` appears, and every occurrence is exactly `yes`.
    pub forwarding_disabled: bool,
    /// `permittty` appears, and every occurrence is exactly `no`.
    pub tty_denied: bool,
    /// The FIRST `port` line's single value (1..=65535, plain decimal, no
    /// leading zero); `None` when there is no port line or the first one is
    /// malformed (later port lines never replace it).
    pub port: Option<u16>,
}
impl SshdEffective {
    /// Parse `sshd -T` output. Keys are compared lower-case (as sshd prints
    /// them); values exactly.
    pub fn parse(text: &str) -> Self {
        let mut facts = Self::default();
        let (mut forwarding_seen, mut forwarding_ok, mut tty_seen, mut tty_ok, mut port_seen) = (false, true, false, true, false);
        for line in text.lines() {
            let words: Vec<&str> = line.split_whitespace().collect();
            let Some(key) = words.first() else { continue };
            match key.to_ascii_lowercase().as_str() {
                "denyusers" => facts.employees_denied |= words[1..].contains(&EMPLOYEE_DENY_PATTERN),
                "disableforwarding" => { forwarding_seen = true; forwarding_ok &= words[1..] == ["yes"]; }
                "permittty" => { tty_seen = true; tty_ok &= words[1..] == ["no"]; }
                "port" if !port_seen => {
                    port_seen = true;
                    facts.port = match words[1..] { [value] => port_number(value), _ => None };
                }
                _ => {}
            }
        }
        facts.forwarding_disabled = forwarding_seen && forwarding_ok;
        facts.tty_denied = tty_seen && tty_ok;
        facts
    }
    /// The account is denied: the employee pattern in `DenyUsers`,
    /// `DisableForwarding yes` and `PermitTTY no` (the port is not part of it).
    pub fn denies_employees(&self) -> bool { self.employees_denied && self.forwarding_disabled && self.tty_denied }
}

/// 1..=65535 written as plain decimal digits without a leading zero.
pub fn port_number(text: &str) -> Option<u16> {
    if text.is_empty() || text.len() > 5 || text.starts_with('0') || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u16>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `/usr/sbin/sshd -T -C user=duduclaw-emp-xcheck,host=localhost,addr=127.0.0.1`
    /// output (Ubuntu 24.04, OpenSSH 9.6p1, spawnd's drop-in), the relevant
    /// lines in the order printed.
    const REAL: &str = "port 22\n\
        permittty no\n\
        disableforwarding yes\n\
        denyusers duduclaw-emp-*\n\
        denyusers duduclaw\n\
        denyusers duduclaw-runsup\n\
        denyusers duduclaw-egress\n\
        denyusers duduclaw-dl\n";

    #[test]
    fn ssh_config_real_openssh_9_6_output_denies_with_one_line_per_pattern() {
        let facts = SshdEffective::parse(REAL);
        assert_eq!(facts, SshdEffective { employees_denied: true, forwarding_disabled: true, tty_denied: true, port: Some(22) });
        assert!(facts.denies_employees());
    }

    #[test]
    fn ssh_config_without_the_employee_pattern_line_is_not_denied() {
        let facts = SshdEffective::parse(&REAL.replace("denyusers duduclaw-emp-*\n", ""));
        assert!(!facts.employees_denied);
        assert!(!facts.denies_employees());
        assert_eq!(facts.port, Some(22));
    }

    #[test]
    fn ssh_config_single_line_multi_pattern_form() {
        let text = "denyusers duduclaw-emp-* duduclaw\ndisableforwarding yes\npermittty no\n";
        assert!(SshdEffective::parse(text).denies_employees());
        // The pattern may be any argument of the line.
        assert!(SshdEffective::parse(&text.replace("duduclaw-emp-* duduclaw", "duduclaw duduclaw-emp-*")).denies_employees());
        // Order of the denyusers lines does not matter.
        let reordered = "denyusers duduclaw-dl\ndenyusers duduclaw-emp-*\ndenyusers duduclaw\ndisableforwarding yes\npermittty no\n";
        assert!(SshdEffective::parse(reordered).denies_employees());
    }

    #[test]
    fn ssh_config_lookalike_patterns_are_not_the_employee_pattern() {
        for line in ["denyusers fake-duduclaw-emp-*", "denyusers duduclaw-emp-", "denyusers duduclaw-emp-*x",
            "denyusers duduclaw-emp-*,duduclaw", "allowusers duduclaw-emp-*", "#denyusers duduclaw-emp-*", "denyusers"] {
            let text = format!("{line}\ndisableforwarding yes\npermittty no\n");
            assert!(!SshdEffective::parse(&text).denies_employees(), "{line}");
        }
        assert!(!SshdEffective::parse(&REAL.replace("denyusers duduclaw-emp-*", "denyusers fake-duduclaw-emp-*")).denies_employees());
    }

    #[test]
    fn ssh_config_every_forwarding_and_tty_occurrence_must_hold() {
        assert!(!SshdEffective::parse(&format!("{REAL}disableforwarding no\n")).denies_employees());
        assert!(!SshdEffective::parse(&format!("{REAL}permittty yes\n")).denies_employees());
        // An earlier contrary occurrence refuses as well.
        assert!(!SshdEffective::parse(&format!("permittty yes\n{REAL}")).denies_employees());
        assert!(!SshdEffective::parse(&format!("disableforwarding no\n{REAL}")).denies_employees());
        // Repeated agreeing occurrences are fine.
        assert!(SshdEffective::parse(&format!("{REAL}permittty no\ndisableforwarding yes\n")).denies_employees());
        // Missing, extra argument or another value.
        for (from, to) in [("disableforwarding yes\n", ""), ("permittty no\n", ""), ("disableforwarding yes", "disableforwarding yes no"),
            ("permittty no", "permittty"), ("disableforwarding yes", "disableforwarding Yes")] {
            assert!(!SshdEffective::parse(&REAL.replace(from, to)).denies_employees(), "{from:?} -> {to:?}");
        }
        // Keys compare lower-case.
        assert!(!SshdEffective::parse(&format!("{REAL}PermitTTY yes\n")).denies_employees());
        assert!(SshdEffective::parse(&REAL.replace("denyusers duduclaw-emp-*", "DenyUsers duduclaw-emp-*")).denies_employees());
        assert!(!SshdEffective::parse("").denies_employees());
    }

    #[test]
    fn ssh_config_port_is_the_first_port_line() {
        assert_eq!(SshdEffective::parse(&REAL.replace("port 22\n", "")).port, None);
        assert!(SshdEffective::parse(&REAL.replace("port 22\n", "")).denies_employees(), "the port is not part of the denial");
        assert_eq!(SshdEffective::parse(&format!("port 2222\n{REAL}")).port, Some(2222));
        for bad in ["port 0", "port 70000", "port 22 23", "port 022", "port", "port +22"] {
            assert_eq!(SshdEffective::parse(&REAL.replace("port 22", bad)).port, None, "{bad}");
            // A malformed first line is not replaced by a later one.
            assert_eq!(SshdEffective::parse(&format!("{bad}\n{REAL}")).port, None, "{bad}");
        }
    }
}
