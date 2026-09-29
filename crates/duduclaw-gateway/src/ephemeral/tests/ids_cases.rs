//! Unit tests for [`super`], moved verbatim out of the former `ephemeral.rs` — ids cases.

use super::*;

/// WP21 欠帳 ②: `duduclaw_core::org_field_guard` guards
/// `<home>/agents/.ephemeral/<id>/agent.toml` against `reports_to` /
/// `department` tampering, but duduclaw-core cannot depend on the gateway
/// so it hard-codes the directory name. Renaming this constant without
/// updating the guard would silently un-protect every ephemeral agent's
/// org record — this test is the tripwire.
#[test]
fn ephemeral_dir_name_is_pinned_for_the_org_field_guard() {
    assert_eq!(EPHEMERAL_DIR_NAME, ".ephemeral");
    let home = Path::new("/home/.duduclaw");
    let path = home
        .join("agents")
        .join(EPHEMERAL_DIR_NAME)
        .join("eph-abc")
        .join("agent.toml");
    assert_eq!(
        duduclaw_core::classify_protected_toml(&path, home),
        Some(duduclaw_core::ProtectedTomlKind::AgentToml),
        "org-field guard no longer recognises the ephemeral agent dir"
    );
}

// ── Privilege escalation (fail-closed subsetting) ────────────────────

#[test]
fn subset_rejects_tool_outside_parent_allowlist() {
    let parent = caps(&["Read", "Grep"], &[]);
    let err = check_tool_subset(&parent, &strs(&["Read", "Bash"])).unwrap_err();
    assert!(err.contains("privilege escalation"), "got: {err}");
}

#[test]
fn subset_rejects_parent_denied_tool_even_when_parent_unrestricted() {
    let parent = caps(&[], &["Bash"]);
    let err = check_tool_subset(&parent, &strs(&["Bash"])).unwrap_err();
    assert!(err.contains("denied_tools"), "got: {err}");
}

#[test]
fn subset_accepts_strict_subset_case_insensitive() {
    let parent = caps(&["Read", "Grep", "WebFetch"], &[]);
    assert!(check_tool_subset(&parent, &strs(&["read", "grep"])).is_ok());
}

#[test]
fn subset_accepts_any_tool_when_parent_unrestricted() {
    let parent = caps(&[], &[]);
    assert!(check_tool_subset(&parent, &strs(&["Read", "Bash(git:*)"])).is_ok());
}

#[test]
fn subset_rejects_empty_request_deny_by_default() {
    let parent = caps(&[], &[]);
    assert!(check_tool_subset(&parent, &[]).is_err());
}

#[test]
fn subset_rejects_malformed_tool_name() {
    let parent = caps(&[], &[]);
    assert!(check_tool_subset(&parent, &strs(&["evil;rm -rf /"])).is_err());
    assert!(check_tool_subset(&parent, &strs(&["../escape"])).is_err());
}

// ── Team intrinsics (WP-2 ↔ WP-5 seam) ───────────────────────────────

#[test]
fn intrinsics_pass_an_allowlist_that_does_not_name_them() {
    let parent = caps(&["Read", "Grep"], &[]);
    // Without the exemption this is the "employee has an allowlist ⇒ no
    // team can ever form" failure.
    assert!(check_tool_subset(&parent, &strs(&["team_handoff", "Read"])).is_err());
    assert!(
        check_tool_subset_with_intrinsics(
            &parent,
            &strs(&["team_handoff", "Read"]),
            TEAM_INTRINSIC_TOOLS,
        )
        .is_ok()
    );
}

#[test]
fn intrinsics_do_not_widen_any_other_tool() {
    let parent = caps(&["Read"], &[]);
    let err = check_tool_subset_with_intrinsics(
        &parent,
        &strs(&["team_handoff", "Bash"]),
        TEAM_INTRINSIC_TOOLS,
    )
    .unwrap_err();
    assert!(err.contains("privilege escalation"), "got: {err}");
    assert!(err.contains("Bash"), "got: {err}");
}

#[test]
fn an_explicitly_denied_intrinsic_still_loses() {
    // deny wins: an operator who denies the handoff channel gets a failed
    // round, not a silent escalation.
    let parent = caps(&[], &["team_handoff"]);
    let err = check_tool_subset_with_intrinsics(
        &parent,
        &strs(&["team_handoff"]),
        TEAM_INTRINSIC_TOOLS,
    )
    .unwrap_err();
    assert!(err.contains("denied_tools"), "got: {err}");
    // …and the charset guard is not skipped either.
    assert!(
        check_tool_subset_with_intrinsics(
            &caps(&[], &[]),
            &strs(&["team_handoff;rm -rf /"]),
            TEAM_INTRINSIC_TOOLS,
        )
        .is_err()
    );
}

/// `parent_capabilities` must report the SAME envelope `scaffold_with`
/// enforces — a caller that derives a tool list from it (the team
/// composer's executor role) would otherwise be rejected on a list it
/// read off the parent itself.
#[test]
fn parent_capabilities_matches_what_the_subset_check_enforces() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(
        home,
        "boss",
        "[capabilities]\nallowed_tools = [\"Read\", \"Write\"]\ndenied_tools = [\"Bash\"]\n",
    );
    let caps = parent_capabilities(home, "boss").unwrap();
    assert_eq!(caps.allowed_tools, strs(&["Read", "Write"]));
    assert_eq!(caps.denied_tools, strs(&["Bash"]));
    // The derived list it enables is accepted by the very check it mirrors.
    assert!(check_tool_subset(&caps, &caps.allowed_tools.clone()).is_ok());
    // Absent / unreadable parent ⇒ None, and the scaffold produces the one
    // authoritative refusal.
    assert!(parent_capabilities(home, "nobody").is_none());
    let spec = EphemeralSpawnSpec {
        parent: "nobody".into(),
        instruction: "x".into(),
        tools: strs(&["Read"]),
        tier: "cheap".into(),
    };
    assert!(scaffold(home, &spec).unwrap_err().contains("fail-closed"));
}

// ── Scaffold ──────────────────────────────────────────────────────────

#[test]
fn scaffold_creates_contained_dir_with_valid_agent_config() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(
        home,
        "boss",
        "[capabilities]\nallowed_tools = [\"Read\", \"Grep\"]\n",
    );

    let spec = EphemeralSpawnSpec {
        parent: "boss".into(),
        instruction: "You summarize logs. 只做摘要。".into(),
        tools: strs(&["Read"]),
        tier: "cheap".into(),
    };
    let result = scaffold(home, &spec).unwrap();

    assert!(is_ephemeral_id(&result.agent_id));
    // Dir is under the ephemeral root.
    assert!(result.dir.starts_with(ephemeral_root(home)));
    // agent.toml parses as a full AgentConfig with the restricted subset.
    let cfg: duduclaw_core::types::AgentConfig =
        toml::from_str(&std::fs::read_to_string(result.dir.join("agent.toml")).unwrap())
            .unwrap();
    assert_eq!(cfg.agent.reports_to, "boss");
    assert_eq!(cfg.capabilities.allowed_tools, vec!["Read".to_string()]);
    // Parent's model config copied verbatim — no hardcoded ids injected.
    assert_eq!(cfg.model.preferred, "parent-preferred-model");
    // SOUL.md carries the instruction (CJK-safe write).
    let soul = std::fs::read_to_string(result.dir.join("SOUL.md")).unwrap();
    assert!(soul.contains("只做摘要"));
    // Metadata sidecar records tier + parent.
    let meta = read_meta(&result.dir).unwrap();
    assert_eq!(meta.parent, "boss");
    assert_eq!(meta.tier, "cheap");
    // Resolvable through the containment-checked resolver.
    assert!(resolve_agent_dir(home, &result.agent_id).is_some());
}

/// The scaffold's only route to the duduclaw MCP server. Nothing else
/// writes this file for an `.ephemeral/` dir — the boot fixup walks
/// `<home>/agents/*` only — so its absence is total tool starvation
/// (`team_handoff` included, i.e. `planner_no_packets`).
#[test]
fn scaffold_writes_mcp_json_with_agent_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "[capabilities]\nallowed_tools = [\"Read\"]\n");

    let spec = EphemeralSpawnSpec {
        parent: "boss".into(),
        instruction: "Summarize.".into(),
        tools: strs(&["Read"]),
        tier: "cheap".into(),
    };
    let result = scaffold(home, &spec).unwrap();

    let cfg = member_mcp_json(&result.dir);
    let server = &cfg["mcpServers"]["duduclaw"];
    assert_eq!(
        server["env"]["DUDUCLAW_AGENT_ID"].as_str(),
        Some(result.agent_id.as_str()),
        "the MCP child must self-identify as this scaffold, not the \
             config.toml default_agent"
    );
    assert_eq!(server["args"][0].as_str(), Some("mcp-server"));
    assert!(
        std::path::Path::new(server["command"].as_str().unwrap()).is_absolute(),
        "a relative command would not resolve from the scaffold's cwd"
    );
    // Deliberately duduclaw-only: the parent's other servers (playwright,
    // …) are NOT copied into a scaffold — see `ensure_scaffold_mcp_config`'s
    // call site for why.
    assert_eq!(
        cfg["mcpServers"].as_object().unwrap().len(),
        1,
        "a scaffold gets the duduclaw server and nothing else"
    );
}

/// Same guarantee on the WP-2 role-member path, whose id shape
/// (`eph-<parent>-r<n>-<role>-<rand>`) is what the live round used.
#[test]
fn role_member_scaffold_writes_mcp_json_with_agent_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "[capabilities]\nallowed_tools = [\"Read\"]\n");
    // With `identity.key` present the env block must also carry a token
    // that verifies against *this* home — proving the extra `.ephemeral/`
    // path segment did not poison the derived key root.
    let key = duduclaw_core::ensure_identity_key(home).unwrap();

    let spec = role_spec("boss", Role::Planner, "claude", "claude-opus-5");
    let result = scaffold_role_member(home, &spec).unwrap();

    let env = member_mcp_json(&result.dir)["mcpServers"]["duduclaw"]["env"].clone();
    assert_eq!(
        env["DUDUCLAW_AGENT_ID"].as_str(),
        Some(result.agent_id.as_str())
    );
    let token = env["DUDUCLAW_AGENT_TOKEN"]
        .as_str()
        .expect("identity token written for a role member");
    assert!(
        duduclaw_core::verify_identity_token(&key, &result.agent_id, token),
        "token must verify under the real home's key, not <home>/agents"
    );
}

#[test]
fn role_member_system_prefix_is_stable_across_tasks() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "[capabilities]\nallowed_tools = [\"Read\"]\n");
    let mut first = role_spec("boss", Role::Planner, "claude", "claude-opus-5");
    first.instruction = "private task alpha".into();
    let mut second = first.clone();
    second.task_id = "task-beta".into();
    second.round = 2;
    second.instruction = "private task beta".into();
    let a = scaffold_role_member(home, &first).unwrap();
    let b = scaffold_role_member(home, &second).unwrap();
    let soul_a = std::fs::read_to_string(a.dir.join("SOUL.md")).unwrap();
    let soul_b = std::fs::read_to_string(b.dir.join("SOUL.md")).unwrap();
    assert_eq!(soul_a, soul_b);
    assert!(soul_a.contains("team_handoff"));
    assert!(!soul_a.contains("private task"));
    let agent_a = member_toml(&a.dir);
    let agent_b = member_toml(&b.dir);
    assert_eq!(
        agent_a["agent"]["display_name"],
        agent_b["agent"]["display_name"]
    );
}

#[test]
fn role_member_keeps_parent_identity_after_role_cache_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "[capabilities]\nallowed_tools = [\"Read\"]\n");
    let parent_soul = home.join("agents/boss/SOUL.md");
    std::fs::write(&parent_soul, "Careful and concise identity").unwrap();
    let spec = role_spec("boss", Role::Planner, "claude", "claude-opus-5");
    let a = scaffold_role_member(home, &spec).unwrap();
    let first = std::fs::read_to_string(a.dir.join("SOUL.md")).unwrap();
    let (role_prefix, identity) = first
        .split_once(crate::direct_api::CACHE_SPLIT_MARKER)
        .unwrap();
    assert!(role_prefix.contains("team_handoff"));
    assert_eq!(identity.trim(), "Careful and concise identity");

    std::fs::write(&parent_soul, "Changed identity").unwrap();
    let b = scaffold_role_member(home, &spec).unwrap();
    let second = std::fs::read_to_string(b.dir.join("SOUL.md")).unwrap();
    assert_eq!(
        second
            .split_once(crate::direct_api::CACHE_SPLIT_MARKER)
            .unwrap()
            .0,
        role_prefix
    );
    assert!(second.ends_with("Changed identity"));
}

/// The marker predicate that gates a role member's side effects (memory
/// distillation). Presence of the section is enough — a malformed one must
/// still read as "role member", unlike [`read_role_member`].
#[test]
fn role_member_marker_is_presence_only_and_ordinary_ephemerals_are_not_members() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "[capabilities]\nallowed_tools = [\"Read\"]\n");

    let member = scaffold_role_member(
        home,
        &role_spec("boss", Role::Planner, "claude", "claude-opus-5"),
    )
    .unwrap();
    assert!(has_role_member_marker(&member.dir));
    assert!(is_role_member(home, &member.agent_id));

    let plain = scaffold(
        home,
        &EphemeralSpawnSpec {
            parent: "boss".into(),
            instruction: "Summarize.".into(),
            tools: strs(&["Read"]),
            tier: "cheap".into(),
        },
    )
    .unwrap();
    assert!(!has_role_member_marker(&plain.dir));
    assert!(!is_role_member(home, &plain.agent_id));

    // An ordinary (non-ephemeral) agent id never touches the filesystem.
    assert!(!is_role_member(home, "boss"));

    // Malformed section ⇒ `read_role_member` gives up, the marker does not.
    let toml_path = member.dir.join("agent.toml");
    let mut raw = std::fs::read_to_string(&toml_path).unwrap();
    raw = raw.replace("round = 2", "round = \"two\"");
    std::fs::write(&toml_path, raw).unwrap();
    assert!(read_role_member(&member.dir).is_none());
    assert!(
        has_role_member_marker(&member.dir),
        "a broken marker must still withhold role-member side effects"
    );
}
