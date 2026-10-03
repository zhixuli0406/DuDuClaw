use super::*;

// ── identity / enforcement surface ─────────────────────────────

#[test]
fn classifies_the_three_identity_surfaces() {
    let h = home();
    assert_eq!(
        classify_identity_surface(&h.join("agents/agnes/.mcp.json"), &h),
        Some(ProtectedSurface::AgentMcpJson)
    );
    assert_eq!(
        classify_identity_surface(&h.join("agents/agnes/.claude/settings.json"), &h),
        Some(ProtectedSurface::HookSettings)
    );
    assert_eq!(
        classify_identity_surface(&h.join("agents/agnes/.claude/settings.local.json"), &h),
        Some(ProtectedSurface::HookSettings)
    );
    assert_eq!(
        classify_identity_surface(&h.join("identity.key"), &h),
        Some(ProtectedSurface::IdentityKey)
    );
    // Out of scope: a user project's own files, and a settings.json that
    // is not under an agent's `.claude/`.
    for p in [
        PathBuf::from("/Users/alice/Project/app/.mcp.json"),
        PathBuf::from("/Users/alice/Project/app/.claude/settings.json"),
        PathBuf::from("/Users/alice/Project/app/identity.key"),
        h.join("agents/agnes/settings.json"),
        h.join("agents/identity.key"),
    ] {
        assert_eq!(classify_identity_surface(&p, &h), None, "{}", p.display());
    }
}

#[test]
fn stealing_a_peer_identity_into_own_mcp_json_is_denied() {
    // The attack the token alone does not stop: `sales-rep` reads the
    // CEO's `.mcp.json` (Read is not gated) and pastes the *valid* pair
    // into its own, so its next MCP server authenticates as `ceo`.
    let p = home().join("agents/sales-rep/.mcp.json");
    let existing = mcp_json("sales-rep", Some("aa11"));
    let stolen = mcp_json("ceo", Some("bb22"));
    let d = check_identity_surface_write(&p, &home(), Some(&existing), Some(&stolen));
    match &d {
        GuardDecision::BlockedIdentitySurface { reason, .. } => {
            assert!(reason.contains("DUDUCLAW_AGENT_ID"));
            // The token value is a credential — never echoed back.
            assert!(!reason.contains("bb22"), "token leaked into the message");
            assert!(!reason.contains("aa11"), "token leaked into the message");
        }
        other => panic!("expected BlockedIdentitySurface, got {other:?}"),
    }
    assert!(!d.is_allowed());
    assert!(d.block_message().unwrap().contains(".mcp.json"));
}

#[test]
fn appending_a_second_duduclaw_server_with_another_id_is_denied() {
    let p = home().join("agents/sales-rep/.mcp.json");
    let existing = mcp_json("sales-rep", Some("aa11"));
    let added = existing.replace(
        "\"mcpServers\":{",
        "\"mcpServers\":{\"duduclaw2\":{\"command\":\"/usr/local/bin/duduclaw\",\"args\":[\"mcp-server\"],\"env\":{\"DUDUCLAW_AGENT_ID\":\"ceo\"}},",
    );
    assert!(matches!(
        check_identity_surface_write(&p, &home(), Some(&existing), Some(&added)),
        GuardDecision::BlockedIdentitySurface { .. }
    ));
}

#[test]
fn adding_an_unrelated_mcp_server_is_allowed() {
    // Agents may still wire up Playwright etc. in their own file; only the
    // identity pairs are frozen.
    let p = home().join("agents/sales-rep/.mcp.json");
    let existing = mcp_json("sales-rep", Some("aa11"));
    let added = existing.replace(
        "\"mcpServers\":{",
        "\"mcpServers\":{\"playwright\":{\"command\":\"npx\",\"args\":[\"-y\",\"@playwright/mcp\"],\"env\":{\"FOO\":\"1\"}},",
    );
    assert_eq!(
        check_identity_surface_write(&p, &home(), Some(&existing), Some(&added)),
        GuardDecision::AllowedAgentWrite
    );
}

#[test]
fn dropping_the_identity_block_is_denied_and_reordering_is_not() {
    let p = home().join("agents/sales-rep/.mcp.json");
    let existing = mcp_json("sales-rep", Some("aa11"));
    // Removing the token downgrades a strict-mode caller to Rejected —
    // and removing the id falls through to `default_agent`.
    assert!(matches!(
        check_identity_surface_write(
            &p,
            &home(),
            Some(&existing),
            Some(&mcp_json("sales-rep", None))
        ),
        GuardDecision::BlockedIdentitySurface { .. }
    ));
    // Key order / formatting is not content.
    let reordered = "{\"mcpServers\":{\"duduclaw\":{\"env\":{\"DUDUCLAW_AGENT_TOKEN\":\"aa11\",\"DUDUCLAW_AGENT_ID\":\"sales-rep\"},\"args\":[\"mcp-server\"],\"command\":\"/usr/local/bin/duduclaw\"}}}";
    assert_eq!(
        check_identity_surface_write(&p, &home(), Some(&existing), Some(reordered)),
        GuardDecision::AllowedAgentWrite
    );
}

#[test]
fn creating_an_mcp_json_that_declares_an_identity_is_denied() {
    // No prior file ⇒ nothing was declared before, so declaring an
    // identity now is a change. `.mcp.json` is written by DuDuClaw, never
    // by an agent.
    let p = home().join("agents/sales-rep/.mcp.json");
    assert!(matches!(
        check_identity_surface_write(&p, &home(), None, Some(&mcp_json("ceo", Some("bb22")))),
        GuardDecision::BlockedIdentitySurface { .. }
    ));
    // …but a file that declares no identity at all is not our concern.
    assert_eq!(
        check_identity_surface_write(
            &p,
            &home(),
            None,
            Some("{\"mcpServers\":{\"playwright\":{\"command\":\"npx\"}}}")
        ),
        GuardDecision::AllowedAgentWrite
    );
}

#[test]
fn unreconstructable_or_malformed_mcp_json_fails_closed() {
    let p = home().join("agents/sales-rep/.mcp.json");
    let existing = mcp_json("sales-rep", Some("aa11"));
    assert!(matches!(
        check_identity_surface_write(&p, &home(), Some(&existing), None),
        GuardDecision::BlockedUnverifiable { .. }
    ));
    assert!(matches!(
        check_identity_surface_write(&p, &home(), Some(&existing), Some("{not json")),
        GuardDecision::BlockedUnverifiable { .. }
    ));
    assert!(matches!(
        check_identity_surface_write(&p, &home(), Some("{not json"), Some(&existing)),
        GuardDecision::BlockedUnverifiable { .. }
    ));
}

#[test]
fn hook_settings_and_identity_key_are_refused_outright() {
    // Disarming the PreToolUse hook would un-protect everything above it.
    let s = home().join("agents/agnes/.claude/settings.json");
    let d = check_identity_surface_write(&s, &home(), Some("{}"), Some("{\"hooks\":{}}"));
    assert!(matches!(d, GuardDecision::BlockedIdentitySurface { .. }));
    assert!(!d.is_allowed());
    // Even a no-op-looking write is refused: there is no legitimate
    // agent-authored shape for this file.
    assert!(matches!(
        check_identity_surface_write(&s, &home(), Some("{}"), Some("{}")),
        GuardDecision::BlockedIdentitySurface { .. }
    ));

    // A wrong-length identity.key silently downgrades strict mode to
    // `Disabled` — the cheapest possible disable of the whole feature.
    let k = home().join("identity.key");
    assert!(matches!(
        check_identity_surface_write(&k, &home(), None, Some("x")),
        GuardDecision::BlockedIdentitySurface { .. }
    ));
}

#[test]
fn identity_surface_ignores_unrelated_paths() {
    assert_eq!(
        check_identity_surface_write(
            &PathBuf::from("/Users/alice/Project/app/.mcp.json"),
            &home(),
            None,
            Some("{}")
        ),
        GuardDecision::NotAgentFile
    );
}

#[test]
fn bash_write_to_identity_surfaces_is_blocked() {
    for cmd in [
        "echo x > /Users/alice/.duduclaw/identity.key",
        "printf '{}' > /Users/alice/.duduclaw/agents/agnes/.claude/settings.json",
        "cp stolen.json /Users/alice/.duduclaw/agents/agnes/.mcp.json",
    ] {
        assert!(
            matches!(
                check_bash_protected_write(cmd, &home(), &HookCaller::Absent),
                GuardDecision::BlockedBashProtectedWrite { .. }
            ),
            "not blocked: {cmd}"
        );
    }
    // Read-only inspection stays allowed (same philosophy as the rest).
    assert_eq!(
        check_bash_protected_write("ls -l ~/.duduclaw/agents/agnes/.mcp.json", &home(), &HookCaller::Absent),
        GuardDecision::NotAgentFile
    );
}

// ── WP22 T2: caller-scoped directory isolation ─────────────────

#[test]
fn writing_another_agents_files_is_denied() {
    // The gap WP21 left open: none of these are org fields or identity
    // surfaces, so the content guards had nothing to say about them.
    for rel in [
        "agents/ceo/SOUL.md",
        "agents/ceo/MEMORY.md",
        "agents/ceo/CLAUDE.md",
        "agents/ceo/notes/scratch.txt",
        "agents/ceo/.claude/hooks/whatever.sh",
    ] {
        let p = home().join(rel);
        match check_caller_scope(&p, &home(), &agent("sales-rep")) {
            GuardDecision::BlockedForeignAgentDir { owner, caller, .. } => {
                assert_eq!(owner, "ceo", "{rel}");
                assert_eq!(caller, "sales-rep", "{rel}");
            }
            other => panic!("expected block for {rel}, got {other:?}"),
        }
    }
    // The victim's id is in the message so the model learns what it hit.
    let d = check_caller_scope(&home().join("agents/ceo/SOUL.md"), &home(), &agent("sales-rep"));
    assert!(!d.is_allowed());
    let msg = d.block_message().unwrap();
    assert!(msg.contains("ceo"), "message must name the other agent: {msg}");
    assert!(msg.contains("其他 AI 員工"));
}

#[test]
fn writing_own_files_falls_through_to_the_content_guards() {
    // `NotAgentFile` here means "this rule has no opinion" — the caller
    // then applies the WP21 org-field / identity-surface guards, which is
    // what keeps `reports_to` frozen even in the agent's own directory.
    for rel in [
        "agents/sales-rep/SOUL.md",
        "agents/sales-rep/agent.toml",
        "agents/sales-rep/wiki/sop.md",
    ] {
        assert_eq!(
            check_caller_scope(&home().join(rel), &home(), &agent("sales-rep")),
            GuardDecision::NotAgentFile,
            "{rel}"
        );
    }
}

#[test]
fn own_org_field_change_is_still_denied_by_the_content_guard() {
    // Regression pin for the two stages together: directory scope allows
    // it, content scope still refuses.
    let p = agent_toml();
    assert_eq!(
        check_caller_scope(&p, &home(), &agent("agnes")),
        GuardDecision::NotAgentFile
    );
    let new = BASE.replace(r#"reports_to = "ceo""#, r#"reports_to = "victim""#);
    assert!(matches!(
        check_protected_toml_write(&p, &home(), Some(BASE), &new),
        GuardDecision::BlockedOrgFieldChange { .. }
    ));
}

#[test]
fn ephemeral_dir_is_owned_by_its_own_id() {
    let mine = home().join("agents/.ephemeral/eph-abc123/SOUL.md");
    assert_eq!(
        check_caller_scope(&mine, &home(), &agent("eph-abc123")),
        GuardDecision::NotAgentFile
    );
    // A regular agent may not reach into an ephemeral scaffold…
    match check_caller_scope(&mine, &home(), &agent("sales-rep")) {
        GuardDecision::BlockedForeignAgentDir { owner, .. } => {
            assert_eq!(owner, "eph-abc123");
        }
        other => panic!("expected block, got {other:?}"),
    }
    // …nor may one ephemeral agent reach into another's.
    assert!(matches!(
        check_caller_scope(&mine, &home(), &agent("eph-zzz999")),
        GuardDecision::BlockedForeignAgentDir { .. }
    ));
}

#[test]
fn caller_scope_ignores_paths_outside_home() {
    // G1 (2026-10): paths under `<home>` that belong to no agent directory
    // (`shared/wiki/…`, the agents root, the ephemeral root) used to fall
    // through here; they are now refused as home state — see
    // `home_state.rs`. Only paths outside `<home>` stay none of this rule's
    // business.
    for p in [
        PathBuf::from("/Users/alice/Project/app/src/main.rs"),
        PathBuf::from("/Users/alice/Project/app/agents/README.md"),
    ] {
        assert_eq!(
            check_caller_scope(&p, &home(), &agent("sales-rep")),
            GuardDecision::NotAgentFile,
            "{}",
            p.display()
        );
    }
    for rel in [
        "shared/wiki/policies/x.md",
        "agents/README.md",
        "agents/.ephemeral/notes.txt",
    ] {
        assert!(matches!(
            check_caller_scope(&home().join(rel), &home(), &agent("sales-rep")),
            GuardDecision::BlockedHomeStateWrite { .. }
        ), "{rel}");
    }
}

#[test]
fn no_caller_identity_changes_nothing() {
    // The hook is only registered inside agent directories, so an
    // invocation without an identity is an operator working by hand.
    for rel in ["agents/ceo/SOUL.md", "config.toml", "agents/ceo/agent.toml"] {
        assert_eq!(
            check_caller_scope(&home().join(rel), &home(), &HookCaller::Absent),
            GuardDecision::NotAgentFile,
            "{rel}"
        );
    }
}

#[test]
fn strict_mode_rejection_denies_every_agent_dir_write() {
    let untrusted = HookCaller::Untrusted("sales-rep".to_string());
    // Even its "own" directory: a rejected claim proves nothing about
    // whose directory this is.
    for rel in [
        "agents/sales-rep/SOUL.md",
        "agents/ceo/SOUL.md",
        "agents/.ephemeral/eph-abc123/agent.toml",
    ] {
        let d = check_caller_scope(&home().join(rel), &home(), &untrusted);
        assert!(
            matches!(d, GuardDecision::BlockedUntrustedCaller { .. }),
            "{rel}: {d:?}"
        );
        assert!(!d.is_allowed());
    }
    let d = check_caller_scope(&home().join("agents/ceo/SOUL.md"), &home(), &untrusted);
    assert!(d.block_message().unwrap().contains("require_identity_token"));
    // …and the home config too.
    assert!(matches!(
        check_caller_scope(&home().join("config.toml"), &home(), &untrusted),
        GuardDecision::BlockedHomeConfigWrite { .. }
    ));
}

#[test]
fn caller_scope_is_case_insensitive_and_normalizing() {
    // Same filesystem-case tolerance as the rest of the guards, and `..`
    // must not launder a foreign path into an own-looking one.
    assert_eq!(
        check_caller_scope(
            &PathBuf::from("/users/ALICE/.duduclaw/agents/Sales-Rep/./SOUL.md"),
            &home(),
            &agent("sales-rep")
        ),
        GuardDecision::NotAgentFile
    );
    assert!(matches!(
        check_caller_scope(
            &home().join("agents/sales-rep/../ceo/SOUL.md"),
            &home(),
            &agent("sales-rep")
        ),
        GuardDecision::BlockedForeignAgentDir { .. }
    ));
}

#[test]
fn home_config_toml_is_wholly_locked_for_agents() {
    // WP22 supersedes WP21's section-level comparison for agent callers:
    // even a `[general] log_level` tweak is refused.
    let p = home().join("config.toml");
    let d = check_caller_scope(&p, &home(), &agent("sales-rep"));
    match &d {
        GuardDecision::BlockedHomeConfigWrite { caller, .. } => {
            assert_eq!(caller, "sales-rep");
        }
        other => panic!("expected BlockedHomeConfigWrite, got {other:?}"),
    }
    assert!(!d.is_allowed());
    assert!(d.block_message().unwrap().contains("config.toml"));
    // A user project's own config.toml is untouched.
    assert_eq!(
        check_caller_scope(
            &PathBuf::from("/Users/alice/Project/app/config.toml"),
            &home(),
            &agent("sales-rep")
        ),
        GuardDecision::NotAgentFile
    );
}

#[test]
fn home_config_without_identity_keeps_the_wp21_section_behaviour() {
    // Operator (no identity): the whole-file lock does not apply, and the
    // WP21 content guard still allows an unrelated section change while
    // refusing `[delegation]`.
    let p = home().join("config.toml");
    assert_eq!(
        check_caller_scope(&p, &home(), &HookCaller::Absent),
        GuardDecision::NotAgentFile
    );
    let benign = CONFIG.replace(r#"log_level = "info""#, r#"log_level = "debug""#);
    assert_eq!(
        check_protected_toml_write(&p, &home(), Some(CONFIG), &benign),
        GuardDecision::AllowedAgentWrite
    );
    let hostile = CONFIG.replace(r#"policy = "department""#, r#"policy = "open""#);
    assert!(matches!(
        check_protected_toml_write(&p, &home(), Some(CONFIG), &hostile),
        GuardDecision::BlockedProtectedSection { .. }
    ));
}

#[test]
fn bash_speed_bump_already_covers_the_whole_home_config() {
    // T2 requires the Bash lane to be whole-file too; the WP21 heuristic
    // matches on the *path*, not the section, so it already is. Pinned so
    // a future narrowing to `[delegation]` text would fail here.
    for cmd in [
        "echo 'log_level = \"debug\"' > /Users/alice/.duduclaw/config.toml",
        "sed -i '' 's/info/debug/' ~/.duduclaw/config.toml",
    ] {
        assert!(
            matches!(
                check_bash_protected_write(cmd, &home(), &HookCaller::Absent),
                GuardDecision::BlockedBashProtectedWrite { .. }
            ),
            "not blocked: {cmd}"
        );
    }
}

#[test]
fn bash_backslash_path_is_normalized() {
    let cmd = r"copy x C:\Users\alice\.duduclaw\agents\agnes\agent.toml";
    // `cp ` is not present, but `copy` contains no verb — use mv instead.
    assert_eq!(
        check_bash_protected_write(cmd, &home(), &HookCaller::Absent),
        GuardDecision::NotAgentFile
    );
    let cmd2 = r"mv x C:\Users\alice\.duduclaw\agents\agnes\agent.toml";
    assert!(matches!(
        check_bash_protected_write(cmd2, &home(), &HookCaller::Absent),
        GuardDecision::BlockedBashProtectedWrite { .. }
    ));
}

// ── WP22 T2 follow-up: Bash cross-agent-directory speed bump ───

#[test]
fn bash_cross_agent_dir_write_is_blocked_for_identified_caller() {
    // Neither `SOUL.md` nor any other basename here is DuDuClaw-specific
    // enough for the checks above to catch — this is exactly the gap
    // `check_caller_scope` closes for Write/Edit, now closed for Bash too.
    let cmd = "cat header.txt > /Users/alice/.duduclaw/agents/ceo/SOUL.md";
    match check_bash_protected_write(cmd, &home(), &agent("sales-rep")) {
        GuardDecision::BlockedForeignAgentDir { caller, owner, .. } => {
            assert_eq!(caller, "sales-rep");
            assert_eq!(owner, "ceo");
        }
        other => panic!("expected BlockedForeignAgentDir, got {other:?}"),
    }
}

#[test]
fn bash_write_to_own_agent_dir_is_not_blocked_by_the_cross_agent_rule() {
    let cmd = "echo 'note' > /Users/alice/.duduclaw/agents/sales-rep/notes.txt";
    assert_eq!(
        check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
        GuardDecision::NotAgentFile
    );
}

#[test]
fn bash_cross_agent_dir_mention_without_write_verb_is_allowed() {
    // Read-only inspection of a peer's directory is none of this rule's
    // business — same philosophy as the rest of this speed bump.
    let cmd = "cat /Users/alice/.duduclaw/agents/ceo/SOUL.md";
    assert_eq!(
        check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
        GuardDecision::NotAgentFile
    );
}

#[test]
fn bash_cross_agent_dir_write_without_caller_identity_is_not_blocked() {
    // No `DUDUCLAW_AGENT_ID` ⇒ operator running by hand, same as
    // `check_caller_scope`'s `HookCaller::Absent` handling — this rule is
    // deliberately a no-op, though the DuDuClaw-specific-basename checks
    // above it (agent.toml / config.toml / …) still apply regardless of
    // caller.
    let cmd = "cat header.txt > /Users/alice/.duduclaw/agents/ceo/SOUL.md";
    assert_eq!(
        check_bash_protected_write(cmd, &home(), &HookCaller::Absent),
        GuardDecision::NotAgentFile
    );
}

#[test]
fn bash_cross_agent_dir_rule_refuses_untrusted_claims_like_write_edit() {
    // G1 / B (2026-10): this used to stay silent for an `Untrusted` claim
    // while the Write/Edit lane refused the same path — the one
    // decision that differed between the two lanes. Both lanes now refuse
    // every write under `agents/` for a rejected claim.
    let cmd = "cat header.txt > /Users/alice/.duduclaw/agents/ceo/SOUL.md";
    let untrusted = HookCaller::Untrusted("sales-rep".to_string());
    assert!(matches!(
        check_bash_protected_write(cmd, &home(), &untrusted),
        GuardDecision::BlockedUntrustedCaller { .. }
    ));
}

// ── WP22 T5: the org store on the Bash lane ────────────────────

#[test]
fn bash_write_or_delete_of_the_org_store_is_blocked() {
    // The whole point of T1 is that `org.toml` is the authority. Deleting
    // it degrades every delegation decision back to the `agent.toml`
    // mirrors, so `rm` matters as much as `>` here — and neither was
    // covered before: this file was on the Write/Edit lane only.
    for cmd in [
        "rm /Users/alice/.duduclaw/org.toml",
        "rm -f ~/.duduclaw/org.toml",
        "echo 'schema = 1' > /Users/alice/.duduclaw/org.toml",
        "rm ~/.duduclaw/.org-seeded",
        "mv /tmp/fake.toml /Users/alice/.duduclaw/org.toml",
    ] {
        match check_bash_protected_write(cmd, &home(), &HookCaller::Absent) {
            GuardDecision::BlockedBashProtectedWrite { file_name, .. } => {
                assert_eq!(file_name, "org.toml", "{cmd}");
            }
            other => panic!("not blocked: {cmd} → {other:?}"),
        }
    }
    // Read-only inspection stays allowed, and a project's own `org.toml`
    // is none of this rule's business.
    assert_eq!(
        check_bash_protected_write("cat ~/.duduclaw/org.toml", &home(), &HookCaller::Absent),
        GuardDecision::NotAgentFile
    );
    assert_eq!(
        check_bash_protected_write(
            "rm /Users/alice/Project/app/org.toml",
            &home(),
            &HookCaller::Absent
        ),
        GuardDecision::NotAgentFile
    );
}

#[test]
fn bash_mentions_other_agent_dir_does_not_false_positive_on_similar_words() {
    // `myagents/` must not match the `agents/` boundary check.
    let cmd = "echo x > /Users/alice/myagents/ceo/SOUL.md";
    assert_eq!(
        check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
        GuardDecision::NotAgentFile
    );
}

// ── WP1.1 C3: SOUL.md self-write guard ──────────────────────────

#[test]
fn own_soul_write_is_blocked() {
    let p = home().join("agents/sales-rep/SOUL.md");
    match check_own_soul_write(&p, &home(), &agent("sales-rep")) {
        GuardDecision::BlockedOwnSoulWrite { caller, .. } => {
            assert_eq!(caller, "sales-rep");
        }
        other => panic!("expected BlockedOwnSoulWrite, got {other:?}"),
    }
    let d = check_own_soul_write(&p, &home(), &agent("sales-rep"));
    assert!(!d.is_allowed());
    let msg = d.block_message().unwrap();
    assert!(msg.contains("SOUL.md"));
    assert!(msg.contains("can_modify_own_soul"));
}

#[test]
fn own_soul_write_is_case_insensitive_and_normalizing() {
    let p = PathBuf::from("/users/ALICE/.duduclaw/agents/Sales-Rep/./SOUL.md");
    assert!(matches!(
        check_own_soul_write(&p, &home(), &agent("sales-rep")),
        GuardDecision::BlockedOwnSoulWrite { .. }
    ));
}

#[test]
fn foreign_soul_write_is_not_this_rules_business() {
    // `check_caller_scope` (Stage 0) already blocks this as
    // `BlockedForeignAgentDir` before Stage 0.5 would even run; this
    // function alone has nothing to say about a directory it does not
    // own.
    let p = home().join("agents/ceo/SOUL.md");
    assert_eq!(
        check_own_soul_write(&p, &home(), &agent("sales-rep")),
        GuardDecision::NotAgentFile
    );
}

#[test]
fn own_soul_write_ignores_non_soul_files_and_absent_caller() {
    let soul = home().join("agents/sales-rep/SOUL.md");
    assert_eq!(
        check_own_soul_write(&soul, &home(), &HookCaller::Absent),
        GuardDecision::NotAgentFile,
        "operator convention — unrestricted"
    );
    let other = home().join("agents/sales-rep/MEMORY.md");
    assert_eq!(
        check_own_soul_write(&other, &home(), &agent("sales-rep")),
        GuardDecision::NotAgentFile,
        "only SOUL.md is in scope for this rule"
    );
    let outside = PathBuf::from("/Users/alice/Project/app/SOUL.md");
    assert_eq!(
        check_own_soul_write(&outside, &home(), &agent("sales-rep")),
        GuardDecision::NotAgentFile,
        "a project file that happens to be named SOUL.md is none of this rule's business"
    );
}

#[test]
fn own_soul_write_not_gated_by_can_modify_own_soul_flag() {
    // C4: the flag is the C2 (MCP) gate's only escape hatch. The
    // filesystem lane has no notion of the flag at all — it always
    // blocks the raw Write/Edit tool regardless of agent.toml content,
    // matching `check_own_soul_write`'s signature (no flag parameter).
    let p = home().join("agents/sales-rep/SOUL.md");
    assert!(matches!(
        check_own_soul_write(&p, &home(), &agent("sales-rep")),
        GuardDecision::BlockedOwnSoulWrite { .. }
    ));
}

#[test]
fn bash_own_soul_write_is_blocked_for_explicit_and_relative_paths() {
    for cmd in [
        "echo hacked > /Users/alice/.duduclaw/agents/sales-rep/SOUL.md",
        "echo hacked > SOUL.md",
        "cat notes.txt >> soul.md",
        "sed -i '' 's/x/y/' SOUL.md",
    ] {
        assert!(
            matches!(
                check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
                GuardDecision::BlockedOwnSoulWrite { .. }
            ),
            "not blocked: {cmd}"
        );
    }
}

#[test]
fn bash_own_soul_write_read_only_is_allowed() {
    let cmd = "cat SOUL.md";
    assert_eq!(
        check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
        GuardDecision::NotAgentFile
    );
}

#[test]
fn bash_own_soul_write_no_caller_identity_is_not_blocked() {
    let cmd = "echo hacked > SOUL.md";
    assert_eq!(
        check_bash_protected_write(cmd, &home(), &HookCaller::Absent),
        GuardDecision::NotAgentFile
    );
}

#[test]
fn bash_own_soul_write_does_not_false_positive_on_lookalike_directory() {
    // Same boundary caveat as `mentions_other_agent_dir`: `myagents/`
    // must not be treated as a real `agents/` path, own or foreign.
    let cmd = "echo x > /Users/alice/myagents/sales-rep/SOUL.md";
    assert_eq!(
        check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
        GuardDecision::NotAgentFile
    );
}

#[test]
fn bash_foreign_agent_dir_rule_still_wins_over_soul_md_check() {
    // Regression pin: the pre-existing cross-agent rule must still fire
    // first when a write targets ANOTHER agent's SOUL.md — this rule
    // must never downgrade that into a same-caller "own soul" story.
    let cmd = "cat header.txt > /Users/alice/.duduclaw/agents/ceo/SOUL.md";
    match check_bash_protected_write(cmd, &home(), &agent("sales-rep")) {
        GuardDecision::BlockedForeignAgentDir { owner, .. } => assert_eq!(owner, "ceo"),
        other => panic!("expected BlockedForeignAgentDir, got {other:?}"),
    }
}

// ── Contract lock: CONTRACT.toml self-write guard ───────────────

#[test]
fn own_contract_write_is_blocked() {
    let p = home().join("agents/sales-rep/CONTRACT.toml");
    match check_own_contract_write(&p, &home(), &agent("sales-rep")) {
        GuardDecision::BlockedOwnContractWrite { caller, attempted_path } => {
            assert_eq!(caller, "sales-rep");
            assert_eq!(attempted_path, p);
        }
        other => panic!("expected BlockedOwnContractWrite, got {other:?}"),
    }
    let msg = check_own_contract_write(&p, &home(), &agent("sales-rep"))
        .block_message()
        .unwrap();
    assert!(msg.contains("CONTRACT.toml"));
    assert!(msg.contains("儀表板"));
    assert!(!msg.contains("can_modify_own_soul"), "the contract has no opt-in");
}

#[test]
fn own_contract_write_is_case_insensitive_and_normalizing() {
    let p = PathBuf::from("/users/ALICE/.duduclaw/agents/Sales-Rep/sub/../contract.TOML");
    assert!(matches!(
        check_own_contract_write(&p, &home(), &agent("sales-rep")),
        GuardDecision::BlockedOwnContractWrite { .. }
    ));
}

#[test]
fn own_contract_rule_ignores_foreign_dirs_other_files_absent_caller_and_projects() {
    // Foreign: Stage 0 (`check_caller_scope`) owns that refusal.
    let foreign = home().join("agents/ceo/CONTRACT.toml");
    assert_eq!(
        check_own_contract_write(&foreign, &home(), &agent("sales-rep")),
        GuardDecision::NotAgentFile
    );
    assert!(matches!(
        check_caller_scope(&foreign, &home(), &agent("sales-rep")),
        GuardDecision::BlockedForeignAgentDir { .. }
    ));
    let own = home().join("agents/sales-rep/CONTRACT.toml");
    assert_eq!(
        check_own_contract_write(&own, &home(), &HookCaller::Absent),
        GuardDecision::NotAgentFile,
        "operator convention — unrestricted"
    );
    assert_eq!(
        check_own_contract_write(&home().join("agents/sales-rep/SOUL.md"), &home(), &agent("sales-rep")),
        GuardDecision::NotAgentFile
    );
    assert_eq!(
        check_own_contract_write(
            &PathBuf::from("/Users/alice/Project/app/CONTRACT.toml"),
            &home(),
            &agent("sales-rep")
        ),
        GuardDecision::NotAgentFile
    );
}

#[test]
fn bash_own_contract_write_is_blocked_and_reads_are_not() {
    for cmd in [
        "echo '' > CONTRACT.toml",
        "rm contract.toml",
        "mv CONTRACT.toml /tmp/x",
        "cat x > /Users/alice/.duduclaw/agents/sales-rep/CONTRACT.toml",
    ] {
        assert!(
            matches!(
                check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
                GuardDecision::BlockedOwnContractWrite { .. }
            ),
            "not blocked: {cmd}"
        );
    }
    for cmd in ["cat CONTRACT.toml", "echo x > old_contract.toml"] {
        assert_eq!(
            check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
            GuardDecision::NotAgentFile,
            "{cmd}"
        );
    }
    assert_eq!(
        check_bash_protected_write("echo x > CONTRACT.toml", &home(), &HookCaller::Absent),
        GuardDecision::NotAgentFile
    );
    // Foreign contract still takes the cross-agent rule.
    assert!(matches!(
        check_bash_protected_write(
            "echo x > /Users/alice/.duduclaw/agents/ceo/CONTRACT.toml",
            &home(),
            &agent("sales-rep")
        ),
        GuardDecision::BlockedForeignAgentDir { .. }
    ));
}

// ── `./`-relative spellings of the caller's own SOUL.md / CONTRACT.toml ──
//
// The hook's Bash cwd is the agent's own directory, so `./SOUL.md`,
// `././SOUL.md`, `.//SOUL.md` and `x/../SOUL.md` all name the same file as a
// bare `SOUL.md`. Normalisation is lexical; the filesystem is never read.

const RELATIVE_SPELLINGS: &[&str] = &["{f}", "./{f}", "././{f}", ".//{f}", "./sub/../{f}"];

fn spelled(template: &str, file: &str) -> String {
    template.replace("{f}", file)
}

#[test]
fn bash_relative_own_soul_write_is_blocked_in_every_spelling() {
    for file in ["SOUL.md", "soul.md", "Soul.MD"] {
        for t in RELATIVE_SPELLINGS {
            let path = spelled(t, file);
            for cmd in [format!("echo x > {path}"), format!("rm {path}"), format!("echo x >{path}")] {
                assert!(
                    matches!(
                        check_bash_protected_write(&cmd, &home(), &agent("sales-rep")),
                        GuardDecision::BlockedOwnSoulWrite { .. }
                    ),
                    "not blocked: {cmd}"
                );
            }
        }
    }
}

#[test]
fn bash_relative_own_contract_write_is_blocked_in_every_spelling() {
    for file in ["CONTRACT.toml", "contract.toml", "Contract.TOML"] {
        for t in RELATIVE_SPELLINGS {
            let path = spelled(t, file);
            for cmd in [format!("echo x > {path}"), format!("mv {path} /tmp/x"), format!("tee '{path}'")] {
                assert!(
                    matches!(
                        check_bash_protected_write(&cmd, &home(), &agent("sales-rep")),
                        GuardDecision::BlockedOwnContractWrite { .. }
                    ),
                    "not blocked: {cmd}"
                );
            }
        }
    }
}

#[test]
fn bash_relative_own_file_reads_still_pass() {
    for file in ["SOUL.md", "CONTRACT.toml"] {
        // Absolute-form read passes today; every relative spelling must too.
        let absolute = format!("cat /Users/alice/.duduclaw/agents/sales-rep/{file}");
        assert_eq!(
            check_bash_protected_write(&absolute, &home(), &agent("sales-rep")),
            GuardDecision::NotAgentFile,
            "{absolute}"
        );
        for t in RELATIVE_SPELLINGS {
            let cmd = format!("cat {}", spelled(t, file));
            assert_eq!(
                check_bash_protected_write(&cmd, &home(), &agent("sales-rep")),
                GuardDecision::NotAgentFile,
                "{cmd}"
            );
        }
    }
}

#[test]
fn bash_relative_spellings_that_leave_the_agent_dir_are_not_own() {
    // `../SOUL.md` is the agents root, not the caller's own directory, and a
    // subdirectory's file is a different file — neither collapses to the
    // bare spelling. (Bash is a speed bump, not a sandbox; these are simply
    // not this rule's match.)
    //
    // G1 (2026-10): the first two now land on the agents root, which is
    // `<home>` state, so they are refused by the home-state rule instead.
    for cmd in ["echo x > ../SOUL.md", "echo x > ./../CONTRACT.toml"] {
        assert!(
            matches!(
                check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
                GuardDecision::BlockedHomeStateWrite { .. }
            ),
            "{cmd}"
        );
    }
    for cmd in [
        "echo x > ./notes/SOUL.md",
        "echo x > ./old_contract.toml",
    ] {
        assert_eq!(
            check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
            GuardDecision::NotAgentFile,
            "{cmd}"
        );
    }
    // No agent identity ⇒ operator at a terminal ⇒ unrestricted, as before.
    assert_eq!(
        check_bash_protected_write("echo x > ./CONTRACT.toml", &home(), &HookCaller::Absent),
        GuardDecision::NotAgentFile
    );
}
