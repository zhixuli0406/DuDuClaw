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
