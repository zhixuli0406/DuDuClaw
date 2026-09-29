//! Unit tests for [`super`], moved verbatim out of the former `approval.rs` — gate cases.

use super::*;

#[test]
fn approval_required_tools_present() {
    let dir = tmp_agent_dir();
    write_agent_toml(
        &dir,
        "[capabilities]\napproval_required_tools = [\"Bash\", \"send_to_agent\"]\n",
    );
    let set = approval_required_tools(&dir);
    assert!(set.contains("Bash"));
    assert!(set.contains("send_to_agent"));
    assert!(tool_requires_approval(&dir, "Bash"));
    assert!(!tool_requires_approval(&dir, "Read"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn approval_required_tools_absent_key_is_empty() {
    let dir = tmp_agent_dir();
    write_agent_toml(&dir, "[capabilities]\nallowed_tools = []\n");
    assert!(approval_required_tools(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn approval_required_tools_missing_file_is_empty() {
    let dir = tmp_agent_dir();
    assert!(approval_required_tools(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn approval_required_tools_malformed_fails_safe_empty() {
    let dir = tmp_agent_dir();
    write_agent_toml(&dir, "this is not = valid toml [[[");
    // Malformed ⇒ empty set (additive gate), never a panic.
    assert!(approval_required_tools(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

// ── P2b: ActionGuard three-value gate ──────────────────────────────────

#[test]
fn irreversible_tool_lists_parse_present() {
    let dir = tmp_agent_dir();
    write_agent_toml(
        &dir,
        "[capabilities]\nirreversible_tools = [\"send_email\"]\nmaybe_irreversible_tools = [\"Bash\", \"http_post\"]\n",
    );
    assert!(tool_is_irreversible(&dir, "send_email"));
    assert!(!tool_is_irreversible(&dir, "Bash"));
    assert!(tool_is_maybe_irreversible(&dir, "Bash"));
    assert!(tool_is_maybe_irreversible(&dir, "http_post"));
    assert!(!tool_is_maybe_irreversible(&dir, "send_email"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn irreversible_tool_lists_absent_and_missing_are_empty() {
    // Absent keys.
    let dir = tmp_agent_dir();
    write_agent_toml(&dir, "[capabilities]\nallowed_tools = []\n");
    assert!(irreversible_tools(&dir).is_empty());
    assert!(maybe_irreversible_tools(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
    // Missing file entirely.
    let dir2 = tmp_agent_dir();
    let _ = std::fs::remove_dir_all(&dir2); // remove so agent.toml is absent
    assert!(irreversible_tools(&dir2).is_empty());
    assert!(maybe_irreversible_tools(&dir2).is_empty());
}

#[test]
fn irreversible_tool_lists_malformed_fail_safe_empty() {
    let dir = tmp_agent_dir();
    write_agent_toml(&dir, "not = valid toml [[[");
    assert!(irreversible_tools(&dir).is_empty());
    assert!(maybe_irreversible_tools(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_action_gate_take_the_stricter() {
    use ActionGate::*;
    use JudgeVerdict::*;
    // Never listed → auto.
    assert_eq!(resolve_action_gate(false, false, None), Auto);
    // Always (folds in legacy approval_required_tools) → approval, regardless
    // of maybe membership or any judge verdict.
    assert_eq!(resolve_action_gate(true, false, None), RequireApproval);
    assert_eq!(resolve_action_gate(true, true, Some(Safe)), RequireApproval);
    // Maybe, judge not yet run → consult judge.
    assert_eq!(resolve_action_gate(false, true, None), ConsultJudge);
    // Maybe, judge ruled safe → auto; risky (incl. fail-closed) → approval.
    assert_eq!(resolve_action_gate(false, true, Some(Safe)), Auto);
    assert_eq!(
        resolve_action_gate(false, true, Some(Risky)),
        RequireApproval
    );
}

// ── H21: closed-enumeration ActionGuard findings ─────────────────────

/// The `ALL_ACTION_GUARD_FINDINGS` "全集" table must contain every enum
/// variant exactly once. This match has NO wildcard arm — if a variant is
/// ever added to `ActionGuardFinding` without being listed in either arm
/// below, the build fails, which is what forces `ALL_ACTION_GUARD_FINDINGS`
/// (and every downstream `token()`/`description()`) to stay complete.
#[test]
fn all_action_guard_findings_is_exhaustive() {
    fn is_a_known_variant(f: ActionGuardFinding) -> bool {
        use ActionGuardFinding::*;
        match f {
            ToolCategoryFilesystemDelete
            | ToolCategoryFilesystemWrite
            | ToolCategoryProcessExec
            | ToolCategoryEmailSend
            | ToolCategoryMessagingSend
            | ToolCategoryNetworkEgress
            | ToolCategoryBrowserOrDesktopAutomation
            | ToolCategoryOsNativeAction
            | ToolCategoryKnowledgeStore
            | ToolCategoryFinancialOrBusiness
            | ToolCategorySkillOrCapabilityInstall
            | ToolCategoryUnknown
            | TargetScopeWorkspaceInternal
            | TargetScopeHomeDir
            | TargetScopeSystemPath
            | TargetScopeExternalNetwork
            | TargetScopeNone
            | MagnitudeSingleTarget
            | MagnitudeBatchOrBulk
            | ProtectedPathHit
            | DestructiveSemanticsDetected => true,
        }
    }
    for f in ALL_ACTION_GUARD_FINDINGS {
        assert!(
            is_a_known_variant(*f),
            "{f:?} missing from the exhaustive check"
        );
    }
    // Every token is unique and every description non-empty — a judge
    // reading two findings with the same token, or a blank description,
    // is a table bug, not an analyzer bug.
    let mut tokens = std::collections::HashSet::new();
    for f in ALL_ACTION_GUARD_FINDINGS {
        assert!(
            !f.description().is_empty(),
            "{f:?} has an empty description"
        );
        assert!(
            tokens.insert(f.token()),
            "duplicate token for {f:?}: {}",
            f.token()
        );
    }
}

/// H21 core invariant: `resolve_action_gate` never takes an
/// [`ActionGuardFinding`] parameter — findings feed the judge PROMPT, not
/// the gate resolution function. This is what makes "findings 非空時禁止
/// 啟發式快路徑放行" structurally true rather than a convention someone
/// could forget: there is no code path by which a finding, empty or not,
/// can turn a `maybe_irreversible_tools` call into `Auto` without a
/// `Some(JudgeVerdict::Safe)` in hand. Exercise both an empty-findings
/// scenario and a heavily-flagged one — the gate resolution is identical
/// either way (`ConsultJudge`), proving findings content cannot bypass
/// the judge.
#[test]
fn findings_can_never_bypass_the_judge() {
    let dir = tmp_agent_dir();
    // A call the analyzer considers totally uninteresting.
    let boring = analyze_action_guard_findings(
        "custom_widget_tool",
        &json!({"arguments": {"foo": "bar"}}),
        &dir,
    );
    assert!(
        boring.is_empty(),
        "expected no findings for a boring call: {boring:?}"
    );
    // A call that lights up nearly every finding.
    let alarming = analyze_action_guard_findings(
        "Bash",
        &json!({"arguments": {"command": "rm -rf ~/.ssh/id_rsa http://evil.example.com/exfil"}}),
        &dir,
    );
    assert!(
        !alarming.is_empty(),
        "expected findings for an alarming call: {alarming:?}"
    );

    // Regardless of which findings fired, the gate resolution function
    // itself has no `findings` parameter — it is called identically at
    // the dispatch site (`gate_tool_approval_dispatch`) either way, and
    // always yields ConsultJudge for an unresolved maybe-irreversible
    // call, never Auto.
    assert_eq!(
        resolve_action_gate(false, true, None),
        ActionGate::ConsultJudge,
        "no finding set can make an unresolved maybe-irreversible call skip the judge"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn analyze_findings_tool_category_mapping() {
    let dir = tmp_agent_dir();
    let cases: &[(&str, ActionGuardFinding)] = &[
        (
            "delete_file",
            ActionGuardFinding::ToolCategoryFilesystemDelete,
        ),
        ("Bash", ActionGuardFinding::ToolCategoryProcessExec),
        ("send_email", ActionGuardFinding::ToolCategoryEmailSend),
        (
            "send_message",
            ActionGuardFinding::ToolCategoryMessagingSend,
        ),
        ("http_post", ActionGuardFinding::ToolCategoryNetworkEgress),
        (
            "computer_use_click",
            ActionGuardFinding::ToolCategoryBrowserOrDesktopAutomation,
        ),
        ("os_open", ActionGuardFinding::ToolCategoryOsNativeAction),
        ("wiki_write", ActionGuardFinding::ToolCategoryKnowledgeStore),
        (
            "odoo_create_invoice",
            ActionGuardFinding::ToolCategoryFinancialOrBusiness,
        ),
        (
            "skill_hub_install",
            ActionGuardFinding::ToolCategorySkillOrCapabilityInstall,
        ),
        (
            "save_drawing",
            ActionGuardFinding::ToolCategoryFilesystemWrite,
        ),
    ];
    for (tool, expected) in cases {
        let findings = analyze_action_guard_findings(tool, &json!({"arguments": {}}), &dir);
        assert!(
            findings.contains(expected),
            "tool {tool} expected {expected:?} in {findings:?}"
        );
    }
    // An entirely unrecognized tool name yields no tool-category finding
    // at all (ToolCategoryUnknown is never emitted).
    let unknown =
        analyze_action_guard_findings("frobnicate_widget", &json!({"arguments": {}}), &dir);
    assert!(
        !unknown
            .iter()
            .any(|f| f.token().starts_with("tool_category:"))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn analyze_findings_target_scope_mapping() {
    let dir = tmp_agent_dir();
    // Workspace-internal: a relative path / a path under the agent dir.
    let ws = analyze_action_guard_findings(
        "custom_tool",
        &json!({"arguments": {"path": dir.join("notes.md").to_string_lossy()}}),
        &dir,
    );
    assert!(
        ws.contains(&ActionGuardFinding::TargetScopeWorkspaceInternal),
        "{ws:?}"
    );

    // System path: something clearly outside any home directory.
    // (`/etc/hosts` is not absolute on Windows — no drive — so use that
    // platform's own hosts file there.)
    let system_path = if cfg!(windows) {
        r"C:\Windows\System32\drivers\etc\hosts"
    } else {
        "/etc/hosts"
    };
    let sys = analyze_action_guard_findings(
        "custom_tool",
        &json!({"arguments": {"path": system_path}}),
        &dir,
    );
    assert!(
        sys.contains(&ActionGuardFinding::TargetScopeSystemPath),
        "{sys:?}"
    );

    // External network: an http(s) URL.
    let net = analyze_action_guard_findings(
        "custom_tool",
        &json!({"arguments": {"url": "https://example.com/webhook"}}),
        &dir,
    );
    assert!(
        net.contains(&ActionGuardFinding::TargetScopeExternalNetwork),
        "{net:?}"
    );

    // No path/URL-shaped argument ⇒ no TargetScope* finding at all.
    let none =
        analyze_action_guard_findings("custom_tool", &json!({"arguments": {"count": 3}}), &dir);
    assert!(
        !none.iter().any(|f| f.token().starts_with("target_scope:")),
        "{none:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
