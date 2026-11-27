use crate::{avatar::AvatarActor, User, UserDb, UserRole};
use duduclaw_core::agent_seats::{seat_holders, SeatAdmission, AVATAR_SEAT_REFUSAL};
use rusqlite::params;
use std::{cell::Cell, path::Path};

fn fixture() -> (tempfile::TempDir, UserDb, User, User) {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let admin = db.create_user("admin@signin.test", "Admin", "password123", UserRole::Admin).unwrap();
    let member = db.create_user("member@signin.test", "Member", "password123", UserRole::Employee).unwrap();
    db.conn().execute("INSERT INTO user_onboarding(user_id,source,avatar)
        VALUES(?1,'google_signin','pending')", [&member.id]).unwrap();
    (home, db, admin, member)
}

fn link(db: &UserDb, member: &str) {
    db.conn().execute("INSERT INTO google_signin_links
        (user_id,google_sub,hd,email_at_link,linked_via,linked_by,linked_at)
        VALUES(?1,'123456','signin.test','member@signin.test','jit_direct','system:google_signin',1)",
        [member]).unwrap();
}

fn publish(home: &Path, agent: &str) -> Result<(), String> {
    let dir = home.join("agents").join(agent);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("agent.toml"), format!("[agent]\nname='{agent}'\nstatus='active'\n"))
        .map_err(|e| e.to_string())
}

// Include all identity writes and onboarding progress in rollback assertions.
fn snapshot(db: &UserDb, member: &str) -> serde_json::Value {
    db.conn().query_row("SELECT avatar_agent_id,primary_agent_id,
        (SELECT avatar FROM user_onboarding WHERE user_id=?1),
        (SELECT COUNT(*) FROM user_agent_bindings),
        (SELECT COUNT(*) FROM isolation_personal_agents),
        (SELECT COUNT(*) FROM avatar_seat_grants),
        (SELECT COUNT(*) FROM auth_audit_log),
        (SELECT value FROM auth_meta WHERE key='acl_generation') FROM users WHERE id=?1",
        [member], |r| Ok(serde_json::json!([
            r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?, r.get::<_, i64>(6)?, r.get::<_, i64>(7)?,
        ]))).unwrap()
}

fn assert_created(db: &UserDb, member: &str, actor: &str, detail: &str) {
    let conn = db.conn();
    let identity: (String, String, String, String) = conn.query_row(
        "SELECT u.avatar_agent_id,u.primary_agent_id,b.access_level,p.owner_user_id FROM users u
        JOIN user_agent_bindings b ON b.user_id=u.id AND b.agent_name=u.avatar_agent_id
        JOIN isolation_personal_agents p ON p.agent_name=u.avatar_agent_id WHERE u.id=?1",
        [member], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
    assert_eq!(identity, ("avatar".into(), "avatar".into(), "owner".into(), member.into()));
    let audit: (String, String, String, Option<String>, String) = conn.query_row(
        "SELECT user_id,target,detail,ip,timestamp FROM auth_audit_log WHERE action='user.create_avatar'",
        [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap();
    assert_eq!((&*audit.0, &*audit.1, &*audit.2, audit.3), (actor, member, detail, None));
    chrono::DateTime::parse_from_rfc3339(&audit.4).unwrap();
}

#[test]
fn signin_actor_requires_link_and_policy() {
    let (home, db, _, member) = fixture();
    let admission = SeatAdmission::acquire(home.path()).unwrap();
    let calls = Cell::new(0);
    let policy = || { calls.set(calls.get() + 1); Ok(()) };
    let denied = || {
        let before = snapshot(&db, &member.id);
        let seats = seat_holders(home.path()).unwrap().len();
        assert_eq!(db.publish_new_avatar_under_as(
            AvatarActor::SigninPolicy { revision: "r1", policy_check: &policy },
            &member.id, "avatar", &admission, 1, |_| panic!("denial must precede publish"),
        ).unwrap_err(), "avatar_signin_required");
        assert_eq!(snapshot(&db, &member.id), before);
        assert_eq!(seat_holders(home.path()).unwrap().len(), seats);
        assert_eq!(calls.get(), 0);
    };
    denied(); // Google onboarding without a link.
    link(&db, &member.id);
    for source in [None, Some("google_signin_extra")] {
        db.conn().execute("UPDATE user_onboarding SET source=?1 WHERE user_id=?2",
            params![source, member.id]).unwrap();
        denied();
    }
    db.conn().execute("DELETE FROM user_onboarding WHERE user_id=?1", [&member.id]).unwrap();
    denied();
    db.conn().execute("INSERT INTO user_onboarding(user_id,source,avatar)
        VALUES(?1,'google_signin','pending')", [&member.id]).unwrap();
    let before = snapshot(&db, &member.id);
    let seats = seat_holders(home.path()).unwrap().len();
    assert_eq!(db.publish_new_avatar_under_as(
        AvatarActor::SigninPolicy { revision: "r1", policy_check: &|| Err("policy_changed".into()) },
        &member.id, "avatar", &admission, 1, |_| panic!("policy denial must precede publish"),
    ).unwrap_err(), "policy_changed");
    assert_eq!(snapshot(&db, &member.id), before);
    assert_eq!(seat_holders(home.path()).unwrap().len(), seats);
    assert!(!home.path().join("agents/avatar").exists());

    // Exercise JSON escaping and preserve the exact revision in the audit.
    let revision = "版本\"r1";
    let generation = db.acl_generation().unwrap();
    db.publish_new_avatar_under_as(
        AvatarActor::SigninPolicy { revision, policy_check: &policy },
        &member.id, "avatar", &admission, 1, |conn| {
            assert!(!conn.is_autocommit());
            assert_eq!(calls.get(), 1);
            publish(home.path(), "avatar")
        },
    ).unwrap();
    assert_created(&db, &member.id, "system:google_signin",
        &serde_json::json!({"agent": "avatar", "revision": revision}).to_string());
    assert_eq!(db.conn().query_row("SELECT avatar FROM user_onboarding WHERE user_id=?1",
        [&member.id], |r| r.get::<_, String>(0)).unwrap(), "done");
    assert!(db.acl_generation().unwrap() > generation);
    assert!(seat_holders(home.path()).unwrap().is_empty());
}

#[test]
fn signin_actor_cannot_convert_existing_employee() {
    let (home, db, admin, member) = fixture();
    link(&db, &member.id);
    let admission = SeatAdmission::acquire(home.path()).unwrap();
    publish(home.path(), "ordinary").unwrap();
    let before = snapshot(&db, &member.id);
    let admin_error = db.publish_new_avatar_under(&admin.id, &member.id, "ordinary", &admission, 1,
        |_| panic!("existing employee must not publish")).unwrap_err();
    assert_eq!(admin_error, "只能為成員建立新的 Personal AI，不能將一般員工改為免計名額分身。");
    assert_eq!(db.publish_new_avatar_under_as(
        AvatarActor::SigninPolicy { revision: "r1", policy_check: &|| panic!("seat check comes first") },
        &member.id, "ordinary", &admission, 1, |_| panic!("existing employee must not publish"),
    ).unwrap_err(), admin_error);
    assert_eq!(snapshot(&db, &member.id), before);
    assert_eq!(seat_holders(home.path()).unwrap().len(), 1);
}

#[test]
fn signin_actor_respects_seat_max() {
    let (home, db, _, member) = fixture();
    link(&db, &member.id);
    let admission = SeatAdmission::acquire(home.path()).unwrap();
    // Personal avatars are exempt, but an already over-cap home must still fail recount.
    publish(home.path(), "ordinary-a").unwrap();
    publish(home.path(), "ordinary-b").unwrap();
    let before = snapshot(&db, &member.id);
    let published = Cell::new(false);
    assert_eq!(db.publish_new_avatar_under_as(
        AvatarActor::SigninPolicy { revision: "r1", policy_check: &|| Ok(()) },
        &member.id, "avatar", &admission, 1, |_| {
            publish(home.path(), "avatar")?;
            published.set(true);
            Ok(())
        },
    ).unwrap_err(), AVATAR_SEAT_REFUSAL);
    assert!(published.get());
    assert_eq!(snapshot(&db, &member.id), before);
    assert_eq!(seat_holders(home.path()).unwrap().len(), 3, "rollback leaves no avatar exemption");
    // Match the existing caller contract: compensate only the newly published directory.
    std::fs::remove_dir_all(home.path().join("agents/avatar")).unwrap();
    assert_eq!(seat_holders(home.path()).unwrap().len(), 2);
}

#[test]
fn signin_actor_member_with_avatar_denied() {
    let (home, db, _, member) = fixture();
    link(&db, &member.id);
    let admission = SeatAdmission::acquire(home.path()).unwrap();
    db.publish_new_avatar_under_as(
        AvatarActor::SigninPolicy { revision: "r1", policy_check: &|| Ok(()) },
        &member.id, "avatar", &admission, 1, |_| publish(home.path(), "avatar"),
    ).unwrap();
    let before = snapshot(&db, &member.id);
    assert_eq!(db.publish_new_avatar_under_as(
        AvatarActor::SigninPolicy { revision: "r1", policy_check: &|| Ok(()) },
        &member.id, "second-avatar", &admission, 1, |_| panic!("member already has an avatar"),
    ).unwrap_err(), "成員必須啟用且尚未設定 AI 分身。");
    assert_eq!(snapshot(&db, &member.id), before);
    assert!(seat_holders(home.path()).unwrap().is_empty());
}

#[test]
fn admin_path_unchanged() {
    for source in [None, Some("google_signin"), Some("other")] {
        let (home, db, admin, member) = fixture();
        db.conn().execute("UPDATE user_onboarding SET source=?1 WHERE user_id=?2",
            params![source, member.id]).unwrap();
        let admission = SeatAdmission::acquire(home.path()).unwrap();
        let before = snapshot(&db, &member.id);
        assert_eq!(db.publish_new_avatar_under(&member.id, &member.id, "avatar", &admission, 1,
            |_| panic!("non-admin must not publish")).unwrap_err(), "avatar_admin_required");
        assert_eq!(snapshot(&db, &member.id), before);
        // Admin creation needs neither a sign-in link nor a policy callback.
        db.publish_new_avatar_under(&admin.id, &member.id, "avatar", &admission, 1,
            |_| publish(home.path(), "avatar")).unwrap();
        assert_created(&db, &member.id, &admin.id, "avatar");
        assert_eq!(db.conn().query_row("SELECT avatar FROM user_onboarding WHERE user_id=?1",
            [&member.id], |r| r.get::<_, String>(0)).unwrap(),
            if source == Some("google_signin") { "done" } else { "pending" });
        assert!(seat_holders(home.path()).unwrap().is_empty());
    }
}
