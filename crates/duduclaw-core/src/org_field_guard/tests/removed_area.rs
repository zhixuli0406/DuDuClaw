use super::*;

fn agent(id: &str) -> HookCaller {
    HookCaller::Agent(id.to_string())
}

fn is_removed_area(d: &GuardDecision) -> bool {
    matches!(d, GuardDecision::BlockedRemovedAgentArea { .. })
}

// ── Write / Edit / MultiEdit lane ──────────────────────────────

#[test]
fn write_into_trash_entry_is_blocked_for_agent() {
    let h = home();
    for p in [
        h.join("agents/_trash/writer_20261002101010/agent.toml"),
        h.join("agents/_trash/writer_20261002101010/CONTRACT.toml"),
        h.join("agents/_trash/note.txt"),
        h.join("agents/_TRASH/writer_20261002101010/SOUL.md"),
    ] {
        let d = check_caller_scope(&p, &h, &agent("ceo"));
        assert!(is_removed_area(&d), "{}: {d:?}", p.display());
        let msg = d.block_message().unwrap();
        assert!(msg.contains("管理者") && !msg.contains("_trash"), "{msg}");
    }
}

#[test]
fn write_into_trash_is_untouched_for_operator() {
    let h = home();
    let p = h.join("agents/_trash/writer_20261002101010/agent.toml");
    assert_eq!(check_caller_scope(&p, &h, &HookCaller::Absent), GuardDecision::NotAgentFile);
}

// ── Bash lane (speed bump) ─────────────────────────────────────

#[test]
fn bash_erasing_or_restoring_trash_is_blocked() {
    let h = home();
    for cmd in [
        "rm -rf ~/.duduclaw/agents/_trash/writer_20261002101010",
        "rm -rf /Users/alice/.duduclaw/agents/_trash",
        "rm -rf ../_trash/writer_20261002101010",
        "cd .. && rm -rf _trash",
        "mv ../_trash/writer_20261002101010 ../writer",
        "mv agents/_trash/writer_20261002101010 agents/writer",
        "python3 -c 'import shutil; shutil.rmtree(\"../_trash\")'",
    ] {
        let d = check_bash_protected_write(cmd, &h, &agent("ceo"));
        assert!(is_removed_area(&d), "{cmd}: {d:?}");
    }
}

#[test]
fn bash_trash_rule_is_component_exact_and_needs_a_write_verb() {
    let h = home();
    for cmd in [
        "ls ../_trash",                    // read-only
        "rm -rf build/my_trash/x",         // different component
        "rm -rf _trash_old",               // different component
        "rm -rf ./trash",                  // different component
    ] {
        let d = check_bash_protected_write(cmd, &h, &agent("ceo"));
        assert!(!is_removed_area(&d), "{cmd}: {d:?}");
    }
    // Operators running by hand are not this rule's business.
    let d = check_bash_protected_write("rm -rf ../_trash", &h, &HookCaller::Absent);
    assert!(!is_removed_area(&d), "{d:?}");
}

#[test]
fn bash_moving_a_live_agent_dir_away_is_already_blocked() {
    // `mv agents/x agents/x.bak` then `create_agent x`: the move is the
    // foreign-directory rule's case. (The follow-up create is refused on the
    // MCP side as well — see `agent_trash`.)
    let h = home();
    for cmd in [
        "mv ~/.duduclaw/agents/writer ~/.duduclaw/agents/writer.bak",
        "mv agents/writer /tmp/writer",
    ] {
        let d = check_bash_protected_write(cmd, &h, &agent("ceo"));
        assert!(
            matches!(d, GuardDecision::BlockedForeignAgentDir { ref owner, .. } if owner == "writer"),
            "{cmd}: {d:?}"
        );
    }
}
