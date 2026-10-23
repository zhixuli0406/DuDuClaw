use super::*;
use duduclaw_auth::{AccessLevel, UserContext, UserDb, UserRole, UserStatus};

struct Fx {
    home: tempfile::TempDir,
    db: UserDb,
    manager: String,
    employee: String,
}

impl Fx {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[people]\nmanager_team_history = true\n",
        )
        .unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let manager = db
            .create_user(
                "manager@test.invalid",
                "Manager",
                "isolated-password",
                UserRole::Manager,
            )
            .unwrap()
            .id;
        let employee = db
            .create_user(
                "employee@test.invalid",
                "Employee",
                "isolated-password",
                UserRole::Employee,
            )
            .unwrap()
            .id;
        db.set_manager(&employee, Some(&manager)).unwrap();
        db.bind_agent(&employee, "sales", AccessLevel::Operator)
            .unwrap();
        crate::team_edition::record(home.path(), duduclaw_core::EditionProfile::Enterprise);
        Self {
            home,
            db,
            manager,
            employee,
        }
    }

    fn context(&self) -> UserContext {
        self.db.live_user_context(&self.manager).unwrap().unwrap()
    }
}

#[test]
fn oversight_uses_live_status_and_all_qualifying_reports() {
    let f = Fx::new();
    let ctx = f.context();
    let permitted = can_view_team_history(f.home.path(), &f.db, &ctx, "sales").unwrap();
    assert!(permitted.reports.contains_key(&f.employee));
    assert!(permitted.since.timestamp_micros() > 0);
    f.db.set_manager(&f.employee, None).unwrap();
    assert!(matches!(
        can_view_team_history(f.home.path(), &f.db, &ctx, "sales"),
        Err(OversightDenied::NotInTeam)
    ));
}

#[test]
fn edition_setting_bad_config_and_viewer_bindings_deny() {
    let f = Fx::new();
    let ctx = f.context();
    crate::team_edition::record(f.home.path(), duduclaw_core::EditionProfile::Personal);
    assert!(matches!(
        can_view_team_history(f.home.path(), &f.db, &ctx, "sales"),
        Err(OversightDenied::Edition)
    ));
    crate::team_edition::record(f.home.path(), duduclaw_core::EditionProfile::Enterprise);
    for config in [
        "[people]\nmanager_team_history = false",
        "[people]\nmanager_team_history = 'bad'",
        "not valid toml",
    ] {
        std::fs::write(f.home.path().join("config.toml"), config).unwrap();
        assert!(matches!(
            can_view_team_history(f.home.path(), &f.db, &ctx, "sales"),
            Err(OversightDenied::SettingOff)
        ));
    }
    std::fs::write(f.home.path().join("config.toml"), "").unwrap();
    f.db.bind_agent(&f.employee, "sales", AccessLevel::Viewer)
        .unwrap();
    assert!(matches!(
        can_view_team_history(f.home.path(), &f.db, &ctx, "sales"),
        Err(OversightDenied::NotInTeam)
    ));
}

#[test]
fn inactive_manager_password_change_and_system_never_gain_oversight() {
    let f = Fx::new();
    let ctx = f.context();
    assert!(matches!(
        can_view_team_history(
            f.home.path(),
            &f.db,
            &UserContext::admin_fallback(),
            "sales"
        ),
        Err(OversightDenied::NotManager)
    ));
    let conn = rusqlite::Connection::open(f.home.path().join("users.db")).unwrap();
    conn.execute(
        "UPDATE users SET must_change_password = 1 WHERE id = ?1",
        [&f.manager],
    )
    .unwrap();
    assert!(matches!(
        can_view_team_history(f.home.path(), &f.db, &ctx, "sales"),
        Err(OversightDenied::PasswordChange)
    ));
    conn.execute(
        "UPDATE users SET must_change_password = 0 WHERE id = ?1",
        [&f.manager],
    )
    .unwrap();
    f.db.set_user_status(&f.manager, UserStatus::Suspended)
        .unwrap();
    assert!(can_view_team_history(f.home.path(), &f.db, &ctx, "sales").is_err());
}

#[test]
fn reenabling_and_edition_changes_start_a_new_history_window() {
    let f = Fx::new();
    let ctx = f.context();
    let first = can_view_team_history(f.home.path(), &f.db, &ctx, "sales")
        .unwrap()
        .since;
    std::fs::write(
        f.home.path().join("config.toml"),
        "[people]\nmanager_team_history = false",
    )
    .unwrap();
    assert!(can_view_team_history(f.home.path(), &f.db, &ctx, "sales").is_err());
    std::fs::write(
        f.home.path().join("config.toml"),
        "[people]\nmanager_team_history = true",
    )
    .unwrap();
    let next = can_view_team_history(f.home.path(), &f.db, &ctx, "sales")
        .unwrap()
        .since;
    assert!(next > first);
    crate::team_edition::record(f.home.path(), duduclaw_core::EditionProfile::Personal);
    assert!(can_view_team_history(f.home.path(), &f.db, &ctx, "sales").is_err());
    crate::team_edition::record(f.home.path(), duduclaw_core::EditionProfile::Enterprise);
    assert!(
        can_view_team_history(f.home.path(), &f.db, &ctx, "sales")
            .unwrap()
            .since
            > next
    );
}

#[test]
fn an_unwritable_transition_audit_never_activates_history() {
    let f = Fx::new();
    std::fs::create_dir(f.home.path().join("security_audit.jsonl")).unwrap();
    assert!(can_view_team_history(f.home.path(), &f.db, &f.context(), "sales").is_err());
}

#[test]
fn failed_disable_survives_reopen_and_never_revives_an_old_window() {
    let f = Fx::new();
    let first = can_view_team_history(f.home.path(), &f.db, &f.context(), "sales")
        .unwrap()
        .since;
    std::fs::remove_file(f.home.path().join("security_audit.jsonl")).unwrap();
    std::fs::create_dir(f.home.path().join("security_audit.jsonl")).unwrap();
    std::fs::write(
        f.home.path().join("config.toml"),
        "[people]\nmanager_team_history=false",
    )
    .unwrap();
    assert!(observe_window(f.home.path(), &f.db, "test").is_err());
    assert!(f.db.team_history_reset_required().unwrap());
    std::fs::remove_dir(f.home.path().join("security_audit.jsonl")).unwrap();
    std::fs::write(
        f.home.path().join("config.toml"),
        "[people]\nmanager_team_history=true",
    )
    .unwrap();
    let reopened = UserDb::new(&f.home.path().join("users.db")).unwrap();
    let current = can_view_team_history(f.home.path(), &reopened, &f.context(), "sales").unwrap();
    assert!(current.since > first);
}

#[test]
fn disclosure_notice_requires_active_manager_and_direct_operator_binding() {
    let f = Fx::new();
    let employee = f.db.live_user_context(&f.employee).unwrap().unwrap();
    assert!(manager_may_read(f.home.path(), &f.db, &employee, "sales"));
    f.db.bind_agent(&f.employee, "sales", duduclaw_auth::AccessLevel::Viewer)
        .unwrap();
    assert!(!manager_may_read(f.home.path(), &f.db, &employee, "sales"));
    f.db.bind_agent(&f.employee, "sales", duduclaw_auth::AccessLevel::Operator)
        .unwrap();
    f.db.set_user_status(&f.manager, UserStatus::Suspended)
        .unwrap();
    assert!(!manager_may_read(f.home.path(), &f.db, &employee, "sales"));
    f.db.set_user_status(&f.manager, UserStatus::Active)
        .unwrap();
    std::fs::write(
        f.home.path().join("config.toml"),
        "[people]\nmanager_team_history=false",
    )
    .unwrap();
    assert!(!manager_may_read(f.home.path(), &f.db, &employee, "sales"));
}

#[test]
fn a_failed_reset_write_latches_the_view_until_a_fresh_window_can_be_recorded() {
    let f = Fx::new();
    let first = can_view_team_history(f.home.path(), &f.db, &f.context(), "sales")
        .unwrap()
        .since;
    let conn = rusqlite::Connection::open(f.home.path().join("users.db")).unwrap();
    conn.execute_batch("CREATE TRIGGER deny_reset BEFORE INSERT ON auth_meta WHEN NEW.key='team_history_reset_required' BEGIN SELECT RAISE(ABORT,'injected reset failure'); END").unwrap();
    std::fs::write(
        f.home.path().join("config.toml"),
        "[people]\nmanager_team_history=false",
    )
    .unwrap();
    assert!(observe_window(f.home.path(), &f.db, "test_boot").is_err());
    std::fs::write(
        f.home.path().join("config.toml"),
        "[people]\nmanager_team_history=true",
    )
    .unwrap();
    assert!(can_view_team_history(f.home.path(), &f.db, &f.context(), "sales").is_err());
    conn.execute_batch("DROP TRIGGER deny_reset").unwrap();
    assert!(
        can_view_team_history(f.home.path(), &f.db, &f.context(), "sales")
            .unwrap()
            .since
            > first
    );
}
