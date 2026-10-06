//! Operator-only DuDuClaw subcommands on the Bash lane.
//!
//! Some `duduclaw` subcommands change state an AI employee must not touch
//! (the LINE inbox, computer-use workspaces, …). They already refuse when
//! DuDuClaw identity variables are present and their state changes wait for
//! a dashboard approval; this check is one more speed bump in front of them
//! for agent and unverified callers. It reads the command the way bash does
//! ([`super::bash_parse::command_words`]: line continuations, quote
//! concatenation, a redirect stuck to a word), so `duduclaw ops \` + newline,
//! `chan''nel-ingress` and `channel-ingress>out` all match. A variable, an
//! alias, a function, a script or a renamed binary still gets past it.

use crate::agent_guard::GuardDecision;

use super::HookCaller;

/// One protected subcommand: the name shown in the refusal and the words
/// that follow the binary (options before and between them are allowed).
#[derive(Debug, Clone, Copy)]
pub struct OperatorCommand {
    pub name: &'static str,
    pub path: &'static [&'static str],
}

const BINARIES: &[&str] = &[
    "duduclaw",
    "duduclaw.exe",
    "duduclaw-pro",
    "duduclaw-pro.exe",
];

/// The first protected subcommand `command` runs, if any. Any word whose
/// basename is a DuDuClaw binary counts as the binary (so `echo duduclaw ops
/// channel-ingress` matches too: false positives over misses).
pub fn bash_invokes_operator_command(
    command: &str,
    protected: &[OperatorCommand],
) -> Option<&'static str> {
    for words in super::bash_parse::command_words(command) {
        let lowered: Vec<String> = words.iter().map(|w| w.to_ascii_lowercase()).collect();
        let Some(bin_at) = lowered.iter().position(|w| {
            let base = w.rsplit(['/', '\\']).next().unwrap_or(w);
            BINARIES.contains(&base)
        }) else {
            continue;
        };
        let rest: Vec<&str> = lowered[bin_at + 1..]
            .iter()
            .map(String::as_str)
            .filter(|w| !w.starts_with('-'))
            .collect();
        for cmd in protected {
            if !cmd.path.is_empty() && rest.windows(cmd.path.len()).any(|w| w == cmd.path) {
                return Some(cmd.name);
            }
        }
    }
    None
}

/// The Bash-lane decision: agent and unverified callers running one of
/// `protected` get [`GuardDecision::BlockedOperatorCommand`]; an operator
/// (no claimed identity) is not judged here.
pub fn bash_operator_command_decision(
    command: &str,
    caller: &HookCaller,
    protected: &[OperatorCommand],
) -> Option<GuardDecision> {
    let id = match caller {
        HookCaller::Agent(id) | HookCaller::Untrusted(id) => id,
        HookCaller::Absent => return None,
    };
    bash_invokes_operator_command(command, protected).map(|name| {
        GuardDecision::BlockedOperatorCommand {
            caller: id.clone(),
            command: name,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const INGRESS: &[OperatorCommand] = &[OperatorCommand {
        name: "duduclaw ops channel-ingress",
        path: &["ops", "channel-ingress"],
    }];

    #[test]
    fn shell_spellings_that_still_run_the_command_match() {
        for cmd in [
            "duduclaw ops channel-ingress list",
            "duduclaw ops \\\nchannel-ingress list",
            "duduclaw ops chan''nel-ingress list",
            "duduclaw ops \"channel\"-ingress list",
            "duduclaw ops channel-ingress>out list",
            "/opt/bin/duduclaw-pro --home /x ops channel-ingress rerun a",
            "env FOO=1 duduclaw OPS channel-ingress show a",
            "echo hi; duduclaw.exe ops channel-ingress resolve a --note n",
            "(cd /tmp && duduclaw ops channel-ingress list)",
            "d\\uduclaw ops channel-ingress list",
        ] {
            assert_eq!(
                bash_invokes_operator_command(cmd, INGRESS),
                Some("duduclaw ops channel-ingress"),
                "{cmd:?}"
            );
        }
    }

    #[test]
    fn mentions_that_do_not_run_it_do_not_match() {
        for cmd in [
            "duduclaw ops doctor",
            "grep channel-ingress docs/guides/durable-line-ingress.md",
            "duduclaw channel-ingress list",
            "duduclaw ops channel-ingressx list",
        ] {
            assert_eq!(bash_invokes_operator_command(cmd, INGRESS), None, "{cmd:?}");
        }
    }

    #[test]
    fn operators_are_not_judged() {
        assert!(
            bash_operator_command_decision(
                "duduclaw ops channel-ingress list",
                &HookCaller::Absent,
                INGRESS
            )
            .is_none()
        );
        assert!(matches!(
            bash_operator_command_decision(
                "duduclaw ops channel-ingress list",
                &HookCaller::Untrusted("x".into()),
                INGRESS
            ),
            Some(GuardDecision::BlockedOperatorCommand { .. })
        ));
    }
}
