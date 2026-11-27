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
        assert_eq!(main_access_listed(f.dir.path(), &f.admin, use_), Ok(true));
        assert_eq!(main_access_listed(f.dir.path(), &f.emp, use_), Ok(false));
        assert_eq!(main_access_listed(f.dir.path(), "missing", use_), Ok(false));
    }
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    for use_ in USES {
        assert_eq!(main_access_listed(f.dir.path(), &f.emp, use_), Ok(true));
        assert_eq!(main_access_listed(f.dir.path(), &format!("{}-extra", f.emp), use_), Ok(false));
        assert_eq!(main_access_listed(f.dir.path(), &format!(" {} ", f.emp), use_), Ok(false));
    }
    f.db.set_main_agent_users(&f.admin, &[], true).unwrap();
    assert_eq!(main_access_listed(f.dir.path(), &f.emp, MainUse::Converse), Ok(false));
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
        assert_eq!(main_access_listed(f.dir.path(), &f.emp, use_), Ok(false));
        assert_eq!(main_access_listed(f.dir.path(), &f.admin, use_), Ok(false));
    }
}

#[test]
fn personal_downgrade_ignores_stale_rows_but_retains_admin() {
    let f = fixture(Some(EditionProfile::Enterprise));
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    crate::team_edition::record(f.dir.path(), EditionProfile::Personal);
    assert!(f.db.is_main_agent_user(&f.emp).unwrap());
    for use_ in USES {
        assert_eq!(main_access_listed(f.dir.path(), &f.emp, use_), Ok(false));
        assert_eq!(main_access_listed(f.dir.path(), &f.admin, use_), Ok(true));
    }
}

#[test]
fn unresolved_edition_ignores_list() {
    let f = fixture(None);
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    for use_ in USES {
        assert_eq!(main_access_listed(f.dir.path(), &f.emp, use_), Ok(false));
        assert_eq!(main_access_listed(f.dir.path(), &f.admin, use_), Ok(true));
    }
}

#[test]
fn missing_or_unreadable_identity_store_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(main_access_listed(dir.path(), "admin", MainUse::Converse).is_err());
    let path = dir.path().join("users.db");
    std::fs::create_dir(&path).unwrap();
    for use_ in USES {
        assert!(main_access_listed(dir.path(), "admin", use_).is_err());
        assert!(requires_main_restriction_for(dir.path(), Some("user:admin"), use_).is_err());
    }
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(path, b"not a SQLite database").unwrap();
    assert!(main_access_listed(dir.path(), "admin", MainUse::Converse).is_err());
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
        assert_eq!(requires_main_restriction_for(f.dir.path(), Some(&employee), use_), Ok(true));
        assert_eq!(requires_main_restriction_for(f.dir.path(), Some("user:missing"), use_), Ok(true));
        assert_eq!(requires_main_restriction_for(f.dir.path(),
            Some(&format!("  {employee}  ")), use_), Ok(true));
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
async fn listed_turns_require_isolation_even_without_shell() {
    let f = fixture(Some(EditionProfile::Enterprise));
    write_main(&f, "[capabilities]\nallowed_tools = ['Read']\ndenied_tools = ['Bash']\n");
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    for isolated in [false, true] {
        for use_ in USES {
            assert_eq!(main_turn_allowed_with_isolation(f.dir.path(), &f.emp, "main", use_, isolated).await,
                Ok(isolated));
            assert_eq!(main_access_allowed_with_isolation(f.dir.path(), &f.emp, use_, isolated), Ok(isolated));
            assert_eq!(main_turn_allowed_with_isolation(f.dir.path(), &f.admin, "missing", use_, isolated).await,
                Ok(true));
            for user in ["missing".to_owned(), format!("{}-extra", f.emp), format!(" {} ", f.emp)] {
                assert_eq!(main_turn_allowed_with_isolation(f.dir.path(), &user, "main", use_, isolated).await,
                    Ok(false));
            }
        }
        assert_eq!(main_tools::listed_access_effective_with_isolation(f.dir.path(), "main", isolated).await,
            Ok(isolated));
    }
    assert!(!duduclaw_core::execution_gate::isolation_required());
    for use_ in USES {
        assert_eq!(main_access_allowed(f.dir.path(), &f.emp, use_), Ok(false));
        assert_eq!(main_access_allowed(f.dir.path(), &f.admin, use_), Ok(true));
        assert_eq!(main_turn_allowed(f.dir.path(), &f.emp, "main", use_).await, Ok(false));
        assert_eq!(requires_main_restriction_for_target(f.dir.path(),
            Some(&format!("user:{}", f.emp)), "main", use_).await, Ok(true));
    }
    // Isolation never substitutes for Enterprise or an active list entry.
    crate::team_edition::record(f.dir.path(), EditionProfile::Personal);
    assert_eq!(main_entry_available_with_isolation(f.dir.path(), &f.emp, &[], true).await, Ok(false));
    assert_eq!(main_turn_allowed_with_isolation(f.dir.path(), &f.emp, "main", MainUse::Converse, true).await,
        Ok(false));
    crate::team_edition::record(f.dir.path(), EditionProfile::Enterprise);
    f.db.set_main_agent_users(&f.admin, &[], true).unwrap();
    assert_eq!(main_entry_available_with_isolation(f.dir.path(), &f.emp, &[], true).await, Ok(false));
    assert_eq!(main_turn_allowed_with_isolation(f.dir.path(), &f.emp, "main", MainUse::Assign, true).await,
        Ok(false));
}

#[tokio::test]
async fn loaded_conversation_assignment_and_visibility_refuse_without_isolation() {
    let f = fixture(Some(EditionProfile::Enterprise));
    write_main(&f, "[capabilities]\nallowed_tools = ['Read']\ndenied_tools = ['Bash']\n");
    std::fs::write(f.dir.path().join("config.toml"), "[general]\ndefault_agent = 'main'\n").unwrap();
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    let mut loaded = AgentRegistry::load_agent(&f.dir.path().join("agents/main")).await.unwrap();
    let wire = format!("user:{}", f.emp);
    assert_eq!(main_entry_available(f.dir.path(), &f.emp, &[loaded.clone()]).await, Ok(false));
    assert_eq!(main_entry_available(f.dir.path(), &f.admin, &[loaded.clone()]).await, Ok(true));
    assert_eq!(loaded_access_effective(&loaded), Ok(false));
    assert_eq!(main_tools::snapshot_access_effective(f.dir.path(), &[loaded.clone()]).await, Ok(false));
    for isolated in [false, true] {
        assert_eq!(main_entry_available_with_isolation(f.dir.path(), &f.emp, &[loaded.clone()], isolated).await,
            Ok(isolated));
        assert_eq!(main_entry_available_with_isolation(f.dir.path(), &f.admin, &[], isolated).await, Ok(true));
    }
    for use_ in USES {
        assert_eq!(check_loaded_main_target_for(f.dir.path(), Some(&wire), &loaded, use_).await,
            Err(MAIN_TURN_REFUSAL.into()));
        assert_eq!(check_main_target_for(f.dir.path(), Some(&wire), "main", use_).await,
            Err(MAIN_TURN_REFUSAL.into()));
        assert!(check_loaded_main_target_for(f.dir.path(), Some(&format!("user:{}", f.admin)), &loaded, use_)
            .await.is_ok());
    }
    assert_eq!(check_loaded_main_target_for_execution(f.dir.path(), Some(&wire), &loaded).await,
        Err(MAIN_TURN_REFUSAL.into()));
    assert_eq!(check_loaded_queue_main_target_for_execution(f.dir.path(), Some(&wire), &loaded, false).await,
        Err(MAIN_TURN_REFUSAL.into()));
    assert!(check_loaded_main_target(f.dir.path(), Some(&wire), &loaded).await.is_err());
    assert!(check_loaded_queue_main_target_for_execution(f.dir.path(), None, &loaded, false).await.is_err());
    assert!(check_loaded_queue_main_target_for_execution(f.dir.path(), None, &loaded, true).await.is_ok());
    // Both configured-default identities retain the main gate.
    loaded.config.agent.role = AgentRole::Specialist;
    loaded.config.agent.name = "renamed-main".into();
    assert_eq!(check_loaded_main_target_for(f.dir.path(), Some(&wire), &loaded, MainUse::Converse).await,
        Err(MAIN_TURN_REFUSAL.into()));
    loaded.dir = f.dir.path().join("agents/ordinary");
    assert!(check_loaded_main_target_for(f.dir.path(), Some(&wire), &loaded, MainUse::Converse).await.is_ok());
}

#[tokio::test]
async fn final_spawn_requires_isolation_and_preserves_identity_gates() {
    use duduclaw_auth::acl::ContextOrigin;
    let f = fixture(Some(EditionProfile::Enterprise));
    write_main(&f, "[capabilities]\nallowed_tools = ['Read']\ndenied_tools = ['Bash']\n");
    std::fs::write(f.dir.path().join("config.toml"), "[general]\ndefault_agent = 'main'\n").unwrap();
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    let dir = f.dir.path().join("agents/main");
    let cmd = frozen_command(f.dir.path(), "Read", serde_json::json!({"duduclaw": {}}));
    for isolated in [false, true] {
        for use_ in USES {
            for command in [None, Some(&cmd)] {
                let result = MAIN_TURN_USE.scope(use_,
                    crate::on_behalf_of::scope_first(Some(format!("user:{}", f.emp)),
                        check_final_main_spawn_with_isolation(f.dir.path(), Some(&dir), command, isolated))).await;
                assert_eq!(result, if isolated { Ok(()) } else { Err(MAIN_TURN_REFUSAL.into()) });
            }
            for user in [&f.emp, &f.admin] {
                let member = MAIN_TURN_USE.scope(use_,
                    crate::webchat::MAIN_CREDENTIAL_ORIGIN.scope(ContextOrigin::MemberKey,
                        crate::on_behalf_of::scope_first(Some(format!("user:{user}")),
                            check_final_main_spawn_with_isolation(f.dir.path(), Some(&dir), Some(&cmd), isolated)))).await;
                assert_eq!(member, Err(MAIN_TURN_REFUSAL.into()));
            }
            let admin = MAIN_TURN_USE.scope(use_,
                crate::on_behalf_of::scope_first(Some(format!("user:{}", f.admin)),
                    check_final_main_spawn_with_isolation(f.dir.path(), Some(&dir), None, isolated))).await;
            assert_eq!(admin, Ok(()));
            let unknown = MAIN_TURN_USE.scope(use_,
                crate::on_behalf_of::scope_first(Some("unknown:rejected-principal".into()),
                    check_final_main_spawn_with_isolation(f.dir.path(), Some(&dir), None, isolated))).await;
            assert_eq!(unknown, Err(MAIN_TURN_REFUSAL.into()));
        }
        for origin in [ContextOrigin::DashboardSession, ContextOrigin::MemberKey, ContextOrigin::Other] {
            let web = crate::webchat::MAIN_CREDENTIAL_ORIGIN.scope(origin,
                crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(f.emp.clone(),
                    check_final_main_spawn_with_isolation(f.dir.path(), Some(&dir), Some(&cmd), isolated))).await;
            assert_eq!(web, if isolated && origin == ContextOrigin::DashboardSession {
                Ok(())
            } else { Err(MAIN_TURN_REFUSAL.into()) });
        }
        // Member-key provenance alone must retain the check without a use marker.
        let member = crate::webchat::MAIN_CREDENTIAL_ORIGIN.scope(ContextOrigin::MemberKey,
            crate::on_behalf_of::scope_first(Some(format!("user:{}", f.emp)),
                check_final_main_spawn_with_isolation(f.dir.path(), Some(&dir), None, isolated))).await;
        assert_eq!(member, Err(MAIN_TURN_REFUSAL.into()));
        assert!(crate::on_behalf_of::scope_first(Some(format!("user:{}", f.emp)),
            check_final_main_spawn_with_isolation(f.dir.path(), Some(&dir), None, isolated)).await.is_ok());
    }
    assert!(MAIN_TURN_USE.try_with(|_| ()).is_err());
    // The final check still reads current list membership under isolation.
    f.db.set_main_agent_users(&f.admin, &[], true).unwrap();
    let revoked = MAIN_TURN_USE.scope(MainUse::Assign,
        crate::on_behalf_of::scope_first(Some(format!("user:{}", f.emp)),
            check_final_main_spawn_with_isolation(f.dir.path(), Some(&dir), Some(&cmd), true))).await;
    assert_eq!(revoked, Err(MAIN_TURN_REFUSAL.into()));
}

#[tokio::test]
async fn sender_refuses_shell_free_main_but_keeps_listed_refusal() {
    use duduclaw_auth::acl::ContextOrigin;
    let f = fixture(Some(EditionProfile::Enterprise));
    write_main(&f, "[capabilities]\nallowed_tools = ['Read']\ndenied_tools = ['Bash']\n");
    f.db.set_main_agent_users(&f.admin, &[f.emp.clone()], true).unwrap();
    let loaded = AgentRegistry::load_agent(&f.dir.path().join("agents/main")).await.unwrap();
    for origin in [ContextOrigin::MemberKey, ContextOrigin::Other, ContextOrigin::DashboardSession] {
        let sender = crate::trigger_route::SenderAccess::no_bindings(&f.emp).with_credential_origin(origin);
        let converse = sender.clone().for_webchat_converse(f.dir.path()).await;
        let assign = sender.for_assign(f.dir.path()).await;
        assert!(!converse.main_converse_allowed(&loaded));
        assert!(!assign.main_assign_allowed(&loaded));
        if origin == ContextOrigin::DashboardSession {
            assert_eq!(assign.main_assign_refusal(&loaded), Some(MAIN_TURN_REFUSAL));
        }
    }
}

fn frozen_command(home: &Path, tools: &str, servers: serde_json::Value) -> tokio::process::Command {
    let path = home.join("frozen-mcp.json");
    std::fs::write(&path, serde_json::to_vec(&serde_json::json!({"mcpServers": servers})).unwrap()).unwrap();
    let mut cmd = tokio::process::Command::new("claude");
    cmd.args(["--tools", tools, "--strict-mcp-config", "--mcp-config"]);
    cmd.arg(path);
    cmd
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
