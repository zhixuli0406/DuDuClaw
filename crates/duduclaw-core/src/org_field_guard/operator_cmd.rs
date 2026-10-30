//! Operator-only DuDuClaw subcommands on the Bash lane.
//!
//! Some `duduclaw` subcommands change state an AI employee must not touch
//! (the LINE inbox, computer-use workspaces, …). They already refuse when
//! DuDuClaw identity variables are present and their state changes wait for
//! a dashboard approval; this check is one more speed bump in front of them
//! for agent and unverified callers. It lexically reads common bash forms
//! ([`super::bash_parse::command_words`]: line continuations, quote
//! concatenation, a redirect stuck to a word), so `duduclaw \` + newline,
//! `chan''nel-ingress` and `channel-ingress>out` all match. A variable, an
//! alias, a function, a script or a renamed binary still gets past it.
//! Shell `-c`, `eval` and each echo/printf argument share a two-level
//! re-parse limit; echo/printf arguments over 64 KiB are not re-parsed.
//! After `-c` (including short-flag clusters), shell option words and their
//! known values are skipped through `--` before taking the command string.
//! ANSI-C `$'…'` quoting and dynamic assembly via command substitution are
//! not supported. Malformed quotes do not fail closed. These remain known
//! gaps; the real gates are the in-process AI-session check and dashboard
//! Admin approval for state changes.
//! Literal binary words are matched even in echo/printf or data arguments:
//! `python report.py duduclaw channel-ingress list` is an accepted false
//! positive (false positives over misses). Only grep/egrep/fgrep mentions
//! are skipped; other interpreters' program strings are not re-parsed.

use crate::agent_guard::GuardDecision;

use super::HookCaller;

/// One protected subcommand: the name shown in the refusal and the words
/// that follow the binary (options before and between them are allowed).
#[derive(Debug, Clone, Copy)]
pub struct OperatorCommand {
    pub name: &'static str,
    pub path: &'static [&'static str],
}

/// Offline egress recovery is denied to employees and unverified callers.
pub const EGRESS_LEASE_COMMANDS: &[OperatorCommand] = &[
    OperatorCommand { name: "duduclaw egress-leases", path: &["egress-leases"] },
];

/// The commands that issue an MCP key. A key is a credential for the MCP
/// server with whatever scopes the command grants, so an AI employee must
/// not mint one for itself: `mcp init` also refuses in an AI session
/// (`ai_session_guard`), `mcp issue-refresh-token` is for operators only.
pub const MCP_KEY_COMMANDS: &[OperatorCommand] = &[
    OperatorCommand {
        name: "duduclaw mcp init",
        path: &["mcp", "init"],
    },
    OperatorCommand {
        name: "duduclaw mcp issue-refresh-token",
        path: &["mcp", "issue-refresh-token"],
    },
];

const BINARIES: &[&str] = &[
    "duduclaw",
    "duduclaw.exe",
    "duduclaw-pro",
    "duduclaw-pro.exe",
];

/// Skip CLI options without treating their values as subcommands. `--home`
/// is retained for wrapper/edition compatibility; the CLI's current global
/// value option is `--redact`. Unknown flags are conservatively skipped.
fn skip_options(words: &[String], mut at: usize) -> usize {
    while let Some(word) = words.get(at) {
        if word == "--" {
            return at + 1;
        }
        if !word.starts_with('-') {
            break;
        }
        at += if matches!(word.as_str(), "--home" | "--redact") {
            2
        } else {
            1
        };
    }
    at
}

/// The first protected subcommand `command` runs, if any. A literal DuDuClaw
/// binary may follow a launcher (`env`, `npx`, …). Ignore literal mentions
/// in grep/egrep/fgrep. Match the path after leading CLI options, never a
/// protected word used as another subcommand's argument.
pub fn bash_invokes_operator_command(
    command: &str,
    protected: &[OperatorCommand],
) -> Option<&'static str> {
    bash_invokes_operator_command_at_depth(command, protected, 0)
}

const MAX_SHELL_REPARSE_DEPTH: usize = 2;
const MAX_PRINT_REPARSE_BYTES: usize = 64 * 1024;

/// Only known shell command text is re-parsed, never Python/Node strings.
/// `eval` joins its arguments with spaces before interpreting them.
pub(super) fn shell_payload(cmd: &str, args: &[String]) -> Option<String> {
    if cmd == "eval" {
        return (!args.is_empty()).then(|| args.join(" "));
    }
    if !matches!(cmd, "bash" | "sh" | "zsh" | "dash") {
        return None;
    }
    let mut args = args.iter();
    let mut command_mode = false;
    while let Some(arg) = args.next() {
        // Before -c, a script path or -- ends this scan. After -c, --
        // ends options and the following word is the command string.
        if arg == "--" {
            return if command_mode { args.next().cloned() } else { None };
        }
        if arg == "-" || !arg.starts_with('-') {
            return command_mode.then(|| arg.clone());
        }
        let short_flags = (!arg.starts_with("--")).then(|| &arg[1..]);
        if short_flags.is_some_and(|flags| flags.contains('c')) {
            command_mode = true;
        }
        // -o/-O consume the next word even inside a short-flag cluster.
        if short_flags.is_some_and(|flags| flags.contains(['o', 'O']))
            || matches!(arg.as_str(), "--rcfile" | "--init-file")
        {
            args.next();
        }
    }
    None
}

fn bash_invokes_operator_command_at_depth(
    command: &str,
    protected: &[OperatorCommand],
    depth: usize,
) -> Option<&'static str> {
    for words in super::bash_parse::command_words(command) {
        let parsed = super::bash_cmd::command_and_args(&words);
        if parsed.as_ref().is_some_and(|p| {
            matches!(p.cmd.as_str(), "grep" | "egrep" | "fgrep")
        }) {
            continue;
        }
        let lowered: Vec<String> = words.iter().map(|w| w.to_ascii_lowercase()).collect();
        // A launcher argument may itself be named `duduclaw` (for example
        // `env -u duduclaw duduclaw …`), so try every literal binary position.
        for (bin_at, word) in lowered.iter().enumerate() {
            let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
            if !BINARIES.contains(&base) {
                continue;
            }
            for cmd in protected {
                let mut at = bin_at + 1;
                if !cmd.path.is_empty()
                    && cmd.path.iter().all(|part| {
                        at = skip_options(&lowered, at);
                        let matched = lowered.get(at).is_some_and(|word| word.as_str() == *part);
                        at += 1;
                        matched
                    })
                {
                    return Some(cmd.name);
                }
            }
        }
        if depth < MAX_SHELL_REPARSE_DEPTH {
            if let Some(p) = parsed.as_ref().filter(|p| matches!(p.cmd.as_str(), "echo" | "printf")) {
                // Deliberately scan each string even without a pipe consumer:
                // literal data may be executable elsewhere; false positives
                // are accepted, but recursive work stays length/depth bounded.
                for arg in p.args.iter().filter(|arg| arg.len() <= MAX_PRINT_REPARSE_BYTES) {
                    if let Some(name) = bash_invokes_operator_command_at_depth(arg, protected, depth + 1) {
                        return Some(name);
                    }
                }
            }
            if let Some(payload) = parsed.and_then(|p| shell_payload(&p.cmd, p.args)) {
                if let Some(name) = bash_invokes_operator_command_at_depth(&payload, protected, depth + 1) {
                    return Some(name);
                }
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
mod bash_operator_tests {
    use super::*;

    const INGRESS: &[OperatorCommand] = &[OperatorCommand {
        name: "duduclaw channel-ingress",
        path: &["channel-ingress"],
    }];

    #[test]
    fn shell_spellings_and_conservative_global_flags_match() {
        for cmd in [
            "duduclaw channel-ingress list",
            "duduclaw --verbose channel-ingress list",
            "duduclaw --redact off channel-ingress list",
            "duduclaw --redact=off channel-ingress list",
            "duduclaw-pro.exe channel-ingress list",
            "duduclaw \\\nchannel-ingress list",
            "duduclaw chan''nel-ingress list",
            "duduclaw \"channel\"-ingress list",
            "duduclaw channel-ingress>out list",
            "/opt/bin/duduclaw-pro --home /x channel-ingress rerun a",
            "env FOO=1 duduclaw channel-ingress show a",
            "env -u duduclaw duduclaw channel-ingress list",
            "exec -a duduclaw duduclaw channel-ingress list",
            "npx duduclaw channel-ingress list",
            "echo hi; duduclaw.exe channel-ingress resolve a --note n",
            "(cd /tmp && duduclaw channel-ingress list)",
            "d\\uduclaw channel-ingress list",
            "echo duduclaw channel-ingress list",
            "printf '%s ' duduclaw channel-ingress list | bash",
            "printf '%s' 'duduclaw channel-ingress list'",
            "printf '%s' 'duduclaw channel-ingress list' | bash",
            "echo 'duduclaw channel-ingress list' | sh",
            "echo duduclaw channel-ingress list | sh",
            // Accepted conservative false positive: these are data arguments.
            "python report.py duduclaw channel-ingress list",
        ] {
            assert_eq!(
                bash_invokes_operator_command(cmd, INGRESS),
                Some("duduclaw channel-ingress"),
                "{cmd:?}"
            );
            for caller in [HookCaller::Agent("alice".into()), HookCaller::Untrusted("x".into())] {
                assert!(matches!(
                    bash_operator_command_decision(cmd, &caller, INGRESS),
                    Some(GuardDecision::BlockedOperatorCommand { command: "duduclaw channel-ingress", .. })
                ), "{cmd:?}: {caller:?}");
            }
            assert!(bash_operator_command_decision(cmd, &HookCaller::Absent, INGRESS).is_none());
        }
    }

    #[test]
    fn mentions_that_do_not_run_it_do_not_match() {
        for cmd in [
            "duduclaw doctor",
            "grep channel-ingress docs/guides/durable-line-ingress.md",
            "echo channel-ingress",
            "grep channel-ingress duduclaw",
            "grep duduclaw channel-ingress list",
            "egrep duduclaw channel-ingress list",
            "fgrep duduclaw channel-ingress list",
            "python -c 'duduclaw channel-ingress list'",
            "node -e 'duduclaw channel-ingress list'",
            "bash script.sh -c 'duduclaw channel-ingress list'",
            "bash -- -c 'duduclaw channel-ingress list'",
            "bash -c '' 'duduclaw channel-ingress list'",
            "bash -c 'echo safe' 'duduclaw channel-ingress list'",
            "bash -c -o 'duduclaw channel-ingress list' 'echo safe'",
            "bash -c -xo 'duduclaw channel-ingress list' 'echo safe'",
            "duduclaw agent inspect channel-ingress",
            "duduclaw --redact channel-ingress doctor",
            "duduclaw --home channel-ingress doctor",
            "duduclaw --redact=channel-ingress doctor",
            "duduclaw computer-workspaces list",
            "duduclaw channel-ingressx list",
        ] {
            assert_eq!(bash_invokes_operator_command(cmd, INGRESS), None, "{cmd:?}");
            for caller in [HookCaller::Agent("alice".into()), HookCaller::Untrusted("x".into()), HookCaller::Absent] {
                assert!(bash_operator_command_decision(cmd, &caller, INGRESS).is_none(), "{cmd:?}");
            }
        }
        let old_spelling = ["duduclaw", "ops", "channel-ingress", "list"].join(" ");
        assert_eq!(bash_invokes_operator_command(&old_spelling, INGRESS), None);
    }

    #[test]
    fn shell_c_and_eval_payloads_are_reparsed() {
        for cmd in [
            r#"bash -c "duduclaw channel-ingress list""#,
            "sh -c 'duduclaw channel-ingress list'",
            "zsh -c 'duduclaw channel-ingress list'",
            "dash -c 'duduclaw channel-ingress list'",
            "eval 'duduclaw channel-ingress list'",
            "eval 'duduclaw' 'channel-ingress list'",
            "env -i /bin/bash -c 'duduclaw channel-ingress list'",
            "bash -lc 'duduclaw channel-ingress list'",
            "bash -c -- 'duduclaw channel-ingress list'",
            "bash -c -x 'duduclaw channel-ingress list'",
            "bash -xc 'duduclaw channel-ingress list'",
            "bash -o pipefail -c 'duduclaw channel-ingress list'",
            "bash -co pipefail 'duduclaw channel-ingress list'",
            "bash -c -xo pipefail 'duduclaw channel-ingress list'",
            "bash -cO extglob 'duduclaw channel-ingress list'",
            "bash -c -e -l -o pipefail -- 'duduclaw channel-ingress list'",
            "bash -o errexit -c 'duduclaw channel-ingress list'",
            "bash -c 'echo safe; duduclaw channel-ingress list'",
            "sh -c 'duduclaw mcp init'",
        ] {
            let protected = if cmd.contains("mcp") { MCP_KEY_COMMANDS } else { INGRESS };
            for caller in [HookCaller::Agent("alice".into()), HookCaller::Untrusted("x".into())] {
                assert!(matches!(
                    bash_operator_command_decision(cmd, &caller, protected),
                    Some(GuardDecision::BlockedOperatorCommand { .. })
                ), "{cmd:?}: {caller:?}");
            }
            assert!(bash_operator_command_decision(cmd, &HookCaller::Absent, protected).is_none());
        }
    }

    #[test]
    fn quoted_echo_profiles_are_blocked_for_agent_and_untrusted() {
        let profiles = &[OperatorCommand {
            name: "duduclaw computer-profiles",
            path: &["computer-profiles"],
        }];
        let cmd = "echo 'duduclaw computer-profiles list' | sh";
        for caller in [HookCaller::Agent("alice".into()), HookCaller::Untrusted("x".into())] {
            assert!(matches!(
                bash_operator_command_decision(cmd, &caller, profiles),
                Some(GuardDecision::BlockedOperatorCommand { command: "duduclaw computer-profiles", .. })
            ));
        }
        assert!(bash_operator_command_decision(cmd, &HookCaller::Absent, profiles).is_none());
    }

    #[test]
    fn echo_printf_reparse_is_bounded_and_shares_shell_depth() {
        for printer in ["echo", "printf '%s'"] {
            let command = "duduclaw channel-ingress list";
            let payload = format!("{}{command}", " ".repeat(MAX_PRINT_REPARSE_BYTES - command.len()));
            assert_eq!(payload.len(), MAX_PRINT_REPARSE_BYTES);
            assert_eq!(
                bash_invokes_operator_command(&format!("{printer} '{payload}'"), INGRESS),
                Some("duduclaw channel-ingress")
            );
            assert_eq!(
                bash_invokes_operator_command(&format!("{printer} ' {payload}'"), INGRESS),
                None
            );
        }
        let mut cmd = "duduclaw channel-ingress list".to_string();
        for depth in 0..=4 {
            assert_eq!(
                bash_invokes_operator_command(&cmd, INGRESS),
                (depth <= MAX_SHELL_REPARSE_DEPTH).then_some("duduclaw channel-ingress"),
                "depth={depth}"
            );
            let printer = if depth % 2 == 0 { "echo" } else { "printf '%s'" };
            cmd = format!("{printer} '{}'", cmd.replace('\'', "'\\''"));
        }
        assert_eq!(
            bash_invokes_operator_command("bash -c 'echo \"duduclaw channel-ingress list\"'", INGRESS),
            Some("duduclaw channel-ingress")
        );
    }

    #[test]
    fn shell_payload_reparse_depth_is_limited_to_two() {
        // Each shell wrapper requires one additional parse of a quoted payload.
        fn wrap(payload: &str) -> String {
            format!("bash -c '{}'", payload.replace('\'', "'\\''"))
        }
        let mut cmd = "duduclaw channel-ingress list".to_string();
        for depth in 0..=8 {
            assert_eq!(
                bash_invokes_operator_command(&cmd, INGRESS),
                (depth <= 2).then_some("duduclaw channel-ingress"),
                "depth={depth}: {cmd:?}"
            );
            cmd = wrap(&cmd);
        }
        let nested_eval = r#"eval 'sh -c "duduclaw channel-ingress list"'"#;
        assert_eq!(bash_invokes_operator_command(nested_eval, INGRESS), Some("duduclaw channel-ingress"));
    }

    #[test]
    fn mcp_key_commands_match_and_neighbours_do_not() {
        for (cmd, want) in [
            (
                "duduclaw mcp init --client claude-code --yes",
                "duduclaw mcp init",
            ),
            ("npx duduclaw mcp init", "duduclaw mcp init"),
            ("env -u duduclaw duduclaw mcp init", "duduclaw mcp init"),
            ("exec -a duduclaw duduclaw mcp init", "duduclaw mcp init"),
            ("duduclaw mcp --redact off init", "duduclaw mcp init"),
            (
                "/usr/local/bin/duduclaw --home /x mcp \\\ninit",
                "duduclaw mcp init",
            ),
            (
                "duduclaw-pro MCP In''it --client print",
                "duduclaw mcp init",
            ),
            (
                "duduclaw mcp issue-refresh-token --client-id x --scopes admin",
                "duduclaw mcp issue-refresh-token",
            ),
            (
                "echo ok && duduclaw.exe mcp issue-refresh-token",
                "duduclaw mcp issue-refresh-token",
            ),
        ] {
            assert_eq!(
                bash_invokes_operator_command(cmd, MCP_KEY_COMMANDS),
                Some(want),
                "{cmd:?}"
            );
        }
        for cmd in [
            "duduclaw mcp-server",
            "duduclaw mcp list-tokens",
            "duduclaw mcp revoke-token abc",
            "grep 'mcp init' docs/guides/mcp-standalone.md",
            "duduclaw mcp initx",
            "duduclaw agent inspect mcp init",
            "duduclaw --redact mcp doctor init",
            "duduclaw mcp list-tokens init",
        ] {
            assert_eq!(
                bash_invokes_operator_command(cmd, MCP_KEY_COMMANDS),
                None,
                "{cmd:?}"
            );
        }
        assert!(matches!(
            bash_operator_command_decision(
                "duduclaw mcp init --client codex",
                &HookCaller::Agent("ceo".into()),
                MCP_KEY_COMMANDS
            ),
            Some(GuardDecision::BlockedOperatorCommand {
                command: "duduclaw mcp init",
                ..
            })
        ));
        assert!(
            bash_operator_command_decision(
                "duduclaw mcp init --client codex",
                &HookCaller::Absent,
                MCP_KEY_COMMANDS
            )
            .is_none()
        );
    }

    #[test]
    fn operators_are_not_judged() {
        assert!(
            bash_operator_command_decision(
                "duduclaw channel-ingress list",
                &HookCaller::Absent,
                INGRESS
            )
            .is_none()
        );
        assert!(matches!(
            bash_operator_command_decision(
                "duduclaw channel-ingress list",
                &HookCaller::Untrusted("x".into()),
                INGRESS
            ),
            Some(GuardDecision::BlockedOperatorCommand { .. })
        ));
    }
}
