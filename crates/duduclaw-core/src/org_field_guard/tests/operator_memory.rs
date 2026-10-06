use super::*;

// ── P2-B C-1: operator-only memory commands ─────────────────────────────

fn decide(cmd: &str, caller: &HookCaller) -> GuardDecision {
    check_bash_protected_write(cmd, &home(), caller)
}

#[test]
fn employees_cannot_run_forget_source_or_migrate_namespace() {
    let me = agent("sales-rep");
    let untrusted = HookCaller::Untrusted("sales-rep".into());
    for cmd in [
        "duduclaw memory forget-source plan --agent sales-rep --session telegram:1",
        "duduclaw memory forget-source apply --plan p1 --confirm",
        "duduclaw memory forget-source list --agent sales-rep",
        "/usr/local/bin/duduclaw memory migrate-namespace list",
        "duduclaw-pro memory migrate-namespace assign --to x --all --confirm",
        "env -u DUDUCLAW_AGENT_ID duduclaw memory forget-source resume --plan p1",
        "cd /tmp && \"duduclaw\" memory forget-source show --plan p1",
        "DUDUCLAW.EXE MEMORY FORGET-SOURCE list --agent x",
        "echo hi; duduclaw --verbose memory forget-source list --agent x",
    ] {
        for caller in [&me, &untrusted] {
            let d = decide(cmd, caller);
            assert!(
                matches!(d, GuardDecision::BlockedOperatorMemoryCommand { .. }),
                "expected refusal for {cmd:?}: {d:?}"
            );
            assert!(d.block_message().unwrap().contains("管理者"));
        }
    }
}

#[test]
fn other_memory_and_duduclaw_commands_and_operators_pass() {
    let me = agent("sales-rep");
    for cmd in [
        "duduclaw memory --help",
        "duduclaw agent list",
        "echo forget-source memory",
        "grep -r 'memory forget-source' docs/",
        "duduclawx memory forget-source list",
    ] {
        let d = decide(cmd, &me);
        assert!(
            !matches!(d, GuardDecision::BlockedOperatorMemoryCommand { .. }),
            "unexpected refusal for {cmd:?}: {d:?}"
        );
    }
    // An operator (no agent identity) is not judged by this rule.
    let d = decide(
        "duduclaw memory forget-source plan --agent a --session s",
        &HookCaller::Absent,
    );
    assert!(d.is_allowed(), "{d:?}");
}

/// N2 (second review): spellings this rule is KNOWN NOT to catch. The Bash
/// rule is a speed bump; the dashboard approval in front of `apply` is the
/// real gate. Pinned here so the gap is visible, not to endorse it:
/// 1. a global flag between `memory` and the subcommand
///    (`--redact` / `--force-disable-redaction` are clap `global = true`);
/// 2. the binary produced by command substitution (`$(command -v duduclaw)`);
/// 3. the words fed to the binary through a pipe (`… | xargs duduclaw`).
#[test]
fn known_bypasses_are_documented_gaps() {
    let me = agent("sales-rep");
    for cmd in [
        "duduclaw memory --redact on forget-source plan --agent a --session s",
        "\"$(command -v duduclaw)\" memory forget-source list --agent a",
        "echo memory forget-source list --agent a | xargs duduclaw",
    ] {
        let d = crate::org_field_guard::bash_lane::bash_operator_memory_command(
            &cmd.to_ascii_lowercase(),
            &me,
        );
        assert!(
            !matches!(d, GuardDecision::BlockedOperatorMemoryCommand { .. }),
            "if this now blocks {cmd:?}, move it to the refused list above"
        );
    }
}
