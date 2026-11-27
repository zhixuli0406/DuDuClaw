use super::*;
use duduclaw_auth::UserDb;
use duduclaw_core::EditionProfile;

struct Fixture {
    dir: tempfile::TempDir,
    db: UserDb,
    admin: String,
    emp: String,
}

fn fixture(edition: Option<EditionProfile>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = UserDb::new(&dir.path().join("users.db")).unwrap();
    let admin = db.create_user("admin@example.invalid", "Admin", "isolated-test-password", UserRole::Admin)
        .unwrap().id;
    let emp = db.create_user("emp@example.invalid", "Employee", "isolated-test-password", UserRole::Employee)
        .unwrap().id;
    if let Some(edition) = edition { crate::team_edition::record(dir.path(), edition); }
    Fixture { dir, db, admin, emp }
}

const USES: [MainUse; 2] = [MainUse::Converse, MainUse::Assign];

#[test]
fn active_admin_and_exact_listed_employee_decisions() {
    let f = fixture(Some(EditionProfile::Enterprise));
    for use_ in USES {
        assert_eq!(main_access_allowed(f.dir.path(), &f.admin, use_), Ok(true));
        assert_eq!(main_access_allowed(f.dir.path(), &f.emp, use_), Ok(false));
        assert_eq!(main_access_allowed(f.dir.path(), "missing", use_), Ok(false));
    }
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    for use_ in USES {
        assert_eq!(main_access_allowed(f.dir.path(), &f.emp, use_), Ok(true));
        assert_eq!(main_access_allowed(f.dir.path(), &format!("{}-extra", f.emp), use_), Ok(false));
        assert_eq!(main_access_allowed(f.dir.path(), &format!(" {} ", f.emp), use_), Ok(false));
    }
    f.db.set_main_agent_users(&f.admin, &[], true).unwrap();
    assert_eq!(main_access_allowed(f.dir.path(), &f.emp, MainUse::Converse), Ok(false));
}

#[test]
fn suspension_removes_list_entry_and_denies_even_admin() {
    let f = fixture(Some(EditionProfile::Enterprise));
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    let conn = rusqlite::Connection::open(f.dir.path().join("users.db")).unwrap();
    conn.execute("UPDATE users SET status='suspended' WHERE id=?1", [&f.emp]).unwrap();
    assert!(!f.db.is_main_agent_user(&f.emp).unwrap());
    assert!(f.db.main_agent_users().unwrap().is_empty());
    conn.execute("UPDATE users SET status='suspended' WHERE id=?1", [&f.admin]).unwrap();
    for use_ in USES {
        assert_eq!(main_access_allowed(f.dir.path(), &f.emp, use_), Ok(false));
        assert_eq!(main_access_allowed(f.dir.path(), &f.admin, use_), Ok(false));
    }
}

#[test]
fn personal_downgrade_ignores_stale_rows_but_retains_admin() {
    let f = fixture(Some(EditionProfile::Enterprise));
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    crate::team_edition::record(f.dir.path(), EditionProfile::Personal);
    assert!(f.db.is_main_agent_user(&f.emp).unwrap());
    for use_ in USES {
        assert_eq!(main_access_allowed(f.dir.path(), &f.emp, use_), Ok(false));
        assert_eq!(main_access_allowed(f.dir.path(), &f.admin, use_), Ok(true));
    }
}

#[test]
fn unresolved_edition_ignores_list() {
    let f = fixture(None);
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    for use_ in USES {
        assert_eq!(main_access_allowed(f.dir.path(), &f.emp, use_), Ok(false));
        assert_eq!(main_access_allowed(f.dir.path(), &f.admin, use_), Ok(true));
    }
}

#[test]
fn missing_or_unreadable_identity_store_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(main_access_allowed(dir.path(), "admin", MainUse::Converse).is_err());
    let path = dir.path().join("users.db");
    std::fs::create_dir(&path).unwrap();
    for use_ in USES {
        assert!(main_access_allowed(dir.path(), "admin", use_).is_err());
        assert!(requires_main_restriction_for(dir.path(), Some("user:admin"), use_).is_err());
    }
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(path, b"not a SQLite database").unwrap();
    assert!(main_access_allowed(dir.path(), "admin", MainUse::Converse).is_err());
}

#[test]
fn principal_classification_and_legacy_admin_only_gate_are_preserved() {
    let f = fixture(Some(EditionProfile::Enterprise));
    let employee = format!("user:{}", f.emp);
    for use_ in USES {
        assert_eq!(requires_main_restriction_for(f.dir.path(), Some(&employee), use_), Ok(true));
    }
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    for use_ in USES {
        assert_eq!(requires_main_restriction_for(f.dir.path(), None, use_), Ok(false));
        assert_eq!(requires_main_restriction_for(f.dir.path(),
            Some("unmapped:line:0123456789abcdef"), use_), Ok(false));
        for unknown in ["", "unknown:rejected-principal", "user:"] {
            assert!(requires_main_restriction_for(f.dir.path(), Some(unknown), use_).is_err());
        }
        assert_eq!(requires_main_restriction_for(f.dir.path(), Some(&employee), use_), Ok(false));
        assert_eq!(requires_main_restriction_for(f.dir.path(), Some("user:missing"), use_), Ok(true));
        assert_eq!(requires_main_restriction_for(f.dir.path(),
            Some(&format!("  {employee}  ")), use_), Ok(false));
    }
    assert_eq!(requires_main_restriction(f.dir.path(), Some(&employee)), Ok(true));
    assert_eq!(requires_main_restriction(f.dir.path(), Some(&format!("user:{}", f.admin))), Ok(false));
}

fn write_main(f: &Fixture, extra: &str) {
    crate::trigger_route::tests::write_agent(
        &f.dir.path().join("agents"), "main", "Main", "", "main", extra,
    );
}

#[tokio::test]
async fn converse_loaded_gate_uses_role_default_name_and_directory_without_changing_legacy() {
    let f = fixture(Some(EditionProfile::Enterprise));
    write_main(&f, "[capabilities]\nallowed_tools = ['Read']\ndenied_tools = ['Bash']\n");
    std::fs::write(f.dir.path().join("config.toml"), "[general]\ndefault_agent = 'main'\n").unwrap();
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    let mut loaded = AgentRegistry::load_agent(&f.dir.path().join("agents/main")).await.unwrap();
    let wire = format!("user:{}", f.emp);
    assert!(check_loaded_main_target_for(f.dir.path(), Some(&wire), &loaded, MainUse::Converse).await.is_ok());
    assert!(check_loaded_main_target(f.dir.path(), Some(&wire), &loaded).await.is_err());
    // The configured default also counts when it is a specialist.
    loaded.config.agent.role = AgentRole::Specialist;
    assert!(check_loaded_main_target_for(f.dir.path(), Some(&wire), &loaded, MainUse::Converse).await.is_ok());
    loaded.config.agent.name = "renamed-main".into();
    f.db.set_main_agent_users(&f.admin, &[], true).unwrap();
    assert!(check_loaded_main_target_for(f.dir.path(), Some(&wire), &loaded, MainUse::Converse).await.is_err());
    // Ordinary non-main targets preserve their existing gate behavior.
    loaded.dir = f.dir.path().join("agents/ordinary");
    assert!(check_loaded_main_target_for(f.dir.path(), Some(&wire), &loaded, MainUse::Converse).await.is_ok());
}

#[tokio::test]
async fn listed_shell_turns_are_restricted_but_admin_is_unaffected() {
    let f = fixture(Some(EditionProfile::Enterprise));
    write_main(&f, "[capabilities]\nallowed_tools = ['Read', 'Bash']\n");
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    assert!(!duduclaw_core::execution_gate::isolation_required());
    for use_ in USES {
        assert_eq!(main_turn_allowed(f.dir.path(), &f.emp, "main", use_).await, Ok(false));
        assert_eq!(requires_main_restriction_for_target(f.dir.path(),
            Some(&format!("user:{}", f.emp)), "main", use_).await, Ok(true));
        assert_eq!(main_turn_allowed(f.dir.path(), &f.admin, "main", use_).await, Ok(true));
        // Admin admission does not depend on reading employee configuration.
        assert_eq!(main_turn_allowed(f.dir.path(), &f.admin, "missing", use_).await, Ok(true));
    }
}

#[tokio::test]
async fn listed_shell_free_turns_are_allowed_with_exact_identity_and_target() {
    let f = fixture(Some(EditionProfile::Enterprise));
    write_main(&f, "[capabilities]\nallowed_tools = ['Read']\ndenied_tools = ['Bash']\n");
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    for use_ in USES {
        assert_eq!(main_turn_allowed(f.dir.path(), &f.emp, "main", use_).await, Ok(true));
        assert_eq!(requires_main_restriction_for_target(f.dir.path(),
            Some(&format!("user:{}", f.emp)), "main", use_).await, Ok(false));
        assert_eq!(main_turn_allowed(f.dir.path(), &format!("{}-extra", f.emp), "main", use_).await, Ok(false));
        assert_eq!(main_turn_allowed(f.dir.path(), &format!(" {} ", f.emp), "main", use_).await, Ok(false));
        for target in ["main-extra", " main ", "../main", ""] {
            assert!(main_turn_allowed(f.dir.path(), &f.emp, target, use_).await.is_err());
        }
        for principal in [None, Some("unmapped:line:0123456789abcdef")] {
            assert_eq!(requires_main_restriction_for_target(f.dir.path(), principal, "main", use_).await, Ok(false));
        }
        for principal in ["", "user:", "unknown:rejected-principal"] {
            assert!(requires_main_restriction_for_target(f.dir.path(), Some(principal), "main", use_).await.is_err());
        }
    }
}

#[tokio::test]
async fn isolation_composition_allows_shell_without_latching_global_state() {
    let f = fixture(Some(EditionProfile::Enterprise));
    write_main(&f, "[capabilities]\nallowed_tools = ['Bash']\n");
    // require_isolation() is irreversible; exercise the production composition
    // with an injected fact instead of contaminating unrelated parallel tests.
    assert_eq!(main_tools::listed_access_effective_with_isolation(f.dir.path(), "main", false).await, Ok(false));
    assert_eq!(main_tools::listed_access_effective_with_isolation(f.dir.path(), "main", true).await, Ok(true));
}

#[tokio::test]
async fn unreadable_or_malformed_tool_configuration_is_an_error() {
    let f = fixture(Some(EditionProfile::Enterprise));
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    for extra in [
        "[capabilities]\nallowed_tools = ['Read', 42]\n",
        "[capabilities]\ndenied_tools = 'Bash'\n",
        "[runtime]\nminimal_context = 'false'\n",
        "[runtime]\nprovider = 'not-a-runtime'\n",
        "[runtime]\nprovider = 'gemini'\n",
        "[runtime]\nprovider = 'antigravity'\n",
        "[runtime]\nfallback = 'not-a-runtime'\n",
    ] {
        write_main(&f, extra);
        assert!(main_turn_allowed(f.dir.path(), &f.emp, "main", MainUse::Converse).await.is_err());
    }
    let path = f.dir.path().join("agents/main/agent.toml");
    std::fs::write(&path, "invalid TOML !").unwrap();
    assert!(listed_access_effective(f.dir.path(), "main").await.is_err());
    std::fs::remove_file(&path).unwrap();
    assert!(listed_access_effective(f.dir.path(), "main").await.is_err());
    // A directory is unreadable as a config even when tests run as root.
    std::fs::create_dir(&path).unwrap();
    assert!(listed_access_effective(f.dir.path(), "main").await.is_err());
}

#[tokio::test]
async fn malformed_resolved_artifact_cannot_fall_back_to_shell_free_source() {
    let f = fixture(Some(EditionProfile::Enterprise));
    write_main(&f, "[capabilities]\nallowed_tools = ['Read']\n");
    let resolved = duduclaw_core::preset::agent_resolved_path(f.dir.path(), "main");
    std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
    std::fs::write(&resolved, "[capabilities]\nallowed_tools = ['Read', 42]\n").unwrap();
    assert!(listed_access_effective(f.dir.path(), "main").await.is_err());
}

#[tokio::test]
async fn other_runtimes_and_fallbacks_do_not_inherit_claude_tool_proof() {
    let f = fixture(Some(EditionProfile::Enterprise));
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    for runtime in ["codex", "grok", "openai-compat"] {
        write_main(&f, &format!(
            "[runtime]\nprovider = '{runtime}'\n[capabilities]\nallowed_tools = ['Read']\ndenied_tools = ['Bash']\n",
        ));
        assert_eq!(main_turn_allowed(f.dir.path(), &f.emp, "main", MainUse::Converse).await, Ok(false));
    }
    write_main(&f, "[runtime]\nfallback = 'codex'\n[capabilities]\nallowed_tools = ['Read']\n");
    assert_eq!(listed_access_effective(f.dir.path(), "main").await, Ok(false));
}

#[test]
fn claude_proof_reuses_both_spawn_tool_surfaces_and_qualified_denies() {
    use duduclaw_core::types::CapabilitiesConfig;
    let mut caps = CapabilitiesConfig::default();
    assert!(main_tools::claude_has_shell(&caps, true));
    caps.denied_tools = vec!["Bash".into()];
    assert!(!main_tools::claude_has_shell(&caps, true));
    for name in duduclaw_core::types::SHELL_TOOLS {
        caps.allowed_tools = vec![(*name).into()];
        caps.denied_tools.clear();
        assert!(main_tools::claude_has_shell(&caps, true), "{name}");
    }
    caps.allowed_tools = vec!["Bash(git:*)".into()];
    caps.denied_tools = vec!["Bash(rm:*)".into()];
    assert!(main_tools::claude_has_shell(&caps, false));
    caps.allowed_tools = vec!["Read".into()];
    assert!(!main_tools::claude_has_shell(&caps, false));
}

#[tokio::test]
async fn assignment_and_execution_recheck_list_and_loaded_shell_without_opening_management() {
    let f = fixture(Some(EditionProfile::Enterprise));
    write_main(&f, "[capabilities]\nallowed_tools = ['Read']\ndenied_tools = ['Bash']\n");
    std::fs::write(f.dir.path().join("config.toml"), "[general]\ndefault_agent = 'main'\n").unwrap();
    let wire = format!("user:{}", f.emp);
    let loaded = AgentRegistry::load_agent(&f.dir.path().join("agents/main")).await.unwrap();
    assert_eq!(check_main_target_for(f.dir.path(), Some(&wire), "main", MainUse::Assign).await,
        Err(REFUSAL.into()));
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    assert!(check_main_target_for(f.dir.path(), Some(&wire), "main", MainUse::Assign).await.is_ok());
    assert!(check_loaded_main_target_for_execution(f.dir.path(), Some(&wire), &loaded).await.is_ok());
    assert!(check_loaded_queue_main_target_for_execution(f.dir.path(), Some(&wire), &loaded, false).await.is_ok());
    assert!(check_loaded_queue_main_target(f.dir.path(), Some(&wire), &loaded, false).await.is_err());
    assert!(check_main_target(f.dir.path(), Some(&wire), "main").await.is_err());
    assert!(check_loaded_queue_main_target_for_execution(f.dir.path(), None, &loaded, false).await.is_err());
    assert!(check_loaded_queue_main_target_for_execution(f.dir.path(), None, &loaded, true).await.is_ok());
    let mut stale_shell = loaded.clone();
    stale_shell.config.capabilities.allowed_tools = vec!["Bash".into()];
    stale_shell.config.capabilities.denied_tools.clear();
    assert_eq!(check_loaded_main_target_for_execution(f.dir.path(), Some(&wire), &stale_shell).await,
        Err(MAIN_TURN_REFUSAL.into()));
    write_main(&f, "[capabilities]\nallowed_tools = ['Read', 'Bash']\n");
    assert_eq!(check_main_target_for(f.dir.path(), Some(&wire), "main", MainUse::Assign).await,
        Err(MAIN_TURN_REFUSAL.into()));
    assert!(check_loaded_main_target_for_execution(f.dir.path(),
        Some(&format!("user:{}", f.admin)), &stale_shell).await.is_ok());
    f.db.set_main_agent_users(&f.admin, &[], true).unwrap();
    assert_eq!(check_loaded_main_target_for_execution(f.dir.path(), Some(&wire), &loaded).await,
        Err(REFUSAL.into()));
}


fn frozen_command(home: &Path, tools: &str, servers: serde_json::Value) -> tokio::process::Command {
    let path = home.join("frozen-mcp.json");
    std::fs::write(&path, serde_json::to_vec(&serde_json::json!({"mcpServers": servers})).unwrap()).unwrap();
    let mut cmd = tokio::process::Command::new("claude");
    cmd.args(["--tools", tools, "--allowedTools", "Read,mcp__duduclaw__*",
        "--disallowedTools", "AskUserQuestion", "--strict-mcp-config", "--mcp-config"]);
    cmd.arg(path);
    cmd
}

#[tokio::test]
async fn mcp_commands_and_foreign_servers_are_not_shell_free() {
    use duduclaw_core::types::COMMAND_CAPABLE_MCP_TOOLS;
    let f = fixture(Some(EditionProfile::Enterprise));
    write_main(&f, "[capabilities]\nallowed_tools = ['Read', 'mcp__duduclaw__*']\n");
    let dir = f.dir.path().join("agents/main");
    let cmd = frozen_command(f.dir.path(), "Read", serde_json::json!({"duduclaw": {}}));
    let mut config = main_tools::read_config(&dir).unwrap();
    assert!(!main_tools::final_command_shell_free(&cmd, &config));
    // An execute_program-only deny leaves other program/desktop entry points.
    config.capabilities.denied_tools = vec!["execute_program".into()];
    assert!(!main_tools::final_command_shell_free(&cmd, &config));
    config.capabilities.denied_tools = COMMAND_CAPABLE_MCP_TOOLS.iter().map(|name| name.to_string()).collect();
    assert!(main_tools::final_command_shell_free(&cmd, &config));
    let mut changed = frozen_command(f.dir.path(), "Read", serde_json::json!({"duduclaw": {}}));
    changed.arg("--tools=Read,Bash");
    assert!(!main_tools::final_command_shell_free(&changed, &config));
    // Both bare and server-qualified denies use the dispatcher's matcher.
    config.capabilities.denied_tools = COMMAND_CAPABLE_MCP_TOOLS.iter()
        .map(|name| format!("mcp__duduclaw__{name}")).collect();
    assert!(main_tools::final_command_shell_free(&cmd, &config));
    for tool in COMMAND_CAPABLE_MCP_TOOLS {
        let mut unsafe_config = config.clone();
        unsafe_config.capabilities.denied_tools.retain(|deny| deny != &format!("mcp__duduclaw__{tool}"));
        assert!(!main_tools::final_command_shell_free(&cmd, &unsafe_config), "{tool}");
    }
    let foreign = frozen_command(f.dir.path(), "Read", serde_json::json!({"duduclaw": {}, "third-party": {}}));
    assert!(!main_tools::final_command_shell_free(&foreign, &config));
    std::fs::write(dir.join(".mcp.json"), r#"{"mcpServers":{"third-party":{"command":"foreign"}}}"#).unwrap();
    assert!(!listed_access_effective(f.dir.path(), "main").await.unwrap_or(false));
}

#[tokio::test]
async fn member_and_unknown_credentials_never_get_listed_converse_or_assign() {
    use duduclaw_auth::acl::ContextOrigin;
    let f = fixture(Some(EditionProfile::Enterprise));
    write_main(&f, "[capabilities]\nallowed_tools = ['Read']\n");
    std::fs::write(f.dir.path().join("config.toml"), "[general]\ndefault_agent = 'main'\n").unwrap();
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    let dir = f.dir.path().join("agents/main");
    let loaded = AgentRegistry::load_agent(&dir).await.unwrap();
    for origin in [ContextOrigin::MemberKey, ContextOrigin::Other, ContextOrigin::DashboardSession] {
        let sender = crate::trigger_route::SenderAccess::no_bindings(&f.emp).with_credential_origin(origin);
        let converse = sender.clone().for_webchat_converse(f.dir.path()).await;
        let assign = sender.for_assign(f.dir.path()).await;
        assert_eq!(converse.main_converse_allowed(&loaded), origin == ContextOrigin::DashboardSession);
        assert_eq!(assign.main_assign_allowed(&loaded), origin == ContextOrigin::DashboardSession);
        let cmd = frozen_command(f.dir.path(), "Read", serde_json::json!({"duduclaw": {}}));
        let result = crate::webchat::MAIN_CREDENTIAL_ORIGIN.scope(origin,
            crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(f.emp.clone(),
                check_final_main_spawn(f.dir.path(), Some(&dir), Some(&cmd)))).await;
        assert_eq!(result.is_ok(), origin == ContextOrigin::DashboardSession);
        if origin != ContextOrigin::DashboardSession { assert_eq!(result, Err(MAIN_TURN_REFUSAL.into())); }
    }
}

#[tokio::test]
async fn final_inputs_recheck_config_for_marked_main_turns() {
    let f = fixture(Some(EditionProfile::Enterprise));
    write_main(&f, "[capabilities]\nallowed_tools = ['Read']\n");
    std::fs::write(f.dir.path().join("config.toml"), "[general]\ndefault_agent = 'main'\n").unwrap();
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    let dir = f.dir.path().join("agents/main");
    assert!(listed_access_effective(f.dir.path(), "main").await.unwrap());
    let wire = format!("user:{}", f.emp);
    for channel in ["telegram:123", "googlechat:spaces/test"] {
        // Shared-app member routes explicitly carry the conversation marker,
        // even though they have no AUTHENTICATED_WEBCHAT_PRINCIPAL.
        let cmd = frozen_command(f.dir.path(), "Read,Bash", serde_json::json!({"duduclaw": {}}));
        let result = crate::on_behalf_of::scope_first(Some(wire.clone()),
            crate::claude_runner::REPLY_CHANNEL.scope(channel.to_owned(),
                crate::shared_app_route::member_turn(
                    check_final_main_spawn(f.dir.path(), Some(&dir), Some(&cmd))))).await;
        assert_eq!(result, Err(MAIN_TURN_REFUSAL.into()), "{channel}");
    }
    let cmd = frozen_command(f.dir.path(), "Read", serde_json::json!({"duduclaw": {}}));
    MAIN_TURN_USE.scope(MainUse::Assign, async {
        assert!(crate::on_behalf_of::scope_first(Some(wire.clone()),
            check_final_main_spawn(f.dir.path(), Some(&dir), Some(&cmd))).await.is_ok());
        // A changed MCP policy after admission is checked against the frozen file.
        write_main(&f, "[capabilities]\nallowed_tools = ['Read', 'mcp__duduclaw__*']\n");
        assert_eq!(crate::on_behalf_of::scope_first(Some(wire.clone()),
            check_final_main_spawn(f.dir.path(), Some(&dir), Some(&cmd))).await, Err(MAIN_TURN_REFUSAL.into()));
        assert_eq!(crate::on_behalf_of::scope_first(Some(wire),
            check_final_main_spawn(f.dir.path(), Some(&dir), None)).await, Err(MAIN_TURN_REFUSAL.into()));
        // An account's Admin role must never promote its member credential.
        let member_admin = crate::webchat::MAIN_CREDENTIAL_ORIGIN.scope(
            duduclaw_auth::acl::ContextOrigin::MemberKey,
            crate::on_behalf_of::scope_first(Some(format!("user:{}", f.admin)),
                check_final_main_spawn(f.dir.path(), Some(&dir), Some(&cmd)))).await;
        assert_eq!(member_admin, Err(MAIN_TURN_REFUSAL.into()));
        // Admin and established background work do not need this proof.
        assert!(crate::on_behalf_of::scope_first(Some(format!("user:{}", f.admin)),
            check_final_main_spawn(f.dir.path(), Some(&dir), None)).await.is_ok());
        assert!(check_final_main_spawn(f.dir.path(), Some(&dir), None).await.is_ok());
    }).await;
}

#[tokio::test]
async fn unlisted_channel_default_keeps_legacy_access_but_webchat_and_dispatch_refuse() {
    use duduclaw_auth::acl::ContextOrigin;
    let f = fixture(Some(EditionProfile::Enterprise));
    write_main(&f, "[capabilities]\nallowed_tools = ['Read', 'Bash']\n");
    std::fs::write(f.dir.path().join("config.toml"), "[general]\ndefault_agent = 'main'\n").unwrap();
    let dir = f.dir.path().join("agents/main");
    let loaded = AgentRegistry::load_agent(&dir).await.unwrap();
    let wire = format!("user:{}", f.emp);
    assert!(!f.db.is_main_agent_user(&f.emp).unwrap());
    for (channel, sender_id) in [("telegram", "42"), ("line", "U-main-regression")] {
        f.db.bind_channel_identity(&f.emp, channel, sender_id, true).unwrap();
        let sender = crate::trigger_route::access::for_channel_sender(f.dir.path(), channel, sender_id);
        assert!(matches!(&sender.sender, crate::trigger_route::Sender::Account { user_id, .. } if user_id == &f.emp));
        assert!(sender.webchat_scope.is_none());
        assert!(sender.may_call(&loaded, Some("main")));
        let cmd = frozen_command(f.dir.path(), "Read,Bash", serde_json::json!({"duduclaw": {}}));
        let result = crate::on_behalf_of::scope_first(Some(wire.clone()),
            crate::claude_runner::REPLY_CHANNEL.scope(format!("{channel}:conversation"),
                check_final_main_spawn(f.dir.path(), Some(&dir), Some(&cmd)))).await;
        assert_eq!(result, Ok(()), "ordinary {channel} default");
        assert!(crate::on_behalf_of::scope_first(Some(wire.clone()),
            check_final_main_spawn(f.dir.path(), Some(&dir), None)).await.is_ok());
    }
    // Use a shell-free command to prove these refusals are list admission,
    // not just a side effect of the command's shell capability.
    write_main(&f, "[capabilities]\nallowed_tools = ['Read']\n");
    let cmd = frozen_command(f.dir.path(), "Read", serde_json::json!({"duduclaw": {}}));
    let web = crate::webchat::MAIN_CREDENTIAL_ORIGIN.scope(ContextOrigin::DashboardSession,
        crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(f.emp.clone(),
            check_final_main_spawn(f.dir.path(), Some(&dir), Some(&cmd)))).await;
    assert_eq!(web, Err(MAIN_TURN_REFUSAL.into()));
    let dispatch = MAIN_TURN_USE.scope(MainUse::Assign,
        crate::on_behalf_of::scope_first(Some(wire.clone()),
            check_final_main_spawn(f.dir.path(), Some(&dir), Some(&cmd)))).await;
    assert_eq!(dispatch, Err(MAIN_TURN_REFUSAL.into()));
    let shared = crate::shared_app_route::member_turn(
        crate::on_behalf_of::scope_first(Some(wire.clone()),
            check_final_main_spawn(f.dir.path(), Some(&dir), Some(&cmd)))).await;
    assert_eq!(shared, Err(MAIN_TURN_REFUSAL.into()), "revoked shared-app list admission");
    assert!(MAIN_TURN_USE.try_with(|_| ()).is_err(), "marker must end with the turn");
    assert!(crate::on_behalf_of::scope_first(Some(wire),
        check_final_main_spawn(f.dir.path(), Some(&dir), None)).await.is_ok());
}

#[tokio::test]
async fn listed_webchat_final_bash_command_is_refused() {
    let f = fixture(Some(EditionProfile::Enterprise));
    write_main(&f, "[capabilities]\nallowed_tools = ['Read']\n");
    std::fs::write(f.dir.path().join("config.toml"), "[general]\ndefault_agent = 'main'\n").unwrap();
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    let dir = f.dir.path().join("agents/main");
    let cmd = frozen_command(f.dir.path(), "Read,Bash", serde_json::json!({"duduclaw": {}}));
    assert!(!duduclaw_core::execution_gate::isolation_required());
    let result = crate::webchat::MAIN_CREDENTIAL_ORIGIN.scope(
        duduclaw_auth::acl::ContextOrigin::DashboardSession,
        crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(f.emp.clone(),
            check_final_main_spawn(f.dir.path(), Some(&dir), Some(&cmd)))).await;
    assert_eq!(result, Err(MAIN_TURN_REFUSAL.into()));
}

#[tokio::test]
async fn non_main_final_target_does_not_read_unreadable_users_db() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[general]\ndefault_agent = 'main'\n").unwrap();
    crate::trigger_route::tests::write_agent(
        &home.path().join("agents"), "worker", "Worker", "", "specialist", "",
    );
    let dir = home.path().join("agents/worker");
    let db = home.path().join("users.db");
    std::fs::create_dir(&db).unwrap();
    for corrupt_file in [false, true] {
        if corrupt_file {
            std::fs::remove_dir(&db).unwrap();
            std::fs::write(&db, b"not a SQLite database").unwrap();
        }
        assert!(admitted_main_role(home.path(), "employee", MainUse::Converse).is_err());
        let web = crate::webchat::MAIN_CREDENTIAL_ORIGIN.scope(
            duduclaw_auth::acl::ContextOrigin::DashboardSession,
            crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope("employee".into(),
                check_final_main_spawn(home.path(), Some(&dir), None))).await;
        assert_eq!(web, Ok(()));
        let dispatch = MAIN_TURN_USE.scope(MainUse::Assign,
            crate::on_behalf_of::scope_first(Some("user:employee".into()),
                check_final_main_spawn(home.path(), Some(&dir), None))).await;
        assert_eq!(dispatch, Ok(()));
    }
}

#[tokio::test]
async fn listed_access_reads_never_materialize_or_delete_agent_artifacts() {
    fn snapshot(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        fn visit(dir: &Path, out: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() { out.insert(path.clone(), Vec::new()); visit(&path, out); }
                else { out.insert(path.clone(), std::fs::read(path).unwrap()); }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        visit(root, &mut out);
        out
    }
    let f = fixture(Some(EditionProfile::Enterprise));
    write_main(&f, "[capabilities]\nallowed_tools = ['Read']\n");
    let before = snapshot(f.dir.path());
    assert!(listed_access_effective(f.dir.path(), "main").await.unwrap());
    assert_eq!(snapshot(f.dir.path()), before);
    // Stale resolved files used to be deleted by AgentRegistry::load_agent.
    let resolved = duduclaw_core::preset::agent_resolved_path(f.dir.path(), "main");
    std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
    std::fs::write(&resolved, "[capabilities]\nallowed_tools = ['Read']\n").unwrap();
    let before = snapshot(f.dir.path());
    assert!(listed_access_effective(f.dir.path(), "main").await.is_err());
    assert_eq!(snapshot(f.dir.path()), before);
}
