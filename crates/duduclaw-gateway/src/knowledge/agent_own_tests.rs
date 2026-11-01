use super::*;

fn table(text: &str) -> toml::Table {
    text.parse().unwrap()
}

#[test]
fn bind_slug_leaves_every_granted_alone() {
    let mut t = table("[agent]\nname = \"a1\"\n");
    assert_eq!(bind_slug(&mut t, "agent-a1"), Ok(false));
    assert!(t.get("knowledge").is_none());
    let mut t = table("[knowledge]\nauto_retrieve = true\n");
    assert_eq!(bind_slug(&mut t, "agent-a1"), Ok(false));
    assert!(t["knowledge"].get("collections").is_none());
}

#[test]
fn bind_slug_appends_to_an_explicit_list_once() {
    let mut t = table("[knowledge]\ncollections = [\"hr\"]\ntop_k = 3\n");
    assert_eq!(bind_slug(&mut t, "agent-a1"), Ok(true));
    assert_eq!(
        t["knowledge"]["collections"],
        toml::Value::Array(vec!["hr".into(), "agent-a1".into()])
    );
    assert_eq!(t["knowledge"]["top_k"].as_integer(), Some(3));
    assert_eq!(bind_slug(&mut t, "agent-a1"), Ok(false));
    // Off ([]) becomes just this collection.
    let mut t = table("[knowledge]\ncollections = []\n");
    assert_eq!(bind_slug(&mut t, "agent-a1"), Ok(true));
    assert_eq!(
        t["knowledge"]["collections"],
        toml::Value::Array(vec!["agent-a1".into()])
    );
}

#[test]
fn bind_slug_refuses_what_it_cannot_read_or_fit() {
    for bad in [
        "knowledge = 3\n",
        "[knowledge]\ncollections = \"hr\"\n",
        "[knowledge]\ncollections = [1]\n",
    ] {
        let mut t = table(bad);
        let before = t.clone();
        assert!(bind_slug(&mut t, "agent-a1").is_err(), "{bad}");
        assert_eq!(t, before, "nothing changed for {bad}");
    }
    let full: Vec<String> = (0..MAX_BOUND_COLLECTIONS)
        .map(|i| format!("\"c{i}\""))
        .collect();
    let mut t = table(&format!(
        "[knowledge]\ncollections = [{}]\n",
        full.join(",")
    ));
    assert!(bind_slug(&mut t, "agent-a1").is_err());
}

#[test]
fn default_readers_follow_bindings_and_skip_inactive_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(UserDb::new(&dir.path().join("users.db")).unwrap());
    let mk = |email: &str| {
        db.create_user(
            email,
            email,
            "isolated-test-password",
            duduclaw_auth::models::UserRole::Employee,
        )
        .unwrap()
        .id
    };
    let (o, op, v, gone) = (
        mk("o@x.invalid"),
        mk("op@x.invalid"),
        mk("v@x.invalid"),
        mk("g@x.invalid"),
    );
    db.bind_agent(&o, "a1", AccessLevel::Owner).unwrap();
    db.bind_agent(&op, "a1", AccessLevel::Operator).unwrap();
    db.bind_agent(&v, "a1", AccessLevel::Viewer).unwrap();
    db.bind_agent(&gone, "a1", AccessLevel::Owner).unwrap();
    db.set_user_status(&gone, UserStatus::Suspended).unwrap();
    let got = default_readers(dir.path(), Some(&db), "a1", false);
    let mut want = vec![
        (PrincipalKind::User, o.clone(), Role::Owner),
        (PrincipalKind::User, op.clone(), Role::Viewer),
        (PrincipalKind::User, v.clone(), Role::Viewer),
    ];
    want.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(got, want);
    assert!(
        default_readers(dir.path(), Some(&db), "a1", true).is_empty(),
        "Personal: no people"
    );
    assert!(default_readers(dir.path(), None, "a1", false).is_empty());
    assert!(default_readers(dir.path(), Some(&db), "a2", false).is_empty());
}

#[test]
fn restricted_default_readers_include_only_active_admins_without_changing_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let h = dir.path();
    std::fs::create_dir_all(h.join("agents/sales")).unwrap();
    let db = Arc::new(UserDb::new(&h.join("users.db")).unwrap());
    let admin = db.create_user("admin@test.invalid", "Admin", "fixture-password", duduclaw_auth::UserRole::Admin).unwrap();
    let employee = db.create_user("owner@test.invalid", "Owner", "fixture-password", duduclaw_auth::UserRole::Employee).unwrap();
    db.bind_agent(&admin.id, "sales", AccessLevel::Owner).unwrap();
    db.bind_agent(&employee.id, "sales", AccessLevel::Owner).unwrap();
    let old = default_readers(h, Some(&db), "sales", false);
    assert_eq!(old.len(), 2);
    duduclaw_core::preset::install_system_operator_preset(h, false).unwrap();
    duduclaw_core::preset::bind(h, "sales", &h.join("agents/sales"), "system-operator", "test", "test").unwrap();
    assert_eq!(default_readers(h, Some(&db), "sales", false), vec![(PrincipalKind::User, admin.id, Role::Owner)]);
    assert_eq!(db.get_agent_users("sales").unwrap().len(), 2);
    duduclaw_core::preset::unbind(h, "sales", "test", "test").unwrap();
    assert_eq!(default_readers(h, Some(&db), "sales", false), old);
}
