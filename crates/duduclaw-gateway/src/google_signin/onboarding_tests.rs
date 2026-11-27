//! P11 integration coverage; compiled and run by the main session.
use std::{collections::BTreeMap, sync::Arc};
use duduclaw_auth::{JwtConfig, UserDb};
use duduclaw_auth::signin_members::{SigninApproval, SigninOutcome, SigninPolicy, VerifiedGoogleIdentity};
use rusqlite::{params, Connection};
use serde_json::json;
use super::{config, onboarding::{avatar_display_name, avatar_name, run_once, should_run}};
use crate::{company_templates, handlers::MethodHandler, protocol::WsFrame};

const NOW: i64 = 1_800_000_000;

struct Fixture {
    home: tempfile::TempDir,
    handler: MethodHandler,
    db: Arc<UserDb>,
    member: String,
}

impl Fixture {
    fn conn(&self) -> Connection {
        let conn = Connection::open(self.home.path().join("users.db")).unwrap();
        conn.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        conn
    }

    fn state(&self) -> (String, Option<String>, i64, Option<i64>, Option<String>) {
        self.conn().query_row("SELECT avatar,avatar_agent_name,avatar_attempts,avatar_next_at,avatar_error
            FROM user_onboarding WHERE user_id=?1", [&self.member],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap()
    }

    fn settings(&self, enabled: bool, template: &str, revision: i64) {
        let root = toml::toml! {
            [google_signin]
            enabled = enabled
            allowed_hd = ["company.test"]
            avatar_template = template
            avatar_template_revision = revision
            [google_signin.enabled_ack]
            by = "fixture-owner"
            at = "2026-11-27T00:00:00Z"
        };
        std::fs::write(self.home.path().join("config.toml"), toml::to_string(&root).unwrap()).unwrap();
        assert_eq!(config::read_strict(self.home.path()).enabled, enabled);
    }
}

async fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let member = match db.resolve_google_signin(true, &VerifiedGoogleIdentity {
        sub: "123456789".into(), email: "member@company.test".into(),
        hd: "company.test".into(), display_name: "新同仁".into(),
    }, &SigninPolicy {
        approval: SigninApproval::Direct, daily_cap: 20, settings_version: "fixture".into(),
        ack_by: "fixture-owner".into(), ack_at: "2026-11-27T00:00:00Z".into(),
    }, NOW, &|_| Ok(None)).unwrap() {
        SigninOutcome::Ok { user_id, created: true, .. } => user_id,
        other => panic!("unexpected provisioning outcome: {other:?}"),
    };
    let handler = MethodHandler::new(home.path().into()).await;
    handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"signin-onboarding-secret-32-bytes!!"))).await;
    handler.set_test_edition(Some(duduclaw_core::EditionProfile::Enterprise)).await;
    handler.set_agent_cap_fixture(Some(0)).await;
    let f = Fixture { home, handler, db, member };
    f.settings(true, "", 0);
    f
}

#[tokio::test]
async fn idempotent_when_avatar_exists() {
    let f = fixture().await;
    f.conn().execute("UPDATE users SET avatar_agent_id='already-present' WHERE id=?1", [&f.member]).unwrap();
    // Completion also repairs jobs that are waiting or not due yet.
    f.conn().execute("UPDATE user_onboarding SET avatar='waiting_admin',avatar_next_at=?2,
        avatar_error='failed' WHERE user_id=?1", params![f.member,NOW+9999]).unwrap();
    run_once(&f.handler, NOW).await;
    assert_eq!(f.state().0, "done");
    assert_eq!(f.state().4, None);
    let state = f.state();
    run_once(&f.handler, NOW+60).await;
    assert_eq!(f.state(), state);
    assert_eq!(f.db.avatar_agent(&f.member).unwrap().as_deref(), Some("already-present"));
    assert!(!f.home.path().join("agents/already-present").exists());
}

#[tokio::test]
async fn template_revision_mismatch_waits() {
    let f = fixture().await;
    let item = company_templates::create(f.home.path(), "Company assistant", "",
        include_str!("../../../../templates/evaluator/agent.toml"), "Helpful assistant", "",
        BTreeMap::new(), false, vec![]).unwrap();
    f.settings(true, &format!("company:{}", item.id), item.revision as i64+1);
    run_once(&f.handler, NOW).await;
    assert_eq!(f.state().0, "waiting_admin");
    assert_eq!(f.state().4.as_deref(), Some("template_changed"));
    assert_eq!(f.state().2, 0);
    assert_eq!(f.db.avatar_agent(&f.member).unwrap(), None);
}

#[tokio::test]
async fn seats_full_waits() {
    let f = fixture().await;
    for name in ["ordinary-one", "ordinary-two"] {
        let frame = f.handler.create_agent_files(json!({"name":name}), None).await;
        assert!(matches!(frame, WsFrame::Response { ok: true, .. }));
    }
    f.handler.set_agent_cap_fixture(Some(1)).await;
    run_once(&f.handler, NOW).await;
    assert_eq!(f.state().0, "waiting_admin");
    assert_eq!(f.state().4.as_deref(), Some("seats_full"));
    assert_eq!(f.state().2, 0);
    assert_eq!(f.db.avatar_agent(&f.member).unwrap(), None);
}

#[tokio::test]
async fn retry_backoff_then_waiting_admin() {
    let f = fixture().await;
    // Abort inside the real publication transaction. This exercises the
    // actual Failed mapping and leaves no partially published member avatar.
    f.conn().execute_batch("CREATE TRIGGER reject_avatar_audit BEFORE INSERT ON auth_audit_log
        WHEN NEW.action='user.create_avatar' BEGIN SELECT RAISE(ABORT,'fixture publish failure'); END;").unwrap();
    for (at, attempts, next) in [(NOW,1,Some(NOW+60)), (NOW+60,2,Some(NOW+660)), (NOW+660,3,None)] {
        run_once(&f.handler, at).await;
        let state = f.state();
        assert_eq!(state.0, if attempts == 3 { "waiting_admin" } else { "retry" });
        assert_eq!(state.2, attempts);
        assert_eq!(state.3, next);
        assert_eq!(state.4.as_deref(), Some("failed"));
        assert_eq!(f.db.avatar_agent(&f.member).unwrap(), None);
        if let Some(next) = next {
            run_once(&f.handler, next-1).await;
            assert_eq!(f.state(), state);
        }
    }
    let state = f.state();
    run_once(&f.handler, NOW+99999).await;
    assert_eq!(f.state(), state);
}

#[tokio::test]
async fn disabled_setting_does_nothing() {
    let f = fixture().await;
    f.settings(false, "", 0);
    f.conn().execute("INSERT INTO google_signin_requests
        (id,google_sub,email,hd,display_name,status,requested_at,expires_at)
        VALUES('old','987654321','pending@company.test','company.test','Pending','pending',?1,?2)",
        params![NOW-100,NOW]).unwrap();
    let before = f.state();
    run_once(&f.handler, NOW).await;
    assert_eq!(f.state(), before);
    assert_eq!(f.db.avatar_agent(&f.member).unwrap(), None);
    let expired: String = f.conn().query_row("SELECT status FROM google_signin_requests WHERE id='old'",
        [], |r| r.get(0)).unwrap();
    assert_eq!(expired, "expired");
}

#[test]
fn only_lock_holder_runs() {
    let home = tempfile::tempdir().unwrap();
    assert!(!should_run(home.path()));
    // Merely having a lock pathname is not possession of the gateway lock.
    std::fs::create_dir(home.path().join("locks")).unwrap();
    std::fs::write(home.path().join("locks/gateway.lock"), "").unwrap();
    assert!(!should_run(home.path()));
}

#[tokio::test]
async fn orphan_directory_not_deleted_and_new_name_used() {
    let f = fixture().await;
    let old_name = "avatar-member-old1";
    f.db.signin_avatar_reserve(&f.member, old_name).unwrap();
    f.db.signin_avatar_creating(&f.member, old_name).unwrap();
    let orphan = f.home.path().join("agents").join(old_name);
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join("keep"), "earlier attempt").unwrap();
    run_once(&f.handler, NOW).await;
    assert_eq!(std::fs::read_to_string(orphan.join("keep")).unwrap(), "earlier attempt");
    assert_eq!(std::fs::read_dir(&orphan).unwrap().count(), 1);
    let state = f.state();
    assert_eq!(state.0, "done");
    assert_ne!(state.1.as_deref(), Some(old_name));
    assert_eq!(state.4.as_deref(), Some("orphan:avatar-member-old1"));
    assert_eq!(f.db.avatar_agent(&f.member).unwrap(), state.1);
}

#[test]
fn name_shape() {
    for (email, prefix) in [
        ("Member@company.test", "avatar-member-"),
        ("A.B+Tag@company.test", "avatar-a-b-tag-"),
        ("..A+++B..@company.test", "avatar-a-b-"),
        ("同仁@company.test", "avatar-member-"),
        ("同A仁B@company.test", "avatar-a-b-"),
        ("---@company.test", "avatar-member-"),
        ("@company.test", "avatar-member-"),
    ] {
        let name = avatar_name(email);
        assert!(duduclaw_core::is_valid_agent_id(&name));
        let suffix = name.strip_prefix(prefix).unwrap();
        assert_eq!(suffix.chars().count(), 4);
        assert!(suffix.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    }
    let name = avatar_name(&format!("{}@company.test", "A".repeat(100)));
    assert_eq!(name.chars().count(), 7+40+1+4);
    assert!(!name.contains("--"));
    let display = avatar_display_name("\0新\n同仁\t");
    assert_eq!(display, "新同仁 的 AI 分身");
    let display = avatar_display_name(&"同仁🦀".repeat(100));
    assert_eq!(display.chars().count(), 200);
    assert!(!display.chars().any(char::is_control));
}
