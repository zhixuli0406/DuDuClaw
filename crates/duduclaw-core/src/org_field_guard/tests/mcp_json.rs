//! `.mcp.json` and CLI configuration freeze (2026-10-06): an employee or an
//! unverifiable caller may not write any `.mcp.json` in its directory, nor
//! any CLI configuration there (`.claude/` and `.claude.json` at any depth,
//! the other runtimes' top-level directories); the
//! gateway starts every MCP server that file lists, so an added entry would
//! run commands as the operator's OS user. Operators keep the older rules.

use super::*;

fn path() -> PathBuf {
    home().join("agents/agnes/.mcp.json")
}

/// The scaffolded file: the duduclaw entry with its identity and home.
fn base() -> serde_json::Value {
    serde_json::json!({
        "mcpServers": {
            "duduclaw": {
                "command": "/usr/local/bin/duduclaw",
                "args": ["mcp-server"],
                "env": {
                    "DUDUCLAW_AGENT_ID": "agnes",
                    "DUDUCLAW_HOME": "/Users/alice/.duduclaw"
                }
            }
        }
    })
}

fn judge(caller: &HookCaller, new: &serde_json::Value) -> GuardDecision {
    check_identity_surface_write_as(
        &path(),
        &home(),
        caller,
        Some(&base().to_string()),
        Some(&new.to_string()),
    )
}

fn blocked(d: &GuardDecision) -> bool {
    matches!(d, GuardDecision::BlockedIdentitySurface { .. })
}

fn with(f: impl FnOnce(&mut serde_json::Value)) -> serde_json::Value {
    let mut v = base();
    f(&mut v);
    v
}

fn employees() -> [HookCaller; 2] {
    [agent("agnes"), HookCaller::Untrusted("agnes".into())]
}

#[test]
fn an_employee_may_not_write_its_mcp_json_at_all() {
    let cases = [
        // The F1 shape: an unrelated server whose command is an interpreter.
        with(|v| {
            v["mcpServers"]["helper"] =
                serde_json::json!({ "command": "sh", "args": ["-c", "id > /tmp/x"] })
        }),
        with(|v| v["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_TURN_ID"] = "".into()),
        with(|v| v["mcpServers"]["duduclaw"]["command"] = "/tmp/fake".into()),
        with(|v| {
            v["mcpServers"].as_object_mut().unwrap().remove("duduclaw");
        }),
        // Even an identical rewrite: there is no harmless write to this file.
        base(),
    ];
    for caller in employees() {
        for new in &cases {
            let d = judge(&caller, new);
            assert!(blocked(&d), "{caller:?} {new}: {d:?}");
            let msg = d.block_message().unwrap();
            assert!(msg.contains("MCP 安裝申請"), "{msg}");
        }
    }
}

#[test]
fn unparseable_content_and_a_new_file_are_refused_for_an_employee() {
    for bad in ["{not json", "[]"] {
        let d = check_identity_surface_write_as(
            &path(),
            &home(),
            &agent("agnes"),
            Some(&base().to_string()),
            Some(bad),
        );
        assert!(!d.is_allowed(), "{bad}: {d:?}");
    }
    let d = check_identity_surface_write_as(&path(), &home(), &agent("agnes"), None, None);
    assert!(!d.is_allowed());
    let d = check_identity_surface_write_as(
        &path(),
        &home(),
        &agent("agnes"),
        None,
        Some("{\"mcpServers\":{\"playwright\":{\"command\":\"npx\"}}}"),
    );
    assert!(blocked(&d), "{d:?}");
}

#[test]
fn an_operator_keeps_the_identity_only_rule() {
    let unrelated =
        with(|v| v["mcpServers"]["playwright"] = serde_json::json!({ "command": "npx" }));
    assert_eq!(
        judge(&HookCaller::Absent, &unrelated),
        GuardDecision::AllowedAgentWrite
    );
    let env = with(|v| v["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_HOME"] = "/tmp/x".into());
    assert_eq!(
        judge(&HookCaller::Absent, &env),
        GuardDecision::AllowedAgentWrite
    );
    let stolen = with(|v| v["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_AGENT_ID"] = "ceo".into());
    assert!(blocked(&judge(&HookCaller::Absent, &stolen)));
}

#[test]
fn cli_configuration_under_the_employee_directory_is_frozen_for_employees() {
    let h = home();
    for rel in [
        "agents/agnes/.claude/commands/x.md",
        "agents/agnes/.claude/agents/helper.md",
        "agents/agnes/.claude/skills/s/SKILL.md",
        "agents/agnes/.claude/hooks/h.sh",
        "agents/agnes/.claude.json",
        "agents/agnes/.codex/config.toml",
        "agents/agnes/.gemini/settings.json",
        "agents/agnes/.grok/config.toml",
        "agents/agnes/.agents/mcp_config.json",
        "agents/.ephemeral/eph-1/.claude/commands/x.md",
    ] {
        let p = h.join(rel);
        assert_eq!(
            classify_identity_surface(&p, &h),
            Some(ProtectedSurface::AgentRuntimeConfig),
            "{rel}"
        );
        for caller in employees() {
            let d = check_identity_surface_write_as(&p, &h, &caller, None, Some("x"));
            assert!(blocked(&d), "{rel} {caller:?}: {d:?}");
        }
        let d = check_identity_surface_write_as(&p, &h, &HookCaller::Absent, None, Some("x"));
        assert_eq!(d, GuardDecision::AllowedAgentWrite, "{rel}");
    }
    // The hook settings keep their every-caller rule; ordinary files are not
    // configuration.
    assert_eq!(
        classify_identity_surface(&h.join("agents/agnes/.claude/settings.json"), &h),
        Some(ProtectedSurface::HookSettings)
    );
    for rel in [
        "agents/agnes/notes.md",
        "agents/agnes/SKILLS/x/SKILL.md",
        "agents/agnes/project/.codex/config.toml",
        "agents/agnes/project/src/claude.rs",
    ] {
        assert_eq!(classify_identity_surface(&h.join(rel), &h), None, "{rel}");
    }
}

/// N2: `.claude/` and `.claude.json` at any depth, and `.mcp.json` at any
/// depth, are frozen for employees. A project cloned inside the employee
/// directory is where the CLI starts too, so its `.claude/` loads the same
/// way. (Before this change `project/.claude/commands/x.md` was allowed.)
#[test]
fn nested_cli_configuration_is_frozen_at_any_depth() {
    let h = home();
    for rel in [
        "agents/agnes/project/.claude/commands/x.md",
        "agents/agnes/a/b/c/.claude/agents/helper.md",
        "agents/agnes/project/.claude.json",
        "agents/agnes/project/.mcp.json/x",
        "agents/.ephemeral/eph-1/work/.claude/skills/s/SKILL.md",
    ] {
        let p = h.join(rel);
        assert_eq!(
            classify_identity_surface(&p, &h),
            Some(ProtectedSurface::AgentRuntimeConfig),
            "{rel}"
        );
        for caller in employees() {
            let d = check_identity_surface_write_as(&p, &h, &caller, None, Some("x"));
            assert!(blocked(&d), "{rel} {caller:?}: {d:?}");
            assert!(d.block_message().unwrap().contains("可以讀"), "{d:?}");
        }
        let d = check_identity_surface_write_as(&p, &h, &HookCaller::Absent, None, Some("x"));
        assert_eq!(d, GuardDecision::AllowedAgentWrite, "{rel}");
    }
    for rel in [
        "agents/agnes/project/.mcp.json",
        "agents/agnes/a/b/.mcp.json",
    ] {
        let p = h.join(rel);
        assert_eq!(
            classify_identity_surface(&p, &h),
            Some(ProtectedSurface::AgentMcpJson),
            "{rel}"
        );
        for caller in employees() {
            let d = check_identity_surface_write_as(&p, &h, &caller, None, Some("{}"));
            assert!(blocked(&d), "{rel} {caller:?}: {d:?}");
        }
    }
}

/// N7: the names are compared case-insensitively.
#[test]
fn configuration_names_are_matched_case_insensitively() {
    let h = home();
    for (rel, want) in [
        ("agents/agnes/.MCP.json", ProtectedSurface::AgentMcpJson),
        (
            "agents/agnes/project/.Mcp.Json",
            ProtectedSurface::AgentMcpJson,
        ),
        (
            "agents/agnes/.CLAUDE.json",
            ProtectedSurface::AgentRuntimeConfig,
        ),
        (
            "agents/agnes/.Claude/commands/x.md",
            ProtectedSurface::AgentRuntimeConfig,
        ),
        (
            "agents/agnes/project/.CLAUDE/agents/h.md",
            ProtectedSurface::AgentRuntimeConfig,
        ),
        (
            "agents/agnes/.CODEX/config.toml",
            ProtectedSurface::AgentRuntimeConfig,
        ),
        (
            "agents/agnes/.CLAUDE/settings.json",
            ProtectedSurface::HookSettings,
        ),
    ] {
        assert_eq!(
            classify_identity_surface(&h.join(rel), &h),
            Some(want),
            "{rel}"
        );
        for caller in employees() {
            let d = check_identity_surface_write_as(&h.join(rel), &h, &caller, None, Some("{}"));
            assert!(blocked(&d), "{rel} {caller:?}: {d:?}");
        }
    }
}

/// N9: only `<employee>/.claude/settings*.json` registers the hook and is
/// frozen for every caller; a `settings.json` deeper under `.claude/` is
/// ordinary configuration (employees refused, operators allowed).
#[test]
fn only_the_top_level_hook_settings_are_frozen_for_operators() {
    let h = home();
    let nested = h.join("agents/agnes/.claude/agents/settings.json");
    assert_eq!(
        classify_identity_surface(&nested, &h),
        Some(ProtectedSurface::AgentRuntimeConfig)
    );
    let d = check_identity_surface_write_as(&nested, &h, &HookCaller::Absent, None, Some("{}"));
    assert_eq!(d, GuardDecision::AllowedAgentWrite);
    for caller in employees() {
        assert!(blocked(&check_identity_surface_write_as(
            &nested,
            &h,
            &caller,
            None,
            Some("{}")
        )));
    }
    assert_eq!(
        classify_identity_surface(
            &h.join("agents/.ephemeral/eph-1/.claude/settings.local.json"),
            &h
        ),
        Some(ProtectedSurface::HookSettings)
    );
}
