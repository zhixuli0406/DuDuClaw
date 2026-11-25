use super::*;
use rusqlite::Connection;

fn fixture() -> (tempfile::TempDir, Connection) {
    let home = tempfile::tempdir().unwrap();
    let db = crate::private_sqlite::open(home.path(), "users.db", crate::private_sqlite::Access::Create).unwrap().unwrap();
    db.execute_batch("CREATE TABLE users(id TEXT PRIMARY KEY, status TEXT, avatar_agent_id TEXT);
        CREATE TABLE user_agent_bindings(user_id TEXT, agent_name TEXT, access_level TEXT);
        CREATE TABLE google_employee_incarnations(agent_id TEXT PRIMARY KEY, nonce TEXT);
        CREATE TABLE avatar_seat_grants(agent_name TEXT PRIMARY KEY, owner_user_id TEXT, nonce TEXT);
        INSERT INTO google_employee_incarnations VALUES('avatar','nonce');
        INSERT INTO avatar_seat_grants VALUES('avatar','member','nonce');
        CREATE TABLE isolation_personal_agents(agent_name TEXT PRIMARY KEY, owner_user_id TEXT);
        INSERT INTO users VALUES('member','active','avatar');
        INSERT INTO user_agent_bindings VALUES('member','avatar','owner');
        INSERT INTO isolation_personal_agents VALUES('avatar','member');").unwrap();
    write_agent(home.path(), "avatar", "active");
    (home, db)
}
fn write_agent(home: &Path, id: &str, status: &str) {
    let dir = home.join("agents").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), format!("[agent]\nname = '{id}'\nstatus = '{status}'\nkind = 'avatar'\navatar = true\n")).unwrap();
}
#[test]
fn exemption_requires_active_unique_member_owner_and_personal_authority() {
    let (home, db) = fixture();
    assert!(seat_holders(home.path()).unwrap().is_empty());
    let seat = resolve_agent_seat(home.path(), "avatar").unwrap().unwrap();
    assert!(seat.is_avatar_exempt());
    assert!(!seat.is_deleted());
    for (invalidate, restore) in [
        ("UPDATE google_employee_incarnations SET nonce='replacement'", "UPDATE google_employee_incarnations SET nonce='nonce'"),
        ("UPDATE user_agent_bindings SET access_level='operator'", "UPDATE user_agent_bindings SET access_level='owner'"),
        ("UPDATE users SET status='suspended'", "UPDATE users SET status='active'"),
        ("UPDATE users SET avatar_agent_id=NULL", "UPDATE users SET avatar_agent_id='avatar'"),
        ("UPDATE isolation_personal_agents SET owner_user_id='other'", "UPDATE isolation_personal_agents SET owner_user_id='member'"),
        ("INSERT INTO users VALUES('other','active','avatar')", "DELETE FROM users WHERE id='other'"),
        ("INSERT INTO user_agent_bindings VALUES('other','avatar','operator')", "DELETE FROM user_agent_bindings WHERE user_id='other'"),
    ] {
        db.execute_batch(invalidate).unwrap();
        assert_eq!(seat_holders(home.path()).unwrap().len(), 1, "{invalidate}");
        db.execute_batch(restore).unwrap();
    }
    write_agent(home.path(), "avatar", "archived");
    assert_eq!(seat_holders(home.path()).unwrap().len(), 1);
    let archived = resolve_agent_seat(home.path(), "avatar").unwrap().unwrap();
    assert!(archived_avatar_will_exempt(home.path(), &archived));
    db.execute("UPDATE user_agent_bindings SET access_level='operator'", []).unwrap();
    assert!(!archived_avatar_will_exempt(home.path(), &archived));
}
#[test]
fn forged_profile_and_unreadable_authority_never_exempt() {
    let (home, db) = fixture();
    db.execute("DELETE FROM isolation_personal_agents", []).unwrap();
    assert_eq!(seat_holders(home.path()).unwrap().len(), 1);
    db.execute("DROP TABLE users", []).unwrap();
    assert_eq!(seat_holders(home.path()).unwrap().len(), 1);
    std::fs::write(home.path().join("agents/avatar/agent.toml"), "broken TOML").unwrap();
    assert_eq!(seat_holders(home.path()).unwrap().len(), 1);
}
#[test]
fn transaction_recount_sees_pending_unset_and_rolls_back() {
    let (home, mut db) = fixture();
    write_agent(home.path(), "ordinary", "active");
    let admission = SeatAdmission::acquire(home.path()).unwrap();
    let tx = db.transaction().unwrap();
    tx.execute("UPDATE users SET avatar_agent_id=NULL", []).unwrap();
    assert!(admission.recount_in(&tx, 1).is_err());
    drop(tx);
    assert_eq!(seat_holders(home.path()).unwrap().len(), 1);
}

#[test]
fn last_ordinary_seat_race_ignores_avatar_under_shared_lock() {
    let (home, _db) = fixture();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut workers = Vec::new();
    for id in ["first", "second"] {
        let home = home.path().to_path_buf();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            let admission = SeatAdmission::acquire(&home).unwrap();
            if matches!(admission.decide(1, 1, None), SeatDecision::Admitted) {
                write_agent(&home, id, "active");
                admission.recount(1).unwrap();
                true
            } else { false }
        }));
    }
    assert_eq!(workers.into_iter().filter_map(|w| w.join().ok()).filter(|won| *won).count(), 1);
    assert_eq!(seat_holders(home.path()).unwrap().len(), 1);
}
