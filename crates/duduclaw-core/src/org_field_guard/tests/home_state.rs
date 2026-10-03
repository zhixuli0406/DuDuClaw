use super::*;

// ── G1: `<home>` state, evidence and yardsticks are not agent-writable ──
//
// `check_caller_scope` used to know only `<home>/config.toml` and other
// agents' directories, so `tool_calls.jsonl` (the evidence the grounding
// precheck, the judge's tool-activity digest and the recent-actions feed all
// read), the eval suites under `<home>/evals/` (including held-out sets and
// other agents' suites), every SQLite store and every breaker state file were
// writable by any agent identity. Now an agent-identified caller may write
// under `<home>` only inside its own agent directory and the shared
// `attachments/` fallback; everything else is refused.

fn untrusted(id: &str) -> HookCaller {
    HookCaller::Untrusted(id.to_string())
}

/// One representative per category of the inventory in the module doc.
const PROTECTED_HOME_PATHS: &[&str] = &[
    // audit / evidence
    "tool_calls.jsonl",
    "tool_calls.jsonl.1",
    "security_audit.jsonl",
    "audit/browser/audit.jsonl",
    "budget_events.jsonl",
    "artifacts.jsonl",
    "role_turns.jsonl",
    // yardsticks: eval suites, own and foreign, held-out spellings
    "evals/sales-rep/case_a.toml",
    "evals/sales-rep/case_a.transcript.jsonl",
    "evals/sales-rep/held-out/h1.toml",
    "evals/sales-rep/_holdout/h1.toml",
    "evals/ceo/case_b.toml",
    "secaudit/reports/r.json",
    // stores
    "tasks.db",
    "approvals.db",
    "events.db",
    "memory.db",
    "evolution.db",
    "prediction.db",
    "decisions.db",
    "cost_telemetry.db",
    "message_queue.db",
    "users.db",
    // breaker / guard state
    "budget_breaker_state.json",
    "dispatch_guard.json",
    "KILLSWITCH.toml",
    "threat_level",
    // authority / identity / licensing
    ".org-seeded",
    "preset_bindings.toml",
    "license.json",
    "jwt_secret",
    ".keyfile",
    "inference.toml",
    // shared surfaces whose only legitimate writer is a gated MCP tool or
    // the gateway itself
    "shared/wiki/policies/x.md",
    "shared/wiki/.scope.toml",
    "skills/evil/SKILL.md",
    "bus_queue.jsonl",
    "cron_tasks.jsonl",
    "mail/inbound/x.eml",
    // anything unknown is refused too (allow-list, not deny-list)
    "some-future-store.db",
    "agents/README.md",
    "agents/.ephemeral/notes.txt",
];

#[test]
fn home_state_is_refused_for_an_agent_caller() {
    for rel in PROTECTED_HOME_PATHS {
        let d = check_caller_scope(&home().join(rel), &home(), &agent("sales-rep"));
        assert!(
            matches!(d, GuardDecision::BlockedHomeStateWrite { .. }),
            "{rel}: {d:?}"
        );
        assert!(!d.is_allowed());
    }
}

#[test]
fn home_state_block_message_points_at_the_writable_places() {
    let d = check_caller_scope(
        &home().join("tool_calls.jsonl"),
        &home(),
        &agent("sales-rep"),
    );
    let msg = d.block_message().unwrap();
    assert!(msg.contains("tool_calls.jsonl"), "{msg}");
    assert!(msg.contains("sales-rep"), "{msg}");
    assert!(msg.contains("attachments"), "{msg}");
}

#[test]
fn home_state_is_refused_for_an_untrusted_caller_too() {
    for rel in ["tool_calls.jsonl", "evals/sales-rep/held-out/h1.toml", "tasks.db"] {
        let d = check_caller_scope(&home().join(rel), &home(), &untrusted("sales-rep"));
        assert!(
            matches!(d, GuardDecision::BlockedHomeStateWrite { .. }),
            "{rel}: {d:?}"
        );
    }
    // The agents root without an owner is still "an agent folder" for an
    // unverifiable caller.
    let d = check_caller_scope(
        &home().join("agents/.ephemeral/notes.txt"),
        &home(),
        &untrusted("sales-rep"),
    );
    assert!(matches!(d, GuardDecision::BlockedUntrustedCaller { .. }), "{d:?}");
}

#[test]
fn own_dir_and_shared_attachments_stay_writable() {
    for rel in [
        "agents/sales-rep/notes.md",
        "agents/sales-rep/attachments/report.docx",
        "agents/sales-rep/evals/x.toml",
        "attachments/1755000000000_report.docx",
        "attachments/sub/x.pdf",
    ] {
        assert_eq!(
            check_caller_scope(&home().join(rel), &home(), &agent("sales-rep")),
            GuardDecision::NotAgentFile,
            "{rel}"
        );
    }
    // An ephemeral agent's own scaffold.
    assert_eq!(
        check_caller_scope(
            &home().join("agents/.ephemeral/eph-abc123/out.md"),
            &home(),
            &agent("eph-abc123")
        ),
        GuardDecision::NotAgentFile
    );
    // Untrusted callers may still drop a deliverable in the shared fallback
    // (it is not identity-bound).
    assert_eq!(
        check_caller_scope(
            &home().join("attachments/a.pdf"),
            &home(),
            &untrusted("sales-rep")
        ),
        GuardDecision::NotAgentFile
    );
}

#[test]
fn replacing_the_attachments_dir_itself_is_refused() {
    let d = check_caller_scope(&home().join("attachments"), &home(), &agent("sales-rep"));
    assert!(matches!(d, GuardDecision::BlockedHomeStateWrite { .. }), "{d:?}");
}

#[test]
fn home_state_rule_is_case_insensitive_and_normalizing() {
    for p in [
        PathBuf::from("/users/ALICE/.DuDuClaw/Tool_Calls.jsonl"),
        PathBuf::from("/Users/alice/.duduclaw/agents/sales-rep/../../evals/ceo/x.toml"),
        PathBuf::from("/Users/alice/.duduclaw/attachments/../tasks.db"),
    ] {
        let d = check_caller_scope(&p, &home(), &agent("sales-rep"));
        assert!(
            matches!(d, GuardDecision::BlockedHomeStateWrite { .. }),
            "{}: {d:?}",
            p.display()
        );
    }
}

#[test]
fn paths_outside_home_are_untouched() {
    for p in [
        PathBuf::from("/Users/alice/Project/app/src/main.rs"),
        PathBuf::from("/Users/alice/Project/app/tool_calls.jsonl"),
        PathBuf::from("/Users/alice/Project/app/evals/held-out/x.toml"),
        PathBuf::from("/Users/alice/.duduclawx/tasks.db"),
        PathBuf::from("/tmp/out.txt"),
    ] {
        assert_eq!(
            check_caller_scope(&p, &home(), &agent("sales-rep")),
            GuardDecision::NotAgentFile,
            "{}",
            p.display()
        );
    }
}

#[test]
fn operator_is_unaffected_by_the_home_state_rule() {
    for rel in PROTECTED_HOME_PATHS {
        assert_eq!(
            check_caller_scope(&home().join(rel), &home(), &HookCaller::Absent),
            GuardDecision::NotAgentFile,
            "{rel}"
        );
    }
    for cmd in [
        "echo x >> /Users/alice/.duduclaw/tool_calls.jsonl",
        "rm -rf ~/.duduclaw/evals/sales-rep/held-out",
        "echo x > ../../tasks.db",
    ] {
        assert_eq!(
            check_bash_protected_write(cmd, &home(), &HookCaller::Absent),
            GuardDecision::NotAgentFile,
            "{cmd}"
        );
    }
}

// ── Bash lane, same rule ─────────────────────────────────────────

#[test]
fn bash_write_to_home_state_is_blocked_for_an_agent() {
    for cmd in [
        "echo x >> '/Users/alice/.duduclaw/tool_calls.jsonl'",
        "echo x >> /Users/alice/.duduclaw/tool_calls.jsonl",
        "sed -i '' 's/a/b/' ~/.duduclaw/evals/sales-rep/held-out/h1.toml",
        "cp fake.toml $DUDUCLAW_HOME/evals/ceo/case_b.toml",
        "cp fake.toml ${DUDUCLAW_HOME}/evals/ceo/case_b.toml",
        "rm -f $HOME/.duduclaw/tasks.db",
        // An agent's Bash cwd is `<home>/agents/<self>`, so `../..` is home.
        "echo x >> ../../tool_calls.jsonl",
        "cp x.toml ../../evals/sales-rep/_holdout/h1.toml",
        "cd ../.. && echo x > anything",
        // Rotated audit file, named anywhere.
        "truncate -s 0 tool_calls.jsonl.1",
        "rm -rf ~/.duduclaw",
    ] {
        let d = check_bash_protected_write(cmd, &home(), &agent("sales-rep"));
        assert!(
            matches!(d, GuardDecision::BlockedHomeStateWrite { .. }),
            "{cmd}: {d:?}"
        );
    }
}

#[test]
fn bash_relative_reach_into_a_peer_dir_is_blocked() {
    // `agents/<id>/` spelled through the cwd, which the old segment scan
    // could not see.
    match check_bash_protected_write("echo x > ../ceo/notes.md", &home(), &agent("sales-rep")) {
        GuardDecision::BlockedForeignAgentDir { owner, .. } => assert_eq!(owner, "ceo"),
        other => panic!("expected BlockedForeignAgentDir, got {other:?}"),
    }
    match check_bash_protected_write(
        "echo x > ~/.duduclaw/agents/.ephemeral/eph-abc123/out.md",
        &home(),
        &agent("sales-rep"),
    ) {
        GuardDecision::BlockedForeignAgentDir { owner, .. } => assert_eq!(owner, "eph-abc123"),
        other => panic!("expected BlockedForeignAgentDir, got {other:?}"),
    }
}

#[test]
fn bash_own_dir_attachments_and_reads_are_allowed() {
    for cmd in [
        "echo x > notes.md",
        "echo x > ./sub/notes.md",
        "echo x > /Users/alice/.duduclaw/agents/sales-rep/notes.md",
        "cp report.docx ~/.duduclaw/attachments/report.docx",
        "cp report.docx ../../attachments/report.docx",
        "cat ~/.duduclaw/tool_calls.jsonl",
        "grep foo ../../evals/sales-rep/case_a.toml",
        "echo x > /tmp/out.txt",
        "echo x > /Users/alice/Project/app/evals/case.toml",
    ] {
        assert_eq!(
            check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
            GuardDecision::NotAgentFile,
            "{cmd}"
        );
    }
}

#[test]
fn bash_home_state_rule_keeps_the_older_messages_for_known_files() {
    // config.toml / org.toml keep their established decisions.
    assert!(matches!(
        check_bash_protected_write(
            "echo x > ~/.duduclaw/config.toml",
            &home(),
            &agent("sales-rep")
        ),
        GuardDecision::BlockedBashProtectedWrite { .. }
    ));
}

// ── B: the untrusted caller is refused the same way on both lanes ──

#[test]
fn bash_untrusted_caller_is_refused_under_agents_like_write_edit() {
    let u = untrusted("sales-rep");
    for cmd in [
        "echo x >> /Users/alice/.duduclaw/agents/peer/notes.md",
        "echo x >> /Users/alice/.duduclaw/agents/sales-rep/notes.md",
        "echo x > ../peer/notes.md",
        "rm -rf ~/.duduclaw/agents/_trash/old_20260101000000",
        "echo x > SOUL.md",
        "echo '' > CONTRACT.toml",
    ] {
        let d = check_bash_protected_write(cmd, &home(), &u);
        assert!(
            matches!(d, GuardDecision::BlockedUntrustedCaller { .. }),
            "{cmd}: {d:?}"
        );
        // And the Write/Edit lane agrees for the explicit spellings.
    }
    for rel in ["agents/peer/notes.md", "agents/sales-rep/notes.md"] {
        assert!(matches!(
            check_caller_scope(&home().join(rel), &home(), &u),
            GuardDecision::BlockedUntrustedCaller { .. }
        ));
    }
    // Home state on both lanes.
    assert!(matches!(
        check_bash_protected_write("echo x >> ~/.duduclaw/tool_calls.jsonl", &home(), &u),
        GuardDecision::BlockedHomeStateWrite { .. }
    ));
    // Reads stay reads.
    assert_eq!(
        check_bash_protected_write("cat /Users/alice/.duduclaw/agents/peer/notes.md", &home(), &u),
        GuardDecision::NotAgentFile
    );
}
