use super::*;

// ── classification ─────────────────────────────────────────────

#[test]
fn classifies_canonical_agent_toml() {
    assert_eq!(
        classify_protected_toml(&agent_toml(), &home()),
        Some(ProtectedTomlKind::AgentToml)
    );
}

#[test]
fn ignores_nested_non_authoritative_agent_toml() {
    // `<home>/agents/agnes/sub/agent.toml` is never loaded by the
    // registry, so guarding it would only produce false positives.
    let p = home().join("agents/agnes/sub/agent.toml");
    assert_eq!(classify_protected_toml(&p, &home()), None);
}

#[test]
fn classifies_ephemeral_agent_toml() {
    // `DispatchOrgView` reads this file directly for `eph-*` ids, so its
    // `reports_to` authorises dispatch just like a registry agent's.
    let p = home().join("agents/.ephemeral/eph-abc123/agent.toml");
    assert_eq!(
        classify_protected_toml(&p, &home()),
        Some(ProtectedTomlKind::AgentToml)
    );
}

#[test]
fn ephemeral_reports_to_change_is_denied() {
    let p = home().join("agents/.ephemeral/eph-abc123/agent.toml");
    let new = BASE.replace(r#"reports_to = "ceo""#, r#"reports_to = "victim""#);
    assert!(matches!(
        check_protected_toml_write(&p, &home(), Some(BASE), &new),
        GuardDecision::BlockedOrgFieldChange { .. }
    ));
}

#[test]
fn ignores_other_depth3_agent_toml() {
    // Only `.ephemeral` gets the depth-3 exemption; a random nested dir
    // is inert and must not be guarded.
    let p = home().join("agents/team/agnes/agent.toml");
    assert_eq!(classify_protected_toml(&p, &home()), None);
}

#[test]
fn ignores_agent_toml_outside_home() {
    // Already handled (blocked) by `agent_guard::check_agent_file_write`.
    let p = PathBuf::from("/Users/alice/Project/x/agent.toml");
    assert_eq!(classify_protected_toml(&p, &home()), None);
}

#[test]
fn classifies_home_config_toml() {
    assert_eq!(
        classify_protected_toml(&home().join("config.toml"), &home()),
        Some(ProtectedTomlKind::HomeConfigToml)
    );
}

#[test]
fn ignores_project_config_toml() {
    let p = PathBuf::from("/Users/alice/Project/rustapp/config.toml");
    assert_eq!(classify_protected_toml(&p, &home()), None);
}

#[test]
fn classification_is_case_insensitive_and_normalizing() {
    let p = PathBuf::from("/users/ALICE/.duduclaw/agents/agnes/./x/../agent.toml");
    assert_eq!(
        classify_protected_toml(&p, &home()),
        Some(ProtectedTomlKind::AgentToml)
    );
}

// ── agent.toml org fields ──────────────────────────────────────

#[test]
fn reports_to_change_is_denied() {
    let new = BASE.replace(r#"reports_to = "ceo""#, r#"reports_to = "victim""#);
    let d = check_protected_toml_write(&agent_toml(), &home(), Some(BASE), &new);
    match &d {
        GuardDecision::BlockedOrgFieldChange { changed, .. } => {
            assert_eq!(changed.len(), 1);
            assert!(changed[0].contains("reports_to"));
            assert!(changed[0].contains("victim"));
        }
        other => panic!("expected BlockedOrgFieldChange, got {other:?}"),
    }
    assert!(!d.is_allowed());
    let msg = d.block_message().unwrap();
    assert!(msg.contains("組織欄位"));
    assert!(msg.contains("agent_update"));
    assert!(msg.contains("儀表板"));
}

#[test]
fn name_change_is_denied() {
    // `[agent] name` is the registry id the `delegation.set` whitelist
    // resolves against — an agent that could rename itself to a value the
    // operator believes belongs to someone else would inherit that pair.
    let new = BASE.replace(r#"name = "agnes""#, r#"name = "ceo-assistant""#);
    match check_protected_toml_write(&agent_toml(), &home(), Some(BASE), &new) {
        GuardDecision::BlockedOrgFieldChange { changed, .. } => {
            assert_eq!(changed.len(), 1);
            assert!(changed[0].contains("name"));
            assert!(changed[0].contains("ceo-assistant"));
        }
        other => panic!("expected BlockedOrgFieldChange, got {other:?}"),
    }
}

#[test]
fn deleting_name_is_denied() {
    let new = BASE.replace("name = \"agnes\"\n", "");
    assert!(matches!(
        check_protected_toml_write(&agent_toml(), &home(), Some(BASE), &new),
        GuardDecision::BlockedOrgFieldChange { .. }
    ));
}

#[test]
fn department_change_is_denied() {
    let new = BASE.replace(r#"department = "engineering""#, r#"department = "finance""#);
    assert!(matches!(
        check_protected_toml_write(&agent_toml(), &home(), Some(BASE), &new),
        GuardDecision::BlockedOrgFieldChange { .. }
    ));
}

#[test]
fn deleting_reports_to_is_denied() {
    let new = BASE.replace("reports_to = \"ceo\"\n", "");
    assert!(matches!(
        check_protected_toml_write(&agent_toml(), &home(), Some(BASE), &new),
        GuardDecision::BlockedOrgFieldChange { .. }
    ));
}

#[test]
fn replacing_agent_table_with_scalar_is_denied() {
    // Cannot launder a change by destroying the table shape.
    let new = "agent = \"agnes\"\n";
    assert!(matches!(
        check_protected_toml_write(&agent_toml(), &home(), Some(BASE), new),
        GuardDecision::BlockedOrgFieldChange { .. }
    ));
}

#[test]
fn both_fields_changed_lists_both() {
    let new = BASE
        .replace(r#"reports_to = "ceo""#, r#"reports_to = "x""#)
        .replace(r#"department = "engineering""#, r#"department = "y""#);
    match check_protected_toml_write(&agent_toml(), &home(), Some(BASE), &new) {
        GuardDecision::BlockedOrgFieldChange { changed, .. } => {
            assert_eq!(changed.len(), 2);
        }
        other => panic!("expected block, got {other:?}"),
    }
}

#[test]
fn unrelated_field_change_is_allowed() {
    let new = BASE.replace(r#"preferred = "sonnet""#, r#"preferred = "opus""#);
    assert_eq!(
        check_protected_toml_write(&agent_toml(), &home(), Some(BASE), &new),
        GuardDecision::AllowedAgentWrite
    );
}

// ── agent.toml [capabilities] (Team-as-Agent review P1) ────────

/// An employee whose `[capabilities]` actually says something, so the
/// tests below exercise a *change* rather than the add-the-section case.
const BASE_WITH_CAPS: &str = r#"
[agent]
name = "agnes"
reports_to = "ceo"
department = "engineering"

[capabilities]
os_native = false
computer_use = false
allowed_tools = ["Read", "Grep"]
denied_tools = ["Bash"]
db_sources = []

[model]
preferred = "sonnet"
"#;

/// Regression (review P1, `ephemeral.rs:1497` + `org_field_guard.rs:73`):
/// a team role member runs in the employee's workspace, so the hook
/// identifies it as the employee — before this, flipping the `os_native`
/// master switch in the employee's own `agent.toml` was ALLOWED and the
/// next round's `check_tool_subset` honoured the widened envelope.
#[test]
fn regression_role_member_cannot_flip_a_capability_master_switch() {
    let new = BASE_WITH_CAPS.replace("os_native = false", "os_native = true");
    let d = check_protected_toml_write(&agent_toml(), &home(), Some(BASE_WITH_CAPS), &new);
    match &d {
        GuardDecision::BlockedProtectedField { changed, .. } => {
            assert_eq!(changed.len(), 1, "{changed:?}");
            assert!(changed[0].contains("os_native"), "{changed:?}");
        }
        other => panic!("expected BlockedProtectedField, got {other:?}"),
    }
    assert!(!d.is_allowed());
    let msg = d.block_message().unwrap();
    assert!(msg.contains("權限設定"), "{msg}");
    assert!(msg.contains("agent_update"), "{msg}");
}

/// Regression: the escalation the finding actually describes — widening
/// `allowed_tools` / emptying `denied_tools` so the member's tool subset
/// check passes anything.
#[test]
fn regression_widening_the_tool_envelope_is_denied() {
    for new in [
        BASE_WITH_CAPS.replace(
            r#"allowed_tools = ["Read", "Grep"]"#,
            r#"allowed_tools = ["Read", "Grep", "Bash", "Write"]"#,
        ),
        BASE_WITH_CAPS.replace(r#"denied_tools = ["Bash"]"#, "denied_tools = []"),
        BASE_WITH_CAPS.replace("denied_tools = [\"Bash\"]\n", ""),
        BASE_WITH_CAPS.replace(r#"db_sources = []"#, r#"db_sources = ["crm"]"#),
    ] {
        assert!(
            matches!(
                check_protected_toml_write(&agent_toml(), &home(), Some(BASE_WITH_CAPS), &new),
                GuardDecision::BlockedProtectedField { .. }
            ),
            "expected a block for:\n{new}"
        );
    }
}

/// Adding a `[capabilities]` section to a file that had none is the same
/// escalation by another route.
#[test]
fn adding_a_capabilities_section_is_denied() {
    let new = format!("{BASE}\n[capabilities]\ncomputer_use = true\n");
    match check_protected_toml_write(&agent_toml(), &home(), Some(BASE), &new) {
        GuardDecision::BlockedProtectedField { changed, .. } => {
            assert!(changed[0].contains("computer_use"), "{changed:?}");
        }
        other => panic!("expected BlockedProtectedField, got {other:?}"),
    }
}

/// Reshaping the table cannot launder a change past the key walk: a
/// top-level `capabilities = "…"` scalar, and deleting the section
/// outright, are both refused.
#[test]
fn reshaping_or_deleting_the_capabilities_table_is_denied() {
    let scalar = "capabilities = \"wide-open\"\n\n[agent]\nname = \"agnes\"\n\
                  reports_to = \"ceo\"\ndepartment = \"engineering\"\n";
    match check_protected_toml_write(&agent_toml(), &home(), Some(BASE_WITH_CAPS), scalar) {
        GuardDecision::BlockedProtectedField { changed, .. } => {
            assert!(changed[0].contains("[capabilities]"), "{changed:?}");
        }
        other => panic!("expected BlockedProtectedField, got {other:?}"),
    }

    let deleted = "[agent]\nname = \"agnes\"\nreports_to = \"ceo\"\n\
                   department = \"engineering\"\n\n[model]\npreferred = \"sonnet\"\n";
    assert!(matches!(
        check_protected_toml_write(&agent_toml(), &home(), Some(BASE_WITH_CAPS), deleted),
        GuardDecision::BlockedProtectedField { .. }
    ));
}

/// A capability key this build has never heard of is frozen too — the
/// diff walks the union of both sides' keys, not a hand-maintained list.
#[test]
fn a_future_capability_key_is_frozen_without_a_list_update() {
    let new = BASE_WITH_CAPS.replace(
        "[capabilities]\n",
        "[capabilities]\nsome_future_switch = true\n",
    );
    match check_protected_toml_write(&agent_toml(), &home(), Some(BASE_WITH_CAPS), &new) {
        GuardDecision::BlockedProtectedField { changed, .. } => {
            assert!(changed[0].contains("some_future_switch"), "{changed:?}");
        }
        other => panic!("expected BlockedProtectedField, got {other:?}"),
    }
}

/// `[capabilities] action_rules` (2026-10) is frozen like every other
/// capability key: an employee can neither add a rule list, nor loosen an
/// existing one, nor rewrite a single verdict. This is why the key lives in
/// `[capabilities]` rather than a section of its own.
#[test]
fn action_rules_are_frozen_against_the_employees_own_write() {
    let with_rules = BASE_WITH_CAPS.replace(
        "[capabilities]\n",
        "[capabilities]\naction_rules = [{ effect = \"send\", verdict = \"ask\" }]\n",
    );
    let added = check_protected_toml_write(&agent_toml(), &home(), Some(BASE_WITH_CAPS), &with_rules);
    match added {
        GuardDecision::BlockedProtectedField { changed, .. } => {
            assert!(changed[0].contains("action_rules"), "{changed:?}");
        }
        other => panic!("adding rules: expected BlockedProtectedField, got {other:?}"),
    }
    for loosened in [
        with_rules.replace("verdict = \"ask\"", "verdict = \"allow\""),
        with_rules.replace("action_rules = [{ effect = \"send\", verdict = \"ask\" }]\n", ""),
    ] {
        assert!(
            matches!(
                check_protected_toml_write(&agent_toml(), &home(), Some(&with_rules), &loosened),
                GuardDecision::BlockedProtectedField { .. }
            ),
            "{loosened}"
        );
    }
}

/// The guard stays narrow: a write that touches neither the org fields nor
/// `[capabilities]` is still allowed, and an unchanged `[capabilities]`
/// never blocks a legitimate edit elsewhere in the file.
#[test]
fn an_edit_elsewhere_in_a_file_that_has_capabilities_is_allowed() {
    let new = BASE_WITH_CAPS.replace(r#"preferred = "sonnet""#, r#"preferred = "opus""#);
    assert_eq!(
        check_protected_toml_write(&agent_toml(), &home(), Some(BASE_WITH_CAPS), &new),
        GuardDecision::AllowedAgentWrite
    );
}

/// Ordinary files the member legitimately works on are untouched by this
/// guard — only the canonical `agent.toml` / `<home>/config.toml` are
/// classified at all, so a `NOTES.md` (or any project file) falls through
/// as `NotAgentFile`.
#[test]
fn an_ordinary_file_is_not_this_guards_business() {
    for p in [
        home().join("agents/agnes/NOTES.md"),
        PathBuf::from("/Users/alice/Project/app/src/main.rs"),
        home().join("agents/agnes/capabilities.toml"),
    ] {
        assert_eq!(
            check_protected_toml_write(&p, &home(), Some(BASE_WITH_CAPS), "anything at all"),
            GuardDecision::NotAgentFile,
            "{}",
            p.display()
        );
    }
}

/// Both kinds of change at once reports the org one — the more serious
/// verdict, and the pre-existing behaviour for pre-existing attacks.
#[test]
fn org_fields_win_when_both_moved() {
    let new = BASE_WITH_CAPS
        .replace(r#"reports_to = "ceo""#, r#"reports_to = "victim""#)
        .replace("os_native = false", "os_native = true");
    assert!(matches!(
        check_protected_toml_write(&agent_toml(), &home(), Some(BASE_WITH_CAPS), &new),
        GuardDecision::BlockedOrgFieldChange { .. }
    ));
}

#[test]
fn reordering_and_reformatting_is_allowed() {
    // Field-wise comparison, not textual — a reformat must not trip it.
    let new = r#"
[model]
preferred = "sonnet"

[agent]
department = "engineering"
reports_to = "ceo"
name = "agnes"
display_name = "Agnes"
role = "assistant"
status = "active"
trigger = "@agnes"
icon = "🐾"
"#;
    assert_eq!(
        check_protected_toml_write(&agent_toml(), &home(), Some(BASE), new),
        GuardDecision::AllowedAgentWrite
    );
}

#[test]
fn new_file_is_allowed() {
    assert_eq!(
        check_protected_toml_write(&agent_toml(), &home(), None, BASE),
        GuardDecision::AllowedAgentWrite
    );
}

#[test]
fn invalid_new_toml_is_denied() {
    let d = check_protected_toml_write(&agent_toml(), &home(), Some(BASE), "[agent\nname =");
    match &d {
        GuardDecision::BlockedUnverifiable { reason, .. } => {
            assert!(reason.contains("TOML"));
        }
        other => panic!("expected BlockedUnverifiable, got {other:?}"),
    }
    assert!(!d.is_allowed());
}

#[test]
fn invalid_new_toml_on_new_file_is_also_denied() {
    // Fail-closed: broken TOML breaks the agent regardless of prior state.
    assert!(matches!(
        check_protected_toml_write(&agent_toml(), &home(), None, "not = = toml"),
        GuardDecision::BlockedUnverifiable { .. }
    ));
}

#[test]
fn invalid_existing_toml_is_denied() {
    let d = check_protected_toml_write(&agent_toml(), &home(), Some("[agent"), BASE);
    match &d {
        GuardDecision::BlockedUnverifiable { reason, .. } => {
            assert!(reason.contains("現有檔案"));
        }
        other => panic!("expected BlockedUnverifiable, got {other:?}"),
    }
}

#[test]
fn non_protected_path_is_not_our_concern() {
    let p = PathBuf::from("/Users/alice/Project/x/Cargo.toml");
    assert_eq!(
        check_protected_toml_write(&p, &home(), Some(""), "x = 1"),
        GuardDecision::NotAgentFile
    );
}

// ── config.toml protected sections ─────────────────────────────

#[test]
fn delegation_policy_change_is_denied() {
    let new = CONFIG.replace(r#"policy = "department""#, r#"policy = "open""#);
    let path = home().join("config.toml");
    let d = check_protected_toml_write(&path, &home(), Some(CONFIG), &new);
    match &d {
        GuardDecision::BlockedProtectedSection { changed, .. } => {
            assert_eq!(changed.len(), 1);
            assert!(changed[0].contains("[delegation]"));
        }
        other => panic!("expected BlockedProtectedSection, got {other:?}"),
    }
    let msg = d.block_message().unwrap();
    assert!(msg.contains("delegation"));
    assert!(msg.contains("儀表板"));
}

#[test]
fn delegation_allow_whitelist_change_is_denied() {
    let new = CONFIG.replace(r#"allow = [["a", "b"]]"#, r#"allow = [["a", "ceo"]]"#);
    assert!(matches!(
        check_protected_toml_write(
            &home().join("config.toml"),
            &home(),
            Some(CONFIG),
            &new
        ),
        GuardDecision::BlockedProtectedSection { .. }
    ));
}

#[test]
fn adding_acp_trusted_is_denied() {
    let new = format!("{CONFIG}\n[acp]\ntrusted = true\n");
    assert!(matches!(
        check_protected_toml_write(
            &home().join("config.toml"),
            &home(),
            Some(CONFIG),
            &new
        ),
        GuardDecision::BlockedProtectedSection { .. }
    ));
}

#[test]
fn deleting_delegation_section_is_denied() {
    let new = "[general]\nlog_level = \"info\"\n";
    assert!(matches!(
        check_protected_toml_write(
            &home().join("config.toml"),
            &home(),
            Some(CONFIG),
            new
        ),
        GuardDecision::BlockedProtectedSection { .. }
    ));
}

#[test]
fn unrelated_config_change_is_allowed() {
    let new = CONFIG.replace(r#"log_level = "info""#, r#"log_level = "debug""#);
    assert_eq!(
        check_protected_toml_write(
            &home().join("config.toml"),
            &home(),
            Some(CONFIG),
            &new
        ),
        GuardDecision::AllowedAgentWrite
    );
}

#[test]
fn project_config_toml_is_untouched() {
    let p = PathBuf::from("/Users/alice/Project/app/config.toml");
    assert_eq!(
        check_protected_toml_write(&p, &home(), Some(CONFIG), "[delegation]\npolicy = \"open\""),
        GuardDecision::NotAgentFile
    );
}

// ── bash speed bump ────────────────────────────────────────────

#[test]
fn bash_echo_redirect_into_agent_toml_is_blocked() {
    let cmd = "echo 'reports_to = \"ceo\"' > /Users/alice/.duduclaw/agents/agnes/agent.toml";
    match check_bash_protected_write(cmd, &home(), &HookCaller::Absent) {
        GuardDecision::BlockedBashProtectedWrite { file_name, .. } => {
            assert_eq!(file_name, "agent.toml");
        }
        other => panic!("expected block, got {other:?}"),
    }
}

#[test]
fn bash_sed_inplace_on_agent_toml_is_blocked() {
    let cmd = "sed -i '' 's/ceo/victim/' agents/agnes/agent.toml";
    assert!(matches!(
        check_bash_protected_write(cmd, &home(), &HookCaller::Absent),
        GuardDecision::BlockedBashProtectedWrite { .. }
    ));
}

#[test]
fn bash_write_to_home_config_is_blocked() {
    let cmd = "printf '[delegation]\\npolicy=\"open\"\\n' >> ~/.duduclaw/config.toml";
    match check_bash_protected_write(cmd, &home(), &HookCaller::Absent) {
        GuardDecision::BlockedBashProtectedWrite { file_name, .. } => {
            assert_eq!(file_name, "config.toml");
        }
        other => panic!("expected block, got {other:?}"),
    }
}

#[test]
fn bash_write_to_project_config_is_allowed() {
    let cmd = "echo 'x=1' > /Users/alice/Project/app/config.toml";
    assert_eq!(
        check_bash_protected_write(cmd, &home(), &HookCaller::Absent),
        GuardDecision::NotAgentFile
    );
}

#[test]
fn bash_read_only_agent_toml_is_allowed() {
    assert_eq!(
        check_bash_protected_write("grep reports_to agents/agnes/agent.toml", &home(), &HookCaller::Absent),
        GuardDecision::NotAgentFile
    );
}

#[test]
fn bash_unrelated_command_is_allowed() {
    assert_eq!(
        check_bash_protected_write("cargo test -p duduclaw-core", &home(), &HookCaller::Absent),
        GuardDecision::NotAgentFile
    );
}

